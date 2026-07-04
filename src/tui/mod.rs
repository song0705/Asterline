//! Chat-first terminal UI: a single scrolling conversation column, a bottom
//! composer, and overlay drawers (logs / team / command palette). State is
//! driven entirely by `RuntimeEvent`s; no string matching.

pub mod app_state;
pub mod attach;
pub mod chat_view;
pub mod commands;
pub mod completion;
pub mod composer;
pub mod drawer_view;
pub mod drawers;
pub mod header;
pub mod keymap;
pub mod markdown;
pub mod rollout_import;
pub mod status_indicator;
pub mod team_builder;
pub mod team_editor;
pub mod theme;
pub mod workflow_view;

use std::io::{self, Write};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEvent, KeyEventKind,
    KeyboardEnhancementFlags, MouseEvent, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::domain::event::{RuntimeEvent, UiCommand};
use crate::domain::team::BackendKind;
use crate::runtime::RuntimeHandle;
use crate::tui::app_state::AppState;
use crate::tui::commands::Submission;
use crate::tui::keymap::Action;
use crate::tui::team_editor::TeamEditorOutcome;

const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Run the TUI to completion. `events` delivers runtime events; `handle` sends
/// commands back. On exit the runtime is asked to shut down.
pub fn run(
    handle: RuntimeHandle,
    events: Receiver<RuntimeEvent>,
    mut state: AppState,
) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    let keyboard_enhancement = enable_keyboard_enhancement(&mut stdout)?;
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(
        &mut terminal,
        &mut state,
        &handle,
        &events,
        keyboard_enhancement,
    );

    disable_raw_mode()?;
    disable_keyboard_enhancement(terminal.backend_mut(), keyboard_enhancement)?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    handle.send(UiCommand::Shutdown);
    result
}

fn keyboard_enhancement_flags() -> KeyboardEnhancementFlags {
    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
}

fn enable_keyboard_enhancement(out: &mut impl Write) -> io::Result<bool> {
    if supports_keyboard_enhancement().unwrap_or(false) {
        execute!(
            out,
            PushKeyboardEnhancementFlags(keyboard_enhancement_flags())
        )?;
        Ok(true)
    } else {
        Ok(false)
    }
}

fn disable_keyboard_enhancement(out: &mut impl Write, enabled: bool) -> io::Result<()> {
    if enabled {
        execute!(out, PopKeyboardEnhancementFlags)?;
    }
    Ok(())
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut AppState,
    handle: &RuntimeHandle,
    events: &Receiver<RuntimeEvent>,
    keyboard_enhancement: bool,
) -> io::Result<()> {
    loop {
        while let Ok(event) = events.try_recv() {
            state.apply(event);
        }

        terminal.draw(|frame| chat_view::render(frame, state))?;

        if event::poll(POLL_INTERVAL)? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if handle_team_editor_key(key, state, handle) {
                        continue;
                    }
                    if let Some(action) = keymap::resolve(key) {
                        handle_action(action, state, handle);
                    }
                }
                Event::Mouse(mouse) => handle_mouse(mouse, state),
                _ => {}
            }
        }

        if let Some(req) = state.take_attach_request() {
            attach_to_member(terminal, state, handle, &req, keyboard_enhancement)?;
        }

        if state.should_quit() {
            return Ok(());
        }
    }
}

fn handle_team_editor_key(key: KeyEvent, state: &mut AppState, handle: &RuntimeHandle) -> bool {
    match state.handle_team_editor_key(key.code, key.modifiers) {
        TeamEditorOutcome::Ignored => false,
        TeamEditorOutcome::Consumed(command) => {
            if let Some(command) = command {
                handle.send(command);
            }
            true
        }
        TeamEditorOutcome::Close => {
            state.close_drawer();
            true
        }
    }
}

/// Mouse wheel scrolls the conversation (or the open drawer), a few lines per
/// tick. Real mouse capture means the wheel no longer arrives as ↑/↓ arrow keys
/// (which now recall prompt history).
fn handle_mouse(mouse: MouseEvent, state: &mut AppState) {
    const STEP: usize = 3;
    let up = match mouse.kind {
        MouseEventKind::ScrollUp => true,
        MouseEventKind::ScrollDown => false,
        _ => return,
    };
    for _ in 0..STEP {
        match (state.drawer().is_some(), up) {
            (true, true) => state.drawer_scroll_up(),
            (true, false) => state.drawer_scroll_down(),
            (false, true) => state.scroll_up(),
            (false, false) => state.scroll_down(),
        }
    }
}

