//! Mouse selection over the rendered terminal buffer.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

type Point = (u16, u16);

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
}
