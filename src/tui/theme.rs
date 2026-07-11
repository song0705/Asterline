//! Central visual theme: the semantic palette, shared style helpers, and
//! width-aware text utilities. All TUI styling goes through here so the look
//! is consistent and tunable in one place.
//!
//! Color semantics (kept deliberately small):
//! - accent — interactive/highlight (titles, commands, selection).
//! - success / warning / error — outcome states.
//! - text / muted / emphasis — content, chrome, and strong content.
//! - Backend identity colors are separate (`backend_color`) and are never
//!   reused for states.

use ratatui::style::{Color, Modifier, Style};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::domain::event::{LogLevel, MemberStatus, WorkflowRunStatus};
use crate::domain::team::BackendKind;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ThemeVariant {
    Dark,
    Light,
}

#[derive(Clone, Copy, Debug)]
struct Palette {
    accent: Color,
    success: Color,
    warning: Color,
    error: Color,
    muted: Color,
    text: Color,
    emphasis: Color,
    user: Color,
    selection_text: Color,
}

fn palette() -> Palette {
    palette_for(theme_variant())
}

fn palette_for(variant: ThemeVariant) -> Palette {
    match variant {
        ThemeVariant::Dark => Palette {
            accent: Color::Rgb(34, 211, 238),
            success: Color::Rgb(74, 222, 128),
            warning: Color::Rgb(251, 191, 36),
            error: Color::Rgb(248, 113, 113),
            muted: Color::Rgb(161, 161, 170),
            text: Color::Rgb(212, 212, 216),
            emphasis: Color::Rgb(250, 250, 250),
            user: Color::Rgb(134, 239, 172),
            selection_text: Color::Black,
        },
        ThemeVariant::Light => Palette {
            accent: Color::Rgb(14, 116, 144),
            success: Color::Rgb(22, 101, 52),
            warning: Color::Rgb(146, 64, 14),
            error: Color::Rgb(185, 28, 28),
            muted: Color::Rgb(82, 82, 91),
            text: Color::Rgb(63, 63, 70),
            emphasis: Color::Rgb(24, 24, 27),
            user: Color::Rgb(22, 101, 52),
            selection_text: Color::White,
        },
    }
}

pub fn accent_color() -> Color {
    palette().accent
}

pub fn success_color() -> Color {
    palette().success
}

pub fn warning_color() -> Color {
    palette().warning
}

pub fn error_color() -> Color {
    palette().error
}

pub fn muted_color() -> Color {
    palette().muted
}

pub fn text_color() -> Color {
    palette().text
}

pub fn emphasis_color() -> Color {
    palette().emphasis
}

pub fn user_color() -> Color {
    palette().user
}

pub fn text() -> Style {
    Style::default().fg(text_color())
}

pub fn muted() -> Style {
    Style::default().fg(muted_color())
}

pub fn muted_italic() -> Style {
    muted().add_modifier(Modifier::ITALIC)
}

pub fn notice() -> Style {
    Style::default().fg(accent_color())
}

pub fn emphasis() -> Style {
    Style::default().fg(emphasis_color())
}

pub fn accent() -> Style {
    Style::default().fg(accent_color())
}

pub fn accent_bold() -> Style {
    accent().add_modifier(Modifier::BOLD)
}

pub fn success() -> Style {
    Style::default().fg(success_color())
}

pub fn success_bold() -> Style {
    success().add_modifier(Modifier::BOLD)
}

pub fn warning() -> Style {
    Style::default().fg(warning_color())
}

pub fn warning_bold() -> Style {
    warning().add_modifier(Modifier::BOLD)
}

pub fn error() -> Style {
    Style::default().fg(error_color())
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
        .fg(palette().selection_text)
        .bg(accent_color())
        .add_modifier(Modifier::BOLD)
}

/// Secondary selection for a focused cell inside an already-selected row.
pub fn selection_cell() -> Style {
    Style::default()
        .fg(palette().selection_text)
        .bg(warning_color())
        .add_modifier(Modifier::BOLD)
}

/// Lightweight focus used by dense editors. It keeps the terminal background
/// untouched and relies on a marker, color, and emphasis instead of a wide bar.
pub fn editor_focus() -> Style {
    accent_bold()
}

pub fn editor_field_focus() -> Style {
    warning_bold()
}

pub fn backend_color(backend: BackendKind) -> Color {
    backend_color_for(theme_variant(), backend)
}

fn backend_color_for(variant: ThemeVariant, backend: BackendKind) -> Color {
    match (variant, backend) {
        (ThemeVariant::Dark, BackendKind::Codex) => Color::Rgb(94, 234, 212),
        (ThemeVariant::Dark, BackendKind::Claude) => Color::Rgb(253, 186, 116),
        (ThemeVariant::Dark, BackendKind::Grok) => Color::Rgb(253, 224, 71),
        (ThemeVariant::Dark, BackendKind::Agy) => Color::Rgb(147, 197, 253),
        (ThemeVariant::Light, BackendKind::Codex) => Color::Rgb(15, 118, 110),
        (ThemeVariant::Light, BackendKind::Claude) => Color::Rgb(154, 52, 18),
        (ThemeVariant::Light, BackendKind::Grok) => Color::Rgb(133, 77, 14),
        (ThemeVariant::Light, BackendKind::Agy) => Color::Rgb(29, 78, 216),
    }
}

fn theme_variant() -> ThemeVariant {
    theme_variant_from(
        std::env::var("ASTERLINE_THEME").ok().as_deref(),
        std::env::var("COLORFGBG").ok().as_deref(),
    )
}