/// Hand the whole terminal to the member's real interactive CLI (resuming its
/// session), then restore Asterline when that CLI exits.
fn attach_to_member(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut AppState,
    handle: &RuntimeHandle,
    req: &attach::AttachRequest,
    keyboard_enhancement: bool,
) -> io::Result<()> {
    let (program, args) = req.command();

    // Snapshot the codex rollout so we can import whatever is typed during the
    // attached session once it exits.
    let snapshot = (req.backend == BackendKind::Codex)
        .then(|| rollout_import::snapshot(req.session.as_deref()));

    // --- Suspend Asterline: hand the real terminal to the child CLI. ---
    // Restore the cooked terminal, leave our alternate screen, and show the
    // cursor, flushing so the child starts from a clean, owned main screen.
    disable_raw_mode()?;
    let mut out = io::stdout();
    disable_keyboard_enhancement(&mut out, keyboard_enhancement)?;
    execute!(
        out,
        DisableMouseCapture,
        LeaveAlternateScreen,
        crossterm::cursor::Show
    )?;
    writeln!(
        out,
        "\n── {} · {} {} ──  (exit the session to return to Asterline)\n",
        req.display_name,
        program,
        args.join(" ")
    )?;
    out.flush()?;

    let result = std::process::Command::new(&program)
        .args(&args)
        .current_dir(&req.cwd)
        .status();

    // --- Resume Asterline: re-enter the alternate screen and repaint. ---
    enable_raw_mode()?;
    if keyboard_enhancement {
        execute!(
            out,
            PushKeyboardEnhancementFlags(keyboard_enhancement_flags())
        )?;
    }
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    out.flush()?;
    // Drop input the child or the terminal left buffered (e.g. the reply to the
    // alternate-screen switch) so the first key after returning isn't a stray
    // escape sequence.
    while event::poll(Duration::from_secs(0))? {
        let _ = event::read()?;
    }
    // Discard ratatui's cached screen contents so the next draw is a full
    // repaint over whatever the child CLI left behind.
    terminal.clear()?;

    match result {
        Ok(_) => state.apply(RuntimeEvent::Notice(format!(
            "returned from {}",
            req.display_name
        ))),
        Err(err) => state.apply(RuntimeEvent::Notice(format!(
            "could not launch {program}: {err}"
        ))),
    }

    // Import any messages exchanged in the attached session so they appear in
    // (and persist to) the Asterline transcript. The runtime records them and
    // emits the events the main loop renders.
    if let Some(snapshot) = snapshot {
        let imported = rollout_import::imported_since(snapshot);
        if !imported.is_empty() {
            handle.send(UiCommand::ImportTranscript {
                member: req.member.clone(),
                items: imported,
            });
        }
    }
    Ok(())
}

