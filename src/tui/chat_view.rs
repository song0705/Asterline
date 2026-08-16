//! Renders the chat-first UI: the header block, the single scrolling
//! conversation column, the bottom composer, a footer hint line, and an
//! optional drawer overlay. Chat-block rendering lives here; the header,
//! drawers, and run presentation live in sibling modules.

use std::cell::RefCell;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph};

use crate::domain::event::{ChatItem, FileChangeItem};
use crate::domain::team::{DefaultTarget, MemberId};
use crate::tui::app_state::{AppState, member_status_is_active};
use crate::tui::completion::Completion;
use crate::tui::drawer_view::render_drawer;
use crate::tui::file_diff;
use crate::tui::header::{render_footer, render_header};
use crate::tui::markdown;
use crate::tui::status_indicator;
use crate::tui::theme;
use crate::tui::theme::{clip_width, truncate_width};
use crate::tui::tool_display;

/// Snapshot of the flattened chat layout for the last frame. Used by
/// content-anchored mouse selection so anchors survive scrolling.
#[derive(Clone, Debug)]
pub struct ChatLayout {
    /// The chat `inner` rect actually rendered into.
    pub area: Rect,
    /// Flattened index of the first visible line.
    pub first_line: usize,
    /// Total flattened lines, including those above/below the viewport.
    pub total_lines: usize,
    /// Wrap width used to build the lines.
    pub width: usize,
    /// Plain text of the visible viewport (unstyled). Index 0 is `first_line`.
    pub lines: Vec<String>,
    /// Completion popup bounds when it is visible. This uses screen-space
    /// selection because popup rows do not belong to chat history.
    pub completion_area: Option<Rect>,
    /// Composer inner rect for drag-select / copy.
    pub composer_area: Option<Rect>,
    /// Text wrap width inside the composer (excludes the `> ` gutter).
    pub composer_wrap: usize,
    /// Visual rows reserved above composer text (attachment chips).
    pub composer_text_origin: u16,
}

impl ChatLayout {
    /// Maximum scroll offset (lines up from the bottom) for this layout.
    pub fn max_scroll(&self) -> usize {
        let height = self.area.height as usize;
        self.total_lines.saturating_sub(height)
    }

    pub fn composer_contains(&self, x: u16, y: u16) -> bool {
        self.composer_area.is_some_and(|area| {
            x >= area.x
                && x < area.x.saturating_add(area.width)
                && y >= area.y
                && y < area.y.saturating_add(area.height)
        })
    }

    pub fn screen_to_composer_index(
        &self,
        composer: &crate::tui::composer::Composer,
        x: u16,
        y: u16,
    ) -> Option<usize> {
        let area = self.composer_area?;
        if !self.composer_contains(x, y) {
            return None;
        }
        let wrap = self.composer_wrap.max(1);
        let row = y
            .saturating_sub(area.y)
            .saturating_sub(self.composer_text_origin) as usize;
        let col = x.saturating_sub(area.x).saturating_sub(2) as usize;
        Some(composer.index_at_visual(wrap, row, col))
    }

    /// Map a screen cell to a content `(line_index, display_column)`, clamping
    /// into the chat area. Returns `None` when the layout is empty.
    pub fn screen_to_content(&self, x: u16, y: u16) -> Option<(usize, usize)> {
        if self.area.is_empty() || self.lines.is_empty() {
            return None;
        }
        let area = self.area;
        let row = (y.clamp(area.y, area.y + area.height.saturating_sub(1)) - area.y) as usize;
        let col = (x.clamp(area.x, area.x + area.width.saturating_sub(1)) - area.x) as usize;
        let line_idx = self
            .first_line
            .saturating_add(row)
            .min(self.total_lines.saturating_sub(1));
        let local = line_idx.saturating_sub(self.first_line);
        let line = self.lines.get(local).map(String::as_str).unwrap_or("");
        let width = theme::display_width(line);
        let col = if width == 0 { 0 } else { col.min(width - 1) };
        Some((line_idx, col))
    }

    /// True when `(x, y)` lies inside the chat content rect.
    pub fn contains(&self, x: u16, y: u16) -> bool {
        !self.area.is_empty()
            && x >= self.area.x
            && x < self.area.x.saturating_add(self.area.width)
            && y >= self.area.y
            && y < self.area.y.saturating_add(self.area.height)
    }

    /// Plain text covered by a drag selection, joined with newlines.
    pub fn selected_text(&self, selection: crate::tui::app_state::ChatSelection) -> String {
        if selection.is_empty() {
            return String::new();
        }
        let (from, to) = selection.normalized();
        let last = self.total_lines.saturating_sub(1);
        let start_line = from.0.max(self.first_line).min(last);
        let end_line = to.0.max(self.first_line).min(last);
        let mut out = String::new();
        for (offset, line) in self.lines.iter().enumerate() {
            let index = self.first_line.saturating_add(offset);
            if index < start_line || index > end_line {
                continue;
            }
            if !out.is_empty() {
                out.push('\n');
            }
            let start_col = if index == start_line { from.1 } else { 0 };
            let end_col = if index == end_line {
                to.1.saturating_add(1)
            } else {
                theme::display_width(line)
            };
            out.push_str(&theme::slice_display_cols(line, start_col, end_col));
        }
        out
    }
}

// Paint the rail as a terminal-cell background instead of a font glyph. This
// fills the complete cell rectangle regardless of font ascent, descent, or
// line-height metrics, so adjacent rows meet without visible seams.
fn chat_rail(color: Color) -> Span<'static> {
    Span::styled(" ", Style::default().bg(color))
}

fn member_rail_color(state: &AppState, member: &MemberId) -> Color {
    if state
        .members()
        .iter()
        .any(|candidate| &candidate.id == member)
    {
        return state.member_color(member);
    }
    state
        .chat()
        .iter()
        .rev()
        .find_map(|item| match item {
            ChatItem::Agent {
                member: candidate,
                backend,
                ..
            }
            | ChatItem::Thinking {
                member: candidate,
                backend,
                ..
            } if candidate == member => Some(theme::backend_color(*backend)),
            _ => None,
        })
        .unwrap_or_else(theme::muted_color)
}

/// Paint the full chat UI. Returns a layout snapshot of the conversation
/// column for content-anchored selection (ignored when a drawer is open).
pub fn render(frame: &mut Frame<'_>, state: &AppState) -> Option<ChatLayout> {
    // The composer grows with its content up to a cap, like a real textarea.
    const MAX_COMPOSER_ROWS: u16 = 8;
    let composer_avail = frame.area().width.saturating_sub(2) as usize;
    let composer_rows =
        (state.composer().visual_line_count(composer_avail) as u16).clamp(1, MAX_COMPOSER_ROWS);
    let image_rows = u16::from(!state.pending_images().is_empty());
    let composer_height = composer_rows + image_rows + 2; // borders + optional chips
    let completion = if state.drawer().is_none() {
        state.completion()
    } else {
        None
    };
    let bottom_height = completion
        .as_ref()
        .map(completion_popup_height)
        .unwrap_or(1);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(composer_height),
            Constraint::Length(bottom_height),
        ])
        .split(frame.area());

    render_header(frame, chunks[0], state);
    let mut layout = render_chat(frame, chunks[1], state);
    render_composer(frame, chunks[2], state);
    let composer_inner = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .inner(chunks[2]);
    layout.composer_area = Some(composer_inner);
    layout.composer_wrap = (composer_inner.width as usize).saturating_sub(2);
    layout.composer_text_origin = u16::from(!state.pending_images().is_empty());
    if let Some(completion) = completion {
        render_popup(frame, chunks[3], &completion, state.popup_selected());
        layout.completion_area = Some(chunks[3]);
    } else {
        render_footer(frame, chunks[3], state);
    }

    if let Some(drawer) = state.drawer() {
        render_drawer(frame, frame.area(), state, &drawer);
        // Drawer overlays the chat; content selection uses the drawer path.
        return None;
    }
    Some(layout)
}

const MAX_COMPLETION_ROWS: usize = 6;