fn theme_variant_from(explicit: Option<&str>, colorfgbg: Option<&str>) -> ThemeVariant {
    match explicit
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("light") => return ThemeVariant::Light,
        Some("dark") => return ThemeVariant::Dark,
        _ => {}
    }

    // COLORFGBG conventionally ends with the ANSI background index. White
    // backgrounds are normally 7 or 15; other or missing values default to
    // the dark palette, which matches the common coding-terminal setup.
    let background = colorfgbg
        .and_then(|value| value.rsplit(';').next())
        .and_then(|value| value.parse::<u8>().ok());
    if matches!(background, Some(7 | 15)) {
        ThemeVariant::Light
    } else {
        ThemeVariant::Dark
    }
}

pub fn backend_bold(backend: BackendKind) -> Style {
    bold(backend_color(backend))
}

pub fn status_color(status: MemberStatus) -> Color {
    match status {
        MemberStatus::Running => warning_color(),
        MemberStatus::Failed => error_color(),
        MemberStatus::NeedsApproval => warning_color(),
        _ => muted_color(),
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
        WorkflowRunStatus::Running | WorkflowRunStatus::Verifying => warning_color(),
        WorkflowRunStatus::Done => success_color(),
        WorkflowRunStatus::Failed | WorkflowRunStatus::Blocked => error_color(),
        WorkflowRunStatus::Planned => muted_color(),
    }
}

pub fn log_color(level: LogLevel) -> Color {
    match level {
        LogLevel::Error => error_color(),
        LogLevel::Warn => warning_color(),
        LogLevel::Info => text_color(),
        LogLevel::Debug => muted_color(),
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

    #[test]
    fn editor_focus_uses_markers_and_emphasis_without_background_or_underline() {
        assert_eq!(editor_focus().bg, None);
        assert_eq!(editor_field_focus().bg, None);
        assert_eq!(editor_field_focus().fg, Some(warning_color()));
        assert!(editor_field_focus().add_modifier.contains(Modifier::BOLD));
        assert!(
            !editor_field_focus()
                .add_modifier
                .contains(Modifier::UNDERLINED)
        );
    }

    #[test]
    fn theme_variant_honors_override_then_colorfgbg() {
        assert_eq!(
            theme_variant_from(Some("light"), Some("15;0")),
            ThemeVariant::Light
        );
        assert_eq!(
            theme_variant_from(Some("dark"), Some("0;15")),
            ThemeVariant::Dark
        );
        assert_eq!(
            theme_variant_from(Some("auto"), Some("0;15")),
            ThemeVariant::Light
        );
        assert_eq!(theme_variant_from(None, Some("15;0")), ThemeVariant::Dark);
        assert_eq!(theme_variant_from(None, None), ThemeVariant::Dark);
    }

    #[test]
    fn backend_palettes_avoid_dark_blue_and_purple() {
        let dark = [
            backend_color_for(ThemeVariant::Dark, BackendKind::Codex),
            backend_color_for(ThemeVariant::Dark, BackendKind::Claude),
            backend_color_for(ThemeVariant::Dark, BackendKind::Grok),
            backend_color_for(ThemeVariant::Dark, BackendKind::Agy),
        ];
        let light = [
            backend_color_for(ThemeVariant::Light, BackendKind::Codex),
            backend_color_for(ThemeVariant::Light, BackendKind::Claude),
            backend_color_for(ThemeVariant::Light, BackendKind::Grok),
            backend_color_for(ThemeVariant::Light, BackendKind::Agy),
        ];
        assert_eq!(
            dark.len(),
            dark.iter().collect::<std::collections::HashSet<_>>().len()
        );
        assert_eq!(
            light.len(),
            light.iter().collect::<std::collections::HashSet<_>>().len()
        );
        assert_eq!(dark[3], Color::Rgb(147, 197, 253));
        assert_eq!(light[3], Color::Rgb(29, 78, 216));

        for color in dark {
            assert!(contrast_ratio(color, Color::Rgb(30, 30, 30)) >= 4.5);
        }
        for color in light {
            assert!(contrast_ratio(color, Color::White) >= 4.5);
        }
    }

    #[test]
    fn semantic_palettes_keep_text_contrast_on_dark_and_light_backgrounds() {
        for (variant, background) in [
            (ThemeVariant::Dark, Color::Rgb(30, 30, 30)),
            (ThemeVariant::Light, Color::White),
        ] {
            let palette = palette_for(variant);
            for color in [
                palette.accent,
                palette.success,
                palette.warning,
                palette.error,
                palette.muted,
                palette.text,
                palette.emphasis,
                palette.user,
            ] {
                assert!(
                    contrast_ratio(color, background) >= 4.5,
                    "{variant:?} color {color:?} lacks text contrast"
                );
            }
        }
    }

    fn contrast_ratio(first: Color, second: Color) -> f64 {
        let first = relative_luminance(first);
        let second = relative_luminance(second);
        let (lighter, darker) = if first >= second {
            (first, second)
        } else {
            (second, first)
        };
        (lighter + 0.05) / (darker + 0.05)
    }

    fn relative_luminance(color: Color) -> f64 {
        let Color::Rgb(red, green, blue) = color else {
            return if color == Color::White { 1.0 } else { 0.0 };
        };
        let linear = |component: u8| {
            let value = f64::from(component) / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(red) + 0.7152 * linear(green) + 0.0722 * linear(blue)
    }
}
