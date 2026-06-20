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

use std::io::{self, Write};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use crossterm::event::{self, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::domain::event::{RuntimeEvent, UiCommand};
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
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, &mut state, &handle, &events);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
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

        if event::poll(POLL_INTERVAL)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && let Some(action) = keymap::resolve(key)
        {
            handle_action(action, state, handle);
        }

        if let Some(req) = state.take_attach_request() {
            attach_to_member(terminal, state, &req)?;
        }

        if state.should_quit() {
            return Ok(());
        }
    }
}

/// Hand the whole terminal to the member's real interactive CLI (resuming its
/// session), then restore Asterline when that CLI exits.
fn attach_to_member(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut AppState,
    req: &attach::AttachRequest,
) -> io::Result<()> {
    let (program, args) = req.command();

    // --- Suspend Asterline: hand the real terminal to the child CLI. ---
    // Restore the cooked terminal, leave our alternate screen, and show the
    // cursor, flushing so the child starts from a clean, owned main screen.
    disable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, LeaveAlternateScreen, crossterm::cursor::Show)?;
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
    execute!(out, EnterAlternateScreen)?;
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
    Ok(())
}

fn handle_action(action: Action, state: &mut AppState, handle: &RuntimeHandle) {
    match action {
        Action::InsertChar(ch) => state.insert_char(ch),
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
            if state.completion().is_some() {
                state.popup_up();
            } else {
                state.scroll_up();
            }
        }
        Action::ScrollDown => {
            if state.completion().is_some() {
                state.popup_down();
            } else {
                state.scroll_down();
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

fn submit(state: &mut AppState, handle: &RuntimeHandle) {
    let text = state.take_composer();
    match commands::parse(&text) {
        Submission::Runtime(command) => {
            if let UiCommand::UserMessage { target, body } = &command {
                state.handle_user_message_submitted(target, body.clone());
            }
            handle.send(command);
        }
        Submission::Drawer(drawer) => state.toggle_drawer(drawer),
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
