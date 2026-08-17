//! Chat-first terminal UI: a single scrolling conversation column, a bottom
//! composer, and overlay drawers (logs / team / command palette). State is
//! driven entirely by `RuntimeEvent`s; no string matching.

pub mod app_state;
pub mod attach;
pub mod chat_view;
pub mod claude_export;
pub mod claude_import;
mod clipboard_image;
pub mod commands;
pub mod completion;
pub mod composer;
pub mod drawer_view;
pub mod drawers;
mod file_diff;
pub mod grok_import;
pub mod header;
mod import_io;
pub mod keymap;
pub mod markdown;
pub mod mode_editor;
pub mod notify;
pub mod rollout_import;
pub mod runs_view;
pub mod session_picker;
pub mod skills;
pub mod status_indicator;
pub mod team_builder;
pub mod team_editor;
pub mod theme;
mod tool_display;

use std::io::{self, Read, Write};
#[cfg(test)]
use std::path::Path;
use std::process::Stdio;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::clipboard::CopyToClipboard;
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyEvent, KeyEventKind, KeyboardEnhancementFlags, MouseButton, MouseEvent,
    MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::domain::event::{ApprovalDecision, RuntimeEvent, UiCommand};
use crate::domain::mode::TerminalMode;
use crate::domain::team::BackendKind;
use crate::runtime::{RuntimeCommandSend, RuntimeHandle};
use crate::tui::app_state::AppState;
use crate::tui::commands::Submission;
use crate::tui::keymap::Action;
use crate::tui::mode_editor::ModeEditorOutcome;
use crate::tui::team_editor::TeamEditorOutcome;

/// Codex schedules spinner frames at 32ms. The same bound makes queued
/// runtime events visible promptly when the terminal is otherwise idle.
const ANIMATION_INTERVAL: Duration = Duration::from_millis(32);
const RUNTIME_EVENT_POLL_INTERVAL: Duration = Duration::from_millis(32);
const MAX_RUNTIME_EVENTS_PER_DRAIN: usize = 1_024;
const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const RESET_KEYBOARD_TO_LEGACY: &[u8] = b"\x1b[=0u";
const WHEEL_SCROLL_LINES: i32 = 5;

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
    bracketed_paste: bool,
    mouse_capture: bool,
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
        if self.bracketed_paste {
            record_cleanup(&mut first_error, execute!(out, DisableBracketedPaste));
            self.bracketed_paste = false;
        }
        if self.mouse_capture {
            record_cleanup(&mut first_error, execute!(out, DisableMouseCapture));
            self.mouse_capture = false;
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
    let _paste_cleanup = clipboard_image::PasteCleanup::enter();
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
    restore.bracketed_paste = true;
    restore.mouse_capture = true;
    // Full-screen alternate buffer so the header stays pinned at the top and
    // the shell's `cargo run` / `ls` output is not mixed into the chat. Mouse
    // capture is required so the wheel can scroll the chat pane; drag-select
    // copy is handled in-process via OSC 52 (native selection is unavailable
    // once the application owns mouse events).
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture
    )?;
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

    handle.shutdown();
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
    let mut last_layout = None;
    let mut dirty = true;
    loop {
        state.poll_team_editor_catalog();
        if drain_runtime_events(state, events, notify_enabled) {
            dirty = true;
        }
        state.warm_model_catalog_once();
        if let Some(member) = state.take_attach_release_pending()
            && !handle.finish_attach(member, Vec::new())
        {
            state.mark_runtime_unavailable();
            dirty = true;
        }

        if dirty || state.needs_animated_frame() || state.drawer().is_some() {
            terminal.draw(|frame| {
                last_layout = chat_view::render(frame, state);
            })?;
            if let Some(layout) = last_layout.as_ref() {
                state.set_chat_page_rows(layout.area.height.saturating_sub(1) as usize);
                state.clamp_scroll(layout.max_scroll());
            }
            dirty = false;
        }

        // Mouse capture emits a Moved event per pixel. Handling one event per
        // redraw leaves keypresses behind a seconds-long queue. Drain the
        // whole burst, drop unused motion, and coalesce drags first.
        let poll_for = if dirty || state.needs_animated_frame() || state.drawer().is_some() {
            ANIMATION_INTERVAL
        } else {
            RUNTIME_EVENT_POLL_INTERVAL
        };
        for event in read_pending_input(poll_for)? {
            dirty = true;
            match event {
                Event::Resize(_, _) => {}
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if !state.runtime_available() {
                        if keymap::resolve(key) == Some(Action::Interrupt) {
                            state.quit();
                        }
                        continue;
                    }
                    if handle_team_editor_key(key, state, handle) {
                        continue;
                    }
                    if handle_mode_editor_key(key, state, handle) {
                        continue;
                    }
                    if let Some(action) = keymap::resolve(key) {
                        handle_action(action, state, handle);
                    }
                }
                Event::Mouse(mouse) => {
                    handle_mouse(mouse, state, last_layout.as_ref());
                }
                Event::Paste(text) => {
                    if !state.runtime_available() {
                        continue;
                    }
                    if !state.insert_team_editor_text(&text)
                        && !state.insert_mode_editor_text(&text)
                    {
                        if text.trim().is_empty() {
                            paste_clipboard_image(state);
                        } else {
                            state.paste_text_or_image(&text);
                        }
                    }
                }
                _ => {}
            }
        }

        if let Some(req) = state.take_attach_request() {
            dirty = true;
            let member = req.member.clone();
            let result = attach_to_member(
                terminal,
                state,
                &req,
                keyboard_enhancement,
                legacy_keyboard_reset,
            );
            match result {
                Ok(outcome) => {
                    if let Some(notice) = outcome.notice {
                        state.apply(RuntimeEvent::Notice(notice));
                    }
                    if !handle.finish_attach_with_session(member, outcome.session, outcome.items) {
                        state.mark_runtime_unavailable();
                    }
                }
                Err(err) => {
                    if !handle.finish_attach(member, Vec::new()) {
                        state.mark_runtime_unavailable();
                    }
                    return Err(err);
                }
            }
        }
        if state.should_quit() {
            return Ok(());
        }
    }
}

