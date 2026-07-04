//! Central visual theme: the semantic palette, shared style helpers, and
//! width-aware text utilities. All TUI styling goes through here so the look
//! is consistent and tunable in one place.
//!
//! Color semantics (kept deliberately small):
//! - `ACCENT` — interactive/highlight (titles, commands, selection).
//! - `SUCCESS` / `WARNING` / `ERROR` — outcome states.
//! - `TEXT` / `MUTED` / `EMPHASIS` — content, chrome, and strong content.
//! - Backend identity colors are separate (`backend_color`) and are never
//!   reused for states.

use ratatui::style::{Color, Modifier, Style};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::domain::event::{LogLevel, MemberStatus, WorkflowRunStatus};
use crate::domain::team::BackendKind;

pub const ACCENT: Color = Color::Cyan;
pub const SUCCESS: Color = Color::Green;
pub const WARNING: Color = Color::Yellow;
pub const ERROR: Color = Color::Red;
pub const MUTED: Color = Color::DarkGray;
pub const TEXT: Color = Color::Gray;
pub const EMPHASIS: Color = Color::White;
/// Marker for user-authored input (the `›` gutter).
pub const USER: Color = Color::Green;

pub fn text() -> Style {
    Style::default().fg(TEXT)
}

pub fn muted() -> Style {
    Style::default().fg(MUTED)
}

pub fn muted_italic() -> Style {
    muted().add_modifier(Modifier::ITALIC)
}

pub fn emphasis() -> Style {
    Style::default().fg(EMPHASIS)
}

pub fn accent() -> Style {
    Style::default().fg(ACCENT)
}

pub fn accent_bold() -> Style {
    accent().add_modifier(Modifier::BOLD)
}

pub fn success() -> Style {
    Style::default().fg(SUCCESS)
}

pub fn success_bold() -> Style {
    success().add_modifier(Modifier::BOLD)
}

pub fn warning() -> Style {
    Style::default().fg(WARNING)
}

pub fn warning_bold() -> Style {
    warning().add_modifier(Modifier::BOLD)
}

pub fn error() -> Style {
    Style::default().fg(ERROR)
}

pub fn error_bold() -> Style {
    error().add_modifier(Modifier::BOLD)
}

pub fn bold(color: Color) -> Style {
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

/// The one selection style: black text on the accent color. Every selected
/// row/cell in the UI uses this, so "selected" always looks the same.
pub fn selection() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(ACCENT)
        .add_modifier(Modifier::BOLD)
}

/// Secondary selection for a focused cell inside an already-selected row.
pub fn selection_cell() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(WARNING)
        .add_modifier(Modifier::BOLD)
}

pub fn backend_color(backend: BackendKind) -> Color {
    match backend {
        BackendKind::Claude => Color::Magenta,
        BackendKind::Codex => Color::Cyan,
        BackendKind::Agy => Color::Blue,
    }
}

pub fn backend_bold(backend: BackendKind) -> Style {
    bold(backend_color(backend))
}

pub fn status_color(status: MemberStatus) -> Color {
    match status {
        MemberStatus::Running => WARNING,
        MemberStatus::Failed => ERROR,
        MemberStatus::NeedsApproval => WARNING,
        _ => MUTED,
    }
}

pub fn status_label(status: MemberStatus) -> &'static str {
    match status {
        MemberStatus::Idle => "idle",
        MemberStatus::Queued => "queued",
        MemberStatus::Running => "running",
        MemberStatus::Waiting => "waiting",
        MemberStatus::NeedsApproval => "approval",
        MemberStatus::Failed => "failed",
    }
}

pub fn workflow_status_color(status: WorkflowRunStatus) -> Color {
    match status {
        WorkflowRunStatus::Running | WorkflowRunStatus::Verifying => WARNING,
        WorkflowRunStatus::Done => SUCCESS,
        WorkflowRunStatus::Failed | WorkflowRunStatus::Blocked => ERROR,
        WorkflowRunStatus::Planned => MUTED,
    }
}

pub fn log_color(level: LogLevel) -> Color {
    match level {
        LogLevel::Error => ERROR,
        LogLevel::Warn => WARNING,
        LogLevel::Info => TEXT,
        LogLevel::Debug => MUTED,
    }
}

/// Terminal display width of a string (CJK and emoji count as 2 columns).
pub fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// Collapse internal whitespace and truncate to at most `max` display
/// columns, appending `…` when the text was cut.
pub fn truncate_width(text: &str, max: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    clip_width(&collapsed, max)
}

/// Truncate to at most `max` display columns without collapsing whitespace,
/// appending `…` when the text was cut.
pub fn clip_width(text: &str, max: usize) -> String {
    let max = max.max(1);
    if display_width(text) <= max {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > max.saturating_sub(1) {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

/// Truncate then right-pad with spaces to exactly `width` display columns.
pub fn pad_width(text: &str, width: usize) -> String {
    let mut out = clip_width(text, width);
    let used = display_width(&out);
    out.push_str(&" ".repeat(width.saturating_sub(used)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_width_counts_display_columns_not_chars() {
        // 4 CJK chars = 8 columns; clipping to 5 keeps 2 chars + ellipsis.
        assert_eq!(clip_width("项目路径名", 5), "项目…");
        assert_eq!(clip_width("abc", 5), "abc");
        assert_eq!(clip_width("abcdef", 5), "abcd…");
    }

    #[test]
    fn truncate_width_collapses_whitespace_first() {
        assert_eq!(truncate_width("a\n  b\tc", 10), "a b c");
        assert_eq!(truncate_width("hello   world", 7), "hello …");
    }

    #[test]
    fn pad_width_yields_exact_display_width() {
        assert_eq!(pad_width("ab", 4), "ab  ");
        assert_eq!(display_width(&pad_width("路径", 5)), 5);
        assert_eq!(display_width(&pad_width("路径很长很长", 5)), 5);
    }
}
