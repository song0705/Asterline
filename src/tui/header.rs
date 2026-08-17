//! The top header (title bar + member chips + rule) and the bottom footer
//! (search prompt, alerts, running status, run hint, or key hints).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::domain::event::MemberStatus;
use crate::tui::app_state::{AppState, MemberView, member_status_is_active};
use crate::tui::runs_view::run_footer_hint;
use crate::tui::status_indicator;
use crate::tui::theme;
use crate::tui::theme::{clip_width, display_width, truncate_width};

/// Header: `Asterline 0.2.9    …    workspace`, one chip per member, and a
/// thin rule that separates the header block from the conversation.
pub(crate) fn render_header(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let width = area.width as usize;
    // Line 1: product and version. Team names and the active mode are
    // metadata, not chrome.
    let brand = " Asterline";
    let version = format!(" {}", env!("CARGO_PKG_VERSION"));
    let workspace = state.workspace().to_string();
    let title_width = display_width(brand) + display_width(&version);
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
        Span::styled(brand, theme::accent_bold()),
        Span::styled(version, theme::muted()),
        Span::raw(" ".repeat(gap)),
        Span::styled(workspace, theme::muted()),
    ]);

    // Line 2: compact member chips. Runtime status belongs in the footer and
    // run views, leaving this roster focused on names and backends.
    let mut chips = vec![Span::raw(" ")];
    for (i, member) in state.members().iter().enumerate() {
        if i > 0 {
            chips.push(Span::raw("  "));
        }
        let name_style = if state.header_selected() == Some(i) {
            theme::selection()
        } else {
            theme::backend_bold_shaded(member.backend, state.member_color_index(&member.id))
        };
        chips.push(Span::styled(member.display_name.clone(), name_style));
        chips.push(Span::styled(
            format!(" · {}", member.backend.as_str()),
            theme::muted(),
        ));
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

pub(crate) fn render_footer(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    // A disconnected runtime is terminal for this TUI instance. Do not leave
    // stale run/verification hints suggesting cancellation is still available.
    if !state.runtime_available() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "runtime stopped · input disabled · Ctrl+C quit",
                theme::error(),
            ))),
            area,
        );
        return;
    }

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
                "● {} pending approval(s) · y agree · n deny",
                state.pending_approvals().len()
            ),
            theme::warning_bold(),
        ));
    }

    let active_members: Vec<&MemberView> = state
        .members()
        .iter()
        .filter(|member| member_status_is_active(member.status))
        .collect();
    let active = active_members.len();
    if active > 0 {
        if !parts.is_empty() {
            parts.push(Span::raw("   "));
        }
        let elapsed = active_members
            .iter()
            .filter_map(|member| state.member_elapsed_secs(&member.id))
            .max();
        let mut names: Vec<String> = active_members
            .iter()
            .take(3)
            .map(|member| {
                if member.status == MemberStatus::Running {
                    member.display_name.clone()
                } else {
                    format!(
                        "{} {}",
                        member.display_name,
                        theme::status_label(member.status)
                    )
                }
            })
            .collect();
        if active > names.len() {
            names.push(format!("+{}", active - names.len()));
        }
        let text = if active_members
            .iter()
            .all(|member| member.status == MemberStatus::Running)
        {
            status_indicator::running_footer_text(
                active,
                elapsed,
                &names,
                status_indicator::spinner(),
            )
        } else {
            status_indicator::active_footer_text(
                active,
                elapsed,
                &names,
                status_indicator::spinner(),
            )
        };
        parts.push(Span::styled(
            text.unwrap_or_default(),
            theme::warning_bold(),
        ));
        let queued = state.queued_prompt_count();
        if queued > 0 {
            parts.push(Span::raw("   "));
            parts.push(Span::styled(
                format!("{queued} queued · Esc send · Shift+← edit"),
                theme::warning_bold(),
            ));
        }
    } else if let Some((text, color)) = run_footer_hint(state) {
        if !parts.is_empty() {
            parts.push(Span::raw("   "));
        }
        parts.push(Span::styled(text, theme::bold(color)));
    } else if parts.is_empty() {
        // Idle: one short, faint key-hint line.
        parts.push(Span::styled(
            "@member first · Enter send · /help",
            theme::muted(),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(parts)), area);
}
