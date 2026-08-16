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

use crate::domain::event::{LogLevel, MemberStatus, RunStatus};
use crate::domain::mode::TerminalMode;
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
    chat_selection_bg: Color,
    chat_selection_fg: Color,
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
            chat_selection_bg: Color::Rgb(63, 63, 70),
            chat_selection_fg: Color::Rgb(244, 244, 245),
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
            chat_selection_bg: Color::Rgb(212, 212, 216),
            chat_selection_fg: Color::Rgb(24, 24, 27),
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

pub fn diff_add() -> Style {
    Style::default().fg(diff_add_fg()).bg(diff_add_bg())
}

pub fn diff_delete() -> Style {
    Style::default().fg(diff_delete_fg()).bg(diff_delete_bg())
}

/// Path / header signs: same green/red ink, no fill. The file-changes
/// summary row is not a hunk and should not look like one.
pub fn diff_add_text() -> Style {
    Style::default().fg(diff_add_fg())
}

pub fn diff_delete_text() -> Style {
    Style::default().fg(diff_delete_fg())
}

fn diff_add_fg() -> Color {
    match theme_variant() {
        ThemeVariant::Dark => Color::Rgb(158, 206, 106),
        ThemeVariant::Light => Color::Rgb(22, 101, 52),
    }
}

fn diff_add_bg() -> Color {
    match theme_variant() {
        ThemeVariant::Dark => Color::Rgb(6, 56, 6),
        ThemeVariant::Light => Color::Rgb(218, 251, 225),
    }
}

fn diff_delete_fg() -> Color {
    match theme_variant() {
        ThemeVariant::Dark => Color::Rgb(247, 118, 142),
        ThemeVariant::Light => Color::Rgb(153, 27, 27),
    }
}

fn diff_delete_bg() -> Color {
    match theme_variant() {
        ThemeVariant::Dark => Color::Rgb(66, 14, 20),
        ThemeVariant::Light => Color::Rgb(255, 235, 233),
    }
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

/// Drag-select highlight in the chat column. Neutral zinc, not the accent
/// cyan used by interactive chrome.
pub fn chat_selection() -> Style {
    let palette = palette();
    Style::default()
        .fg(palette.chat_selection_fg)
        .bg(palette.chat_selection_bg)
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

/// Stable colors for collaboration modes. These are semantic identities, not
/// status colors or backend identities.
pub fn mode_color(mode: TerminalMode) -> Color {
    match (theme_variant(), mode) {
        (ThemeVariant::Dark, TerminalMode::Normal) => Color::Rgb(212, 212, 216),
        (ThemeVariant::Dark, TerminalMode::Review) => Color::Rgb(192, 132, 252),
        (ThemeVariant::Dark, TerminalMode::Plan) => Color::Rgb(251, 146, 60),
        (ThemeVariant::Dark, TerminalMode::Brainstorm) => Color::Rgb(56, 189, 248),
        (ThemeVariant::Dark, TerminalMode::Team) => Color::Rgb(74, 222, 128),
        (ThemeVariant::Light, TerminalMode::Normal) => Color::Rgb(63, 63, 70),
        (ThemeVariant::Light, TerminalMode::Review) => Color::Rgb(126, 34, 206),
        (ThemeVariant::Light, TerminalMode::Plan) => Color::Rgb(194, 65, 12),
        (ThemeVariant::Light, TerminalMode::Brainstorm) => Color::Rgb(3, 105, 161),
        (ThemeVariant::Light, TerminalMode::Team) => Color::Rgb(21, 128, 61),
    }
}

pub fn backend_color(backend: BackendKind) -> Color {
    backend_color_shaded(backend, 0)
}

/// Same-backend teammates share a hue. Later roster siblings get a darker or
/// lighter step so two Codex (or two Grok) members stay distinguishable.
pub fn backend_color_shaded(backend: BackendKind, same_backend_index: usize) -> Color {
    shade_backend_color(theme_variant(), backend, same_backend_index)
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
    backend_bold_shaded(backend, 0)
}

pub fn backend_bold_shaded(backend: BackendKind, same_backend_index: usize) -> Style {
    bold(backend_color_shaded(backend, same_backend_index))
}

fn shade_backend_color(
    variant: ThemeVariant,
    backend: BackendKind,
    same_backend_index: usize,
) -> Color {
    let Color::Rgb(red, green, blue) = backend_color_for(variant, backend) else {
        return backend_color_for(variant, backend);
    };
    let factor = match (variant, same_backend_index % 3) {
        (_, 0) => 1.0,
        (ThemeVariant::Dark, 1) => 0.78,
        (ThemeVariant::Dark, _) => 0.60,
        (ThemeVariant::Light, 1) => 1.38,
        (ThemeVariant::Light, _) => 0.78,
    };
    Color::Rgb(
        scale_channel(red, factor),
        scale_channel(green, factor),
        scale_channel(blue, factor),
    )
}

fn scale_channel(channel: u8, factor: f32) -> u8 {
    (f32::from(channel) * factor).round().clamp(0.0, 255.0) as u8
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

pub fn run_status_color(status: RunStatus) -> Color {
    match status {
        RunStatus::Running | RunStatus::Verifying => warning_color(),
        RunStatus::Done => success_color(),
        RunStatus::Failed | RunStatus::Blocked => error_color(),
        RunStatus::Planned => muted_color(),
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

/// Extract the substring covering display columns `[start, end)`.
pub fn slice_display_cols(text: &str, start: usize, end: usize) -> String {
    let end = end.max(start);
    let mut out = String::new();
    let mut col = 0;
    for ch in text.chars() {
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if col >= end {
            break;
        }
        if col + width > start {
            out.push(ch);
        }
        col += width;
    }
    out
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
    fn chat_selection_uses_neutral_zinc_not_accent_cyan() {
        let dark = palette_for(ThemeVariant::Dark);
        let light = palette_for(ThemeVariant::Light);
        assert_eq!(dark.chat_selection_bg, Color::Rgb(63, 63, 70));
        assert_eq!(light.chat_selection_bg, Color::Rgb(212, 212, 216));
        assert_ne!(dark.chat_selection_bg, dark.accent);
        assert_ne!(light.chat_selection_bg, light.accent);
        assert_eq!(chat_selection().bg, Some(dark.chat_selection_bg));
        assert_eq!(chat_selection().fg, Some(dark.chat_selection_fg));
    }

    #[test]
    fn slice_display_cols_extracts_a_column_range() {
        assert_eq!(slice_display_cols("hello world", 0, 5), "hello");
        assert_eq!(slice_display_cols("hello world", 6, 11), "world");
    }

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
    fn same_backend_shades_differ_by_lightness() {
        for backend in [
            BackendKind::Codex,
            BackendKind::Claude,
            BackendKind::Grok,
            BackendKind::Agy,
        ] {
            let first = shade_backend_color(ThemeVariant::Dark, backend, 0);
            let second = shade_backend_color(ThemeVariant::Dark, backend, 1);
            assert_ne!(first, second, "{backend:?} shades must differ");
            assert!(
                contrast_ratio(second, Color::Rgb(30, 30, 30)) >= 4.5,
                "{backend:?} second shade {second:?} lacks contrast"
            );
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
