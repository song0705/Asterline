//! Mouse selection over the rendered terminal buffer (drawers) and over
//! content-anchored chat coordinates (conversation column).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use unicode_width::UnicodeWidthChar;

use crate::tui::chat_view::ChatLayout;
use crate::tui::theme;

type Point = (u16, u16);

/// Content position in the flattened chat: display-cell column within a line.
pub type ContentPoint = (usize, usize);

// ---------------------------------------------------------------------------
// Screen-space selection (drawers / overlays)
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct MouseSelection {
    anchor: Option<Point>,
    head: Option<Point>,
    bounds: Option<Rect>,
}

impl MouseSelection {
    pub fn is_active(&self) -> bool {
        self.anchor.is_some() && self.head.is_some()
    }

    pub fn begin(&mut self, x: u16, y: u16) {
        self.anchor = Some((x, y));
        self.head = Some((x, y));
        self.bounds = None;
    }

    /// Begin a selection constrained to an overlay or panel. A press outside
    /// the supplied rectangle does not start a selection.
    pub fn begin_bounded(&mut self, x: u16, y: u16, bounds: Rect) {
        if !contains(bounds, x, y) {
            self.clear();
            return;
        }
        self.anchor = Some((x, y));
        self.head = Some((x, y));
        self.bounds = Some(bounds);
    }

    pub fn update(&mut self, x: u16, y: u16) {
        if self.anchor.is_some() {
            self.head = Some((x, y));
        }
    }

    pub fn finish(&mut self, x: u16, y: u16, buffer: &Buffer) -> Option<String> {
        self.update(x, y);
        let (bounds, start, end) = self.range(buffer)?;
        if start == end {
            self.clear();
            return None;
        }
        let text = selected_text(buffer, bounds, start, end);
        if text.is_empty() {
            self.clear();
            None
        } else {
            Some(text)
        }
    }

    pub fn clear(&mut self) {
        self.anchor = None;
        self.head = None;
        self.bounds = None;
    }

    pub fn render(&self, buffer: &mut Buffer) {
        let Some((bounds, start, end)) = self.range(buffer) else {
            return;
        };
        for_each_selected(buffer, bounds, start, end, |buffer, point| {
            if let Some(cell) = buffer.cell_mut(point) {
                cell.modifier.insert(Modifier::REVERSED);
            }
        });
    }

    fn range(&self, buffer: &Buffer) -> Option<(Rect, Point, Point)> {
        let bounds = intersect(self.bounds.unwrap_or(buffer.area), buffer.area)?;
        let anchor = clamp(self.anchor?, bounds)?;
        let head = clamp(self.head?, bounds)?;
        Some(if row_major(anchor) <= row_major(head) {
            (bounds, anchor, head)
        } else {
            (bounds, head, anchor)
        })
    }
}

// ---------------------------------------------------------------------------
// Content-anchored chat selection (survives scroll)
// ---------------------------------------------------------------------------

/// Selection over flattened chat lines. Anchors are content coordinates
/// `(line_index, display_column)`, so wheel scroll does not discard them.
///
/// Limitation (v1): when streaming output re-wraps the transcript, line
/// indices can drift relative to the original anchor; we do not re-map
/// endpoints after content growth.
#[derive(Default)]
pub struct ChatSelection {
    anchor: Option<ContentPoint>,
    head: Option<ContentPoint>,
    /// Wrap width captured when the selection began; a mismatch clears it.
    anchor_width: Option<usize>,
}

impl ChatSelection {
    pub fn is_active(&self) -> bool {
        self.anchor.is_some() && self.head.is_some()
    }

    pub fn begin(&mut self, point: ContentPoint, width: usize) {
        self.anchor = Some(point);
        self.head = Some(point);
        self.anchor_width = Some(width);
    }

    pub fn update(&mut self, point: ContentPoint) {
        if self.anchor.is_some() {
            self.head = Some(point);
        }
    }

    pub fn clear(&mut self) {
        self.anchor = None;
        self.head = None;
        self.anchor_width = None;
    }

