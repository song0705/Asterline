//! The top header (title bar + member chips + rule) and the bottom footer
//! (search prompt, alerts, running status, workflow hint, or key hints).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::domain::event::MemberStatus;
use crate::tui::app_state::{AppState, MemberView};
use crate::tui::status_indicator;
use crate::tui::theme;
use crate::tui::theme::{clip_width, display_width, truncate_width};
use crate::tui::workflow_view::workflow_footer_hint;

/// Header: `Asterline · team    …    workspace`, one chip per member, and a
/// thin rule that separates the header block from the conversation.
pub(crate) fn render_header(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let width = area.width as usize;

    // Line 1: title on the left, workspace path on the right.
    let title = format!(
        " Asterline · {} · mode:{}",
        state.team(),
        state.active_mode()
    );
    let workspace = state.workspace().to_string();
    let title_width = display_width(&title);
    let space = width.saturating_sub(title_width).saturating_sub(1);
    let workspace = if workspace.is_empty() || space < 8 {
        String::new()
    } else {
        clip_width(&workspace, space)
    };
    let gap = width
        .saturating_sub(title_width)
        .saturating_sub(display_width(&workspace))
        .saturating_sub(1);
    let title_line = Line::from(vec![
        Span::styled(title, theme::accent_bold()),
        Span::raw(" ".repeat(gap)),
        Span::styled(workspace, theme::muted()),
    ]);

    // Line 2: member chips.
    let mut chips = vec![Span::raw(" ")];
    for (i, member) in state.members().iter().enumerate() {
        if i > 0 {
            chips.push(Span::styled("  ·  ", theme::muted()));
        }
        let glyph = match member.status {
            MemberStatus::Running | MemberStatus::Queued => status_indicator::spinner(),
            MemberStatus::NeedsApproval => "⚠",
            MemberStatus::Failed => "✘",
            _ => "○",
        };
        let selected = state.header_selected() == Some(i);
        let (glyph_style, name_style) = if selected {
            (theme::selection(), theme::selection())
        } else {
            (
                ratatui::style::Style::default().fg(theme::status_color(member.status)),
                theme::backend_bold(member.backend),
            )
        };
        chips.push(Span::styled(format!("{glyph} "), glyph_style));
        chips.push(Span::styled(member.display_name.clone(), name_style));
        if let Some(profile) = chip_profile(member) {
            chips.push(Span::styled(format!(" ·{profile}"), theme::muted()));
        }
    }
    if state.members().is_empty() {
        chips.push(Span::styled("starting…", theme::muted()));
    }

    // Line 3: thin rule closing the header block.
    let rule = Line::from(Span::styled("─".repeat(width.max(1)), theme::muted()));

    frame.render_widget(
        Paragraph::new(vec![title_line, Line::from(chips), rule]),
        area,
    );
}

/// `model/effort` chip suffix; omitted when neither is configured.
fn chip_profile(member: &MemberView) -> Option<String> {
    match (member.model.as_deref(), member.effort) {
        (None, None) => None,
        (model, effort) => {
            let mut parts = Vec::new();
            if let Some(model) = model {
                parts.push(truncate_width(model, 16));
            }
            if let Some(effort) = effort {
                parts.push(effort.as_str().to_string());
            }
            Some(parts.join("/"))
        }
    }
}

pub(crate) fn render_footer(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    // Reverse history search (Ctrl+R) takes over the footer while active.
    if let Some((query, matched)) = state.history_search() {
        let mut spans = vec![
            Span::styled("(reverse-search) ", theme::bold(theme::accent_color())),
            Span::styled(format!("`{query}`"), theme::bold(theme::emphasis_color())),
            Span::styled(" → ", theme::muted()),
        ];
        match matched {
            Some(text) => spans.push(Span::styled(
                truncate_width(text, area.width as usize),
                theme::text(),
            )),
            None => spans.push(Span::styled(
                "no match (Esc to cancel)",
                theme::muted_italic(),
            )),
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }

    // Transcript find (`/find`) takes over the footer while active.
    if let Some((query, current, total)) = state.find() {
        let mut spans = vec![Span::styled(
            format!("find: \"{query}\" ({current}/{total})"),
            theme::accent(),
        )];
        if total == 0 {
            spans.push(Span::styled(" · no matches", theme::accent()));
        } else {
            spans.push(Span::styled(" · n/p jump · Esc clear", theme::accent()));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }

    let mut parts = Vec::new();
    if state.paused_routes() > 0 {
        parts.push(Span::styled(
            format!("● {} paused route(s) · /retry", state.paused_routes()),
            theme::warning_bold(),
        ));
    }
    if !state.pending_approvals().is_empty() {
        if !parts.is_empty() {
            parts.push(Span::raw("   "));
        }
        parts.push(Span::styled(
            format!(
                "● {} pending approval(s) · /approve",
                state.pending_approvals().len()
            ),
            theme::warning_bold(),
        ));
    }

    let running_members: Vec<&MemberView> = state
        .members()
        .iter()
        .filter(|member| member.status == MemberStatus::Running)
        .collect();
    let running = running_members.len();
    if running > 0 {
        if !parts.is_empty() {
            parts.push(Span::raw("   "));
        }
        let elapsed = running_members
            .iter()
            .filter_map(|member| state.member_elapsed_secs(&member.id))
            .max();
        let mut names: Vec<String> = running_members
            .iter()
            .take(3)
            .map(|member| member.display_name.clone())
            .collect();
        if running > names.len() {
            names.push(format!("+{}", running - names.len()));
        }
        parts.push(Span::styled(
            status_indicator::running_footer_text(
                running,
                elapsed,
                &names,
                status_indicator::spinner(),
            )
            .unwrap_or_default(),
            theme::warning_bold(),
        ));
    } else if let Some((text, color)) = workflow_footer_hint(state) {
        if !parts.is_empty() {
            parts.push(Span::raw("   "));
        }
        parts.push(Span::styled(text, theme::bold(color)));
    } else if parts.is_empty() {
        // Idle: one short, faint key-hint line.
        parts.push(Span::styled(
            "@member first · Enter send · Ctrl+O tools · /skills · /help",
            theme::muted(),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(parts)), area);
}