fn handle_mouse(mouse: MouseEvent, state: &mut AppState, layout: Option<&chat_view::ChatLayout>) {
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            if state.drawer().is_some() {
                state.drawer_scroll_by(-WHEEL_SCROLL_LINES);
            } else if state.completion().is_some() {
                for _ in 0..WHEEL_SCROLL_LINES {
                    state.popup_up();
                }
            } else {
                state.scroll_by(WHEEL_SCROLL_LINES);
            }
        }
        MouseEventKind::ScrollDown => {
            if state.drawer().is_some() {
                state.drawer_scroll_by(WHEEL_SCROLL_LINES);
            } else if state.completion().is_some() {
                for _ in 0..WHEEL_SCROLL_LINES {
                    state.popup_down();
                }
            } else {
                state.scroll_by(-WHEEL_SCROLL_LINES);
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            if state.drawer().is_some() {
                return;
            }
            if let Some(index) = layout.and_then(|layout| {
                layout.screen_to_composer_index(state.composer(), mouse.column, mouse.row)
            }) {
                state.begin_composer_selection(index);
                return;
            }
            match layout.and_then(|layout| {
                layout
                    .contains(mouse.column, mouse.row)
                    .then(|| layout.screen_to_content(mouse.column, mouse.row))
                    .flatten()
            }) {
                Some(pos) => state.begin_chat_selection(pos),
                None => state.clear_chat_selection(),
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(index) = layout.and_then(|layout| {
                layout.screen_to_composer_index(state.composer(), mouse.column, mouse.row)
            }) {
                state.update_composer_selection(index);
                return;
            }
            if let Some(pos) =
                layout.and_then(|layout| layout.screen_to_content(mouse.column, mouse.row))
            {
                state.update_chat_selection(pos);
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            let composer_text = state.finish_composer_selection();
            if !composer_text.is_empty() {
                copy_to_clipboard(&composer_text);
                return;
            }
            if let (Some(layout), Some(selection)) = (layout, state.chat_selection()) {
                let text = layout.selected_text(selection);
                if text.trim().is_empty() {
                    state.clear_chat_selection();
                } else {
                    copy_to_clipboard(&text);
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn copy_to_clipboard(text: &str) {
    let mut out = io::stdout();
    let _ = execute!(out, CopyToClipboard::to_clipboard_from(text));
    let _ = execute!(out, CopyToClipboard::to_primary_from(text));
}

/// Wait up to `first_wait` for the next input, then drain everything already
/// queued. Motion events are dropped while reading so a trackpad burst cannot
/// allocate or delay the following key.
fn read_pending_input(first_wait: Duration) -> io::Result<Vec<Event>> {
    if !event::poll(first_wait)? {
        return Ok(Vec::new());
    }
    let mut events = Vec::new();
    loop {
        match event::read()? {
            Event::Mouse(mouse) if matches!(mouse.kind, MouseEventKind::Moved) => {}
            event => events.push(event),
        }
        if !event::poll(Duration::ZERO)? {
            break;
        }
    }
    Ok(coalesce_mouse_drags(events))
}

/// Keep the latest position from a burst of left-button drags so selection
/// tracks the pointer instead of replaying every intermediate cell.
fn coalesce_mouse_drags(events: Vec<Event>) -> Vec<Event> {
    let mut out = Vec::with_capacity(events.len());
    for event in events {
        match event {
            Event::Mouse(mouse)
                if matches!(mouse.kind, MouseEventKind::Drag(MouseButton::Left)) =>
            {
                if let Some(Event::Mouse(previous)) = out.last_mut()
                    && matches!(previous.kind, MouseEventKind::Drag(MouseButton::Left))
                {
                    *previous = mouse;
                    continue;
                }
                out.push(Event::Mouse(mouse));
            }
            other => out.push(other),
        }
    }
    out
}

fn handle_team_editor_key(key: KeyEvent, state: &mut AppState, handle: &RuntimeHandle) -> bool {
    match state.handle_team_editor_key(key.code, key.modifiers) {
        TeamEditorOutcome::Ignored => false,
        TeamEditorOutcome::Consumed(command) => {
            if let Some(command) = command {
                send_runtime(state, handle, command);
            }
            true
        }
        TeamEditorOutcome::Close => {
            state.close_drawer();
            true
        }
    }
}

fn handle_mode_editor_key(key: KeyEvent, state: &mut AppState, handle: &RuntimeHandle) -> bool {
    match state.handle_mode_editor_key(key.code, key.modifiers) {
        ModeEditorOutcome::Ignored => false,
        ModeEditorOutcome::Consumed(commands) => {
            let close_after = commands
                .iter()
                .any(|command| matches!(command, crate::domain::event::UiCommand::SetMode { .. }));
            for command in commands {
                send_runtime(state, handle, command);
            }
            if close_after {
                state.close_drawer();
            }
            true
        }
        ModeEditorOutcome::Close => {
            state.close_drawer();
            true
        }
    }
}

/// Hand the whole terminal to the member's real interactive CLI (resuming its
/// session), then restore Asterline when that CLI exits.
fn attach_to_member(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut AppState,
    req: &attach::AttachRequest,
    keyboard_enhancement: bool,
    legacy_keyboard_reset: bool,
) -> io::Result<attach::AttachOutcome> {
    let (program, args) = req.command();
    let exit_hint = attach_exit_hint();

    // Bail out before suspending the terminal if the backend CLI is missing,
    // so the user never sees a blank screen + confusing error.
    let Some(program_path) = crate::domain::config::resolve_binary_on_path(&program) else {
        state.apply(RuntimeEvent::Notice(format!(
            "could not attach: {program} is not on PATH"
        )));
        return Ok(attach::AttachOutcome::default());
    };

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
            req.transcript_session(),
            &req.cwd,
        ))),
        BackendKind::Grok | BackendKind::Agy => None,
    };

    // --- Suspend Asterline: hand the real terminal to the child CLI. ---
    let mut out = io::stdout();
    disable_keyboard_enhancement(&mut out, keyboard_enhancement)?;
    disable_raw_mode()?;
    execute!(
        out,
        DisableMouseCapture,
        DisableBracketedPaste,
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

    let result = std::process::Command::new(&program_path)
        .args(&args)
        .current_dir(&req.cwd)
        .status();
    let attached_cli_ran = result.is_ok();

    // --- Resume Asterline: re-enter the alternate screen and repaint. ---
    enable_raw_mode()?;
    execute!(
        out,
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture
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
    let mut imported = if attached_cli_ran && let Some(snapshot) = snapshot {
        match snapshot {
            AttachSnapshot::Codex(s) => rollout_import::imported_attach_since(s),
            AttachSnapshot::Claude(s) => claude_import::imported_attach_since(s),
        }
    } else {
        attach::AttachOutcome::default()
    };
    // The fresh Claude UUID was generated by Asterline and supplied directly
    // to the successfully launched CLI, so it is a deterministic identity
    // even if the user exits before Claude writes an importable chat row.
    if attached_cli_ran && req.backend == BackendKind::Claude && imported.session.is_none() {
        imported.session = req.fresh_session.clone();
    }
    Ok(imported)
}

fn attach_exit_hint() -> &'static str {
    if cfg!(windows) {
        "type /exit or press Ctrl+Z then Enter"
    } else {
        "type /exit or press Ctrl+D"
    }
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
    if action == Action::InsertChar('x') && state.toggle_runs_detail() {
        return;
    }
    if handle_approval_keys(action, state, handle) {
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
        Action::ClearLine => state.clear_line(),
        Action::EditQueued => {
            send_runtime(
                state,
                handle,
                UiCommand::EditQueuedPrompt {
                    member: state.last_resolvable_message_target().and_then(
                        |target| match target {
                            crate::domain::event::MessageTarget::Member(id) => Some(id),
                            _ => None,
                        },
                    ),
                },
            );
        }
        Action::CursorLeft => {
            if state.drawer() == Some(drawers::Drawer::Runs) {
                state.select_older_run();
            } else if state.header_selected().is_some() {
                state.select_prev_member();
            } else {
                state.cursor_left();
            }
        }
        Action::CursorRight => {
            if state.drawer() == Some(drawers::Drawer::Runs) {
                state.select_newer_run();
            } else if state.header_selected().is_some() {
                state.select_next_member();
            } else {
                state.cursor_right();
            }
        }
        Action::Home => state.cursor_home(),
        Action::End => state.cursor_end(),
        Action::ScrollUp => {
            if state.drawer() == Some(drawers::Drawer::Resume) {
                state.select_previous_resume();
            } else if state.drawer().is_some() {
                state.drawer_scroll_up();
            } else if state.completion().is_some() {
                state.popup_up();
            } else {
                state.scroll_up();
            }
        }
        Action::ScrollDown => {
            if state.drawer() == Some(drawers::Drawer::Resume) {
                state.select_next_resume();
            } else if state.drawer().is_some() {
                state.drawer_scroll_down();
            } else if state.completion().is_some() {
                state.popup_down();
            } else {
                state.scroll_down();
            }
        }
        Action::HistoryPrev => {
            if state.drawer() == Some(drawers::Drawer::Runs) {
                if !state.select_previous_run_step() {
                    state.select_newer_run();
                }
            } else if state.drawer() == Some(drawers::Drawer::Resume) {
                state.select_previous_resume();
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
                if !state.select_next_run_step() {
                    state.select_older_run();
                }
            } else if state.drawer() == Some(drawers::Drawer::Resume) {
                state.select_next_resume();
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
        Action::ToggleThinking => state.toggle_thinking_expansion(),
        Action::ToggleDiffs => state.toggle_diffs_expansion(),
        Action::ToggleTools => state.toggle_tools_expansion(),
        Action::NextMember => state.select_next_member(),
        Action::PrevMember => state.select_prev_member(),
        Action::Complete => {
            if state.drawer() == Some(drawers::Drawer::Runs) && state.stage_selected_run_dispatch()
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
            } else {
                abort_active_work(state, handle);
            }
        }
        Action::PasteClipboard => paste_clipboard_image(state),
        Action::Interrupt => {
            if state.is_quit_armed() {
                state.quit();
            } else {
                let aborted = abort_active_work(state, handle);
                if !aborted && state.has_composer_draft() {
                    state.clear_composer();
                }
                state.request_quit();
            }
        }
        Action::Submit => {
            if state.drawer() == Some(drawers::Drawer::Resume) {
                if let Some(command) = state.selected_resume_command() {
                    state.close_drawer();
                    send_runtime(state, handle, command);
                }
                return;
            }
            if state.drawer() == Some(drawers::Drawer::Runs) && state.stage_selected_run_action() {
                return;
            }
            // With the popup open, Enter accepts the highlighted item; if the
            // token is already complete (no change), fall through to submit.
            if state.completion().is_some() && state.accept_completion() {
                return;
            }
            if let Some(idx) = state.header_selected() {
                // Selecting a member and pressing Enter attaches to its live
                // backend session after the runtime grants an ordered,
                // globally-quiescent reservation.
                if let Some(member) = state.request_attach(idx)
                    && !send_runtime(state, handle, UiCommand::RequestAttach { member })
                {
                    state.attach_request_send_failed();
                }
                return;
            }
            submit(state, handle);
        }
    }
}

fn abort_active_work(state: &mut AppState, handle: &RuntimeHandle) -> bool {
    if let Some(member) = state.cancel_pending_attach() {
        if !handle.finish_attach(member, Vec::new()) {
            state.mark_runtime_unavailable();
        }
        return true;
    }
    if !state.has_cancelable_work() {
        return false;
    }
    state.disarm_quit();
    send_runtime(state, handle, UiCommand::Cancel { member: None });
    true
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

/// Capture staged and unstaged working-tree changes, including untracked files
/// (mirrors codex's `/diff`). Returns a human-readable message on failure.
fn compute_git_diff(workspace: &str) -> String {
    const MAX_DIFF_BYTES: usize = 2 * 1024 * 1024;
    const MAX_UNTRACKED_BYTES: usize = 256 * 1024;
    const MAX_UNTRACKED_FILES: usize = 2_000;

    let dir = if workspace.is_empty() { "." } else { workspace };
    let (mut out, diff_truncated) = match tracked_git_diff(dir, MAX_DIFF_BYTES) {
        Ok(diff) => diff,
        Err(message) => return message.to_string(),
    };
    if diff_truncated {
        out.push_str("\n[diff output truncated at 2 MiB]\n");
    }
    // Codex's /diff also surfaces untracked files; list them after the diff.
    if let Ok((untracked, byte_truncated)) = run_git_bounded(
        dir,
        &["ls-files", "--others", "--exclude-standard"],
        MAX_UNTRACKED_BYTES,
    ) && !untracked.trim().is_empty()
    {
        out.push_str("\nUntracked files:\n");
        let mut lines = untracked.lines();
        for file in lines.by_ref().take(MAX_UNTRACKED_FILES) {
            out.push_str("  ");
            out.push_str(file);
            out.push('\n');
        }
        if byte_truncated || lines.next().is_some() {
            out.push_str("  [untracked file list truncated]\n");
        }
    }
    out
}

fn tracked_git_diff(dir: &str, limit: usize) -> Result<(String, bool), &'static str> {
    // Against an existing HEAD this is one coherent patch containing index and
    // worktree changes. An unborn repository has no HEAD, so fall back to the
    // two comparisons Git supports there: empty-tree→index and index→worktree.
    if let Ok(diff) = run_git_bounded(
        dir,
        &[
            "--no-pager",
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "HEAD",
            "--",
        ],
        limit,
    ) {
        return Ok(diff);
    }

    let (staged, staged_truncated) = run_git_bounded(
        dir,
        &[
            "--no-pager",
            "diff",
            "--cached",
            "--no-ext-diff",
            "--no-textconv",
            "--",
        ],
        limit,
    )?;
    let remaining = limit
        .saturating_sub(staged.len())
        .saturating_sub(usize::from(!staged.is_empty()));
    let (unstaged, unstaged_truncated) = run_git_bounded(
        dir,
        &["--no-pager", "diff", "--no-ext-diff", "--no-textconv", "--"],
        remaining,
    )?;
    let mut combined = staged;
    if !combined.is_empty() && !unstaged.is_empty() {
        combined.push('\n');
    }
    combined.push_str(&unstaged);
    Ok((combined, staged_truncated || unstaged_truncated))
}

fn run_git_bounded(dir: &str, args: &[&str], limit: usize) -> Result<(String, bool), &'static str> {
    let mut command = std::process::Command::new("git");
    command
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    crate::adapter::process::configure_process_tree(&mut command);
    let mut child = command
        .spawn()
        .map_err(|_| "not a git repository (or git is unavailable)")?;
    let tree = match crate::adapter::process::ChildProcessTree::attach(&mut child) {
        Ok(tree) => tree,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err("could not isolate the git process");
        }
    };
    let stdout = child.stdout.take().ok_or("git stdout was unavailable")?;
    let (capture_tx, capture_rx) = std::sync::mpsc::sync_channel(1);
    let reader = thread::spawn(move || {
        let _ = capture_tx.send(read_bounded(stdout, limit));
    });
    let deadline = Instant::now() + GIT_COMMAND_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = tree.terminate_with_fallback(&mut child);
                let _ = child.wait();
                let _ = reader.join();
                return Err("git command timed out after 10 seconds");
            }
            Err(_) => {
                let _ = tree.terminate_with_fallback(&mut child);
                let _ = child.wait();
                let _ = reader.join();
                return Err("git command failed");
            }
        }
    };
    let captured = match capture_rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
    {
        Ok(captured) => captured,
        Err(_) => {
            let _ = tree.terminate_with_fallback(&mut child);
            let _ = reader.join();
            return Err("git command timed out after 10 seconds");
        }
    };
    let _ = reader.join();
    let (bytes, truncated) = captured.map_err(|_| "git output reader failed")?;
    if !status.success() {
        return Err("not a git repository (or git is unavailable)");
    }
    Ok((String::from_utf8_lossy(&bytes).into_owned(), truncated))
}

