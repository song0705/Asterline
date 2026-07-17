//! Chat-first terminal UI: a single scrolling conversation column, a bottom
//! composer, and overlay drawers (logs / team / command palette). State is
//! driven entirely by `RuntimeEvent`s; no string matching.

pub mod app_state;
pub mod attach;
pub mod chat_view;
pub mod claude_import;
pub mod commands;
pub mod completion;
pub mod composer;
pub mod drawer_view;
pub mod drawers;
pub mod header;
pub mod keymap;
pub mod markdown;
pub mod notify;
pub mod rollout_import;
pub mod selection;
pub mod session_picker;
pub mod skills;
pub mod status_indicator;
pub mod team_builder;
pub mod team_editor;
pub mod theme;
pub mod workflow_view;

use std::io::{self, Write};
use std::path::Path;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use crossterm::clipboard::CopyToClipboard;
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyboardEnhancementFlags, MouseButton, MouseEvent,
    MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::domain::event::{RuntimeEvent, UiCommand};
use crate::domain::mode::TerminalMode;
use crate::domain::team::BackendKind;
use crate::runtime::RuntimeHandle;
use crate::tui::app_state::AppState;
use crate::tui::commands::Submission;
use crate::tui::keymap::Action;
use crate::tui::selection::MouseSelection;
use crate::tui::team_editor::TeamEditorOutcome;

const POLL_INTERVAL: Duration = Duration::from_millis(50);
const RESET_KEYBOARD_TO_LEGACY: &[u8] = b"\x1b[=0u";

/// Asterline uses color for backend identity, status, and selection—not only
/// decoration. Full-screen interactive sessions therefore keep color enabled
/// even when a parent process injects `NO_COLOR` into the environment.
fn enable_tui_colors() {
    crossterm::style::force_color_output(true);
}

/// Best-effort terminal state restoration. A fresh stdout handle is used so
/// cleanup still runs if terminal construction, drawing, or the event loop
/// returns early. `Drop` is the final safety net for panic/error paths.
#[derive(Default)]
struct TerminalRestore {
    raw_mode: bool,
    keyboard_enhancement: bool,
    alternate_screen: bool,
    mouse_capture: bool,
    bracketed_paste: bool,
    legacy_keyboard_reset: bool,
}

