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

    /// Insert a hard line break at the cursor (multi-line composer).
    pub fn insert_newline(&mut self) {
        self.insert('\n');
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
        self.cursor = self.line_bounds().0;
    }

    pub fn end(&mut self) {
        self.cursor = self.line_bounds().1;
    }

    /// Char index range `[start, end)` of the line containing the cursor.
    fn line_bounds(&self) -> (usize, usize) {
        let start = self.chars[..self.cursor]
            .iter()
            .rposition(|&c| c == '\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let end = self.chars[self.cursor..]
            .iter()
            .position(|&c| c == '\n')
            .map(|i| self.cursor + i)
            .unwrap_or(self.chars.len());
        (start, end)
    }

    /// Char index of the start of each visual line.
    fn line_starts(&self) -> Vec<usize> {
        let mut starts = vec![0];
        for (i, &c) in self.chars.iter().enumerate() {
            if c == '\n' {
                starts.push(i + 1);
            }
        }
        starts
    }

    /// Number of visual lines (≥ 1).
    pub fn line_count(&self) -> usize {
        self.chars.iter().filter(|&&c| c == '\n').count() + 1
    }

    /// Cursor position as a (row, column) pair in characters.
    pub fn cursor_row_col(&self) -> (usize, usize) {
        let mut row = 0;
        let mut col = 0;
        for &c in &self.chars[..self.cursor] {
            if c == '\n' {
                row += 1;
                col = 0;
            } else {
                col += 1;
            }
        }
        (row, col)
    }

    /// Move the cursor up one visual line, keeping the column. Returns false if
    /// already on the first line (so the caller can fall back to history recall).
    pub fn up(&mut self) -> bool {
        let (row, col) = self.cursor_row_col();
        if row == 0 {
            return false;
        }
        let starts = self.line_starts();
        let prev_start = starts[row - 1];
        let prev_len = (starts[row] - 1).saturating_sub(prev_start);
        self.cursor = prev_start + col.min(prev_len);
        true
    }

    /// Move the cursor down one visual line, keeping the column. Returns false if
    /// already on the last line.
    pub fn down(&mut self) -> bool {
        let (row, col) = self.cursor_row_col();
        let starts = self.line_starts();
        if row + 1 >= starts.len() {
            return false;
        }
        let next_start = starts[row + 1];
        let next_end = if row + 2 < starts.len() {
            starts[row + 2] - 1
        } else {
            self.chars.len()
        };
        self.cursor = next_start + col.min(next_end - next_start);
        true
    }

    /// The text before the cursor (used to compute completions).
    pub fn head(&self) -> String {
        self.chars[..self.cursor].iter().collect()
    }

    /// Replace the characters in `start..cursor` with `insert`, leaving the
    /// cursor at the end of the inserted text. Used to accept a completion.
    pub fn replace_token(&mut self, start: usize, insert: &str) {
        let end = self.cursor.min(self.chars.len());
        let start = start.min(end);
        self.chars.drain(start..end);
        let inserted: Vec<char> = insert.chars().collect();
        let count = inserted.len();
        for (offset, ch) in inserted.into_iter().enumerate() {
            self.chars.insert(start + offset, ch);
        }
        self.cursor = start + count;
    }

    /// Take the current text and clear the composer.
    pub fn take(&mut self) -> String {
        let text = self.text();
        self.clear();
        text
    }

    /// Replace the entire contents, leaving the cursor at the end. Used by
    /// prompt-history recall to load a previous submission into the composer.
    pub fn set_text(&mut self, text: &str) {
        self.chars = text.chars().collect();
        self.cursor = self.chars.len();
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

    #[test]
    fn set_text_replaces_and_moves_cursor_to_end() {
        let mut c = typed("old");
        c.home();
        c.set_text("recalled");
        assert_eq!(c.text(), "recalled");
        assert_eq!(c.cursor(), 8);
    }

    #[test]
    fn multiline_navigation_and_line_aware_home_end() {
        let mut c = typed("ab\ncde");
        assert_eq!(c.line_count(), 2);
        assert_eq!(c.cursor_row_col(), (1, 3)); // end of "cde"

        // Up keeps the column (clamped to the shorter first line).
        assert!(c.up());
        assert_eq!(c.cursor_row_col(), (0, 2)); // "ab" has length 2
        // Already on the first line: up returns false (history fallback).
        assert!(!c.up());

        // Home/End act on the current line.
        c.home();
        assert_eq!(c.cursor(), 0);
        c.end();
        assert_eq!(c.cursor(), 2); // before the newline

        assert!(c.down());
        assert_eq!(c.cursor_row_col().0, 1);
        assert!(!c.down());
    }

    #[test]
    fn insert_newline_grows_lines() {
        let mut c = typed("a");
        c.insert_newline();
        c.insert('b');
        assert_eq!(c.text(), "a\nb");
        assert_eq!(c.line_count(), 2);
    }
}