fn handle_action(action: Action, state: &mut AppState, handle: &RuntimeHandle) {
    if action != Action::Interrupt {
        state.disarm_quit();
    }
    // Reverse history search (Ctrl+R) captures input until accepted/cancelled.
    if state.in_history_search() {
        handle_search_action(action, state);
        return;
    }
    if action == Action::InsertChar('x') && state.toggle_workflow_runs_detail() {
        return;
    }
    match action {
        Action::InsertChar(ch) => state.insert_char(ch),
        Action::InsertNewline => state.insert_newline(),
        Action::Backspace => state.backspace(),
        Action::DeleteWord => state.delete_word(),
        Action::ClearLine => state.clear_composer(),
        Action::CursorLeft => {
            if state.drawer() == Some(drawers::Drawer::Runs) {
                state.select_older_workflow_run();
            } else if state.header_selected().is_some() {
                state.select_prev_member();
            } else {
                state.cursor_left();
            }
        }
        Action::CursorRight => {
            if state.drawer() == Some(drawers::Drawer::Runs) {
                state.select_newer_workflow_run();
            } else if state.header_selected().is_some() {
                state.select_next_member();
            } else {
                state.cursor_right();
            }
        }
        Action::Home => state.cursor_home(),
        Action::End => state.cursor_end(),
        Action::ScrollUp => {
            if state.drawer().is_some() {
                state.drawer_scroll_up();
            } else if state.completion().is_some() {
                state.popup_up();
            } else {
                state.scroll_up();
            }
        }
        Action::ScrollDown => {
            if state.drawer().is_some() {
                state.drawer_scroll_down();
            } else if state.completion().is_some() {
                state.popup_down();
            } else {
                state.scroll_down();
            }
        }
        Action::HistoryPrev => {
            if state.drawer() == Some(drawers::Drawer::Runs) {
                if !state.select_previous_workflow_step() {
                    state.select_newer_workflow_run();
                }
            } else if state.drawer().is_some() {
                state.drawer_scroll_up();
            } else if state.completion().is_some() {
                state.popup_up();
            } else if !state.composer_up() {
                // Already on the first composer line — recall older history.
                state.history_prev();
            }
        }
        Action::HistoryNext => {
            if state.drawer() == Some(drawers::Drawer::Runs) {
                if !state.select_next_workflow_step() {
                    state.select_older_workflow_run();
                }
            } else if state.drawer().is_some() {
                state.drawer_scroll_down();
            } else if state.completion().is_some() {
                state.popup_down();
            } else if !state.composer_down() {
                // Already on the last composer line — recall newer history.
                state.history_next();
            }
        }
        Action::ToggleLogs => state.toggle_drawer(drawers::Drawer::Logs),
        Action::TogglePalette => state.toggle_drawer(drawers::Drawer::Palette),
        Action::HistorySearch => state.start_history_search(),
        Action::ToggleExpand => state.toggle_tools_expansion(),
        Action::NextMember => state.select_next_member(),
        Action::PrevMember => state.select_prev_member(),
        Action::Complete => {
            if state.drawer() == Some(drawers::Drawer::Runs)
                && state.stage_selected_workflow_dispatch()
            {
                return;
            }
            state.accept_completion();
        }
        Action::CloseOverlay => {
            if state.completion().is_some() {
                state.dismiss_popup();
            } else if state.header_selected().is_some() {
                state.clear_header_selection();
            } else if state.drawer().is_some() {
                state.close_drawer();
            }
        }
        Action::Interrupt => {
            if state.running_count() > 0 {
                state.disarm_quit();
                handle.send(UiCommand::Cancel { member: None });
            } else if !state.composer().is_empty() {
                state.clear_composer();
            } else {
                state.request_quit();
            }
        }
        Action::Submit => {
            if state.drawer() == Some(drawers::Drawer::Runs)
                && state.stage_selected_workflow_action()
            {
                return;
            }
            // With the popup open, Enter accepts the highlighted item; if the
            // token is already complete (no change), fall through to submit.
            if state.completion().is_some() && state.accept_completion() {
                return;
            }
            if let Some(idx) = state.header_selected() {
                // Selecting a member and pressing Enter attaches to its live
                // backend session (hands the terminal to the real codex/claude).
                state.request_attach(idx);
                return;
            }
            submit(state, handle);
        }
    }
}

/// Handle keys while a reverse history search (Ctrl+R) is active.
fn handle_search_action(action: Action, state: &mut AppState) {
    match action {
        Action::InsertChar(ch) => state.history_search_input(ch),
        Action::Backspace => state.history_search_backspace(),
        // Ctrl+R again steps to the next older match.
        Action::HistorySearch => state.history_search_again(),
        // Enter accepts the match into the composer.
        Action::Submit => state.accept_history_search(),
        // Esc / Ctrl+C leave search without changing the composer.
        Action::CloseOverlay | Action::Interrupt => state.cancel_history_search(),
        _ => {}
    }
}

/// Capture the workspace's working-tree git diff, including untracked files
/// (mirrors codex's `/diff`). Returns a human-readable message on failure.
fn compute_git_diff(workspace: &str) -> String {
    let dir = if workspace.is_empty() { "." } else { workspace };
    let run = |args: &[&str]| -> Option<String> {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
    };

    let mut out = match run(&["--no-pager", "diff"]) {
        Some(diff) => diff,
        None => return "not a git repository (or git is unavailable)".to_string(),
    };
    // Codex's /diff also surfaces untracked files; list them after the diff.
    if let Some(untracked) = run(&["ls-files", "--others", "--exclude-standard"])
        && !untracked.trim().is_empty()
    {
        out.push_str("\nUntracked files:\n");
        for file in untracked.lines() {
            out.push_str("  ");
            out.push_str(file);
            out.push('\n');
        }
    }
    out
}

