//! Key bindings. No function keys are ever bound (a hard product requirement);
//! `resolve` returns `None` for `KeyCode::F(_)`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A resolved input action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Submit,
    InsertChar(char),
    InsertNewline,
    Backspace,
    DeleteWord,
    ClearLine,
    CursorLeft,
    CursorRight,
    Home,
    End,
    ScrollUp,
    ScrollDown,
    HistoryPrev,
    HistoryNext,
    ToggleLogs,
    TogglePalette,
    HistorySearch,
    CloseOverlay,
    Complete,
    Interrupt,
    ToggleExpand,
    NextMember,
    PrevMember,
}

/// Map a key press to an action, or `None` if unbound. Function keys are
/// intentionally never mapped.
pub fn resolve(key: KeyEvent) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    match key.code {
        KeyCode::F(_) => None,
        // Alt+Enter / Shift+Enter insert a newline; plain Enter submits.
        KeyCode::Enter if alt || key.modifiers.contains(KeyModifiers::SHIFT) => {
            Some(Action::InsertNewline)
        }
        KeyCode::Enter => Some(Action::Submit),
        KeyCode::Tab => Some(Action::Complete),
        KeyCode::Esc => Some(Action::CloseOverlay),
        KeyCode::Backspace => Some(Action::Backspace),
        KeyCode::Left => Some(Action::CursorLeft),
        KeyCode::Right => Some(Action::CursorRight),
        KeyCode::Home => Some(Action::Home),
        KeyCode::End => Some(Action::End),
        // Arrows recall prompt history (or move the popup selection); page
        // keys scroll the conversation. Mirrors shell / codex conventions.
        KeyCode::Up => Some(Action::HistoryPrev),
        KeyCode::Down => Some(Action::HistoryNext),
        KeyCode::PageUp => Some(Action::ScrollUp),
        KeyCode::PageDown => Some(Action::ScrollDown),
        KeyCode::Char('c') if ctrl => Some(Action::Interrupt),
        KeyCode::Char('g') if ctrl => Some(Action::ToggleExpand),
        KeyCode::Char('l') if ctrl => Some(Action::ToggleLogs),
        KeyCode::Char('o') if ctrl => Some(Action::ToggleExpand),
        KeyCode::Char('t') if ctrl => Some(Action::ToggleExpand),
        KeyCode::Char('r') if ctrl => Some(Action::HistorySearch),
        KeyCode::Char('p') if ctrl => Some(Action::TogglePalette),
        KeyCode::Char('u') if ctrl => Some(Action::ClearLine),
        KeyCode::Char('w') if ctrl => Some(Action::DeleteWord),
        KeyCode::Char('a') if ctrl => Some(Action::Home),
        KeyCode::Char('e') if ctrl => Some(Action::End),
        KeyCode::Char('n') if ctrl => Some(Action::NextMember),
        KeyCode::Char('b') if ctrl => Some(Action::PrevMember),
        KeyCode::Char(c) if !ctrl && !alt => Some(Action::InsertChar(c)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn function_keys_are_never_bound() {
        for n in 1..=12 {
            assert_eq!(resolve(key(KeyCode::F(n), KeyModifiers::NONE)), None);
        }
    }

    #[test]
    fn core_bindings_resolve() {
        assert_eq!(
            resolve(key(KeyCode::Enter, KeyModifiers::NONE)),
            Some(Action::Submit)
        );
        assert_eq!(
            resolve(key(KeyCode::Esc, KeyModifiers::NONE)),
            Some(Action::CloseOverlay)
        );
        assert_eq!(
            resolve(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Action::Interrupt)
        );
        assert_eq!(
            resolve(key(KeyCode::Char('l'), KeyModifiers::CONTROL)),
            Some(Action::ToggleLogs)
        );
        assert_eq!(
            resolve(key(KeyCode::Char('a'), KeyModifiers::NONE)),
            Some(Action::InsertChar('a'))
        );
    }

    #[test]
    fn arrows_recall_history_and_page_keys_scroll() {
        assert_eq!(
            resolve(key(KeyCode::Up, KeyModifiers::NONE)),
            Some(Action::HistoryPrev)
        );
        assert_eq!(
            resolve(key(KeyCode::Down, KeyModifiers::NONE)),
            Some(Action::HistoryNext)
        );
        assert_eq!(
            resolve(key(KeyCode::PageUp, KeyModifiers::NONE)),
            Some(Action::ScrollUp)
        );
        assert_eq!(
            resolve(key(KeyCode::PageDown, KeyModifiers::NONE)),
            Some(Action::ScrollDown)
        );
    }

    #[test]
    fn enter_submits_but_modified_enter_inserts_newline() {
        assert_eq!(
            resolve(key(KeyCode::Enter, KeyModifiers::NONE)),
            Some(Action::Submit)
        );
        assert_eq!(
            resolve(key(KeyCode::Enter, KeyModifiers::ALT)),
            Some(Action::InsertNewline)
        );
        assert_eq!(
            resolve(key(KeyCode::Enter, KeyModifiers::SHIFT)),
            Some(Action::InsertNewline)
        );
    }

    #[test]
    fn control_letters_are_not_inserted_as_text() {
        // Ctrl+x is unbound, but must not be typed into the composer.
        assert_eq!(
            resolve(key(KeyCode::Char('x'), KeyModifiers::CONTROL)),
            None
        );
    }
}