impl TerminalRestore {
    fn restore(&mut self) -> io::Result<()> {
        let mut first_error = None;
        let mut out = io::stdout();

        if self.keyboard_enhancement {
            record_cleanup(&mut first_error, execute!(out, PopKeyboardEnhancementFlags));
            self.keyboard_enhancement = false;
        }
        if self.mouse_capture {
            record_cleanup(&mut first_error, execute!(out, DisableMouseCapture));
            self.mouse_capture = false;
        }
        if self.bracketed_paste {
            record_cleanup(&mut first_error, execute!(out, DisableBracketedPaste));
            self.bracketed_paste = false;
        }
        if self.alternate_screen {
            record_cleanup(&mut first_error, execute!(out, LeaveAlternateScreen));
            self.alternate_screen = false;
        }
        if self.legacy_keyboard_reset {
            record_cleanup(&mut first_error, reset_keyboard_to_legacy(&mut out));
        }
        record_cleanup(&mut first_error, execute!(out, crossterm::cursor::Show));
        if self.raw_mode {
            record_cleanup(&mut first_error, disable_raw_mode());
            self.raw_mode = false;
        }
        record_cleanup(&mut first_error, out.flush());

        match first_error {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }
}

impl Drop for TerminalRestore {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

fn record_cleanup(first_error: &mut Option<io::Error>, result: io::Result<()>) {
    if let Err(err) = result
        && first_error.is_none()
    {
        *first_error = Some(err);
    }
}

/// Run the TUI to completion. `events` delivers runtime events; `handle` sends
/// commands back. On exit the runtime is asked to shut down.
pub fn run(
    handle: RuntimeHandle,
    events: Receiver<RuntimeEvent>,
    mut state: AppState,
) -> io::Result<()> {
    enable_tui_colors();
    let mut restore = TerminalRestore::default();
    let term_program = std::env::var("TERM_PROGRAM").ok();
    let vscode_pid = std::env::var_os("VSCODE_PID").is_some();
    let multiplexed = std::env::var_os("TMUX").is_some() || std::env::var_os("STY").is_some();
    let keyboard_enhancement_allowed =
        terminal_program_allows_keyboard_enhancement(term_program.as_deref(), vscode_pid);
    restore.legacy_keyboard_reset =
        terminal_requires_legacy_reset(term_program.as_deref(), vscode_pid, multiplexed);
    let mut stdout = io::stdout();
    if restore.legacy_keyboard_reset {
        reset_keyboard_to_legacy(&mut stdout)?;
        stdout.flush()?;
    }
    enable_raw_mode()?;
    restore.raw_mode = true;
    restore.alternate_screen = true;
    restore.mouse_capture = true;
    restore.bracketed_paste = true;
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    // Kitty keeps separate keyboard-mode stacks for the main and alternate
    // screens. Push only after entering the alternate screen so cleanup pops
    // the same stack before leaving it.
    let keyboard_enhancement =
        enable_keyboard_enhancement(&mut stdout, keyboard_enhancement_allowed)?;
    restore.keyboard_enhancement = keyboard_enhancement;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(
        &mut terminal,
        &mut state,
        &handle,
        &events,
        keyboard_enhancement,
        restore.legacy_keyboard_reset,
    );

    // Always attempt every cleanup action; one failed escape write must not
    // leave the keyboard protocol or raw mode enabled in the user's shell.
    let cleanup = restore.restore();

    handle.send(UiCommand::Shutdown);
    result.and(cleanup)
}

fn keyboard_enhancement_flags() -> KeyboardEnhancementFlags {
    // Modifier disambiguation is sufficient for Shift/Alt+Enter. Requesting
    // REPORT_EVENT_TYPES makes terminals emit `:3u` key-release sequences;
    // those become visible garbage if a terminal fails to restore its stack.
    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
}

fn enable_keyboard_enhancement(out: &mut impl Write, allowed: bool) -> io::Result<bool> {
    if allowed && supports_keyboard_enhancement().unwrap_or(false) {
        execute!(
            out,
            PushKeyboardEnhancementFlags(keyboard_enhancement_flags())
        )?;
        Ok(true)
    } else {
        Ok(false)
    }
}

fn terminal_program_allows_keyboard_enhancement(
    term_program: Option<&str>,
    vscode_pid_present: bool,
) -> bool {
    if vscode_pid_present {
        return false;
    }
    !term_program.is_some_and(|program| {
        let program = program.to_ascii_lowercase();
        program.contains("vscode") || program.contains("cursor")
    })
}

fn terminal_requires_legacy_reset(
    term_program: Option<&str>,
    vscode_pid_present: bool,
    multiplexed: bool,
) -> bool {
    !multiplexed && !terminal_program_allows_keyboard_enhancement(term_program, vscode_pid_present)
}

fn reset_keyboard_to_legacy(out: &mut impl Write) -> io::Result<()> {
    out.write_all(RESET_KEYBOARD_TO_LEGACY)
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
    legacy_keyboard_reset: bool,
) -> io::Result<()> {
    let notify_enabled = notify::enabled_from_env();
    let mut selection = MouseSelection::default();
    loop {
        while let Ok(event) = events.try_recv() {
            if notify_enabled && let Some(title) = notify_title_for(&event) {
                let mut out = io::stdout();
                let _ = notify::emit(&mut out, title);
                let _ = out.flush();
            }
            state.apply(event);
        }

        let screen = terminal
            .draw(|frame| {
                chat_view::render(frame, state);
                selection.render(frame.buffer_mut());
            })?
            .buffer
            .clone();

        if event::poll(POLL_INTERVAL)? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if selection.is_active() && key.code == KeyCode::Esc {
                        selection.clear();
                        continue;
                    }
                    selection.clear();
                    if handle_team_editor_key(key, state, handle) {
                        continue;
                    }
                    if let Some(action) = keymap::resolve(key) {
                        handle_action(action, state, handle);
                    }
                }
                Event::Mouse(mouse) => handle_mouse(mouse, state, &mut selection, &screen)?,
                Event::Paste(text) => {
                    selection.clear();
                    if !state.insert_team_editor_text(&text) {
                        state.insert_text(&text);
                    }
                }
                _ => {}
            }
        }