fn completion_popup_height(completion: &Completion) -> u16 {
    completion.items.len().min(MAX_COMPLETION_ROWS) as u16
}

fn render_popup(frame: &mut Frame<'_>, area: Rect, completion: &Completion, selected: usize) {
    let count = completion.items.len();
    let shown = count.min(MAX_COMPLETION_ROWS);
    let selected = selected.min(count.saturating_sub(1));
    let start = if selected >= shown {
        selected + 1 - shown
    } else {
        0
    };
    let name_width = completion
        .items
        .iter()
        .filter_map(|item| {
            let (name, description) = completion_parts(&item.label);
            description.map(|_| theme::display_width(name))
        })
        .max()
        .unwrap_or(0)
        .min(18);
    let lines: Vec<Line> = completion
        .items
        .iter()
        .enumerate()
        .skip(start)
        .take(shown)
        .map(|(i, item)| {
            let (name, description) = completion_parts(&item.label);
            let is_selected = i == selected;
            let name_style = if is_selected {
                theme::accent()
            } else {
                theme::emphasis()
            };
            let marker_style = if is_selected {
                theme::accent()
            } else {
                Style::default()
            };
            let marker = if is_selected { "› " } else { "  " };
            let mut spans = vec![
                Span::styled(marker, marker_style),
                Span::styled(name.to_string(), name_style),
            ];
            if let Some(description) = description {
                let padding = name_width.saturating_sub(theme::display_width(name)) + 2;
                spans.push(Span::raw(" ".repeat(padding)));
                spans.push(Span::styled(
                    description.to_string(),
                    if is_selected {
                        theme::accent()
                    } else {
                        theme::muted()
                    },
                ));
            }
            Line::from(spans)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

fn completion_parts(label: &str) -> (&str, Option<&str>) {
    match label.split_once(" — ") {
        Some((name, description)) => (name, Some(description)),
        None => (label, None),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PaintKey {
    width: usize,
    roster_revision: u64,
    thinking_expanded: bool,
    diffs_expanded: bool,
    tools_expanded: bool,
    find_current: Option<usize>,
}

#[derive(Default)]
struct ChatPaintCache {
    key: Option<PaintKey>,
    item_revs: Vec<u64>,
    /// Finished prefix kept as one entry so streaming only re-paints the tail.
    prefix: Option<CachedChunk>,
    tail: Option<CachedChunk>,
    live_slots: Vec<LiveSlot>,
}

#[derive(Clone, Default)]
struct CachedChunk {
    item_lo: usize,
    item_hi: usize,
    height: usize,
    lines: Option<Vec<Line<'static>>>,
    plain: Option<Vec<String>>,
}

#[derive(Clone, Debug)]
struct LiveSlot {
    at: usize,
    member: MemberId,
    show_header: bool,
}

thread_local! {
    static CHAT_PAINT: RefCell<ChatPaintCache> = const { RefCell::new(ChatPaintCache {
        key: None,
        item_revs: Vec::new(),
        prefix: None,
        tail: None,
        live_slots: Vec::new(),
    }) };
}

enum LiveRender<'a> {
    #[cfg(test)]
    Inline,
    Record(&'a mut Vec<LiveSlot>),
}

fn paint_key(state: &AppState, width: usize) -> PaintKey {
    PaintKey {
        width,
        roster_revision: state.roster_revision(),
        thinking_expanded: state.thinking_expanded(),
        diffs_expanded: state.diffs_expanded(),
        tools_expanded: state.tools_expanded(),
        find_current: state.find_current_chat_index(),
    }
}

fn line_to_plain(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn render_chat(frame: &mut Frame<'_>, area: Rect, state: &AppState) -> ChatLayout {
    let block = Block::default().padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let width = inner.width as usize;
    let height = inner.height as usize;
    let (mut visible, plain, total) = painted_chat(state, width, state.scroll(), height);

    let max_start = total.saturating_sub(height);
    let start = max_start.saturating_sub(state.scroll());
    if let Some(selection) = state.chat_selection() {
        apply_selection_style(&mut visible, start, selection);
    }
    frame.render_widget(Paragraph::new(visible), inner);

    ChatLayout {
        area: inner,
        first_line: start,
        total_lines: total,
        width,
        lines: plain,
        completion_area: None,
        composer_area: None,
        composer_wrap: 0,
        composer_text_origin: 0,
    }
}

/// Grok Build keeps a pinned header and virtualizes the middle scrollback:
/// each finished chunk is measured once, off-screen render caches are
/// dropped, and a live tail is the only thing rebuilt while streaming.
fn painted_chat(
    state: &AppState,
    width: usize,
    scroll: usize,
    height: usize,
) -> (Vec<Line<'static>>, Vec<String>, usize) {
    CHAT_PAINT.with(|cell| {
        let mut cache = cell.borrow_mut();
        ensure_scrollback_cached(state, width, &mut cache);
        compose_scrollback(&mut cache, state, width, scroll, height)
    })
}

fn ensure_scrollback_cached(state: &AppState, width: usize, cache: &mut ChatPaintCache) {
    let key = paint_key(state, width);
    let spinner_frame = status_indicator::spinner();
    let revs: Vec<u64> = state
        .chat()
        .iter()
        .map(|item| paint_item_revision(state, item, spinner_frame))
        .collect();
    let flags_changed = cache.key != Some(key);
    let dirty = if flags_changed {
        0
    } else {
        rewind_work_run(state.chat(), first_rev_mismatch(&cache.item_revs, &revs))
    };
    if !flags_changed
        && cache.item_revs.len() == revs.len()
        && dirty == revs.len()
        && cache.tail.is_some()
    {
        cache.item_revs = revs;
        return;
    }

    if dirty == 0 {
        cache.prefix = None;
        cache.tail = None;
        cache.live_slots.clear();
    } else if cache
        .prefix
        .as_ref()
        .is_some_and(|prefix| prefix.item_hi > dirty)
    {
        cache.prefix = None;
        cache.live_slots.clear();
    }
    if cache.prefix.is_none() && dirty > 0 {
        cache.live_slots.clear();
        cache.prefix = Some(render_chunk(state, width, 0, dirty, &mut cache.live_slots));
    }

    let prefix_height = cache.prefix.as_ref().map_or(0, |prefix| prefix.height);
    cache.live_slots.retain(|slot| slot.at <= prefix_height);
    let tail_start = cache.prefix.as_ref().map_or(0, |prefix| prefix.item_hi);
    let slot_from = cache.live_slots.len();
    cache.tail = Some(render_chunk(
        state,
        width,
        tail_start,
        state.chat().len(),
        &mut cache.live_slots,
    ));
    for slot in cache.live_slots.iter_mut().skip(slot_from) {
        slot.at = slot.at.saturating_add(prefix_height);
    }
    cache.item_revs = revs;
    cache.key = Some(key);
}

/// Live thinking and in-flight tools both own a spinner. Include its frame in
/// their cache revision so the virtualized tail is repainted while they run.
fn paint_item_revision(state: &AppState, item: &ChatItem, spinner_frame: &str) -> u64 {
    let revision = item_rev(item);
    match item {
        ChatItem::Thinking { member, .. } if state.is_thinking_live(member) => {
            revision ^ fnv1a64_bytes(spinner_frame.as_bytes())
        }
        ChatItem::Tool { ok: None, .. } => revision ^ fnv1a64_bytes(spinner_frame.as_bytes()),
        _ => revision,
    }
}

fn first_rev_mismatch(old: &[u64], new: &[u64]) -> usize {
    let shared = old.len().min(new.len());
    for index in 0..shared {
        if old[index] != new[index] {
            return index;
        }
    }
    shared.min(new.len())
}

fn rewind_work_run(items: &[ChatItem], dirty: usize) -> usize {
    if let Some(ChatItem::User { targets, .. }) = items.get(dirty) {
        // A newly targeted prompt starts a new live region. Repaint that
        // member's prior region as well, otherwise a cached trailing Working
        // slot remains attached to the completed answer above it.
        return targets
            .iter()
            .filter_map(|target| {
                items[..dirty]
                    .iter()
                    .rposition(|item| item_member(item).is_some_and(|member| member == target))
            })
            .min()
            .unwrap_or(dirty);
    }
    let Some(ChatItem::Tool { member, .. } | ChatItem::Diff { member, .. }) = items.get(dirty)
    else {
        return dirty;
    };
    let mut start = dirty;
    while start > 0 {
        match &items[start - 1] {
            ChatItem::Tool {
                member: prev_member,
                ..
            }
            | ChatItem::Diff {
                member: prev_member,
                ..
            } if prev_member == member => start -= 1,
            _ => break,
        }
    }
    start
}

fn render_chunk(
    state: &AppState,
    width: usize,
    start: usize,
    end: usize,
    live_slots: &mut Vec<LiveSlot>,
) -> CachedChunk {
    let mut lines = Vec::new();
    if start == 0 && state.chat().is_empty() {
        lines.push(Line::raw(""));
        lines.extend(quick_start_lines(state));
        lines.push(Line::raw(""));
    }
    render_chat_history_range(
        state,
        width,
        start,
        end,
        &mut lines,
        LiveRender::Record(live_slots),
    );
    if end == state.chat().len() && state.omitted_active_output_count() > 0 {
        let omitted = state.omitted_active_output_count();
        let text = format!(
            "… {omitted} active output cell(s) omitted by the TUI memory limit; final results will appear on completion"
        );
        for wrapped in markdown::wrap(&text, width.max(1)) {
            lines.push(Line::from(Span::styled(wrapped, theme::warning_bold())));
        }
        lines.push(Line::raw(""));
    }
    CachedChunk {
        item_lo: start,
        item_hi: end,
        height: lines.len(),
        plain: Some(lines.iter().map(line_to_plain).collect()),
        lines: Some(lines),
    }
}

fn item_rev(item: &ChatItem) -> u64 {
    let (tag, len, bytes) = match item {
        ChatItem::User { body, .. } => (1u64, body.len(), body.as_bytes()),
        ChatItem::Agent { text, .. } => (2, text.len(), text.as_bytes()),
        ChatItem::Thinking {
            text, elapsed_secs, ..
        } => (3 ^ elapsed_secs.unwrap_or(0), text.len(), text.as_bytes()),
        ChatItem::Tool { detail, name, .. } => (4, detail.len() + name.len(), detail.as_bytes()),
        ChatItem::Diff { files, .. } => {
            let len = files.iter().map(|file| file.path.len()).sum();
            (
                5,
                len,
                files
                    .first()
                    .map(|file| file.path.as_bytes())
                    .unwrap_or(&[]),
            )
        }
        ChatItem::Route { body, .. } => (6, body.len(), body.as_bytes()),
        ChatItem::Notice { text } => (7, text.len(), text.as_bytes()),
        ChatItem::Error { message, .. } => (8, message.len(), message.as_bytes()),
        ChatItem::Verdict { summary, .. } => (9, summary.len(), summary.as_bytes()),
    };
    tag.wrapping_shl(56) ^ (len as u64).wrapping_shl(32) ^ fnv1a64_bytes(bytes)
}

fn fnv1a64_bytes(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn compose_scrollback(
    cache: &mut ChatPaintCache,
    state: &AppState,
    width: usize,
    scroll: usize,
    height: usize,
) -> (Vec<Line<'static>>, Vec<String>, usize) {
    let prefix_h = cache.prefix.as_ref().map_or(0, |chunk| chunk.height);
    let tail_h = cache.tail.as_ref().map_or(0, |chunk| chunk.height);
    let live = realized_live_slots(cache, state, width);
    let extra: usize = live.iter().map(|(_, lines)| lines.len()).sum();
    let total = prefix_h + tail_h + extra;
    let max_start = total.saturating_sub(height);
    let start = max_start.saturating_sub(scroll);
    let end = start.saturating_add(height);

    evict_offscreen_chunk(cache.prefix.as_mut(), 0, start, end);
    evict_offscreen_chunk(cache.tail.as_mut(), prefix_h, start, end);
    ensure_chunk_lines(state, width, cache.prefix.as_mut());
    ensure_chunk_lines(state, width, cache.tail.as_mut());

    let mut visible = Vec::with_capacity(height.min(total));
    let mut plain = Vec::with_capacity(height.min(total));
    let mut index = 0usize;
    let mut live_at = 0usize;
    let mut emit = |line: &Line<'static>| {
        if index >= start && visible.len() < height {
            plain.push(line_to_plain(line));
            visible.push(line.clone());
        }
        index += 1;
    };
    let flush_live_at = |at: usize, live_at: &mut usize, emit: &mut dyn FnMut(&Line<'static>)| {
        while *live_at < live.len() && live[*live_at].0 == at {
            for live_line in &live[*live_at].1 {
                emit(live_line);
            }
            *live_at += 1;
        }
    };
    let emit_chunk = |chunk: Option<&CachedChunk>,
                      origin: usize,
                      live_at: &mut usize,
                      emit: &mut dyn FnMut(&Line<'static>)| {
        if let Some(chunk) = chunk
            && let Some(lines) = chunk.lines.as_ref()
        {
            for (offset, line) in lines.iter().enumerate() {
                flush_live_at(origin + offset, live_at, emit);
                emit(line);
            }
        } else if let Some(chunk) = chunk {
            for offset in 0..chunk.height {
                flush_live_at(origin + offset, live_at, emit);
                emit(&Line::raw(""));
            }
        }
    };
    emit_chunk(cache.prefix.as_ref(), 0, &mut live_at, &mut emit);
    emit_chunk(cache.tail.as_ref(), prefix_h, &mut live_at, &mut emit);
    while live_at < live.len() {
        for live_line in &live[live_at].1 {
            emit(live_line);
        }
        live_at += 1;
    }
    (visible, plain, total)
}

fn evict_offscreen_chunk(
    chunk: Option<&mut CachedChunk>,
    origin: usize,
    view_start: usize,
    view_end: usize,
) {
    let Some(chunk) = chunk else {
        return;
    };
    let chunk_end = origin.saturating_add(chunk.height);
    if chunk_end <= view_start || origin >= view_end {
        chunk.lines = None;
        chunk.plain = None;
    }
}

fn ensure_chunk_lines(state: &AppState, width: usize, chunk: Option<&mut CachedChunk>) {
    let Some(chunk) = chunk else {
        return;
    };
    if chunk.lines.is_some() {
        return;
    }
    let mut unused = Vec::new();
    *chunk = render_chunk(state, width, chunk.item_lo, chunk.item_hi, &mut unused);
}

fn realized_live_slots(
    cache: &ChatPaintCache,
    state: &AppState,
    width: usize,
) -> Vec<(usize, Vec<Line<'static>>)> {
    let mut out = Vec::new();
    for slot in &cache.live_slots {
        let mut lines = Vec::new();
        if render_live_member_activity(state, width, &slot.member, slot.show_header, &mut lines) {
            if slot.show_header {
                lines.push(Line::raw(""));
            }
            out.push((slot.at, lines));
        }
    }
    out
}

#[cfg(test)]
fn render_chat_history(state: &AppState, width: usize, start: usize, out: &mut Vec<Line<'static>>) {
    render_chat_history_range(
        state,
        width,
        start,
        state.chat().len(),
        out,
        LiveRender::Inline,
    );
}

fn apply_selection_style(
    visible: &mut [Line<'_>],
    first_line: usize,
    selection: crate::tui::app_state::ChatSelection,
) {
    let (from, to) = selection.normalized();
    for (offset, line) in visible.iter_mut().enumerate() {
        let index = first_line + offset;
        if index < from.0 || index > to.0 {
            continue;
        }
        let start_col = if index == from.0 { from.1 } else { 0 };
        let end_col = if index == to.0 {
            to.1.saturating_add(1)
        } else {
            usize::MAX
        };
        *line = restyle_column_range(line, start_col, end_col);
    }
}

fn restyle_column_range(line: &Line<'_>, start_col: usize, end_col: usize) -> Line<'static> {
    let end_col = end_col.max(start_col);
    let mut out = Vec::new();
    let mut col = 0;
    for span in &line.spans {
        let mut unselected = String::new();
        let mut selected = String::new();
        for ch in span.content.chars() {
            let width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if col >= start_col && col < end_col {
                if !unselected.is_empty() {
                    out.push(Span::styled(std::mem::take(&mut unselected), span.style));
                }
                selected.push(ch);
            } else {
                if !selected.is_empty() {
                    out.push(Span::styled(
                        std::mem::take(&mut selected),
                        theme::chat_selection(),
                    ));
                }
                unselected.push(ch);
            }
            col += width;
        }
        if !unselected.is_empty() {
            out.push(Span::styled(unselected, span.style));
        }
        if !selected.is_empty() {
            out.push(Span::styled(selected, theme::chat_selection()));
        }
    }
    Line::from(out)
}

/// Display order that keeps one member's work together only when a later
/// prompt was sent while that member was already working. Sequential turns
/// stay chronological.
fn grouped_chat_indices(items: &[ChatItem], start: usize, end: usize) -> Vec<usize> {
    let start = start.min(items.len());
    let end = end.min(items.len()).max(start);
    let mut used = vec![false; end];
    let mut out = Vec::with_capacity(end.saturating_sub(start));
    for i in start..end {
        if used[i] {
            continue;
        }
        used[i] = true;
        out.push(i);
        let Some(member) = item_member(&items[i]).cloned() else {
            continue;
        };
        for j in (i + 1)..end {
            if used[j] {
                continue;
            }
            if user_breaks_member_region(&items[j], &member)
                || relay_starts_member_region(&items[j], &member)
            {
                break;
            }
            if item_member(&items[j]) == Some(&member) {
                used[j] = true;
                out.push(j);
            }
        }
    }
    out
}

/// An incoming member relay is a fresh unit of work for its recipient. Do not
/// pull that recipient's later tools back under their earlier reply just
/// because another member's route sat between the two in the event timeline.
fn relay_starts_member_region(item: &ChatItem, member: &MemberId) -> bool {
    matches!(item, ChatItem::Route { to, .. } if to.iter().any(|target| target == member.as_str()))
}

fn user_breaks_member_region(item: &ChatItem, member: &MemberId) -> bool {
    match item {
        ChatItem::User {
            targets,
            interrupted,
            ..
        } => {
            if targets.iter().any(|target| target == member) {
                return true;
            }
            // A prompt sent while this member was already working should not
            // split their later output into the other member's region.
            !interrupted.iter().any(|busy| busy == member)
        }
        _ => false,
    }
}

fn render_chat_history_range(
    state: &AppState,
    width: usize,
    start: usize,
    end: usize,
    out: &mut Vec<Line<'static>>,
    mut live: LiveRender<'_>,
) {
    let items = state.chat();
    let start = start.min(items.len());
    let end = end.min(items.len()).max(start);
    let order = grouped_chat_indices(items, start, end);
    let mut saw_work_activity = start > 0 && items[..start].iter().any(is_work_activity);
    let mut rendered_live = std::collections::HashSet::new();
    let mut skip_until = 0usize;
    for (pos, &i) in order.iter().enumerate() {
        if pos < skip_until {
            continue;
        }
        let item = &items[i];
        // Thinking rows from older saved conversations are retained in the
        // database for compatibility, but thinking is now transient progress.
        if matches!(item, ChatItem::Thinking { .. }) {
            continue;
        }
        if matches!(item, ChatItem::User { .. }) && saw_work_activity {
            render_turn_separator(width, out);
            saw_work_activity = false;
        }
        if is_work_activity(item) {
            saw_work_activity = true;
        }
        let before = out.len();
        let previous = order[..pos]
            .iter()
            .rev()
            .map(|&p| &items[p])
            .find(|item| item_renders(item, state))
            .or_else(|| {
                (start > 0)
                    .then(|| {
                        items[..start]
                            .iter()
                            .rev()
                            .find(|item| item_renders(item, state))
                    })
                    .flatten()
            });
        let previous_sender = previous.and_then(item_sender);
        // A new user or agent reply keeps its own title. Mid-turn text after
        // the same member's work stays on that member header.
        let show_sender_header = match item {
            ChatItem::User { .. } => true,
            ChatItem::Agent { member, .. } => match previous {
                Some(ChatItem::Agent { member: prev, .. }) if prev == member => true,
                Some(prev) if item_sender(prev) == Some(ChatSender::Agent(member.clone())) => false,
                _ => true,
            },
            _ => item_sender(item) != previous_sender,
        };
        let run_end = consecutive_work_run_end(items, &order, pos);
        let is_find_current = order[pos..=run_end]
            .iter()
            .any(|&idx| state.find_current_chat_index() == Some(idx));
        if matches!(item, ChatItem::Tool { .. } | ChatItem::Diff { .. }) {
            let run: Vec<&ChatItem> = order[pos..=run_end].iter().map(|&j| &items[j]).collect();
            render_work_run(&run, width, state, out, show_sender_header);
            skip_until = run_end + 1;
        } else {
            render_item(item, width, state, out, show_sender_header);
        }
        if is_find_current && let Some(line) = out.get_mut(before) {
            // Marker in the gutter for the current `/find` match.
            let mut spans = vec![Span::styled("»", theme::selection())];
            spans.append(&mut line.spans);
            line.spans = spans;
        }
        let item = &items[order[run_end]];
        let next = order.get(run_end + 1).map(|&j| &items[j]);
        let member_block_ends = item_member(item)
            .is_some_and(|member| next.and_then(item_member).is_none_or(|next| next != member));
        if member_block_ends
            && let Some(member) = item_member(item)
            && live_activity_belongs_after(items, &order, run_end, member)
        {
            // Thinking is already a live indicator. Mark the region claimed
            // so the trailing fallback does not reprint Working at the bottom.
            match &mut live {
                #[cfg(test)]
                LiveRender::Inline => {
                    if matches!(item, ChatItem::Thinking { .. })
                        || render_live_member_activity(state, width, member, false, out)
                    {
                        rendered_live.insert(member.clone());
                    }
                }
                LiveRender::Record(slots) => {
                    if matches!(item, ChatItem::Thinking { .. }) {
                        rendered_live.insert(member.clone());
                    } else {
                        slots.push(LiveSlot {
                            at: out.len(),
                            member: member.clone(),
                            show_header: false,
                        });
                        rendered_live.insert(member.clone());
                    }
                }
            }
        }
        // Same-member tools / thinking / diffs stay on one rail. A completed
        // tool/diff run gets a rail gap before the member's next reply;
        // distinct replies and members get a plain gap.
        if out.len() > before {
            let same_member = next.is_some_and(|next| same_member_thread(item, next));
            let glued_replies = is_speech_item(item) && next.is_some_and(is_speech_item);
            let work_to_reply = matches!(item, ChatItem::Tool { .. } | ChatItem::Diff { .. })
                && matches!(next, Some(ChatItem::Agent { .. }));
            if work_to_reply && same_member {
                if let Some(member) = item_member(item) {
                    out.push(Line::from(vec![
                        chat_rail(member_rail_color(state, member)),
                        Span::raw(""),
                    ]));
                }
            } else if !same_member || glued_replies {
                out.push(Line::raw(""));
            }
        }
    }
    if end == items.len() && saw_work_activity && state.running_count() == 0 {
        render_turn_separator(width, out);
    }
    if end == items.len() {
        for member in state.members() {
            if rendered_live.contains(&member.id) {
                continue;
            }
            match &mut live {
                #[cfg(test)]
                LiveRender::Inline => {
                    if member_status_is_active(member.status)
                        && render_live_member_activity(state, width, &member.id, true, out)
                    {
                        out.push(Line::raw(""));
                    }
                }
                LiveRender::Record(slots) => {
                    slots.push(LiveSlot {
                        at: out.len(),
                        member: member.id.clone(),
                        show_header: true,
                    });
                }
            }
        }
    }
}

/// Live thinking belongs on this member's latest region, not on an earlier
/// finished reply. A later prompt only closes the region when it would also
/// break grouping — talking to someone else while this member is still
/// working must leave their thinking in their own block.
fn live_activity_belongs_after(
    items: &[ChatItem],
    order: &[usize],
    pos: usize,
    member: &MemberId,
) -> bool {
    !order.iter().skip(pos + 1).any(|&j| {
        let later = &items[j];
        item_member(later) == Some(member) || user_breaks_member_region(later, member)
    })
}

fn render_live_member_activity(
    state: &AppState,
    width: usize,
    member_id: &MemberId,
    show_placeholder_header: bool,
    out: &mut Vec<Line<'static>>,
) -> bool {
    let Some(member) = state.members().iter().find(|m| &m.id == member_id) else {
        return false;
    };
    if !member_status_is_active(member.status) {
        return false;
    }
    // A streaming answer is itself the live view; do not add a second row.
    if state.has_active_message(&member.id) {
        return false;
    }
    if show_placeholder_header {
        out.push(agent_header_line(
            &member.display_name,
            member.backend,
            state.member_color(&member.id),
        ));
    }
    let reasoning = state.active_reasoning().get(member_id).map(String::as_str);
    let line_text = status_indicator::member_activity_text(
        member.status,
        reasoning,
        reasoning.is_some(),
        state.member_elapsed_secs(&member.id),
        status_indicator::spinner(),
        Some(&state.member_runtime_profile(member)),
    );
    for wrapped in markdown::wrap(&line_text, width.saturating_sub(2).max(1)) {
        out.push(Line::from(vec![
            chat_rail(state.member_color(&member.id)),
            Span::raw(" "),
            Span::styled(wrapped, theme::muted_italic()),
        ]));
    }
    true
}

fn consecutive_work_run_end(items: &[ChatItem], order: &[usize], pos: usize) -> usize {
    let member = match order.get(pos).map(|&i| &items[i]) {
        Some(ChatItem::Tool { member, .. } | ChatItem::Diff { member, .. }) => member,
        _ => return pos,
    };
    let mut end = pos;
    while let Some(&j) = order.get(end + 1) {
        match &items[j] {
            ChatItem::Tool { member: next, .. } | ChatItem::Diff { member: next, .. }
                if next == member =>
            {
                end += 1;
            }
            _ => break,
        }
    }
    end
}

fn render_work_run(
    run: &[&ChatItem],
    width: usize,
    state: &AppState,
    out: &mut Vec<Line<'static>>,
    show_sender_header: bool,
) {
    let all_tools: Vec<&ChatItem> = run
        .iter()
        .copied()
        .filter(|item| matches!(item, ChatItem::Tool { .. }))
        .collect();
    let diffs: Vec<&ChatItem> = run
        .iter()
        .copied()
        .filter(|item| matches!(item, ChatItem::Diff { .. }))
        .collect();
    let mut diff_member = None;
    let mut files = Vec::new();
    let mut diff_ok = true;
    if let Some((member, merged, ok)) = merge_diff_run(&diffs) {
        diff_member = Some(member);
        files = merged;
        diff_ok = ok;
    }
    let edit_tools: Vec<&ChatItem> = all_tools
        .iter()
        .copied()
        .filter(|item| {
            is_edit_tool(item)
                && !matches!(
                    item,
                    ChatItem::Tool {
                        ok: Some(false),
                        ..
                    }
                )
        })
        .collect();
    for tool in &edit_tools {
        let ChatItem::Tool {
            name,
            summary,
            detail,
            ok,
            ..
        } = tool
        else {
            continue;
        };
        if let Some(file) = tool_display::file_change_from_edit_tool(name, summary, detail) {
            merge_file_change(&mut files, file);
        }
        diff_ok &= *ok != Some(false);
    }
    let tools: Vec<&ChatItem> = all_tools
        .iter()
        .copied()
        .filter(|item| {
            !is_edit_tool(item)
                || matches!(
                    item,
                    ChatItem::Tool {
                        ok: Some(false),
                        ..
                    }
                )
        })
        .collect();
    if !tools.is_empty() {
        render_tool_group(&tools, width, state, out, show_sender_header);
    }
    let member = diff_member.or_else(|| {
        edit_tools.iter().find_map(|item| match item {
            ChatItem::Tool { member, .. } => Some(member),
            _ => None,
        })
    });
    if let Some(member) = member.filter(|_| !files.is_empty()) {
        if tools.is_empty() && show_sender_header {
            let (display_name, backend) = state.member_meta(member);
            out.push(agent_header_line(
                &display_name,
                backend,
                state.member_color(member),
            ));
        }
        for (index, files) in split_files_by_edit(&edit_tools, files)
            .into_iter()
            .enumerate()
        {
            if index > 0 {
                out.push(Line::from(vec![
                    chat_rail(member_rail_color(state, member)),
                    Span::raw(""),
                ]));
            }
            render_file_changes(state, member, &files, diff_ok, width, out);
        }
    }
}

fn is_edit_tool(item: &ChatItem) -> bool {
    matches!(item, ChatItem::Tool { name, .. } if tool_display::tool_kind(name) == "Edit")
}

fn split_files_by_edit(
    tools: &[&ChatItem],
    files: Vec<FileChangeItem>,
) -> Vec<Vec<FileChangeItem>> {
    let mut assigned = vec![false; files.len()];
    let mut groups = Vec::new();
    for tool in tools {
        let ChatItem::Tool {
            name,
            summary,
            detail,
            ..
        } = tool
        else {
            continue;
        };
        if tool_display::tool_kind(name) != "Edit" {
            continue;
        }
        let target = tool_display::tool_target(name, summary, detail);
        if target.is_empty() {
            continue;
        }
        let edited = files
            .iter()
            .enumerate()
            .filter_map(|(index, file)| {
                (!assigned[index] && paths_match(&target, &file.path)).then(|| {
                    assigned[index] = true;
                    file.clone()
                })
            })
            .collect::<Vec<_>>();
        if !edited.is_empty() {
            groups.push(edited);
        }
    }
    let unassigned = files
        .into_iter()
        .enumerate()
        .filter_map(|(index, file)| (!assigned[index]).then_some(file))
        .collect::<Vec<_>>();
    if !unassigned.is_empty() {
        groups.push(unassigned);
    }
    groups
}

fn paths_match(edit_target: &str, file_path: &str) -> bool {
    let edit_target = edit_target.trim();
    edit_target == file_path
        || file_path
            .strip_suffix(edit_target)
            .is_some_and(|prefix| prefix.ends_with('/'))
        || edit_target
            .strip_suffix(file_path)
            .is_some_and(|prefix| prefix.ends_with('/'))
}

fn merge_diff_run<'a>(diffs: &[&'a ChatItem]) -> Option<(&'a MemberId, Vec<FileChangeItem>, bool)> {
    let mut member = None;
    let mut files: Vec<FileChangeItem> = Vec::new();
    let mut ok = true;
    for item in diffs {
        let ChatItem::Diff {
            member: next,
            files: next_files,
            ok: next_ok,
        } = item
        else {
            continue;
        };
        member = Some(next);
        ok &= *next_ok;
        for file in next_files {
            if let Some(existing) = files.iter_mut().find(|seen| seen.path == file.path) {
                *existing = file.clone();
            } else {
                files.push(file.clone());
            }
        }
    }
    Some((member?, files, ok))
}

fn merge_file_change(files: &mut Vec<FileChangeItem>, incoming: FileChangeItem) {
    if let Some(existing) = files
        .iter_mut()
        .find(|existing| paths_match(&incoming.path, &existing.path))
    {
        if file_change_detail_len(&incoming) > file_change_detail_len(existing) {
            *existing = incoming;
        }
    } else {
        files.push(incoming);
    }
}

fn file_change_detail_len(file: &FileChangeItem) -> usize {
    file.old_text.as_ref().map_or(0, String::len)
        + file.new_text.as_ref().map_or(0, String::len)
        + file.patch.as_ref().map_or(0, String::len)
}

fn render_tool_group(
    tools: &[&ChatItem],
    width: usize,
    state: &AppState,
    out: &mut Vec<Line<'static>>,
    show_sender_header: bool,
) {
    let Some(ChatItem::Tool { member, .. }) = tools.first().copied() else {
        return;
    };
    if show_sender_header {
        let (display_name, backend) = state.member_meta(member);
        out.push(agent_header_line(
            &display_name,
            backend,
            state.member_color(member),
        ));
    }
    let rail_color = member_rail_color(state, member);
    let all_failed = tools.iter().all(|item| {
        matches!(
            item,
            ChatItem::Tool {
                ok: Some(false),
                ..
            }
        )
    });
    let (marker, title_style) = if all_failed {
        ("✕", theme::error_bold())
    } else {
        ("⚒", theme::accent_bold())
    };
    out.push(Line::from(vec![
        chat_rail(rail_color),
        Span::styled(format!("   {marker} tools"), title_style),
    ]));
    for item in tools {
        let ChatItem::Tool {
            name,
            summary,
            detail,
            ok,
            ..
        } = item
        else {
            continue;
        };
        render_tool_row(rail_color, name, summary, detail, *ok, width, state, out);
    }
}

fn render_tool_row(
    rail_color: Color,
    name: &str,
    summary: &str,
    detail: &str,
    ok: Option<bool>,
    width: usize,
    state: &AppState,
    out: &mut Vec<Line<'static>>,
) {
    let (prefix, color, text_style) = match ok {
        None => (
            status_indicator::spinner(),
            theme::warning_color(),
            theme::emphasis(),
        ),
        Some(true) => ("·", theme::success_color(), theme::text()),
        Some(false) => ("✕", theme::error_color(), theme::error()),
    };
    let kind = tool_display::tool_kind(name);
    let target = tool_display::tool_target(name, summary, detail);
    let kind_col = theme::pad_width(&kind, 6);
    let rest_width = width.saturating_sub(8).max(10);
    let mut spans = vec![
        chat_rail(rail_color),
        Span::styled(format!("     {prefix} "), Style::default().fg(color)),
        Span::styled(kind_col, text_style),
    ];
    if !target.is_empty() {
        let shown = truncate_width(&target, rest_width.saturating_sub(8).max(4));
        spans.push(Span::raw("  "));
        spans.push(Span::styled(shown, theme::muted()));
    }
    out.push(Line::from(spans));
    if detail.trim().is_empty() {
        return;
    }
    let detail_style = theme::muted();
    let detail_width = width.saturating_sub(10).max(1);
    let expanded = state.tools_expanded();
    let failure = ok == Some(false);
    let (lines, clipped) = if failure {
        let body = tool_display::tool_error_body(detail);
        if body.is_empty() {
            (Vec::new(), false)
        } else if expanded {
            (markdown::wrap(&body, detail_width), false)
        } else {
            let (preview, clipped) = tool_display::compact_tool_error(&body);
            let wrapped = markdown::wrap(&preview, detail_width);
            let clipped = clipped || wrapped.len() > 6;
            (wrapped.into_iter().take(6).collect::<Vec<_>>(), clipped)
        }
    } else if expanded {
        let wrapped = markdown::wrap(&tool_display::tool_body(detail), detail_width);
        (wrapped, false)
    } else {
        (Vec::new(), false)
    };
    for (idx, line) in lines.into_iter().enumerate() {
        let line_style = if failure && is_tool_error_line(&line) {
            theme::error()
        } else {
            detail_style
        };
        out.push(Line::from(vec![
            chat_rail(rail_color),
            Span::raw(if idx == 0 { "       ↳ " } else { "         " }),
            Span::styled(line, line_style),
        ]));
    }
    if clipped && !expanded {
        out.push(Line::from(vec![
            chat_rail(rail_color),
            Span::styled(
                "         … Ctrl+O expand tool output",
                theme::muted_italic(),
            ),
        ]));
    }
}

fn is_tool_error_line(line: &str) -> bool {
    let line = line.trim_start().to_ascii_lowercase();
    line.starts_with("error:")
        || line.contains("test result: failed")
        || line.contains("panicked at")
        || line.starts_with("failures:")
        || line.contains("assertion")
        || line.starts_with("failure:")
}

fn render_file_changes(
    state: &AppState,
    member: &MemberId,
    files: &[FileChangeItem],
    ok: bool,
    width: usize,
    out: &mut Vec<Line<'static>>,
) {
    let rail_color = member_rail_color(state, member);
    let (marker, title_style) = if ok {
        ("✎", theme::accent_bold())
    } else {
        ("✕", theme::error_bold())
    };
    if !state.diffs_expanded() {
        let count = files.len();
        let summary = if count == 1 {
            files
                .first()
                .map(|file| truncate_width(&file.path, width.saturating_sub(36).max(8)))
                .unwrap_or_else(|| "1 file".to_string())
        } else {
            format!("{count} files")
        };
        out.push(Line::from(vec![
            chat_rail(rail_color),
            Span::styled(format!("   {marker} file changes"), title_style),
            Span::styled(
                format!(" · {summary} · Ctrl+G expand"),
                theme::muted_italic(),
            ),
        ]));
        return;
    }
    out.push(Line::from(vec![
        chat_rail(rail_color),
        Span::styled(format!("   {marker} file changes"), title_style),
        Span::styled(" · Ctrl+G collapse", theme::muted_italic()),
    ]));
    for (index, file) in files.iter().enumerate() {
        if index > 0 {
            out.push(Line::from(vec![chat_rail(rail_color), Span::raw("")]));
        }
        let (sign, style) = match file.kind.as_str() {
            "add" => ("+", theme::diff_add_text()),
            "delete" => ("-", theme::diff_delete_text()),
            _ => ("Edit", theme::text()),
        };
        let shown = truncate_width(&file.path, width.saturating_sub(11).max(10));
        out.push(Line::from(vec![
            chat_rail(rail_color),
            Span::styled(format!("     {sign} "), style),
            Span::styled(shown, theme::warning()),
        ]));
        let hunks = file
            .patch
            .as_deref()
            .map(file_diff::unified_hunks)
            .filter(|hunks| !hunks.is_empty())
            .unwrap_or_else(
                || match (file.old_text.as_deref(), file.new_text.as_deref()) {
                    (None, None) => Vec::new(),
                    (None, Some(new_text)) => file_diff::line_hunks("", new_text),
                    (Some(old_text), None) => file_diff::line_hunks(old_text, ""),
                    (Some(old_text), Some(new_text)) => file_diff::line_hunks(old_text, new_text),
                },
            );
        let max_hunks = 80;
        let clipped = hunks.len() > max_hunks;
        let line_number_width = hunks
            .iter()
            .filter_map(|line| line.number)
            .map(|number| number.to_string().len())
            .max()
            .unwrap_or(1);
        for line in hunks.into_iter().take(max_hunks) {
            let (mark, number_style, content_style) = match line.kind {
                '+' => ("+", theme::diff_add_text(), theme::diff_add()),
                '-' => ("-", theme::diff_delete_text(), theme::diff_delete()),
                _ => (" ", theme::muted(), theme::muted()),
            };
            let number = line
                .number
                .map(|number| format!("{number:>line_number_width$}"))
                .unwrap_or_else(|| " ".repeat(line_number_width));
            let shown = clip_width(
                &line.text,
                width.saturating_sub(line_number_width + 8).max(8),
            );
            out.push(Line::from(vec![
                chat_rail(rail_color),
                Span::styled(format!("     {number} "), number_style),
                Span::styled(
                    pad_diff_content(&format!("{mark}{shown}"), width, line_number_width),
                    content_style,
                ),
            ]));
        }
        if clipped {
            out.push(Line::from(vec![
                chat_rail(rail_color),
                Span::styled("       …", theme::muted_italic()),
            ]));
        }
    }
}

fn pad_diff_content(text: &str, width: usize, line_number_width: usize) -> String {
    let target = width.saturating_sub(line_number_width + 7);
    let used = theme::display_width(text);
    if used >= target {
        return text.to_string();
    }
    format!("{text}{}", " ".repeat(target - used))
}

fn is_work_activity(item: &ChatItem) -> bool {
    matches!(
        item,
        ChatItem::Tool { ok: Some(_), .. } | ChatItem::Diff { .. } | ChatItem::Route { .. }
    )
}

fn is_speech_item(item: &ChatItem) -> bool {
    matches!(item, ChatItem::User { .. } | ChatItem::Agent { .. })
}

fn thinking_label(live: bool, stored: Option<u64>, live_secs: Option<u64>, lines: usize) -> String {
    let lines = format!("{lines} lines");
    if live {
        match live_secs {
            Some(secs) => format!(
                "thinking · {} · {lines}",
                status_indicator::fmt_elapsed_compact(secs)
            ),
            None => format!("thinking · {lines}"),
        }
    } else if let Some(secs) = stored {
        format!(
            "thinking for {} · {lines}",
            status_indicator::fmt_elapsed_compact(secs)
        )
    } else {
        format!("thought · {lines}")
    }
}

fn same_member_thread(current: &ChatItem, next: &ChatItem) -> bool {
    item_member(current).is_some_and(|member| item_member(next) == Some(member))
}

fn item_member(item: &ChatItem) -> Option<&MemberId> {
    match item {
        ChatItem::Agent { member, .. }
        | ChatItem::Thinking { member, .. }
        | ChatItem::Tool { member, .. }
        | ChatItem::Diff { member, .. } => Some(member),
        ChatItem::Route { from, .. } => Some(from),
        ChatItem::Error { member, .. } => member.as_ref(),
        ChatItem::Verdict { member, .. } => Some(member),
        ChatItem::User { .. } | ChatItem::Notice { .. } => None,
    }
}

/// Empty closed agent cells stay in the timeline after a tool split but
/// must not count as a previous speaker, or the next tool loses its header.
fn item_renders(item: &ChatItem, state: &AppState) -> bool {
    match item {
        ChatItem::Agent { text, member, .. } => {
            !text.is_empty() || state.has_active_message(member)
        }
        ChatItem::Thinking { .. } => false,
        _ => true,
    }
}

/// A full-width rule between finished work turns.
fn render_turn_separator(width: usize, out: &mut Vec<Line<'static>>) {
    while out.last().is_some_and(line_is_blank) {
        out.pop();
    }
    let rule_width = width.max(1);
    out.push(Line::from(Span::styled(
        "─".repeat(rule_width),
        theme::muted(),
    )));
    out.push(Line::raw(""));
}

fn line_is_blank(line: &Line<'_>) -> bool {
    line.spans.iter().all(|span| span.content.trim().is_empty())
}

fn quick_start_lines(state: &AppState) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(" Asterline", theme::accent_bold()),
        Span::styled(" · Multi-Agent Coding Console", theme::muted()),
    ]));

    if state.members().is_empty() {
        lines.push(Line::styled(" Team is loading...", theme::muted()));
        return lines;
    }

    let members = state
        .members()
        .iter()
        .map(|member| {
            format!(
                "{} ({}, {})",
                member.id,
                member.backend.as_str(),
                member.role
            )
        })
        .collect::<Vec<_>>()
        .join("  ");
    lines.push(Line::from(vec![
        Span::styled(" Members: ", theme::muted()),
        Span::styled(members, theme::text()),
    ]));
    lines.push(Line::raw(""));

    let example_member = state
        .members()
        .iter()
        .find(|member| match state.default_target() {
            Some(DefaultTarget::Member(id)) => &member.id == id,
            _ => false,
        })
        .or_else(|| state.members().first())
        .map(|member| member.id.to_string())
        .unwrap_or_else(|| "member".to_string());
    let examples = [
        (format!("@{example_member} <message>"), "message one member"),
        ("/mode".to_string(), "choose a collaboration mode"),
        ("/help".to_string(), "all commands"),
    ];
    for (i, (cmd, desc)) in examples.iter().enumerate() {
        lines.push(Line::from(vec![
            Span::styled(if i == 0 { " Try:  " } else { "       " }, theme::muted()),
            Span::styled(format!("{cmd:<24}"), theme::accent_bold()),
            Span::styled(desc.to_string(), theme::muted()),
        ]));
    }
    lines
}

fn agent_header_line(
    display_name: &str,
    backend: crate::domain::team::BackendKind,
    color: Color,
) -> Line<'static> {
    Line::from(vec![
        chat_rail(color),
        Span::raw(" "),
        Span::styled("◆ ", theme::bold(color)),
        Span::styled(display_name.to_string(), theme::bold(color)),
        Span::styled(format!("  · {}", backend.as_str()), theme::muted()),
    ])
}

fn user_header_line(state: &AppState, targets: &[MemberId]) -> Line<'static> {
    let mut spans = vec![
        Span::styled("◆ ", theme::bold(theme::user_color())),
        Span::styled("You", theme::bold(theme::user_color())),
    ];
    if targets.is_empty() {
        return Line::from(spans);
    }
    spans.push(Span::styled(" → ", theme::muted()));
    let roster = state.members();
    if roster.len() > 1
        && targets.len() == roster.len()
        && roster
            .iter()
            .all(|member| targets.iter().any(|target| target == &member.id))
    {
        spans.push(Span::styled("all", theme::emphasis()));
        return Line::from(spans);
    }
    for (index, target) in targets.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(", ", theme::muted()));
        }
        let (name, _) = state.member_meta(target);
        spans.push(Span::styled(name, state.member_color_bold(target)));
    }
    Line::from(spans)
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ChatSender {
    User,
    Agent(MemberId),
}

