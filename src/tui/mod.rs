//! Chat-first terminal UI: a single scrolling conversation column, a bottom
//! composer, and overlay drawers (logs / team / command palette). State is
//! driven entirely by `RuntimeEvent`s; no string matching.

pub mod app_state;
pub mod chat_view;
pub mod commands;
pub mod completion;
pub mod composer;
pub mod drawers;
pub mod keymap;

use std::io;
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

        if state.should_quit() {
            return Ok(());
        }
    }
}

fn handle_action(action: Action, state: &mut AppState, handle: &RuntimeHandle) {
    match action {
        Action::InsertChar(ch) => state.insert_char(ch),
        Action::Backspace => state.backspace(),
        Action::DeleteWord => state.delete_word(),
        Action::ClearLine => state.clear_composer(),
        Action::CursorLeft => state.cursor_left(),
        Action::CursorRight => state.cursor_right(),
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
        Action::Complete => {
            state.accept_completion();
        }
        Action::CloseOverlay => {
            if state.completion().is_some() {
                state.dismiss_popup();
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
            submit(state, handle);
        }
    }
}

fn submit(state: &mut AppState, handle: &RuntimeHandle) {
    let text = state.take_composer();
    match commands::parse(&text) {
        Submission::Runtime(command) => {
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
