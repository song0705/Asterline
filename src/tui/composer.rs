//! The bottom composer: a single logical input line with a movable cursor and
//! word/line editing. Stores characters so cursor math is Unicode-safe.

#[derive(Debug, Default)]
pub struct Composer {
    chars: Vec<char>,
    cursor: usize,
}

impl Composer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    /// Cursor position as a character index.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn insert(&mut self, ch: char) {
        self.chars.insert(self.cursor, ch);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.chars.remove(self.cursor);
        }
    }

    /// Delete the word (and preceding whitespace) before the cursor.
    pub fn delete_word(&mut self) {
        let mut end = self.cursor;
        while end > 0 && self.chars[end - 1].is_whitespace() {
            end -= 1;
        }
        let mut start = end;
        while start > 0 && !self.chars[start - 1].is_whitespace() {
            start -= 1;
        }
        self.chars.drain(start..self.cursor);
        self.cursor = start;
    }

    pub fn clear(&mut self) {
        self.chars.clear();
        self.cursor = 0;
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        if self.cursor < self.chars.len() {
            self.cursor += 1;
        }
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.chars.len();
    }

    /// Take the current text and clear the composer.
    pub fn take(&mut self) -> String {
        let text = self.text();
        self.clear();
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(s: &str) -> Composer {
        let mut c = Composer::new();
        for ch in s.chars() {
            c.insert(ch);
        }
        c
    }

    #[test]
    fn insert_and_take() {
        let mut c = typed("hello");
        assert_eq!(c.text(), "hello");
        assert_eq!(c.cursor(), 5);
        assert_eq!(c.take(), "hello");
        assert!(c.is_empty());
    }

    #[test]
    fn backspace_at_cursor() {
        let mut c = typed("abc");
        c.left();
        c.backspace();
        assert_eq!(c.text(), "ac");
        assert_eq!(c.cursor(), 1);
    }

    #[test]
    fn delete_word_removes_trailing_word_and_space() {
        let mut c = typed("build the parser");
        c.delete_word();
        assert_eq!(c.text(), "build the ");
        c.delete_word();
        assert_eq!(c.text(), "build ");
    }

    #[test]
    fn cursor_movement_bounds() {
        let mut c = typed("ab");
        c.right();
        assert_eq!(c.cursor(), 2);
        c.home();
        assert_eq!(c.cursor(), 0);
        c.left();
        assert_eq!(c.cursor(), 0);
        c.end();
        assert_eq!(c.cursor(), 2);
    }

    #[test]
    fn insert_in_middle() {
        let mut c = typed("ac");
        c.left();
        c.insert('b');
        assert_eq!(c.text(), "abc");
    }
}