fn item_sender(item: &ChatItem) -> Option<ChatSender> {
    match item {
        ChatItem::User { .. } => Some(ChatSender::User),
        ChatItem::Agent { member, .. } => Some(ChatSender::Agent(member.clone())),
        // Tools, diffs, relays, and member-attributed failures belong to the
        // same visible speaker block as the response they lead to. Treating
        // them as anonymous made a tool appear before its member title.
        _ => item_member(item).cloned().map(ChatSender::Agent),
    }
}

fn render_item(
    item: &ChatItem,
    width: usize,
    state: &AppState,
    out: &mut Vec<Line<'static>>,
    show_sender_header: bool,
) {
    if show_sender_header
        && !matches!(item, ChatItem::User { .. } | ChatItem::Agent { .. })
        && let Some(member) = item_member(item)
    {
        let (display_name, backend) = state.member_meta(member);
        out.push(agent_header_line(
            &display_name,
            backend,
            state.member_color(member),
        ));
    }
    match item {
        ChatItem::User { body, targets, .. } => {
            if show_sender_header {
                out.push(user_header_line(state, targets));
            }
            let body = crate::adapter::prompt_images::display_prompt_images(body);
            for line in markdown::wrap(&body, width.saturating_sub(2).max(1)) {
                out.push(Line::from(vec![
                    chat_rail(theme::user_color()),
                    Span::raw(" "),
                    Span::styled(line, theme::emphasis()),
                ]));
            }
        }
        ChatItem::Agent {
            member,
            display_name,
            backend,
            text,
            ..
        } => {
            if text.is_empty() && !state.has_active_message(member) {
                return;
            }
            if show_sender_header {
                out.push(agent_header_line(
                    display_name,
                    *backend,
                    state.member_color(member),
                ));
            }
            for line in markdown::render(text, width.saturating_sub(2).max(1)) {
                let mut spans = vec![chat_rail(state.member_color(member)), Span::raw(" ")];
                spans.extend(line.spans);
                out.push(Line::from(spans));
            }
        }
        ChatItem::Thinking {
            member,
            text,
            elapsed_secs,
            ..
        } => {
            if text.is_empty() {
                return;
            }
            let rail_color = member_rail_color(state, member);
            let lines = text
                .lines()
                .filter(|line| !line.trim().is_empty())
                .count()
                .max(1);
            let live = elapsed_secs.is_none() && state.is_thinking_live(member);
            let label =
                thinking_label(live, *elapsed_secs, state.thinking_live_secs(member), lines);
            let label = if live {
                format!("{} {label}", status_indicator::spinner())
            } else {
                format!("✻ {label}")
            };
            if !state.thinking_expanded() {
                out.push(Line::from(vec![
                    chat_rail(rail_color),
                    Span::raw("   "),
                    Span::styled(format!("{label} · Ctrl+T expand"), theme::muted_italic()),
                ]));
                return;
            }
            out.push(Line::from(vec![
                chat_rail(rail_color),
                Span::raw("   "),
                Span::styled(format!("{label} · Ctrl+T collapse"), theme::muted_italic()),
            ]));
            for line in markdown::wrap(text.trim(), width.saturating_sub(6).max(1)) {
                out.push(Line::from(vec![
                    chat_rail(rail_color),
                    Span::raw("     "),
                    Span::styled(line, theme::muted_italic()),
                ]));
            }
        }
        ChatItem::Tool { .. } => {
            render_tool_group(&[item], width, state, out, false);
        }
        ChatItem::Diff { member, files, ok } => {
            render_file_changes(state, member, files, *ok, width, out);
        }
        ChatItem::Route { from, to, body } => {
            let from_backend = member_rail_color(state, from);
            out.push(Line::from(vec![
                chat_rail(from_backend),
                Span::styled("   ↳ ", theme::accent()),
                Span::styled(
                    format!("{from} → {}", to.join(", ")),
                    theme::bold(from_backend),
                ),
            ]));
            for line in markdown::wrap(body, width.saturating_sub(6).max(1)) {
                out.push(Line::from(vec![
                    chat_rail(from_backend),
                    Span::styled(format!("     {line}"), theme::muted()),
                ]));
            }
        }
        ChatItem::Notice { text } => {
            push_wrapped(&format!("  • {text}"), width, "", theme::notice(), out);
        }
        ChatItem::Error { member, message } => {
            if let Some(member) = member {
                let rail_color = member_rail_color(state, member);
                let text = format!("✕ {member}: {message}");
                for line in markdown::wrap(&text, width.saturating_sub(4).max(1)) {
                    out.push(Line::from(vec![
                        chat_rail(rail_color),
                        Span::styled(format!("   {line}"), theme::error()),
                    ]));
                }
            } else {
                push_wrapped(&format!("  ✕ {message}"), width, "", theme::error(), out);
            }
        }
        ChatItem::Verdict {
            approve, summary, ..
        } => {
            if *approve {
                out.push(Line::from(Span::styled(
                    "  ✓ review approved",
                    theme::success_bold(),
                )));
            } else {
                out.push(Line::from(Span::styled(
                    "  ✗ changes requested",
                    theme::warning_bold(),
                )));
            }
            let summary = summary.trim();
            if !summary.is_empty() {
                push_wrapped(summary, width, "    ", theme::text(), out);
            }
        }
    }
}