/// Keep at most `limit` bytes while continuing to drain the reader so the
/// producer cannot block on a full pipe.
fn read_bounded(mut reader: impl Read, limit: usize) -> io::Result<(Vec<u8>, bool)> {
    let mut kept = Vec::with_capacity(limit.min(64 * 1024));
    let mut buffer = [0_u8; 16 * 1024];
    let mut truncated = false;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(kept.len());
        let take = read.min(remaining);
        kept.extend_from_slice(&buffer[..take]);
        truncated |= take < read;
    }
    Ok((kept, truncated))
}

fn submit(state: &mut AppState, handle: &RuntimeHandle) {
    let text = state.composer().text();
    let mut reset_scroll = true;
    let text_without_images = crate::adapter::prompt_images::strip_image_placeholders(
        &text,
        state.pending_images().len(),
    );
    if !state.pending_images().is_empty()
        && (commands::parse_target_only(&text_without_images).is_some()
            || text_without_images.trim().is_empty())
    {
        submit_image_only(state, handle, &text_without_images);
        if reset_scroll {
            state.reset_scroll();
        }
        return;
    }
    match commands::parse(&text) {
        Submission::Exit => {
            state.take_composer();
            state.quit();
        }
        Submission::Attach { member } => {
            if state.member_backend(&member).is_none() {
                state.apply(RuntimeEvent::Notice(format!("unknown member: {member}")));
            } else if let Some(member) = state.request_attach_member_by_name(&member) {
                if send_runtime(state, handle, UiCommand::RequestAttach { member }) {
                    state.record_submission(&text);
                    state.take_composer();
                } else {
                    state.attach_request_send_failed();
                }
            }
        }
        Submission::TargetedSlash { member, body } => {
            if let Some(command) = state.targeted_skill_command(&member, &body) {
                if send_runtime(state, handle, command) {
                    state.record_submission(&text);
                    state.take_composer();
                }
            } else {
                state.apply(RuntimeEvent::Notice(format!(
                    "{body} is not a discovered prompt-invocable skill for {member}; use /attach <member> for that backend's native CLI"
                )));
            }
        }
        Submission::Runtime(command) => {
            let command = state.normalize_known_skill_invocation(command);
            // `/mode` and `/new` can be rejected by the runtime (for example,
            // when persistence fails or work is still active). Apply their UI
            // state only after the corresponding runtime event arrives.
            let user_target = match &command {
                UiCommand::UserMessage { target, .. }
                    if state.active_mode() == TerminalMode::Normal =>
                {
                    Some(target.clone())
                }
                _ => None,
            };
            if send_runtime(state, handle, command) {
                state.record_submission(&text);
                state.take_composer();
                if let Some(target) = user_target.as_ref() {
                    state.remember_user_message_target(target);
                }
            }
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
                if send_runtime(state, handle, UiCommand::Approve { id, decision }) {
                    state.record_submission(&text);
                    state.take_composer();
                }
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
        Submission::Invalid(message) => state.apply(RuntimeEvent::Notice(message)),
        Submission::NeedsTarget => {
            if state.active_mode() != TerminalMode::Normal {
                let body = text.trim().to_string();
                if send_runtime(
                    state,
                    handle,
                    UiCommand::UserMessage {
                        target: crate::domain::event::MessageTarget::Default,
                        body: body.clone(),
                    },
                ) {
                    state.record_submission(&body);
                    state.take_composer();
                }
            } else if let Some((target, body)) = state.inherited_user_message(&text) {
                if send_runtime(
                    state,
                    handle,
                    UiCommand::UserMessage {
                        target: target.clone(),
                        body: body.clone(),
                    },
                ) {
                    state.record_submission(&body);
                    state.take_composer();
                    state.remember_user_message_target(&target);
                }
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

fn drain_runtime_events(
    state: &mut AppState,
    events: &Receiver<RuntimeEvent>,
    notify_enabled: bool,
) -> bool {
    let mut changed = false;
    for _ in 0..MAX_RUNTIME_EVENTS_PER_DRAIN {
        match events.try_recv() {
            Ok(event) => {
                if notify_enabled && let Some(title) = notify_title_for(&event) {
                    let mut out = io::stdout();
                    let _ = notify::emit(&mut out, title);
                    let _ = out.flush();
                }
                state.apply(event);
                changed = true;
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                state.mark_runtime_unavailable();
                changed = true;
                break;
            }
        }
    }
    changed
}

fn paste_clipboard_image(state: &mut AppState) {
    match clipboard_image::paste_clipboard_image(state.workspace()) {
        Ok(image) => {
            if let Err(err) = state.attach_pending_image(image) {
                state.apply(RuntimeEvent::Notice(err));
            }
        }
        // Text-only clipboards are handled by Event::Paste; stay quiet.
        Err(err) if err.contains("clipboard has no image") => {}
        Err(err) => state.apply(RuntimeEvent::Notice(err)),
    }
}

fn submit_image_only(state: &mut AppState, handle: &RuntimeHandle, text: &str) {
    let (target, body) = if let Some(target) = commands::parse_target_only(text) {
        let body = image_only_user_body(&target);
        (target, body)
    } else if state.active_mode() != TerminalMode::Normal {
        (crate::domain::event::MessageTarget::Default, String::new())
    } else if let Some(target) = state.last_resolvable_message_target() {
        let body = image_only_user_body(&target);
        (target, body)
    } else {
        state.apply(RuntimeEvent::Notice(
            "image needs a target prefix: @member, @all, /ask, or /all (draft kept)".to_string(),
        ));
        return;
    };
    if send_runtime(
        state,
        handle,
        UiCommand::UserMessage {
            target: target.clone(),
            body: body.clone(),
        },
    ) {
        if !body.is_empty() {
            state.record_submission(&body);
        }
        state.take_composer();
        if state.active_mode() == TerminalMode::Normal {
            state.remember_user_message_target(&target);
        }
    }
}

fn image_only_user_body(target: &crate::domain::event::MessageTarget) -> String {
    match target {
        crate::domain::event::MessageTarget::All => "@all".to_string(),
        crate::domain::event::MessageTarget::Member(member) => format!("@{member}"),
        crate::domain::event::MessageTarget::Default
        | crate::domain::event::MessageTarget::Members(_) => String::new(),
    }
}

fn attach_pending_images(state: &AppState, command: UiCommand) -> UiCommand {
    match command {
        UiCommand::UserMessage { target, mut body } => {
            for image in state.pending_images() {
                crate::adapter::prompt_images::append_prompt_image(&mut body, image);
            }
            UiCommand::UserMessage { target, body }
        }
        other => other,
    }
}

fn handle_approval_keys(action: Action, state: &mut AppState, handle: &RuntimeHandle) -> bool {
    if state.drawer().is_some() || state.find_active() || state.pending_approvals().is_empty() {
        return false;
    }
    if !state.composer().is_empty() {
        return false;
    }
    match action {
        Action::InsertChar('y' | 'Y') | Action::Submit => {
            resolve_selected_approval(state, handle, ApprovalDecision::Approve)
        }
        Action::InsertChar('n' | 'N') => {
            resolve_selected_approval(state, handle, ApprovalDecision::Reject)
        }
        Action::CursorLeft => {
            state.select_prev_pending_approval();
            true
        }
        Action::CursorRight => {
            state.select_next_pending_approval();
            true
        }
        _ => false,
    }
}

fn resolve_selected_approval(
    state: &mut AppState,
    handle: &RuntimeHandle,
    decision: ApprovalDecision,
) -> bool {
    let Some(id) = state.selected_pending_approval().map(|pending| pending.id) else {
        return false;
    };
    send_runtime(state, handle, UiCommand::Approve { id, decision })
}

fn send_runtime(state: &mut AppState, handle: &RuntimeHandle, command: UiCommand) -> bool {
    let command = attach_pending_images(state, command);
    match handle.try_send(command) {
        RuntimeCommandSend::Sent => true,
        RuntimeCommandSend::Full => {
            state.apply(RuntimeEvent::Notice(
                "runtime input queue is busy; command was not sent — try again".to_string(),
            ));
            false
        }
        RuntimeCommandSend::Disconnected => {
            state.mark_runtime_unavailable();
            false
        }
    }
}

/// Titles for attention-needed runtime events (terminal BEL + OSC 9).
fn notify_title_for(event: &RuntimeEvent) -> Option<&'static str> {
    match event {
        RuntimeEvent::ApprovalRequested { .. } => Some("Asterline: approval needed"),
        RuntimeEvent::RoutePaused { .. } => Some("Asterline: route paused"),
        RuntimeEvent::RunUpdated { run }
            if run.status == crate::domain::event::RunStatus::Blocked =>
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
        ChatItem, MemberStatus, MemberSummary, RunId, RunStatus, RunStepStatus, RunStepSummary,
        RunSummary,
    };
    use crate::domain::team::{
        BackendKind, DefaultTarget, MemberId, PermissionMode, SandboxPolicy, SessionPolicy,
        TeamConfig, TeamMember,
    };
    use crate::runtime::{self, Runners};
    use crate::store::sqlite::SqliteStore;
    use crate::tui::drawers::Drawer;
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::SystemTime;

    fn git_test_repo(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "asterline-diff-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        git_test(&dir, &["init", "--quiet"]);
        dir
    }

    fn git_test(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    fn mouse_event(kind: MouseEventKind, column: u16, row: u16) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: crossterm::event::KeyModifiers::NONE,
        })
    }

    #[test]
    fn coalesce_mouse_drags_keeps_the_latest_left_drag() {
        let events = vec![
            mouse_event(MouseEventKind::Down(MouseButton::Left), 1, 1),
            mouse_event(MouseEventKind::Drag(MouseButton::Left), 2, 1),
            mouse_event(MouseEventKind::Drag(MouseButton::Left), 8, 3),
            mouse_event(MouseEventKind::Up(MouseButton::Left), 8, 3),
        ];
        let coalesced = coalesce_mouse_drags(events);
        assert_eq!(coalesced.len(), 3);
        match &coalesced[1] {
            Event::Mouse(mouse) => {
                assert!(matches!(
                    mouse.kind,
                    MouseEventKind::Drag(MouseButton::Left)
                ));
                assert_eq!((mouse.column, mouse.row), (8, 3));
            }
            other => panic!("expected drag, got {other:?}"),
        }
    }

    #[test]
    fn coalesce_mouse_drags_does_not_drop_keys_or_scrolls() {
        let events = vec![
            mouse_event(MouseEventKind::ScrollUp, 1, 1),
            Event::Key(KeyEvent::new(
                crossterm::event::KeyCode::Char('a'),
                crossterm::event::KeyModifiers::NONE,
            )),
            mouse_event(MouseEventKind::ScrollDown, 1, 1),
        ];
        assert_eq!(coalesce_mouse_drags(events.clone()), events);
    }

    #[test]
    fn attach_exit_hint_matches_platform_eof_sequence() {
        if cfg!(windows) {
            assert_eq!(attach_exit_hint(), "type /exit or press Ctrl+Z then Enter");
        } else {
            assert_eq!(attach_exit_hint(), "type /exit or press Ctrl+D");
        }
    }

    #[test]
    fn bounded_capture_keeps_prefix_and_drains_the_rest() {
        let input = std::io::Cursor::new(b"0123456789".to_vec());

        let (captured, truncated) = read_bounded(input, 4).unwrap();

        assert_eq!(captured, b"0123");
        assert!(truncated);
    }

    #[test]
    fn diff_includes_staged_and_unstaged_changes() {
        let dir = git_test_repo("head");
        git_test(&dir, &["config", "user.email", "tests@example.invalid"]);
        git_test(&dir, &["config", "user.name", "Asterline Tests"]);
        std::fs::write(dir.join("tracked.txt"), "base\n").unwrap();
        git_test(&dir, &["add", "tracked.txt"]);
        git_test(&dir, &["commit", "--quiet", "-m", "base"]);

        std::fs::write(dir.join("tracked.txt"), "unstaged\n").unwrap();
        std::fs::write(dir.join("staged.txt"), "staged\n").unwrap();
        git_test(&dir, &["add", "staged.txt"]);

        let diff = compute_git_diff(dir.to_str().unwrap());

        assert!(diff.contains("tracked.txt"));
        assert!(diff.contains("+unstaged"));
        assert!(diff.contains("staged.txt"));
        assert!(diff.contains("+staged"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn diff_handles_unborn_head_and_lists_untracked_files() {
        let dir = git_test_repo("unborn");
        std::fs::write(dir.join("new.txt"), "from index\n").unwrap();
        git_test(&dir, &["add", "new.txt"]);
        std::fs::write(dir.join("new.txt"), "from index\nfrom worktree\n").unwrap();
        std::fs::write(dir.join("untracked.txt"), "not added\n").unwrap();

        let diff = compute_git_diff(dir.to_str().unwrap());

        assert!(diff.contains("+from index"));
        assert!(diff.contains("+from worktree"));
        assert!(diff.contains("Untracked files:"));
        assert!(diff.contains("untracked.txt"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn runtime_event_drain_yields_after_a_bounded_batch() {
        let (tx, rx) = mpsc::channel();
        for index in 0..=MAX_RUNTIME_EVENTS_PER_DRAIN {
            tx.send(RuntimeEvent::Notice(format!("event {index}")))
                .unwrap();
        }
        let mut state = AppState::new(Vec::new());

        drain_runtime_events(&mut state, &rx, false);

        assert!(
            rx.try_recv().is_ok(),
            "one event should remain for next frame"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn keyboard_enhancement_cleanup_emits_protocol_pop() {
        let mut bytes = Vec::new();
        disable_keyboard_enhancement(&mut bytes, true).unwrap();
        assert_eq!(bytes, b"\x1b[<1u");
    }

    #[cfg(not(windows))]
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
    fn disconnected_runtime_event_channel_disables_input() {
        let (event_tx, event_rx) = mpsc::channel::<RuntimeEvent>();
        drop(event_tx);
        let mut state = AppState::new(Vec::new());

        drain_runtime_events(&mut state, &event_rx, false);

        assert!(!state.runtime_available());
        assert!(matches!(
            state.chat().last(),
            Some(ChatItem::Error { member: None, message })
                if message.contains("input is disabled")
        ));
    }

    #[test]
    fn failed_runtime_send_keeps_draft_and_disables_input() {
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
        handle.send(UiCommand::Shutdown);
        let _ = join.join();
        let mut state = AppState::new(Vec::new());
        state.insert_text("/retry");

        submit(&mut state, &handle);

        assert!(!state.runtime_available());
        assert_eq!(state.composer().text(), "/retry");
    }

    #[test]
    fn exit_command_quits_the_tui_without_waiting_for_runtime_input() {
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
        state.insert_text("/exit");

        submit(&mut state, &handle);

        assert!(state.should_quit());
        assert!(state.composer().is_empty());
        handle.send(UiCommand::Shutdown);
        let _ = join.join();
    }

    #[test]
    fn targeted_attach_opens_the_members_native_session() {
        let config = TeamConfig::new("test", "/tmp/ws").with_member(TeamMember::new(
            "builder",
            "Builder",
            BackendKind::Codex,
            "implementation",
        ));
        let (event_tx, event_rx) = mpsc::channel();
        let (handle, join) = runtime::spawn(
            config,
            SqliteStore::in_memory().unwrap(),
            Runners::new(),
            event_tx,
            true,
            true,
            None,
        );
        let mut state = AppState::new(Vec::new());
        state.apply(RuntimeEvent::Ready {
            modes: Default::default(),
            mode_overrides: Default::default(),
            suggested_verify: None,
            team: "test".to_string(),
            workspace: "/tmp/ws".to_string(),
            default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
            runs: Vec::new(),
            members: vec![MemberSummary {
                id: MemberId::new("builder"),
                display_name: "Builder".to_string(),
                backend: BackendKind::Codex,
                role: "implementation".to_string(),
                status: MemberStatus::Idle,
                session: None,
                cwd: String::new(),
                model: None,
                effort: None,
                sandbox: SandboxPolicy::ReadOnly,
                permission_mode: Some(PermissionMode::Default),
                approvals_reviewer: crate::domain::team::CodexApprovalsReviewer::User,
                session_policy: SessionPolicy::Resume,
            }],
        });
        state.insert_text("@Builder /attach");

        submit(&mut state, &handle);

        assert!(state.composer().is_empty());
        let granted = (0..4)
            .filter_map(|_| event_rx.recv_timeout(Duration::from_secs(1)).ok())
            .find(|event| matches!(event, RuntimeEvent::AttachGranted { .. }))
            .expect("attach grant");
        state.apply(granted);
        let request = state.take_attach_request().expect("native attach request");
        assert_eq!(request.member, MemberId::new("builder"));
        assert_eq!(request.display_name, "Builder");

        handle.send(UiCommand::Shutdown);
        let _ = join.join();
    }

    #[test]
    fn unknown_targeted_slash_stays_out_of_noninteractive_prompt_delivery() {
        let (event_tx, _event_rx) = mpsc::channel();
        let (handle, join) = runtime::spawn(
            TeamConfig::new("test", "/tmp/ws"),
            SqliteStore::in_memory().unwrap(),
            Runners::new(),
            event_tx,
            true,
            true,
            None,
        );
        let mut state = AppState::new(Vec::new());
        state.apply(RuntimeEvent::Ready {
            modes: Default::default(),
            mode_overrides: Default::default(),
            suggested_verify: None,
            team: "test".to_string(),
            workspace: "/tmp/ws".to_string(),
            default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
            runs: Vec::new(),
            members: vec![MemberSummary {
                id: MemberId::new("builder"),
                display_name: "Builder".to_string(),
                backend: BackendKind::Codex,
                role: "implementation".to_string(),
                status: MemberStatus::Idle,
                session: None,
                cwd: String::new(),
                model: None,
                effort: None,
                sandbox: SandboxPolicy::ReadOnly,
                permission_mode: Some(PermissionMode::Default),
                approvals_reviewer: crate::domain::team::CodexApprovalsReviewer::User,
                session_policy: SessionPolicy::Resume,
            }],
        });
        state.insert_text("@builder /not-a-native-command");

        submit(&mut state, &handle);

        assert_eq!(state.composer().text(), "@builder /not-a-native-command");
        assert!(matches!(
            state.chat().last(),
            Some(ChatItem::Notice { text }) if text.contains("not a discovered prompt-invocable skill")
        ));
        handle.send(UiCommand::Shutdown);
        let _ = join.join();
    }

    #[test]
    fn busy_runtime_queue_keeps_draft_without_disabling_input() {
        let (evt_tx, _evt_rx) = mpsc::sync_channel(0);
        let (handle, join) = runtime::spawn_bounded(
            TeamConfig::new("test", "/tmp/ws"),
            SqliteStore::in_memory().unwrap(),
            Runners::new(),
            evt_tx,
            true,
            true,
            None,
        );
        // Ready cannot enter the zero-capacity event sink, so the runtime
        // deliberately stops consuming ordinary UI work. Fill that bounded
        // queue and exercise the product submit path at its Full boundary.
        while handle.try_send(UiCommand::Retry) == RuntimeCommandSend::Sent {}

        let mut state = AppState::new(Vec::new());
        state.insert_text("/retry");
        submit(&mut state, &handle);

        assert!(state.runtime_available());
        assert_eq!(state.composer().text(), "/retry");
        assert!(matches!(
            state.chat().last(),
            Some(ChatItem::Notice { text }) if text.contains("queue is busy")
        ));

        assert_eq!(
            handle.try_send(UiCommand::Shutdown),
            RuntimeCommandSend::Sent
        );
        join.join().unwrap();
    }

    #[test]
    fn no_argument_command_with_trailing_text_keeps_state_and_draft() {
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
        let mut state = AppState::new(vec![ChatItem::Notice {
            text: "existing chat".to_string(),
        }]);
        state.apply(RuntimeEvent::ModeChanged {
            mode: TerminalMode::Plan,
        });
        state.insert_text("/new accidental");

        submit(&mut state, &handle);

        assert_eq!(state.active_mode(), TerminalMode::Plan);
        assert_eq!(state.composer().text(), "/new accidental");
        assert!(matches!(
            state.chat().last(),
            Some(ChatItem::Notice { text }) if text.contains("does not accept arguments")
        ));

        handle.send(UiCommand::Shutdown);
        let _ = join.join();
    }

    #[test]
    fn escape_and_interrupt_abort_verification_only_work() {
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
        handle.send(UiCommand::Shutdown);
        let _ = join.join();

        let verifying_state = || {
            let mut state = AppState::new(Vec::new());
            state.apply(RuntimeEvent::RunUpdated {
                run: RunSummary {
                    id: RunId(1),
                    number: 0,
                    goal: "ship parser".to_string(),
                    status: RunStatus::Verifying,
                    coordinator: None,
                    verification: None,
                    created_at: "2026-08-09 10:00:00".to_string(),
                    updated_at: "2026-08-09 10:00:00".to_string(),
                    attempt: 1,
                    events: Vec::new(),
                    steps: Vec::new(),
                    mode: None,
                    legacy_mode: None,
                },
            });
            assert_eq!(state.running_count(), 0);
            assert!(state.verification_active());
            state
        };

        let mut escape = verifying_state();
        handle_action(Action::CloseOverlay, &mut escape, &handle);
        assert!(
            !escape.runtime_available(),
            "Esc must dispatch global cancellation"
        );

        let mut interrupt = verifying_state();
        interrupt.insert_text("keep this draft");
        handle_action(Action::Interrupt, &mut interrupt, &handle);
        assert!(
            !interrupt.runtime_available(),
            "Ctrl+C must dispatch global cancellation"
        );
        assert!(!interrupt.should_quit());
        assert_eq!(interrupt.composer().text(), "keep this draft");
    }

    #[test]
    fn escape_aborts_paused_route_even_without_running_member() {
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
        handle.send(UiCommand::Shutdown);
        let _ = join.join();
        let mut state = AppState::new(Vec::new());
        state.apply(RuntimeEvent::RoutePaused {
            turn: crate::domain::event::TurnId(1),
            from: MemberId::new("builder"),
            to: vec!["reviewer".to_string()],
            reason: "relay paused".to_string(),
            queued: 1,
        });

        handle_action(Action::CloseOverlay, &mut state, &handle);

        assert!(
            !state.runtime_available(),
            "Esc must dispatch global cancellation"
        );
        assert_eq!(state.paused_routes(), 0);
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
    fn mode_and_new_session_wait_for_runtime_acknowledgement() {
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
        let mut state = AppState::new(vec![ChatItem::Notice {
            text: "existing chat".to_string(),
        }]);

        for ch in "/mode plan".chars() {
            state.insert_char(ch);
        }
        submit(&mut state, &handle);
        assert_eq!(state.active_mode(), TerminalMode::Normal);

        state.apply(RuntimeEvent::ModeChanged {
            mode: TerminalMode::Plan,
        });
        for ch in "/new".chars() {
            state.insert_char(ch);
        }
        submit(&mut state, &handle);
        assert_eq!(state.active_mode(), TerminalMode::Plan);
        assert!(!state.chat().is_empty());

        state.apply(RuntimeEvent::SessionReset);
        assert_eq!(state.active_mode(), TerminalMode::Normal);
        assert!(state.chat().is_empty());

        handle.send(UiCommand::Shutdown);
        let _ = join.join();
    }

    #[test]
    fn submitting_bare_mode_opens_the_required_mode_picker() {
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
        state.insert_text("/mode");

        handle_action(Action::Submit, &mut state, &handle);

        assert_eq!(state.drawer(), Some(Drawer::Mode));
        assert!(state.mode_editor().is_some());
        assert!(state.composer().is_empty());

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
            modes: Default::default(),
            mode_overrides: Default::default(),
            suggested_verify: None,
            team: "test".to_string(),
            workspace: "/tmp/ws".to_string(),
            default_target: None,
            runs: Vec::new(),
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
                approvals_reviewer: crate::domain::team::CodexApprovalsReviewer::User,
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
        assert_eq!(
            state.inherited_user_message("one more"),
            Some((
                crate::domain::event::MessageTarget::Member(crate::domain::team::MemberId::new(
                    "builder"
                )),
                "@builder one more".to_string()
            ))
        );

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
            modes: Default::default(),
            mode_overrides: Default::default(),
            suggested_verify: None,
            team: "test".to_string(),
            workspace: "/tmp/ws".to_string(),
            default_target: None,
            runs: Vec::new(),
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
                approvals_reviewer: crate::domain::team::CodexApprovalsReviewer::User,
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
        state.apply(RuntimeEvent::SessionReset);
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
            modes: Default::default(),
            mode_overrides: Default::default(),
            suggested_verify: None,
            team: "mixed".to_string(),
            workspace: "/tmp/ws".to_string(),
            default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
            runs: vec![RunSummary {
                id: RunId(1),
                number: 0,
                goal: "ship parser".to_string(),
                status: RunStatus::Done,
                coordinator: Some(MemberId::new("builder")),
                verification: None,
                created_at: "2026-06-28 10:00:00".to_string(),
                updated_at: "2026-06-28 10:00:00".to_string(),
                attempt: 1,
                events: Vec::new(),
                steps: Vec::new(),
                mode: None,
                legacy_mode: None,
            }],
            members: Vec::new(),
        });
        state.toggle_drawer(Drawer::Runs);

        handle_action(Action::Submit, &mut state, &handle);

        assert_eq!(state.drawer(), None);
        assert_eq!(state.composer().text(), "/mode plan");

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
            modes: Default::default(),
            mode_overrides: Default::default(),
            suggested_verify: None,
            team: "mixed".to_string(),
            workspace: "/tmp/ws".to_string(),
            default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
            runs: vec![RunSummary {
                id: RunId(1),
                number: 0,
                goal: "ship parser".to_string(),
                status: RunStatus::Running,
                coordinator: Some(MemberId::new("builder")),
                verification: None,
                created_at: "2026-06-28 10:00:00".to_string(),
                updated_at: "2026-06-28 10:00:00".to_string(),
                attempt: 1,
                events: Vec::new(),
                steps: vec![RunStepSummary {
                    number: 1,
                    status: RunStepStatus::Doing,
                    owner: None,
                    title: "Wire checklist UI".to_string(),
                    note: None,
                    updated_at: "2026-06-28 10:05:00".to_string(),
                }],
                mode: None,
                legacy_mode: None,
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
            modes: Default::default(),
            mode_overrides: Default::default(),
            suggested_verify: None,
            team: "mixed".to_string(),
            workspace: "/tmp/ws".to_string(),
            default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
            runs: vec![RunSummary {
                id: RunId(1),
                number: 0,
                goal: "ship parser".to_string(),
                status: RunStatus::Running,
                coordinator: Some(MemberId::new("builder")),
                verification: None,
                created_at: "2026-06-28 10:00:00".to_string(),
                updated_at: "2026-06-28 10:00:00".to_string(),
                attempt: 1,
                events: Vec::new(),
                steps: vec![RunStepSummary {
                    number: 1,
                    status: RunStepStatus::Todo,
                    owner: Some(MemberId::new("builder")),
                    title: "Wire checklist UI".to_string(),
                    note: None,
                    updated_at: "2026-06-28 10:05:00".to_string(),
                }],
                mode: None,
                legacy_mode: None,
            }],
            members: Vec::new(),
        });
        state.toggle_drawer(Drawer::Runs);

        handle_action(Action::HistoryNext, &mut state, &handle);
        handle_action(Action::Complete, &mut state, &handle);

        assert_eq!(state.drawer(), None);
        assert_eq!(
            state.composer().text(),
            "@builder Start run-1 step #1: Wire checklist UI. Update the checklist with @@run_step as you progress."
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
            modes: Default::default(),
            mode_overrides: Default::default(),
            suggested_verify: None,
            team: "mixed".to_string(),
            workspace: "/tmp/ws".to_string(),
            default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
            runs: Vec::new(),
            members: Vec::new(),
        });
        state.toggle_drawer(Drawer::Runs);

        assert!(!state.runs_detail());
        handle_action(Action::InsertChar('x'), &mut state, &handle);
        assert!(state.runs_detail());
        assert!(state.composer().is_empty());

        state.insert_char('a');
        handle_action(Action::InsertChar('x'), &mut state, &handle);
        assert!(state.runs_detail());
        assert_eq!(state.composer().text(), "ax");

        handle.send(UiCommand::Shutdown);
        let _ = join.join();
    }

    #[test]
    fn user_message_picks_up_pending_images() {
        let dir =
            std::env::temp_dir().join(format!("asterline-pending-img-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("shot.png");
        std::fs::write(&path, [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']).unwrap();
        let mut state = AppState::new(Vec::new());
        let image = crate::adapter::prompt_images::PromptImage::from_path(&path).unwrap();
        state.attach_pending_image(image).unwrap();
        let command = attach_pending_images(
            &state,
            UiCommand::UserMessage {
                target: crate::domain::event::MessageTarget::Member(MemberId::new("builder")),
                body: "@builder look".to_string(),
            },
        );
        match command {
            UiCommand::UserMessage { body, .. } => {
                assert!(body.starts_with("@builder look"));
                assert!(body.contains("[asterline-image]:"));
                assert!(body.contains("shot.png"));
            }
            other => panic!("expected user message, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
