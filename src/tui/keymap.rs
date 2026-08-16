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
    ToggleThinking,
    ToggleDiffs,
    ToggleTools,
    NextMember,
    PrevMember,
    /// Pull the last queued (not-yet-started) prompt back into the composer.
    EditQueued,
    /// Read a bitmap from the OS clipboard and attach it to the next send.
    /// Text paste still arrives as `Event::Paste`; this action never inserts
    /// clipboard text, so Cmd/Ctrl+V does not double-paste.
    PasteClipboard,
}

/// Map a key press to an action, or `None` if unbound. Function keys are
/// intentionally never mapped.
pub fn resolve(key: KeyEvent) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let super_key = key.modifiers.contains(KeyModifiers::SUPER);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    match key.code {
        KeyCode::F(_) => None,
        // Shift+Enter inserts a newline; Alt+Enter is kept as a fallback for
        // terminals that cannot report Shift+Enter distinctly.
        KeyCode::Enter if alt || key.modifiers.contains(KeyModifiers::SHIFT) => {
            Some(Action::InsertNewline)
        }
        KeyCode::Enter => Some(Action::Submit),
        KeyCode::Tab => Some(Action::Complete),
        KeyCode::Esc => Some(Action::CloseOverlay),
        // macOS Cmd+Backspace: some terminals report SUPER; others remap it to
        // Ctrl+U. Both must clear the current composer line only.
        KeyCode::Backspace if super_key || ctrl => Some(Action::ClearLine),
        KeyCode::Backspace => Some(Action::Backspace),
        KeyCode::Left if shift => Some(Action::EditQueued),
        KeyCode::Left => Some(Action::CursorLeft),
        KeyCode::Right => Some(Action::CursorRight),
        KeyCode::Home => Some(Action::Home),
        KeyCode::End => Some(Action::End),
        // Actual mouse wheel events are handled separately by the TUI. Arrows
        // retain shell-style prompt history (or move a popup selection).
        KeyCode::Up => Some(Action::HistoryPrev),
        KeyCode::Down => Some(Action::HistoryNext),
        KeyCode::PageUp => Some(Action::ScrollUp),
        KeyCode::PageDown => Some(Action::ScrollDown),
        KeyCode::Char('c') if ctrl => Some(Action::Interrupt),
        // Ctrl+V / Cmd+V / Ctrl+Shift+V attach a clipboard image. Do not bind
        // Ctrl+C — that remains interrupt.
        KeyCode::Char('v' | 'V') if ctrl || super_key => Some(Action::PasteClipboard),
        KeyCode::Char('g') if ctrl => Some(Action::ToggleDiffs),
        KeyCode::Char('l') if ctrl => Some(Action::ToggleLogs),
        KeyCode::Char('o') if ctrl => Some(Action::ToggleTools),
        KeyCode::Char('t') if ctrl => Some(Action::ToggleThinking),
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
    fn command_or_ctrl_backspace_clears_the_current_line() {
        assert_eq!(
            resolve(key(KeyCode::Backspace, KeyModifiers::SUPER)),
            Some(Action::ClearLine)
        );
        assert_eq!(
            resolve(key(KeyCode::Backspace, KeyModifiers::CONTROL)),
            Some(Action::ClearLine)
        );
        assert_eq!(
            resolve(key(KeyCode::Char('u'), KeyModifiers::CONTROL)),
            Some(Action::ClearLine)
        );
        assert_eq!(
            resolve(key(KeyCode::Backspace, KeyModifiers::NONE)),
            Some(Action::Backspace)
        );
    }

    #[test]
    fn expand_shortcuts_are_separate() {
        assert_eq!(
            resolve(key(KeyCode::Char('t'), KeyModifiers::CONTROL)),
            Some(Action::ToggleThinking)
        );
        assert_eq!(
            resolve(key(KeyCode::Char('g'), KeyModifiers::CONTROL)),
            Some(Action::ToggleDiffs)
        );
        assert_eq!(
            resolve(key(KeyCode::Char('o'), KeyModifiers::CONTROL)),
            Some(Action::ToggleTools)
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

    #[test]
    fn paste_image_is_ctrl_or_cmd_v_not_ctrl_c() {
        assert_eq!(
            resolve(key(KeyCode::Char('v'), KeyModifiers::CONTROL)),
            Some(Action::PasteClipboard)
        );
        assert_eq!(
            resolve(key(KeyCode::Char('v'), KeyModifiers::SUPER)),
            Some(Action::PasteClipboard)
        );
        assert_eq!(
            resolve(key(
                KeyCode::Char('v'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )),
            Some(Action::PasteClipboard)
        );
        assert_eq!(
            resolve(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Action::Interrupt)
        );
    }

    #[test]
    fn shift_left_edits_queued_prompt_and_shift_right_is_not_selection() {
        assert_eq!(
            resolve(key(KeyCode::Left, KeyModifiers::SHIFT)),
            Some(Action::EditQueued)
        );
        assert_eq!(
            resolve(key(KeyCode::Right, KeyModifiers::SHIFT)),
            Some(Action::CursorRight)
        );
        assert_eq!(
            resolve(key(KeyCode::Left, KeyModifiers::NONE)),
            Some(Action::CursorLeft)
        );
    }
}