fn push_wrapped(
    text: &str,
    width: usize,
    indent: &str,
    style: Style,
    out: &mut Vec<Line<'static>>,
) {
    let wrap_width = width.saturating_sub(indent.len()).max(1);
    for line in markdown::wrap(text, wrap_width) {
        out.push(Line::from(Span::styled(format!("{indent}{line}"), style)));
    }
}

fn render_composer(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let (border_color, title_text) = if !state.pending_approvals().is_empty() {
        (
            theme::warning_color(),
            format!(
                " {} pending approval(s) · /approve ",
                state.pending_approvals().len()
            ),
        )
    } else if state.paused_routes() > 0 {
        (
            theme::warning_color(),
            format!(" {} route(s) paused · /retry ", state.paused_routes()),
        )
    } else if state.running_count() > 0 {
        (theme::muted_color(), " processing… ".to_string())
    } else {
        // Idle: a clean open composer (no title), like codex.
        (theme::muted_color(), String::new())
    };

    // Open composer: top and bottom rules only, no enclosing side bars.
    let mode = state.active_mode();
    let mode_title = Line::from(Span::styled(
        format!(" mode:{mode} "),
        theme::bold(theme::mode_color(mode)),
    ))
    .right_aligned();
    let block = Block::default()
        .title(title_text)
        .title_bottom(mode_title)
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(border_color));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = inner.height as usize;
    if rows == 0 {
        return;
    }
    let avail = (inner.width as usize).saturating_sub(2); // "> " / "  " gutter

    let mut out_lines: Vec<Line> = Vec::new();
    let mut cursor_screen: Option<(u16, u16)> = None;
    let mut text_origin = 0usize;
    if !state.pending_images().is_empty() {
        let labels = state
            .pending_images()
            .iter()
            .map(|image| image.label())
            .collect::<Vec<_>>()
            .join(" · ");
        out_lines.push(Line::from(Span::styled(
            format!("📎 {labels}"),
            theme::muted(),
        )));
        text_origin = 1;
        if rows == 1 {
            frame.render_widget(Paragraph::new(out_lines), inner);
            return;
        }
    }

    // Visual lines with wrapping so long input is fully visible (no horizontal
    // clipping). The cursor maps directly to a screen cell.
    let (visual_lines, cursor_row, cursor_col) = state.composer().visual_lines_with_cursor(avail);
    let line_starts = state.composer().visual_lines_with_starts(avail);
    let selection = state.composer().selection_range();
    let text_rows = rows.saturating_sub(text_origin);

    // Vertical scroll so the cursor's visual line stays visible.
    let top = if cursor_row >= text_rows {
        cursor_row - text_rows + 1
    } else {
        0
    };

    for (offset, row) in (top..top + text_rows).enumerate() {
        let Some(line) = visual_lines.get(row) else {
            break;
        };
        let prefix = if row == 0 { "> " } else { "  " };
        let start_char = line_starts.get(row).map(|(_, start)| *start).unwrap_or(0);
        let cursor_width = if row == cursor_row { cursor_col } else { 0 };
        let mut spans = vec![Span::styled(
            prefix.to_string(),
            Style::default().fg(border_color),
        )];
        spans.extend(style_composer_line(line, start_char, selection));
        out_lines.push(Line::from(spans));
        if row == cursor_row {
            cursor_screen = Some((
                inner.x + 2 + cursor_width as u16,
                inner.y + (text_origin + offset) as u16,
            ));
        }
    }
    frame.render_widget(Paragraph::new(out_lines), inner);

    if state.drawer().is_none()
        && let Some((col, row)) = cursor_screen
    {
        frame.set_cursor_position((col, row));
    }
}