    /// Drop the selection when the wrap width no longer matches the anchor.
    pub fn clear_if_width_changed(&mut self, width: usize) {
        if self.anchor_width.is_some_and(|w| w != width) {
            self.clear();
        }
    }

    /// Extract selected text. A zero-size selection (plain click) returns
    /// `None` and clears, matching drawer behavior.
    pub fn finish(&mut self, point: ContentPoint, layout: &ChatLayout) -> Option<String> {
        self.clear_if_width_changed(layout.width);
        if !self.is_active() {
            return None;
        }
        self.update(point);
        let (start, end) = self.ordered()?;
        if start == end {
            self.clear();
            return None;
        }
        let text = extract_content(&layout.lines, start, end);
        if text.is_empty() {
            self.clear();
            None
        } else {
            Some(text)
        }
    }

    /// Highlight visible cells that intersect the content selection.
    pub fn render(&self, buffer: &mut Buffer, layout: &ChatLayout) {
        if self.anchor_width.is_some_and(|w| w != layout.width) {
            return;
        }
        let Some((start, end)) = self.ordered() else {
            return;
        };
        if layout.area.is_empty() {
            return;
        }
        let height = layout.area.height as usize;
        for row in 0..height {
            let line_idx = layout.first_line + row;
            if line_idx < start.0 || line_idx > end.0 {
                continue;
            }
            let line = layout.lines.get(line_idx).map(String::as_str).unwrap_or("");
            let line_cells = theme::display_width(line);
            if line_cells == 0 {
                // Empty lines still occupy a row; reverse the first cell so
                // multi-line ranges remain visible across blank separators.
                if line_idx > start.0 && line_idx < end.0 {
                    paint_reversed(buffer, layout.area.x, layout.area.y + row as u16);
                }
                continue;
            }
            let from_col = if line_idx == start.0 { start.1 } else { 0 };
            let to_col = if line_idx == end.0 {
                end.1.min(line_cells.saturating_sub(1))
            } else {
                line_cells.saturating_sub(1)
            };
            if from_col > to_col {
                continue;
            }
            let y = layout.area.y + row as u16;
            let max_x = layout.area.x + layout.area.width.saturating_sub(1);
            for col in from_col..=to_col {
                let x = layout.area.x.saturating_add(col as u16);
                if x > max_x {
                    break;
                }
                paint_reversed(buffer, x, y);
            }
        }
    }

    fn ordered(&self) -> Option<(ContentPoint, ContentPoint)> {
        let a = self.anchor?;
        let h = self.head?;
        Some(if content_ord(a) <= content_ord(h) {
            (a, h)
        } else {
            (h, a)
        })
    }
}

fn paint_reversed(buffer: &mut Buffer, x: u16, y: u16) {
    if let Some(cell) = buffer.cell_mut((x, y)) {
        cell.modifier.insert(Modifier::REVERSED);
    }
}

fn content_ord((line, col): ContentPoint) -> (usize, usize) {
    (line, col)
}

/// Extract plain text for an ordered content range (inclusive endpoints).
pub fn extract_content(lines: &[String], start: ContentPoint, end: ContentPoint) -> String {
    if start.0 > end.0 || start.0 >= lines.len() {
        return String::new();
    }
    let end_line = end.0.min(lines.len().saturating_sub(1));
    let mut out = Vec::new();
    for (line_idx, line) in lines.iter().enumerate().take(end_line + 1).skip(start.0) {
        let piece = if start.0 == end_line {
            // Single line: column range inclusive.
            slice_display_cols(line, start.1, end.1)
        } else if line_idx == start.0 {
            slice_display_cols(line, start.1, usize::MAX)
        } else if line_idx == end_line {
            slice_display_cols(line, 0, end.1)
        } else {
            line.trim_end().to_string()
        };
        out.push(if line_idx == start.0 || line_idx == end_line {
            piece.trim_end().to_string()
        } else {
            piece
        });
    }
    out.join("\n")
}

