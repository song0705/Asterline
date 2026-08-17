//! The bottom composer: a single logical input line with a movable cursor and
//! word/line editing. Cursor movement and deletion follow user-visible graphemes.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(crate) const MAX_COMPOSER_BYTES: usize = 256 * 1024;

#[derive(Debug, Default)]
pub struct Composer {
    chars: Vec<char>,
    cursor: usize,
    bytes: usize,
    /// Other end of a drag / Shift selection, if any.
    selection_anchor: Option<usize>,
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

    /// Cursor position as a Unicode scalar index kept on a grapheme boundary.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn selection_range(&self) -> Option<(usize, usize)> {
        let anchor = self.selection_anchor?;
        let (start, end) = if anchor <= self.cursor {
            (anchor, self.cursor)
        } else {
            (self.cursor, anchor)
        };
        (start < end).then_some((start, end))
    }

    pub fn selected_text(&self) -> String {
        let Some((start, end)) = self.selection_range() else {
            return String::new();
        };
        self.chars[start..end].iter().collect()
    }

    pub fn clear_selection(&mut self) {
        self.selection_anchor = None;
    }

    pub fn set_cursor_index(&mut self, index: usize) {
        self.cursor = self.grapheme_boundary_at_or_after(index.min(self.chars.len()));
    }

    pub fn begin_selection_at(&mut self, index: usize) {
        self.set_cursor_index(index);
        self.selection_anchor = Some(self.cursor);
    }

    pub fn extend_selection_to(&mut self, index: usize) {
        if self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.cursor);
        }
        self.set_cursor_index(index);
    }

    pub fn delete_selection(&mut self) -> bool {
        let Some((start, end)) = self.selection_range() else {
            return false;
        };
        self.delete_range(start, end);
        true
    }

    /// Delete a character range and leave the cursor at its start.
    pub fn delete_range(&mut self, start: usize, end: usize) {
        let start = start.min(self.chars.len());
        let end = end.min(self.chars.len()).max(start);
        self.bytes -= self.chars[start..end]
            .iter()
            .map(|ch| ch.len_utf8())
            .sum::<usize>();
        self.chars.drain(start..end);
        self.cursor = start;
        self.selection_anchor = None;
    }

    /// Replace an exact character range without truncating the replacement.
    pub fn replace_range(&mut self, start: usize, end: usize, text: &str) -> bool {
        let start = start.min(self.chars.len());
        let end = end.min(self.chars.len()).max(start);
        let old_cursor = self.cursor;
        let removed_bytes = self.chars[start..end]
            .iter()
            .map(|ch| ch.len_utf8())
            .sum::<usize>();
        if self
            .bytes
            .saturating_sub(removed_bytes)
            .saturating_add(text.len())
            > MAX_COMPOSER_BYTES
        {
            return false;
        }
        let inserted = text.chars().collect::<Vec<_>>();
        let inserted_chars = inserted.len();
        self.chars.splice(start..end, inserted);
        self.bytes = self.bytes - removed_bytes + text.len();
        self.cursor = if old_cursor <= start {
            old_cursor
        } else if old_cursor < end {
            start + inserted_chars
        } else {
            old_cursor - (end - start) + inserted_chars
        };
        self.selection_anchor = None;
        true
    }

    /// Insert all of `text` or leave the composer untouched.
    pub fn insert_text_exact(&mut self, text: &str) -> bool {
        if !self.can_insert_text_exact(text) {
            return false;
        }
        self.delete_selection();
        let inserted = text.chars().collect::<Vec<_>>();
        let count = inserted.len();
        self.chars.splice(self.cursor..self.cursor, inserted);
        self.bytes += text.len();
        self.cursor += count;
        true
    }

    pub fn can_insert_text_exact(&self, text: &str) -> bool {
        let selected_bytes = self
            .selection_range()
            .map(|(start, end)| {
                self.chars[start..end]
                    .iter()
                    .map(|ch| ch.len_utf8())
                    .sum::<usize>()
            })
            .unwrap_or(0);
        self.bytes
            .saturating_sub(selected_bytes)
            .saturating_add(text.len())
            <= MAX_COMPOSER_BYTES
    }

    /// Insert one scalar, returning whether it fit within the hard input cap.
    pub fn insert(&mut self, ch: char) -> bool {
        self.delete_selection();
        if self.bytes.saturating_add(ch.len_utf8()) > MAX_COMPOSER_BYTES {
            return false;
        }
        self.chars.insert(self.cursor, ch);
        self.bytes += ch.len_utf8();
        self.cursor += 1;
        self.cursor = self.grapheme_boundary_at_or_after(self.cursor);
        true
    }

    /// Insert a paste in one operation, leaving the cursor after the retained
    /// UTF-8 prefix. Returns false when the input had to be truncated.
    pub fn insert_text(&mut self, text: &str) -> bool {
        self.delete_selection();
        let remaining = MAX_COMPOSER_BYTES.saturating_sub(self.bytes);
        let end = utf8_prefix_len(text, remaining);
        let inserted = text[..end].chars().collect::<Vec<_>>();
        let count = inserted.len();
        self.chars.splice(self.cursor..self.cursor, inserted);
        self.bytes += end;
        self.cursor += count;
        self.cursor = self.grapheme_boundary_at_or_after(self.cursor);
        end == text.len()
    }

    /// Insert a hard line break at the cursor (multi-line composer).
    pub fn insert_newline(&mut self) -> bool {
        self.insert('\n')
    }

    pub fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.cursor > 0 {
            let start = self.previous_grapheme_boundary(self.cursor);
            self.bytes -= self.chars[start..self.cursor]
                .iter()
                .map(|ch| ch.len_utf8())
                .sum::<usize>();
            self.chars.drain(start..self.cursor);
            self.cursor = start;
        }
    }

    /// Delete the logical line that contains the cursor. Other lines stay.
    /// The line break that belonged to this line is removed so a middle line
    /// does not leave a blank gap.
    pub fn delete_line(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.chars.is_empty() {
            return;
        }
        let (start, end) = self.line_bounds();
        let (drain_start, drain_end, cursor) = if end < self.chars.len() {
            (start, end + 1, start)
        } else if start > 0 {
            (start - 1, end, start - 1)
        } else {
            (start, end, 0)
        };
        self.bytes -= self.chars[drain_start..drain_end]
            .iter()
            .map(|ch| ch.len_utf8())
            .sum::<usize>();
        self.chars.drain(drain_start..drain_end);
        self.cursor = cursor.min(self.chars.len());
        self.cursor = self.grapheme_boundary_at_or_after(self.cursor);
    }

    /// Delete the word (and preceding whitespace) before the cursor.
    pub fn delete_word(&mut self) {
        if self.delete_selection() {
            return;
        }
        let mut end = self.cursor;
        while end > 0 && self.chars[end - 1].is_whitespace() {
            end -= 1;
        }
        let mut start = end;
        while start > 0 && !self.chars[start - 1].is_whitespace() {
            start -= 1;
        }
        self.bytes -= self.chars[start..self.cursor]
            .iter()
            .map(|ch| ch.len_utf8())
            .sum::<usize>();
        self.chars.drain(start..self.cursor);
        self.cursor = start;
    }

    pub fn clear(&mut self) {
        self.chars.clear();
        self.cursor = 0;
        self.bytes = 0;
        self.selection_anchor = None;
    }

    pub fn left(&mut self) {
        self.selection_anchor = None;
        self.cursor = self.previous_grapheme_boundary(self.cursor);
    }

    pub fn right(&mut self) {
        self.selection_anchor = None;
        self.cursor = self.next_grapheme_boundary(self.cursor);
    }

    pub fn home(&mut self) {
        self.selection_anchor = None;
        self.cursor = self.line_bounds().0;
    }

    pub fn end(&mut self) {
        self.selection_anchor = None;
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

    /// Cursor position as a (row, column) pair in grapheme clusters.
    pub fn cursor_row_col(&self) -> (usize, usize) {
        let mut row = 0;
        let mut line_start = 0;
        for (index, &ch) in self.chars[..self.cursor].iter().enumerate() {
            if ch == '\n' {
                row += 1;
                line_start = index + 1;
            }
        }
        let line: String = self.chars[line_start..self.cursor].iter().collect();
        let col = line.graphemes(true).count();
        (row, col)
    }

    /// Move the cursor up one visual line, keeping the column. Returns false if
    /// already on the first line (so the caller can fall back to history recall).
    pub fn up(&mut self) -> bool {
        self.selection_anchor = None;
        let (row, col) = self.cursor_row_col();
        if row == 0 {
            return false;
        }
        let starts = self.line_starts();
        let prev_start = starts[row - 1];
        let prev_end = (starts[row] - 1).max(prev_start);
        self.cursor = self.grapheme_column_index(prev_start, prev_end, col);
        true
    }

    /// Move the cursor down one visual line, keeping the column. Returns false if
    /// already on the last line.
    pub fn down(&mut self) -> bool {
        self.selection_anchor = None;
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
        self.cursor = self.grapheme_column_index(next_start, next_end, col);
        true
    }

    /// The text before the cursor (used to compute completions).
    pub fn head(&self) -> String {
        self.chars[..self.cursor].iter().collect()
    }

    /// Replace the characters in `start..cursor` with `insert`, leaving the
    /// cursor at the end of the inserted text. Used to accept a completion.
    pub fn replace_token(&mut self, start: usize, insert: &str) -> bool {
        let end = self.cursor.min(self.chars.len());
        let start = start.min(end);
        let removed_bytes = self.chars[start..end]
            .iter()
            .map(|ch| ch.len_utf8())
            .sum::<usize>();
        self.chars.drain(start..end);
        self.bytes -= removed_bytes;
        let available = MAX_COMPOSER_BYTES.saturating_sub(self.bytes);
        let insert_end = utf8_prefix_len(insert, available);
        let inserted: Vec<char> = insert[..insert_end].chars().collect();
        let count = inserted.len();
        self.chars.splice(start..start, inserted);
        self.bytes += insert_end;
        self.cursor = self.grapheme_boundary_at_or_after(start + count);
        insert_end == insert.len()
    }

    /// Take the current text and clear the composer.
    pub fn take(&mut self) -> String {
        let text = self.text();
        self.clear();
        text
    }

    /// Replace the entire contents, leaving the cursor at the end. Used by
    /// prompt-history recall to load a previous submission into the composer.
    pub fn set_text(&mut self, text: &str) -> bool {
        self.selection_anchor = None;
        let end = utf8_prefix_len(text, MAX_COMPOSER_BYTES);
        self.chars = text[..end].chars().collect();
        self.bytes = end;
        self.cursor = self.chars.len();
        end == text.len()
    }

    /// Map a visual (row, column) in wrapped composer space to a char index.
    pub fn index_at_visual(&self, width: usize, row: usize, col: usize) -> usize {
        let width = width.max(1);
        let mut visual_row = 0usize;
        let mut visual_col = 0usize;
        let mut index = 0usize;
        let text = self.text();
        for grapheme in text.graphemes(true) {
            if visual_row > row {
                return index;
            }
            if grapheme == "\n" {
                if visual_row == row {
                    return index;
                }
                visual_row += 1;
                visual_col = 0;
                index += grapheme.chars().count();
                continue;
            }
            let w = UnicodeWidthStr::width(grapheme);
            if visual_col > 0 && visual_col + w > width {
                visual_row += 1;
                visual_col = 0;
            }
            if visual_row == row && visual_col >= col {
                return index;
            }
            if visual_row == row && visual_col + w > col {
                return index;
            }
            visual_col += w;
            index += grapheme.chars().count();
        }
        self.chars.len()
    }

    /// Wrapped lines with the starting char index of each visual line.
    pub fn visual_lines_with_starts(&self, width: usize) -> Vec<(String, usize)> {
        let width = width.max(1);
        let mut lines = vec![(String::new(), 0usize)];
        let mut col = 0usize;
        let mut index = 0usize;
        let text = self.text();
        for grapheme in text.graphemes(true) {
            if grapheme == "\n" {
                index += grapheme.chars().count();
                lines.push((String::new(), index));
                col = 0;
                continue;
            }
            let w = UnicodeWidthStr::width(grapheme);
            if col > 0 && col + w > width {
                lines.push((String::new(), index));
                col = 0;
            }
            lines.last_mut().unwrap().0.push_str(grapheme);
            col += w;
            index += grapheme.chars().count();
        }
        lines
    }

    fn grapheme_boundaries(&self) -> Vec<usize> {
        let text = self.text();
        let mut scalar = 0;
        let mut boundaries = vec![0];
        for grapheme in text.graphemes(true) {
            scalar += grapheme.chars().count();
            boundaries.push(scalar);
        }
        boundaries
    }

    fn previous_grapheme_boundary(&self, cursor: usize) -> usize {
        self.grapheme_boundaries()
            .into_iter()
            .take_while(|boundary| *boundary < cursor)
            .last()
            .unwrap_or(0)
    }

    fn next_grapheme_boundary(&self, cursor: usize) -> usize {
        self.grapheme_boundaries()
            .into_iter()
            .find(|boundary| *boundary > cursor)
            .unwrap_or(self.chars.len())
    }

    fn grapheme_boundary_at_or_after(&self, cursor: usize) -> usize {
        self.grapheme_boundaries()
            .into_iter()
            .find(|boundary| *boundary >= cursor)
            .unwrap_or(self.chars.len())
    }

    fn grapheme_column_index(&self, start: usize, end: usize, column: usize) -> usize {
        let line: String = self.chars[start..end].iter().collect();
        start
            + line
                .graphemes(true)
                .take(column)
                .map(|grapheme| grapheme.chars().count())
                .sum::<usize>()
    }

    /// Wrap the composer text to `width` display columns and return every
    /// visual line plus the cursor's visual (row, column) position. Each
    /// logical line (`\n`) starts a new visual line; long lines wrap at the
    /// exact column boundary (character-level, like a plain textarea) so the
    /// cursor maps directly to a screen cell.
    pub fn visual_lines_with_cursor(&self, width: usize) -> (Vec<String>, usize, usize) {
        let width = width.max(1);
        let mut lines: Vec<String> = vec![String::new()];
        let mut cur_col = 0usize;
        let mut cursor_row = 0usize;
        let mut cursor_col = 0usize;

        let text = self.text();
        let mut scalar_index = 0;
        for grapheme in text.graphemes(true) {
            if scalar_index == self.cursor {
                if cur_col >= width {
                    cursor_row = lines.len();
                    cursor_col = 0;
                } else {
                    cursor_row = lines.len() - 1;
                    cursor_col = cur_col;
                }
            }
            if grapheme == "\n" {
                lines.push(String::new());
                cur_col = 0;
            } else {
                let w = UnicodeWidthStr::width(grapheme);
                if cur_col > 0 && cur_col + w > width {
                    lines.push(String::new());
                    cur_col = 0;
                    if scalar_index == self.cursor {
                        cursor_row = lines.len() - 1;
                        cursor_col = 0;
                    }
                }
                lines.last_mut().unwrap().push_str(grapheme);
                cur_col += w;
            }
            scalar_index += grapheme.chars().count();
        }
        // Cursor at the very end of the text.
        if self.cursor == self.chars.len() {
            if cur_col >= width {
                lines.push(String::new());
                cursor_row = lines.len() - 1;
                cursor_col = 0;
            } else {
                cursor_row = lines.len() - 1;
                cursor_col = cur_col;
            }
        }
        (lines, cursor_row, cursor_col)
    }

    /// Number of visual lines after wrapping to `width` (≥ 1).
    pub fn visual_line_count(&self, width: usize) -> usize {
        self.visual_lines_with_cursor(width).0.len()
    }
}