fn style_composer_line(
    line: &str,
    start_char: usize,
    selection: Option<(usize, usize)>,
) -> Vec<Span<'static>> {
    let Some((sel_start, sel_end)) = selection else {
        return vec![Span::raw(line.to_string())];
    };
    let mut spans = Vec::new();
    let mut index = start_char;
    let mut plain = String::new();
    let mut selected = String::new();
    for grapheme in unicode_segmentation::UnicodeSegmentation::graphemes(line, true) {
        let next = index + grapheme.chars().count();
        if index >= sel_start && index < sel_end {
            if !plain.is_empty() {
                spans.push(Span::raw(std::mem::take(&mut plain)));
            }
            selected.push_str(grapheme);
        } else {
            if !selected.is_empty() {
                spans.push(Span::styled(
                    std::mem::take(&mut selected),
                    theme::chat_selection(),
                ));
            }
            plain.push_str(grapheme);
        }
        index = next;
    }
    if !plain.is_empty() {
        spans.push(Span::raw(plain));
    }
    if !selected.is_empty() {
        spans.push(Span::styled(selected, theme::chat_selection()));
    }
    if spans.is_empty() {
        spans.push(Span::raw(String::new()));
    }
    spans
}

#[cfg(test)]
#[path = "chat_view_tests/mod.rs"]
mod tests;
