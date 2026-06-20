//! Chat-first terminal UI: a single scrolling conversation column, a bottom
//! composer, and overlay drawers (logs / team / command palette). State is
//! driven entirely by `RuntimeEvent`s; no string matching.

pub mod app_state;
pub mod attach;
pub mod chat_view;
pub mod commands;
pub mod completion;
pub mod composer;
pub mod drawers;
pub mod keymap;
pub mod markdown;
pub mod rollout_import;

use std::io::{self, Write};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::domain::event::{RuntimeEvent, UiCommand};
use crate::domain::team::BackendKind;
use crate::runtime::RuntimeHandle;
use crate::tui::app_state::AppState;
use crate::tui::commands::Submission;
use crate::tui::keymap::Action;

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
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, &mut state, &handle, &events);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    handle.send(UiCommand::Shutdown);
    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut AppState,
    handle: &RuntimeHandle,
    events: &Receiver<RuntimeEvent>,
) -> io::Result<()> {
    loop {
        while let Ok(event) = events.try_recv() {
            state.apply(event);
        }

        terminal.draw(|frame| chat_view::render(frame, state))?;

        if event::poll(POLL_INTERVAL)? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if let Some(action) = keymap::resolve(key) {
                        handle_action(action, state, handle);
                    }
                }
                Event::Mouse(mouse) => handle_mouse(mouse, state),
                _ => {}
            }
        }

        if let Some(req) = state.take_attach_request() {
            attach_to_member(terminal, state, handle, &req)?;
        }

        if state.should_quit() {
            return Ok(());
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
    match action {
        Action::InsertChar(ch) => state.insert_char(ch),
        Action::InsertNewline => state.insert_newline(),
        Action::Backspace => state.backspace(),
        Action::DeleteWord => state.delete_word(),
        Action::ClearLine => state.clear_composer(),
        Action::CursorLeft => {
            if state.header_selected().is_some() {
                state.select_prev_member();
            } else {
                state.cursor_left();
            }
        }
        Action::CursorRight => {
            if state.header_selected().is_some() {
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
            if state.drawer().is_some() {
                state.drawer_scroll_up();
            } else if state.completion().is_some() {
                state.popup_up();
            } else if !state.composer_up() {
                // Already on the first composer line — recall older history.
                state.history_prev();
            }
        }
        Action::HistoryNext => {
            if state.drawer().is_some() {
                state.drawer_scroll_down();
            } else if state.completion().is_some() {
                state.popup_down();
            } else if !state.composer_down() {
                // Already on the last composer line — recall newer history.
                state.history_next();
            }
        }
        Action::ToggleLogs => state.toggle_drawer(drawers::Drawer::Logs),
        Action::ToggleTeam => state.toggle_drawer(drawers::Drawer::Team),
        Action::TogglePalette => state.toggle_drawer(drawers::Drawer::Palette),
        Action::ToggleExpand => state.toggle_tools_expansion(),
        Action::NextMember => state.select_next_member(),
        Action::PrevMember => state.select_prev_member(),
        Action::Complete => {
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
                handle.send(UiCommand::Cancel { member: None });
            } else if !state.composer().is_empty() {
                state.clear_composer();
            } else {
                state.quit();
            }
        }
        Action::Submit => {
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
    let text = state.take_composer();
    // Record every non-blank submission for shell-style ↑/↓ recall.
    state.record_submission(&text);
    match commands::parse(&text) {
        Submission::Runtime(command) => {
            if let UiCommand::UserMessage { target, body } = &command {
                state.handle_user_message_submitted(target, body.clone());
            }
            handle.send(command);
        }
        Submission::Drawer(drawer) => {
            // `/diff` captures the live working-tree diff just before opening.
            if drawer == drawers::Drawer::Diff && state.drawer() != Some(drawers::Drawer::Diff) {
                let diff = compute_git_diff(state.workspace());
                state.set_diff(diff);
            }
            state.toggle_drawer(drawer);
        }
        Submission::ApproveFirst(decision) => match state.first_pending_approval() {
            Some(id) => {
                handle.send(UiCommand::Approve { id, decision });
            }
            None => state.apply(RuntimeEvent::Notice("no pending approval".to_string())),
        },
        Submission::Help => state.toggle_drawer(drawers::Drawer::Palette),
        Submission::Empty => {}
    }
    state.reset_scroll();
}