fn utf8_prefix_len(text: &str, max_bytes: usize) -> usize {
    let mut end = text.len().min(max_bytes);
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    end
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
    fn drag_selection_copies_range_and_insert_replaces_it() {
        let mut c = typed("hello world");
        c.begin_selection_at(0);
        c.extend_selection_to(5);
        assert_eq!(c.selected_text(), "hello");
        assert!(c.insert_text("hey"));
        assert_eq!(c.text(), "hey world");
        assert!(c.selected_text().is_empty());
    }

    #[test]
    fn index_at_visual_hits_wrapped_column() {
        let c = typed("abcdefghij");
        assert_eq!(c.index_at_visual(4, 0, 2), 2);
        assert_eq!(c.index_at_visual(4, 1, 1), 5);
        assert_eq!(c.index_at_visual(4, 9, 0), 10);
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
    fn inserts_pasted_text_atomically_at_cursor() {
        let mut composer = Composer::new();
        composer.insert_text("ab\ncd");
        composer.left();
        composer.insert_text("XY");
        assert_eq!(composer.text(), "ab\ncXYd");
        assert_eq!(composer.cursor(), 6);
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
    fn all_write_paths_obey_utf8_byte_limit() {
        let oversized = "你".repeat(MAX_COMPOSER_BYTES);
        let mut composer = Composer::new();
        assert!(!composer.insert_text(&oversized));
        let text = composer.text();
        assert!(text.len() <= MAX_COMPOSER_BYTES);
        assert!(text.is_char_boundary(text.len()));
        assert!(!composer.insert('界'));

        assert!(!composer.set_text(&oversized));
        assert!(composer.text().len() <= MAX_COMPOSER_BYTES);
        composer.home();
        assert!(!composer.replace_token(0, &oversized));
        assert!(composer.text().len() <= MAX_COMPOSER_BYTES);
    }

    #[test]
    fn deleting_text_releases_byte_budget() {
        let mut composer = Composer::new();
        composer.set_text(&"x".repeat(MAX_COMPOSER_BYTES));
        composer.backspace();
        composer.backspace();
        composer.backspace();
        assert!(composer.insert('你'));
        assert_eq!(composer.text().len(), MAX_COMPOSER_BYTES);
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
    fn delete_line_removes_only_the_current_logical_line() {
        let mut middle = typed("keep\nremove\nkeep");
        middle.home();
        middle.left(); // end of "remove"
        middle.delete_line();
        assert_eq!(middle.text(), "keep\nkeep");
        assert_eq!(middle.cursor(), 5); // start of the following "keep"

        let mut first = typed("gone\nstay");
        first.home();
        assert!(first.up());
        first.delete_line();
        assert_eq!(first.text(), "stay");
        assert_eq!(first.cursor(), 0);

        let mut last = typed("stay\ngone");
        last.delete_line();
        assert_eq!(last.text(), "stay");
        assert_eq!(last.cursor(), 4);

        let mut only = typed("all of this");
        only.home();
        only.right();
        only.delete_line();
        assert!(only.is_empty());
        assert_eq!(only.cursor(), 0);

        let mut blank = typed("stay\n\nstay");
        blank.up();
        blank.delete_line();
        assert_eq!(blank.text(), "stay\nstay");
    }

    #[test]
    fn insert_newline_grows_lines() {
        let mut c = typed("a");
        c.insert_newline();
        c.insert('b');
        assert_eq!(c.text(), "a\nb");
        assert_eq!(c.line_count(), 2);
    }

    #[test]
    fn visual_lines_wrap_long_line() {
        let c = typed("abcdefghij"); // 10 chars
        let (lines, row, col) = c.visual_lines_with_cursor(4);
        // 10 chars wrapped at width 4 → 3 lines: "abcd" "efgh" "ij"
        assert_eq!(lines, vec!["abcd", "efgh", "ij"]);
        // Cursor at end → last line, col 2.
        assert_eq!(row, 2);
        assert_eq!(col, 2);
    }

    #[test]
    fn visual_lines_respect_newlines_and_cursor() {
        let mut c = typed("hello world");
        c.left(); // cursor after "hello worl"
        c.left();
        let (lines, row, col) = c.visual_lines_with_cursor(5);
        // "hello world" wrapped at 5 → "hello" " worl" "d"
        assert_eq!(lines, vec!["hello", " worl", "d"]);
        // Cursor is after "hello wor" (9 chars) → line 1 (" worl"), col 4.
        assert_eq!(row, 1);
        assert_eq!(col, 4);
    }

    #[test]
    fn visual_line_count_empty_is_one() {
        let c = Composer::new();
        assert_eq!(c.visual_line_count(10), 1);
    }

    #[test]
    fn movement_and_backspace_keep_graphemes_intact() {
        let mut combining = typed("e\u{301}");
        assert_eq!(combining.cursor(), 2);
        combining.left();
        assert_eq!(combining.cursor(), 0);
        combining.right();
        assert_eq!(combining.cursor(), 2);
        combining.backspace();
        assert!(combining.is_empty());

        let mut family = typed("👨‍👩‍👧‍👦");
        family.left();
        assert_eq!(family.cursor(), 0);
        family.right();
        family.backspace();
        assert!(family.is_empty());
    }

    #[test]
    fn cursor_wraps_to_a_new_visual_line_at_exact_width() {
        let c = typed("abcd");
        let (lines, row, col) = c.visual_lines_with_cursor(4);

        assert_eq!(lines, vec!["abcd", ""]);
        assert_eq!((row, col), (1, 0));
    }
}
