//! Key bindings. No function keys are ever bound (a hard product requirement);
//! `resolve` returns `None` for `KeyCode::F(_)`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A resolved input action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Submit,
    InsertChar(char),
    Backspace,
    DeleteWord,
    ClearLine,
    CursorLeft,
    CursorRight,
    Home,
    End,
    ScrollUp,
    ScrollDown,
    ToggleLogs,
    ToggleTeam,
    TogglePalette,
    CloseOverlay,
    Interrupt,
}

/// Map a key press to an action, or `None` if unbound. Function keys are
/// intentionally never mapped.
pub fn resolve(key: KeyEvent) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    match key.code {
        KeyCode::F(_) => None,
        KeyCode::Enter => Some(Action::Submit),
        KeyCode::Esc => Some(Action::CloseOverlay),
        KeyCode::Backspace => Some(Action::Backspace),
        KeyCode::Left => Some(Action::CursorLeft),
        KeyCode::Right => Some(Action::CursorRight),
        KeyCode::Home => Some(Action::Home),
        KeyCode::End => Some(Action::End),
        KeyCode::Up | KeyCode::PageUp => Some(Action::ScrollUp),
        KeyCode::Down | KeyCode::PageDown => Some(Action::ScrollDown),
        KeyCode::Char('c') if ctrl => Some(Action::Interrupt),
        KeyCode::Char('l') if ctrl => Some(Action::ToggleLogs),
        KeyCode::Char('r') if ctrl => Some(Action::ToggleTeam),
        KeyCode::Char('p') if ctrl => Some(Action::TogglePalette),
        KeyCode::Char('u') if ctrl => Some(Action::ClearLine),
        KeyCode::Char('w') if ctrl => Some(Action::DeleteWord),
        KeyCode::Char('a') if ctrl => Some(Action::Home),
        KeyCode::Char('e') if ctrl => Some(Action::End),
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
    fn control_letters_are_not_inserted_as_text() {
        // Ctrl+x is unbound, but must not be typed into the composer.
        assert_eq!(
            resolve(key(KeyCode::Char('x'), KeyModifiers::CONTROL)),
            None
        );
    }
}