/// Take characters whose display-cell range intersects `[from, to]` inclusive.
/// `to == usize::MAX` means "through end of line".
fn slice_display_cols(line: &str, from: usize, to: usize) -> String {
    if to < from {
        return String::new();
    }
    let mut out = String::new();
    let mut col = 0usize;
    for ch in line.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if w == 0 {
            // Zero-width / combining: attach to previous cluster if we already
            // started emitting, otherwise skip until we enter the range.
            if !out.is_empty() {
                out.push(ch);
            }
            continue;
        }
        let end = col + w - 1;
        if end < from {
            col += w;
            continue;
        }
        if col > to {
            break;
        }
        // Intersects [from, to].
        out.push(ch);
        col += w;
    }
    out
}

// ---------------------------------------------------------------------------
// Screen-space helpers (drawers)
// ---------------------------------------------------------------------------

fn clamp((x, y): Point, area: Rect) -> Option<Point> {
    if area.is_empty() {
        return None;
    }
    Some((
        x.clamp(area.x, area.x + area.width - 1),
        y.clamp(area.y, area.y + area.height - 1),
    ))
}

fn contains(area: Rect, x: u16, y: u16) -> bool {
    !area.is_empty()
        && x >= area.x
        && x < area.x.saturating_add(area.width)
        && y >= area.y
        && y < area.y.saturating_add(area.height)
}

fn intersect(a: Rect, b: Rect) -> Option<Rect> {
    let left = a.x.max(b.x);
    let top = a.y.max(b.y);
    let right = a.x.saturating_add(a.width).min(b.x.saturating_add(b.width));
    let bottom =
        a.y.saturating_add(a.height)
            .min(b.y.saturating_add(b.height));
    (right > left && bottom > top).then(|| Rect::new(left, top, right - left, bottom - top))
}

fn row_major((x, y): Point) -> (u16, u16) {
    (y, x)
}

fn for_each_selected(
    buffer: &mut Buffer,
    bounds: Rect,
    start: Point,
    end: Point,
    mut visit: impl FnMut(&mut Buffer, Point),
) {
    let left = bounds.x;
    let right = bounds.x + bounds.width - 1;
    for y in start.1..=end.1 {
        let from = if y == start.1 { start.0 } else { left };
        let to = if y == end.1 { end.0 } else { right };
        for x in from..=to {
            visit(buffer, (x, y));
        }
    }
}