        if let Some(req) = state.take_attach_request() {
            attach_to_member(
                terminal,
                state,
                handle,
                &req,
                keyboard_enhancement,
                legacy_keyboard_reset,
            )?;
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
/// tick. Mouse capture keeps wheel events distinct from keyboard arrow keys.
fn handle_mouse(
    mouse: MouseEvent,
    state: &mut AppState,
    selection: &mut MouseSelection,
    screen: &ratatui::buffer::Buffer,
) -> io::Result<()> {
    const STEP: usize = 6;
    match mouse.kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            selection.clear();
            let up = mouse.kind == MouseEventKind::ScrollUp;
            for _ in 0..STEP {
                match (state.drawer().is_some(), up) {
                    (true, true) => state.drawer_scroll_up(),
                    (true, false) => state.drawer_scroll_down(),
                    (false, true) => state.scroll_up(),
                    (false, false) => state.scroll_down(),
                }
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            if state.drawer().is_some() {
                selection.begin_bounded(
                    mouse.column,
                    mouse.row,
                    drawer_view::drawer_rect(screen.area),
                );
            } else {
                selection.begin(mouse.column, mouse.row);
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => selection.update(mouse.column, mouse.row),
        MouseEventKind::Up(MouseButton::Left) => {
            if let Some(text) = selection.finish(mouse.column, mouse.row, screen) {
                execute!(io::stdout(), CopyToClipboard::to_clipboard_from(text))?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Check whether `name` is an executable on the current `PATH`.
fn binary_on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(name).is_file()))
        .unwrap_or(false)
}

/// Hand the whole terminal to the member's real interactive CLI (resuming its
/// session), then restore Asterline when that CLI exits.
fn attach_to_member(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut AppState,
    handle: &RuntimeHandle,
    req: &attach::AttachRequest,
    keyboard_enhancement: bool,
    legacy_keyboard_reset: bool,
) -> io::Result<()> {
    let (program, args) = req.command();
    let exit_hint = match req.backend {
        BackendKind::Codex => "type /exit or press Ctrl+D",
        BackendKind::Claude => "type /exit or press Ctrl+D",
        BackendKind::Grok => "type /exit or press Ctrl+D",
        BackendKind::Agy => "type /exit or press Ctrl+D",
    };

    // Bail out before suspending the terminal if the backend CLI is missing,
    // so the user never sees a blank screen + confusing error.
    if !binary_on_path(&program) {
        state.apply(RuntimeEvent::Notice(format!(
            "could not attach: {program} is not on PATH"
        )));
        return Ok(());
    }

    // Snapshot the backend transcript so we can import whatever is typed during
    // the attached session once it exits (codex rollouts / claude session jsonl).
    enum AttachSnapshot {
        Codex(rollout_import::RolloutSnapshot),
        Claude(claude_import::ClaudeSnapshot),
    }
    let snapshot = match req.backend {
        BackendKind::Codex => Some(AttachSnapshot::Codex(rollout_import::snapshot(
            req.session.as_deref(),
            &req.cwd,
        ))),
        BackendKind::Claude => Some(AttachSnapshot::Claude(claude_import::snapshot(
            req.session.as_deref(),
            &req.cwd,
        ))),
        BackendKind::Grok | BackendKind::Agy => None,
    };

    // --- Suspend Asterline: hand the real terminal to the child CLI. ---
    // Restore the cooked terminal, leave our alternate screen, and show the
    // cursor, flushing so the child starts from a clean, owned main screen.
    let mut out = io::stdout();
    disable_keyboard_enhancement(&mut out, keyboard_enhancement)?;
    disable_raw_mode()?;
    execute!(
        out,
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen,
        crossterm::cursor::Show
    )?;
    if legacy_keyboard_reset {
        reset_keyboard_to_legacy(&mut out)?;
    }
    writeln!(
        out,
        "\n── {} · {} {} ──\n  Asterline suspended. To return: {exit_hint}\n  (Ctrl+C is sent to the CLI and may not exit it.)\n",
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
    execute!(
        out,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    if keyboard_enhancement {
        execute!(
            out,
            PushKeyboardEnhancementFlags(keyboard_enhancement_flags())
        )?;
    }
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
        let imported = match snapshot {
            AttachSnapshot::Codex(s) => rollout_import::imported_since(s),
            AttachSnapshot::Claude(s) => claude_import::imported_since(s),
        };
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
    // Transcript find: n/p jump when active, composer empty, no drawer.
    if state.find_active() && state.composer().is_empty() && state.drawer().is_none() {
        match action {
            Action::InsertChar('n') => {
                state.find_next();
                return;
            }
            Action::InsertChar('p') => {
                state.find_prev();
                return;
            }
            _ => {}
        }
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
            } else if state.drawer() == Some(drawers::Drawer::Skills) {
                state.select_previous_skill();
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
            } else if state.drawer() == Some(drawers::Drawer::Skills) {
                state.select_next_skill();
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
            if state.drawer() == Some(drawers::Drawer::Skills) && state.stage_selected_skill() {
                return;
            }
            if state.drawer() == Some(drawers::Drawer::Runs)
                && state.stage_selected_workflow_dispatch()
            {
                return;
            }
            state.accept_completion();
        }
        Action::CloseOverlay => {
            if state.find_active() {
                state.clear_find();
            } else if state.completion().is_some() {
                state.dismiss_popup();
            } else if state.header_selected().is_some() {
                state.clear_header_selection();
            } else if state.drawer().is_some() {
                state.close_drawer();
            } else if state.running_count() > 0 {
                handle.send(UiCommand::Cancel { member: None });
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
            if state.drawer() == Some(drawers::Drawer::Skills) && state.stage_selected_skill() {
                return;
            }
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
    let mut reset_scroll = true;
    match commands::parse(&text) {
        Submission::Runtime(command) => {
            state.record_submission(&text);
            state.take_composer();
            if let UiCommand::UserMessage { target, body } = &command {
                // Collaboration/workflow modes emit their own canonical user
                // event after resolving participants. Only normal chat can be
                // rendered optimistically with a known local target.
                if state.active_mode() == TerminalMode::Normal {
                    state.handle_user_message_submitted(target, body.clone());
                }
            } else if let UiCommand::SetMode { mode } = &command {
                // Mirror Codex's mode picker: update the terminal UI state
                // immediately, then let the runtime acknowledge the setting.
                state.apply(RuntimeEvent::ModeChanged { mode: *mode });
            } else if matches!(command, UiCommand::NewSession) {
                state.clear_last_message_target();
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
            if drawer == drawers::Drawer::Skills && state.drawer() != Some(drawers::Drawer::Skills)
            {
                let workspace = Path::new(state.workspace());
                state.set_skills(skills::discover(workspace));
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
        Submission::FindInChat(query) => {
            state.record_submission(&text);
            state.take_composer();
            state.set_find(&query);
            // Keep the jump from set_find; do not snap back to bottom.
            reset_scroll = false;
        }
        Submission::Help => {
            state.record_submission(&text);
            state.take_composer();
            state.toggle_drawer(drawers::Drawer::Palette);
        }
        Submission::NeedsTarget => {
            if state.active_mode() != TerminalMode::Normal {
                let body = text.trim().to_string();
                state.record_submission(&body);
                state.take_composer();
                handle.send(UiCommand::UserMessage {
                    target: crate::domain::event::MessageTarget::Default,
                    body,
                });
            } else if let Some((target, body)) = state.inherited_user_message(&text) {
                state.record_submission(&body);
                state.take_composer();
                state.handle_user_message_submitted(&target, body.clone());
                handle.send(UiCommand::UserMessage { target, body });
            } else {
                state.apply(RuntimeEvent::Notice(
                    "message needs a target prefix: @member, @all, /ask, or /all (draft kept)"
                        .to_string(),
                ));
            }
        }
        Submission::Empty => {
            state.take_composer();
        }
    }
    if reset_scroll {
        state.reset_scroll();
    }
}

/// Titles for attention-needed runtime events (terminal BEL + OSC 9).
fn notify_title_for(event: &RuntimeEvent) -> Option<&'static str> {
    match event {
        RuntimeEvent::ApprovalRequested { .. } => Some("Asterline: approval needed"),
        RuntimeEvent::RoutePaused { .. } => Some("Asterline: route paused"),
        RuntimeEvent::WorkflowRunUpdated { run }
            if run.status == crate::domain::event::WorkflowRunStatus::Blocked =>
        {
            Some("Asterline: run blocked")
        }
        RuntimeEvent::MemberError { .. } => Some("Asterline: member error"),
        _ => None,
    }
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
    fn keyboard_enhancement_cleanup_emits_protocol_pop() {
        let mut bytes = Vec::new();
        disable_keyboard_enhancement(&mut bytes, true).unwrap();
        assert_eq!(bytes, b"\x1b[<1u");
    }

    #[test]
    fn keyboard_enhancement_push_uses_only_disambiguation() {
        let mut bytes = Vec::new();
        execute!(
            bytes,
            PushKeyboardEnhancementFlags(keyboard_enhancement_flags())
        )
        .unwrap();
        assert_eq!(bytes, b"\x1b[>1u");
    }

    #[test]
    fn embedded_terminal_legacy_reset_uses_explicit_protocol_reset() {
        let mut bytes = Vec::new();
        reset_keyboard_to_legacy(&mut bytes).unwrap();
        assert_eq!(bytes, b"\x1b[=0u");

        assert!(terminal_requires_legacy_reset(Some("vscode"), false, false));
        assert!(!terminal_requires_legacy_reset(Some("vscode"), false, true));
        assert!(!terminal_requires_legacy_reset(Some("kitty"), false, false));
    }

    #[test]
    fn keyboard_enhancement_never_requests_release_events() {
        let flags = keyboard_enhancement_flags();
        assert!(flags.contains(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES));
        assert!(!flags.contains(KeyboardEnhancementFlags::REPORT_EVENT_TYPES));
    }

    #[test]
    fn keyboard_enhancement_is_disabled_in_vscode_family_terminals() {
        assert!(!terminal_program_allows_keyboard_enhancement(
            Some("vscode"),
            false
        ));
        assert!(!terminal_program_allows_keyboard_enhancement(
            Some("cursor"),
            false
        ));
        assert!(!terminal_program_allows_keyboard_enhancement(
            Some("xterm"),
            true
        ));
        assert!(terminal_program_allows_keyboard_enhancement(
            Some("kitty"),
            false
        ));

        let mut bytes = Vec::new();
        assert!(!enable_keyboard_enhancement(&mut bytes, false).unwrap());
        assert!(bytes.is_empty());
    }

    #[test]
    fn mouse_wheel_scrolls_chat_independently_of_arrow_history() {
        let mut state = AppState::new(Vec::new());
        let mut selection = MouseSelection::default();
        let screen = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 80, 24));
        let mouse = |kind| MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };

        handle_mouse(
            mouse(MouseEventKind::ScrollUp),
            &mut state,
            &mut selection,
            &screen,
        )
        .unwrap();
        assert_eq!(state.scroll(), 6);
        handle_mouse(
            mouse(MouseEventKind::ScrollDown),
            &mut state,
            &mut selection,
            &screen,
        )
        .unwrap();
        assert_eq!(state.scroll(), 0);
    }

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
    fn selected_mode_accepts_plain_text_without_an_inherited_target() {
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
        state.apply(RuntimeEvent::ModeChanged {
            mode: TerminalMode::Review,
        });
        for ch in "build the parser".chars() {
            state.insert_char(ch);
        }

        submit(&mut state, &handle);

        assert!(state.composer().is_empty());
        assert_eq!(state.active_mode(), TerminalMode::Review);
        assert!(!state.chat().iter().any(|item| matches!(
            item,
            ChatItem::Notice { text } if text.contains("draft kept")
        )));

        handle.send(UiCommand::Shutdown);
        let _ = join.join();
    }

    #[test]
    fn untargeted_text_reuses_previous_target() {
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
            team: "test".to_string(),
            workspace: "/tmp/ws".to_string(),
            default_target: None,
            workflow_runs: Vec::new(),
            members: vec![crate::domain::event::MemberSummary {
                id: crate::domain::team::MemberId::new("builder"),
                display_name: "Builder".to_string(),
                backend: crate::domain::team::BackendKind::Codex,
                role: "build".to_string(),
                status: crate::domain::event::MemberStatus::Idle,
                session: None,
                cwd: String::new(),
                model: None,
                effort: None,
                sandbox: crate::domain::team::SandboxPolicy::WorkspaceWrite,
                permission_mode: None,
                session_policy: crate::domain::team::SessionPolicy::Resume,
            }],
        });

        for ch in "@builder build the parser".chars() {
            state.insert_char(ch);
        }
        submit(&mut state, &handle);
        for ch in "now add tests".chars() {
            state.insert_char(ch);
        }
        submit(&mut state, &handle);

        assert!(state.composer().is_empty());
        assert!(state.chat().iter().any(|item| matches!(
            item,
            ChatItem::User { body } if body == "@builder now add tests"
        )));

        handle.send(UiCommand::Shutdown);
        let _ = join.join();
    }

    #[test]
    fn new_session_clears_inherited_target() {
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
            team: "test".to_string(),
            workspace: "/tmp/ws".to_string(),
            default_target: None,
            workflow_runs: Vec::new(),
            members: vec![crate::domain::event::MemberSummary {
                id: crate::domain::team::MemberId::new("builder"),
                display_name: "Builder".to_string(),
                backend: crate::domain::team::BackendKind::Codex,
                role: "build".to_string(),
                status: crate::domain::event::MemberStatus::Idle,
                session: None,
                cwd: String::new(),
                model: None,
                effort: None,
                sandbox: crate::domain::team::SandboxPolicy::WorkspaceWrite,
                permission_mode: None,
                session_policy: crate::domain::team::SessionPolicy::Resume,
            }],
        });

        for ch in "@builder build the parser".chars() {
            state.insert_char(ch);
        }
        submit(&mut state, &handle);
        for ch in "/new".chars() {
            state.insert_char(ch);
        }
        submit(&mut state, &handle);
        for ch in "now add tests".chars() {
            state.insert_char(ch);
        }
        submit(&mut state, &handle);

        assert_eq!(state.composer().text(), "now add tests");
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
                mode: None,
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
                mode: None,
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
                mode: None,
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