fn submit(state: &mut AppState, handle: &RuntimeHandle) {
    let text = state.composer().text();
    match commands::parse(&text) {
        Submission::Runtime(command) => {
            state.record_submission(&text);
            state.take_composer();
            if let UiCommand::UserMessage { target, body } = &command {
                state.handle_user_message_submitted(target, body.clone());
            }
            handle.send(command);
        }
        Submission::Drawer(drawer) => {
            state.record_submission(&text);
            state.take_composer();
            // `/diff` captures the live working-tree diff just before opening.
            if drawer == drawers::Drawer::Diff && state.drawer() != Some(drawers::Drawer::Diff) {
                let diff = compute_git_diff(state.workspace());
                state.set_diff(diff);
            }
            state.toggle_drawer(drawer);
        }
        Submission::ApproveFirst(decision) => match state.first_pending_approval() {
            Some(id) => {
                state.record_submission(&text);
                state.take_composer();
                handle.send(UiCommand::Approve { id, decision });
            }
            None => state.apply(RuntimeEvent::Notice("no pending approval".to_string())),
        },
        Submission::Help => {
            state.record_submission(&text);
            state.take_composer();
            state.toggle_drawer(drawers::Drawer::Palette);
        }
        Submission::NeedsTarget => state.apply(RuntimeEvent::Notice(
            "message needs a target prefix: @member, @all, /ask, or /all (draft kept)".to_string(),
        )),
        Submission::Empty => {
            state.take_composer();
        }
    }
    state.reset_scroll();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::event::{
        ChatItem, WorkflowRunId, WorkflowRunStatus, WorkflowRunSummary, WorkflowStepStatus,
        WorkflowStepSummary,
    };
    use crate::domain::team::{DefaultTarget, MemberId, TeamConfig};
    use crate::runtime::{self, Runners};
    use crate::store::sqlite::SqliteStore;
    use crate::tui::drawers::Drawer;
    use std::sync::mpsc;

    #[test]
    fn untargeted_text_keeps_the_draft() {
        let (evt_tx, _evt_rx) = mpsc::channel();
        let (handle, join) = runtime::spawn(
            TeamConfig::new("test", "/tmp/ws"),
            SqliteStore::in_memory().unwrap(),
            Runners::new(),
            evt_tx,
            true,
            true,
            None,
        );
        let mut state = AppState::new(Vec::new());
        for ch in "build the parser".chars() {
            state.insert_char(ch);
        }

        submit(&mut state, &handle);

        assert_eq!(state.composer().text(), "build the parser");
        assert!(state.chat().iter().any(|item| matches!(
            item,
            ChatItem::Notice { text } if text.contains("draft kept")
        )));

        handle.send(UiCommand::Shutdown);
        let _ = join.join();
    }

    #[test]
    fn enter_in_runs_drawer_stages_next_action() {
        let (evt_tx, _evt_rx) = mpsc::channel();
        let (handle, join) = runtime::spawn(
            TeamConfig::new("test", "/tmp/ws"),
            SqliteStore::in_memory().unwrap(),
            Runners::new(),
            evt_tx,
            true,
            true,
            None,
        );
        let mut state = AppState::new(Vec::new());
        state.apply(RuntimeEvent::Ready {
            team: "mixed".to_string(),
            workspace: "/tmp/ws".to_string(),
            default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
            workflow_runs: vec![WorkflowRunSummary {
                id: WorkflowRunId(1),
                goal: "ship parser".to_string(),
                status: WorkflowRunStatus::Done,
                coordinator: Some(MemberId::new("builder")),
                verification: None,
                created_at: "2026-06-28 10:00:00".to_string(),
                updated_at: "2026-06-28 10:00:00".to_string(),
                attempt: 1,
                events: Vec::new(),
                steps: Vec::new(),
            }],
            members: Vec::new(),
        });
        state.toggle_drawer(Drawer::Runs);

        handle_action(Action::Submit, &mut state, &handle);

        assert_eq!(state.drawer(), None);
        assert_eq!(state.composer().text(), "/verify run-1");

        handle.send(UiCommand::Shutdown);
        let _ = join.join();
    }

    #[test]
    fn runs_drawer_arrow_selects_step_before_staging_action() {
        let (evt_tx, _evt_rx) = mpsc::channel();
        let (handle, join) = runtime::spawn(
            TeamConfig::new("test", "/tmp/ws"),
            SqliteStore::in_memory().unwrap(),
            Runners::new(),
            evt_tx,
            true,
            true,
            None,
        );
        let mut state = AppState::new(Vec::new());
        state.apply(RuntimeEvent::Ready {
            team: "mixed".to_string(),
            workspace: "/tmp/ws".to_string(),
            default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
            workflow_runs: vec![WorkflowRunSummary {
                id: WorkflowRunId(1),
                goal: "ship parser".to_string(),
                status: WorkflowRunStatus::Running,
                coordinator: Some(MemberId::new("builder")),
                verification: None,
                created_at: "2026-06-28 10:00:00".to_string(),
                updated_at: "2026-06-28 10:00:00".to_string(),
                attempt: 1,
                events: Vec::new(),
                steps: vec![WorkflowStepSummary {
                    number: 1,
                    status: WorkflowStepStatus::Doing,
                    owner: None,
                    title: "Wire checklist UI".to_string(),
                    note: None,
                    updated_at: "2026-06-28 10:05:00".to_string(),
                }],
            }],
            members: Vec::new(),
        });
        state.toggle_drawer(Drawer::Runs);

        handle_action(Action::HistoryNext, &mut state, &handle);
        handle_action(Action::Submit, &mut state, &handle);

        assert_eq!(state.drawer(), None);
        assert_eq!(state.composer().text(), "/step done run-1 1");

        handle.send(UiCommand::Shutdown);
        let _ = join.join();
    }

    #[test]
    fn runs_drawer_tab_dispatches_selected_step_to_owner() {
        let (evt_tx, _evt_rx) = mpsc::channel();
        let (handle, join) = runtime::spawn(
            TeamConfig::new("test", "/tmp/ws"),
            SqliteStore::in_memory().unwrap(),
            Runners::new(),
            evt_tx,
            true,
            true,
            None,
        );
        let mut state = AppState::new(Vec::new());
        state.apply(RuntimeEvent::Ready {
            team: "mixed".to_string(),
            workspace: "/tmp/ws".to_string(),
            default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
            workflow_runs: vec![WorkflowRunSummary {
                id: WorkflowRunId(1),
                goal: "ship parser".to_string(),
                status: WorkflowRunStatus::Running,
                coordinator: Some(MemberId::new("builder")),
                verification: None,
                created_at: "2026-06-28 10:00:00".to_string(),
                updated_at: "2026-06-28 10:00:00".to_string(),
                attempt: 1,
                events: Vec::new(),
                steps: vec![WorkflowStepSummary {
                    number: 1,
                    status: WorkflowStepStatus::Todo,
                    owner: Some(MemberId::new("builder")),
                    title: "Wire checklist UI".to_string(),
                    note: None,
                    updated_at: "2026-06-28 10:05:00".to_string(),
                }],
            }],
            members: Vec::new(),
        });
        state.toggle_drawer(Drawer::Runs);

        handle_action(Action::HistoryNext, &mut state, &handle);
        handle_action(Action::Complete, &mut state, &handle);

        assert_eq!(state.drawer(), None);
        assert_eq!(
            state.composer().text(),
            "@builder Start run-1 step #1: Wire checklist UI. Update the checklist with @@workflow_step as you progress."
        );

        handle.send(UiCommand::Shutdown);
        let _ = join.join();
    }

    #[test]
    fn runs_drawer_x_toggles_detail_without_typing() {
        let (evt_tx, _evt_rx) = mpsc::channel();
        let (handle, join) = runtime::spawn(
            TeamConfig::new("test", "/tmp/ws"),
            SqliteStore::in_memory().unwrap(),
            Runners::new(),
            evt_tx,
            true,
            true,
            None,
        );
        let mut state = AppState::new(Vec::new());
        state.apply(RuntimeEvent::Ready {
            team: "mixed".to_string(),
            workspace: "/tmp/ws".to_string(),
            default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
            workflow_runs: Vec::new(),
            members: Vec::new(),
        });
        state.toggle_drawer(Drawer::Runs);

        assert!(!state.workflow_runs_detail());
        handle_action(Action::InsertChar('x'), &mut state, &handle);
        assert!(state.workflow_runs_detail());
        assert!(state.composer().is_empty());

        state.insert_char('a');
        handle_action(Action::InsertChar('x'), &mut state, &handle);
        assert!(state.workflow_runs_detail());
        assert_eq!(state.composer().text(), "ax");

        handle.send(UiCommand::Shutdown);
        let _ = join.join();
    }
}