fn selected_text(buffer: &Buffer, bounds: Rect, start: Point, end: Point) -> String {
    let left = bounds.x;
    let right = bounds.x + bounds.width - 1;
    let mut lines = Vec::new();
    for y in start.1..=end.1 {
        let from = if y == start.1 { start.0 } else { left };
        let to = if y == end.1 { end.0 } else { right };
        let mut line = String::new();
        for x in from..=to {
            if let Some(cell) = buffer.cell((x, y)) {
                line.push_str(cell.symbol());
            }
        }
        lines.push(line.trim_end().to_string());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- drawer / screen-space (unchanged) ---

    #[test]
    fn drag_extracts_multiline_text_and_marks_selection() {
        let buffer = Buffer::with_lines(["hello", "world"]);
        let mut selection = MouseSelection::default();
        selection.begin(1, 0);
        assert_eq!(
            selection.finish(2, 1, &buffer),
            Some("ello\nwor".to_string())
        );

        let mut rendered = buffer.clone();
        selection.render(&mut rendered);
        assert!(
            rendered
                .cell((1, 0))
                .unwrap()
                .modifier
                .contains(Modifier::REVERSED)
        );
        assert!(
            rendered
                .cell((2, 1))
                .unwrap()
                .modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn click_without_drag_does_not_copy() {
        let buffer = Buffer::with_lines(["hello"]);
        let mut selection = MouseSelection::default();
        selection.begin(2, 0);
        assert_eq!(selection.finish(2, 0, &buffer), None);
        assert!(!selection.is_active());
    }

    #[test]
    fn bounded_drag_never_selects_outside_the_panel() {
        let buffer = Buffer::with_lines(["0123456789", "abcdefghij", "ABCDEFGHIJ"]);
        let mut selection = MouseSelection::default();
        selection.begin_bounded(4, 0, Rect::new(2, 0, 5, 3));

        assert_eq!(
            selection.finish(5, 2, &buffer),
            Some("456\ncdefg\nCDEF".to_string())
        );

        let mut rendered = buffer.clone();
        selection.render(&mut rendered);
        assert!(
            !rendered
                .cell((1, 1))
                .unwrap()
                .modifier
                .contains(Modifier::REVERSED)
        );
        assert!(
            rendered
                .cell((2, 1))
                .unwrap()
                .modifier
                .contains(Modifier::REVERSED)
        );
        assert!(
            rendered
                .cell((6, 1))
                .unwrap()
                .modifier
                .contains(Modifier::REVERSED)
        );
        assert!(
            !rendered
                .cell((7, 1))
                .unwrap()
                .modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn bounded_selection_ignores_presses_outside_the_panel() {
        let mut selection = MouseSelection::default();
        selection.begin_bounded(1, 1, Rect::new(2, 2, 4, 4));
        assert!(!selection.is_active());
    }

    // --- content-anchored chat selection ---

    fn sample_layout(first_line: usize, width: usize) -> ChatLayout {
        ChatLayout {
            area: Rect::new(1, 2, 20, 4),
            first_line,
            width,
            lines: vec![
                "hello world".into(),
                "second line".into(),
                "third".into(),
                "fourth line here".into(),
                "fifth".into(),
            ],
        }
    }

    #[test]
    fn chat_extracts_multiline_with_column_ranges() {
        let layout = sample_layout(0, 20);
        let mut sel = ChatSelection::default();
        sel.begin((0, 6), layout.width); // 'w' of "hello world"
        assert_eq!(
            sel.finish((2, 2), &layout),
            Some("world\nsecond line\nthi".to_string())
        );
    }

    #[test]
    fn chat_selection_survives_simulated_scroll() {
        // Same content, different first_line (as after wheel scroll).
        let layout_a = sample_layout(0, 20);
        let layout_b = sample_layout(2, 20);
        let mut sel = ChatSelection::default();
        sel.begin((0, 0), 20);
        sel.update((3, 5));
        let start = (0usize, 0usize);
        let end = (3usize, 5usize);
        let text_a = extract_content(&layout_a.lines, start, end);
        let text_b = extract_content(&layout_b.lines, start, end);
        assert_eq!(text_a, text_b);
        assert_eq!(text_a, "hello world\nsecond line\nthird\nfourth");
        // Highlight re-maps: only lines visible under layout_b (2..6).
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 10));
        sel.render(&mut buf, &layout_b);
        // Row 0 of area (y=2) is layout line 2 ("third") — fully selected.
        assert!(
            buf.cell((1, 2))
                .unwrap()
                .modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn chat_wide_char_column_extraction() {
        let lines = vec!["a中b".into(), "xy".into()];
        // "a中b": cols 0=a, 1-2=中, 3=b
        assert_eq!(slice_display_cols("a中b", 1, 2), "中");
        assert_eq!(slice_display_cols("a中b", 0, 0), "a");
        assert_eq!(slice_display_cols("a中b", 3, 3), "b");
        assert_eq!(
            extract_content(&lines, (0, 1), (1, 1)),
            "中b\nxy".to_string()
        );
    }

    #[test]
    fn chat_width_change_clears_selection() {
        let layout = sample_layout(0, 20);
        let mut sel = ChatSelection::default();
        sel.begin((0, 0), 20);
        sel.update((1, 3));
        assert!(sel.is_active());
        let mut narrow = layout;
        narrow.width = 10;
        assert_eq!(sel.finish((1, 3), &narrow), None);
        assert!(!sel.is_active());
    }

    #[test]
    fn chat_click_without_drag_does_not_copy() {
        let layout = sample_layout(0, 20);
        let mut sel = ChatSelection::default();
        sel.begin((1, 2), 20);
        assert_eq!(sel.finish((1, 2), &layout), None);
        assert!(!sel.is_active());
    }
}
