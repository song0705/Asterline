//! Renders the chat-first UI: the header block, the single scrolling
//! conversation column, the bottom composer, a footer hint line, and an
//! optional drawer overlay. Chat-block rendering lives here; the header,
//! drawers, and workflow presentation live in sibling modules.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph};

use crate::domain::event::{ChatItem, MemberStatus};
use crate::domain::team::{DefaultTarget, MemberId};
use crate::tui::app_state::AppState;
use crate::tui::completion::Completion;
use crate::tui::drawer_view::render_drawer;
use crate::tui::header::{render_footer, render_header};
use crate::tui::markdown;
use crate::tui::status_indicator;
use crate::tui::theme;
use crate::tui::theme::truncate_width;

// Paint the rail as a terminal-cell background instead of a font glyph. This
// fills the complete cell rectangle regardless of font ascent, descent, or
// line-height metrics, so adjacent rows meet without visible seams.
fn chat_rail(color: Color) -> Span<'static> {
    Span::styled(" ", Style::default().bg(color))
}

fn member_rail_color(state: &AppState, member: &MemberId) -> Color {
    state
        .members()
        .iter()
        .find(|candidate| &candidate.id == member)
        .map(|candidate| candidate.backend)
        .or_else(|| {
            state.chat().iter().rev().find_map(|item| match item {
                ChatItem::Agent {
                    member: candidate,
                    backend,
                    ..
                } if candidate == member => Some(*backend),
                _ => None,
            })
        })
        .map(theme::backend_color)
        .unwrap_or_else(theme::muted_color)
}

pub fn render(frame: &mut Frame<'_>, state: &AppState) {
    // The composer grows with its content up to a cap, like a real textarea.
    const MAX_COMPOSER_ROWS: u16 = 8;
    let composer_avail = frame.area().width.saturating_sub(2) as usize;
    let composer_rows =
        (state.composer().visual_line_count(composer_avail) as u16).clamp(1, MAX_COMPOSER_ROWS);
    let composer_height = composer_rows + 2; // borders
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
    render_chat(frame, chunks[1], state);
    render_composer(frame, chunks[2], state);
    if let Some(completion) = completion {
        render_popup(frame, chunks[3], &completion, state.popup_selected());
    } else {
        render_footer(frame, chunks[3], state);
    }

    if let Some(drawer) = state.drawer() {
        render_drawer(frame, frame.area(), state, &drawer);
    }
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
            let selected_name_style = theme::selection();
            let selected_text_style = theme::selection();
            let name_style = if is_selected {
                selected_name_style
            } else {
                theme::accent()
            };
            let marker_style = if is_selected {
                selected_name_style
            } else {
                Style::default()
            };
            let marker = if is_selected { "› " } else { "  " };
            let mut used_width = theme::display_width(marker) + theme::display_width(name);
            let mut spans = vec![
                Span::styled(marker, marker_style),
                Span::styled(name.to_string(), name_style),
            ];
            if let Some(description) = description {
                let padding = name_width.saturating_sub(theme::display_width(name)) + 2;
                used_width += padding + theme::display_width(description);
                let padding_style = if is_selected {
                    selected_text_style
                } else {
                    Style::default()
                };
                let description_style = if is_selected {
                    selected_text_style
                } else {
                    theme::muted()
                };
                spans.push(Span::styled(" ".repeat(padding), padding_style));
                spans.push(Span::styled(description.to_string(), description_style));
            }
            if is_selected {
                spans.push(Span::styled(
                    " ".repeat((area.width as usize).saturating_sub(used_width)),
                    selected_text_style,
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

fn render_chat(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let block = Block::default().padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let width = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();

    if state.chat().is_empty() {
        lines.push(Line::raw(""));
        lines.extend(quick_start_lines(state));
        lines.push(Line::raw(""));
    }

    render_chat_history(state, width, &mut lines);

    // Append live activity lines for members that are currently busy.
    let active_members: Vec<_> = state
        .members()
        .iter()
        .filter(|m| m.status != MemberStatus::Idle)
        .collect();

    let spin_char = status_indicator::spinner();
    for member in active_members {
        // A member that hasn't started its message yet gets a placeholder
        // header; one that has only surfaces its live reasoning.
        let show_placeholder = !state.has_active_message(&member.id);
        let reasoning = state
            .active_reasoning()
            .get(&member.id)
            .map(String::as_str)
            .filter(|s| !s.is_empty());
        if !show_placeholder && reasoning.is_none() {
            continue;
        }
        if show_placeholder {
            lines.push(agent_header_line(&member.display_name, member.backend));
        }
        let line_text = status_indicator::member_activity_text(
            member.status,
            reasoning,
            state.member_elapsed_secs(&member.id),
            spin_char,
            Some(&member_runtime_profile(member)),
        );
        for wrapped in markdown::wrap(&line_text, width.saturating_sub(2).max(1)) {
            lines.push(Line::from(vec![
                chat_rail(theme::backend_color(member.backend)),
                Span::raw(" "),
                Span::styled(wrapped, theme::muted_italic()),
            ]));
        }
        if show_placeholder {
            lines.push(Line::raw(""));
        }
    }

    let height = inner.height as usize;
    let total = lines.len();
    let max_start = total.saturating_sub(height);
    let start = max_start.saturating_sub(state.scroll());
    let visible: Vec<Line> = lines.into_iter().skip(start).take(height).collect();

    frame.render_widget(Paragraph::new(visible), inner);
}

fn render_chat_history(state: &AppState, width: usize, out: &mut Vec<Line<'static>>) {
    let items = state.chat();
    let mut saw_work_activity = false;
    for (i, item) in items.iter().enumerate() {
        if matches!(item, ChatItem::User { .. }) && saw_work_activity {
            render_turn_separator(width, out);
            saw_work_activity = false;
        }
        if is_work_activity(item) {
            saw_work_activity = true;
        }
        let before = out.len();
        let previous_sender = if i == 0 {
            None
        } else {
            items.get(i - 1).and_then(item_sender)
        };
        let show_sender_header = item_sender(item) != previous_sender;
        let is_find_current = state.find_current_chat_index() == Some(i);
        render_item(item, width, state, out, show_sender_header);
        if is_find_current && let Some(line) = out.get_mut(before) {
            // Marker in the gutter for the current `/find` match.
            let mut spans = vec![Span::styled("»", theme::selection())];
            spans.append(&mut line.spans);
            line.spans = spans;
        }
        // Keep one member's answer, tools, routes, diffs, and errors on the
        // same uninterrupted visual rail. Separate unrelated blocks.
        if out.len() > before {
            let next = items.get(i + 1);
            let grouped = (is_compact(item) && next.is_some_and(is_compact))
                || item_sender(item)
                    .is_some_and(|sender| next.and_then(item_sender).as_ref() == Some(&sender))
                || next.is_some_and(|next| same_member_thread(item, next));
            if !grouped {
                out.push(Line::raw(""));
            }
        }
    }
    if saw_work_activity && state.running_count() == 0 {
        render_turn_separator(width, out);
    }
}

fn is_work_activity(item: &ChatItem) -> bool {
    matches!(
        item,
        ChatItem::Tool { ok: Some(_), .. } | ChatItem::Diff { .. } | ChatItem::Route { .. }
    )
}

/// Compact items render as one or two lines and cluster without blank lines.
fn is_compact(item: &ChatItem) -> bool {
    matches!(
        item,
        ChatItem::Tool { .. }
            | ChatItem::Diff { .. }
            | ChatItem::Notice { .. }
            | ChatItem::Verdict { .. }
    )
}

fn same_member_thread(current: &ChatItem, next: &ChatItem) -> bool {
    item_member(current).is_some_and(|member| item_member(next) == Some(member))
}

fn item_member(item: &ChatItem) -> Option<&MemberId> {
    match item {
        ChatItem::Agent { member, .. }
        | ChatItem::Tool { member, .. }
        | ChatItem::Diff { member, .. } => Some(member),
        ChatItem::Route { from, .. } => Some(from),
        ChatItem::Error { member, .. } => member.as_ref(),
        ChatItem::Verdict { member, .. } => Some(member),
        ChatItem::User { .. } | ChatItem::Notice { .. } => None,
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
        ("/plan <goal>".to_string(), "run a tracked team workflow"),
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
) -> Line<'static> {
    Line::from(vec![
        Span::styled("◆ ", theme::backend_bold(backend)),
        Span::styled(display_name.to_string(), theme::backend_bold(backend)),
        Span::styled(format!("  · {}", backend.as_str()), theme::muted()),
    ])
}

fn user_header_line() -> Line<'static> {
    Line::from(vec![
        Span::styled("◆ ", theme::bold(theme::user_color())),
        Span::styled("You", theme::bold(theme::user_color())),
    ])
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
        ChatItem::Tool { .. }
        | ChatItem::Diff { .. }
        | ChatItem::Route { .. }
        | ChatItem::Notice { .. }
        | ChatItem::Error { .. }
        | ChatItem::Verdict { .. } => None,
    }
}

fn render_item(
    item: &ChatItem,
    width: usize,
    state: &AppState,
    out: &mut Vec<Line<'static>>,
    show_sender_header: bool,
) {
    match item {
        ChatItem::User { body } => {
            if show_sender_header {
                out.push(user_header_line());
            }
            for line in markdown::wrap(body, width.saturating_sub(2).max(1)) {
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
                out.push(agent_header_line(display_name, *backend));
            }
            for line in markdown::render(text, width.saturating_sub(2).max(1)) {
                let mut spans = vec![chat_rail(theme::backend_color(*backend)), Span::raw(" ")];
                spans.extend(line.spans);
                out.push(Line::from(spans));
            }
        }
        ChatItem::Tool {
            member,
            name,
            summary,
            detail,
            ok,
        } => {
            let (marker, marker_color, text_style) = match ok {
                None => (
                    status_indicator::spinner(),
                    theme::warning_color(),
                    theme::emphasis(),
                ),
                Some(true) => ("✓", theme::success_color(), theme::text()),
                Some(false) => ("✕", theme::error_color(), theme::error()),
            };
            let command = tool_display_text(name, summary);
            let command_width = width.saturating_sub(6).max(12);
            let rail_color = member_rail_color(state, member);
            out.push(Line::from(vec![
                chat_rail(rail_color),
                Span::raw("   "),
                Span::styled(format!("{marker} "), theme::bold(marker_color)),
                Span::styled(truncate_width(&command, command_width), text_style),
            ]));
            if !detail.trim().is_empty() {
                let detail_style = if *ok == Some(false) {
                    theme::error()
                } else {
                    theme::muted()
                };
                let max_lines = if state.tools_expanded() {
                    usize::MAX
                } else if *ok == Some(false) {
                    20
                } else {
                    1
                };
                let wrapped = markdown::wrap(detail.trim(), width.saturating_sub(8).max(1));
                let clipped = wrapped.len() > max_lines;
                for (idx, line) in wrapped.into_iter().take(max_lines).enumerate() {
                    out.push(Line::from(vec![
                        chat_rail(rail_color),
                        Span::raw(if idx == 0 { "     ↳ " } else { "       " }),
                        Span::styled(line, detail_style),
                    ]));
                }
                if clipped && !state.tools_expanded() {
                    out.push(Line::from(vec![
                        chat_rail(rail_color),
                        Span::styled("       … Ctrl+O expand tool output", theme::muted_italic()),
                    ]));
                }
            }
        }
        ChatItem::Diff { member, files } => {
            let rail_color = member_rail_color(state, member);
            out.push(Line::from(vec![
                chat_rail(rail_color),
                Span::styled("   ✎ file changes", theme::accent_bold()),
            ]));
            for (path, kind) in files {
                let (sign, color) = match kind.as_str() {
                    "add" => ("+", theme::success_color()),
                    "delete" => ("-", theme::error_color()),
                    _ => ("~", theme::warning_color()),
                };
                let shown = truncate_width(path, width.saturating_sub(8).max(10));
                out.push(Line::from(vec![
                    chat_rail(rail_color),
                    Span::styled(format!("     {sign} "), Style::default().fg(color)),
                    Span::styled(shown, Style::default().fg(color)),
                ]));
            }
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

fn tool_display_text(name: &str, summary: &str) -> String {
    let summary = summary.trim();
    if summary.is_empty() || summary == name {
        name.to_string()
    } else {
        format!("{name}  {summary}")
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
    let block = Block::default()
        .title(title_text)
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(border_color));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = inner.height as usize;
    if rows == 0 {
        return;
    }
    let avail = (inner.width as usize).saturating_sub(2); // "> " / "  " gutter

    // Visual lines with wrapping so long input is fully visible (no horizontal
    // clipping). The cursor maps directly to a screen cell.
    let (visual_lines, cursor_row, cursor_col) = state.composer().visual_lines_with_cursor(avail);

    // Vertical scroll so the cursor's visual line stays visible.
    let top = if cursor_row >= rows {
        cursor_row - rows + 1
    } else {
        0
    };

    let mut out_lines: Vec<Line> = Vec::new();
    let mut cursor_screen: Option<(u16, u16)> = None;
    for (offset, row) in (top..top + rows).enumerate() {
        let Some(line) = visual_lines.get(row) else {
            break;
        };
        let prefix = if row == 0 { "> " } else { "  " };
        let (shown, cursor_width) = if row == cursor_row {
            (line.clone(), cursor_col)
        } else {
            (line.clone(), 0)
        };
        out_lines.push(Line::from(vec![
            Span::styled(prefix.to_string(), Style::default().fg(border_color)),
            Span::raw(shown),
        ]));
        if row == cursor_row {
            cursor_screen = Some((inner.x + 2 + cursor_width as u16, inner.y + offset as u16));
        }
    }
    frame.render_widget(Paragraph::new(out_lines), inner);

    if state.drawer().is_none()
        && let Some((col, row)) = cursor_screen
    {
        frame.set_cursor_position((col, row));
    }
}

fn member_runtime_profile(member: &crate::tui::app_state::MemberView) -> String {
    format!(
        "model: {} • effort: {}",
        member.model.as_deref().unwrap_or("default"),
        member
            .effort
            .map(|effort| effort.as_str())
            .unwrap_or("default")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::event::{
        MemberStatus, RuntimeEvent, WorkflowRunEventSummary, WorkflowRunId, WorkflowRunStatus,
        WorkflowRunSummary, WorkflowStepStatus, WorkflowStepSummary, WorkflowVerification,
    };
    use crate::domain::team::{
        BackendKind, DefaultTarget, Effort, MemberId, PermissionMode, SandboxPolicy, SessionPolicy,
    };
    use crate::tui::drawers::Drawer;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn member_summary(
        id: &str,
        display_name: &str,
        backend: BackendKind,
        role: &str,
        status: MemberStatus,
    ) -> crate::domain::event::MemberSummary {
        crate::domain::event::MemberSummary {
            id: MemberId::new(id),
            display_name: display_name.to_string(),
            backend,
            role: role.to_string(),
            status,
            session: None,
            cwd: String::new(),
            model: None,
            effort: None,
            sandbox: SandboxPolicy::ReadOnly,
            permission_mode: Some(PermissionMode::Default),
            session_policy: SessionPolicy::Resume,
        }
    }

    fn plain_text(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    fn is_separator_text(text: &str) -> bool {
        let trimmed = text.trim();
        !trimmed.is_empty() && trimmed.chars().all(|ch| ch == '─')
    }

    #[test]
    fn fmt_elapsed_compact_scales_units() {
        assert_eq!(status_indicator::fmt_elapsed_compact(8), "8s");
        assert_eq!(status_indicator::fmt_elapsed_compact(64), "1m 04s");
        assert_eq!(status_indicator::fmt_elapsed_compact(3723), "1h 02m 03s");
    }

    #[test]
    fn renders_empty_state_quick_start() {
        let mut state = AppState::new(Vec::new());
        state.apply(RuntimeEvent::Ready {
            team: "default-mixed".to_string(),
            workspace: "/Users/me/proj".to_string(),
            default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
            workflow_runs: Vec::new(),
            members: vec![member_summary(
                "builder",
                "Builder",
                BackendKind::Codex,
                "implementation",
                MemberStatus::Idle,
            )],
        });

        let mut terminal = Terminal::new(TestBackend::new(96, 16)).unwrap();
        terminal.draw(|frame| render(frame, &state)).unwrap();
        let view = format!("{}", terminal.backend());
        eprintln!("\n{view}");

        assert!(view.contains("Members:"));
        assert!(view.contains("builder (codex, implementation)"));
        assert!(view.contains("@builder <message>"));
        assert!(view.contains("/plan <goal>"));
        assert!(view.contains("/help"));
    }

    #[test]
    fn renders_a_clean_layout_snapshot() {
        let mut state = AppState::new(Vec::new());
        state.apply(RuntimeEvent::Ready {
            team: "default-mixed".to_string(),
            workspace: "/Users/me/proj".to_string(),
            default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
            workflow_runs: Vec::new(),
            members: vec![
                member_summary(
                    "builder",
                    "Builder",
                    BackendKind::Codex,
                    "implementation",
                    MemberStatus::Running,
                ),
                member_summary(
                    "reviewer",
                    "Reviewer",
                    BackendKind::Claude,
                    "review",
                    MemberStatus::Idle,
                ),
            ],
        });
        state.apply(RuntimeEvent::Notice("welcome to Asterline".to_string()));
        state.apply(RuntimeEvent::Route {
            turn: crate::domain::event::TurnId(1),
            from: MemberId::new("builder"),
            to: vec!["reviewer".to_string()],
            body: "please review the parser".to_string(),
        });

        let mut terminal = Terminal::new(TestBackend::new(90, 16)).unwrap();
        terminal.draw(|frame| render(frame, &state)).unwrap();
        let view = format!("{}", terminal.backend());
        eprintln!("\n{view}");

        assert!(view.contains("Asterline"));
        assert!(view.contains("Builder"));
        assert!(view.contains("builder → reviewer"));
        // The running member surfaces a working indicator + interrupt hint.
        assert!(view.contains("Working"));
        assert!(view.contains("interrupt"));
        // The composer is open (top/bottom rules only) — no enclosing box or
        // rounded corners around the conversation or input.
        assert!(!view.contains('╭'));
    }

    #[test]
    fn header_clips_workspace_by_display_width() {
        let mut state = AppState::new(Vec::new());
        state.apply(RuntimeEvent::Ready {
            team: "t".to_string(),
            workspace: "/Users/我/很长的项目路径名称超级超级长/子目录".to_string(),
            default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
            workflow_runs: Vec::new(),
            members: vec![member_summary(
                "builder",
                "Builder",
                BackendKind::Codex,
                "impl",
                MemberStatus::Idle,
            )],
        });

        // Narrow terminal: the CJK path must clip by display width, not chars.
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        terminal.draw(|frame| render(frame, &state)).unwrap();
        let view = format!("{}", terminal.backend());
        eprintln!("\n{view}");

        assert!(view.contains("Asterline · t"));
        assert!(view.contains('…'));
    }

    #[test]
    fn renders_completion_popup() {
        let mut state = AppState::new(Vec::new());
        state.apply(RuntimeEvent::Ready {
            team: "t".to_string(),
            workspace: ".".to_string(),
            default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
            workflow_runs: Vec::new(),
            members: vec![member_summary(
                "builder",
                "Builder",
                BackendKind::Codex,
                "impl",
                MemberStatus::Idle,
            )],
        });
        for ch in "/a".chars() {
            state.insert_char(ch);
        }

        let mut terminal = Terminal::new(TestBackend::new(70, 14)).unwrap();
        terminal.draw(|frame| render(frame, &state)).unwrap();
        let view = format!("{}", terminal.backend());
        eprintln!("\n{view}");

        assert!(view.contains("/ask"));
        assert!(view.contains("/all"));
        assert!(!view.contains("╭"));
        assert!(!view.contains("@member to send"));
        assert!(view.contains("› /ask      send to one member"));
    }

    #[test]
    fn running_status_shows_model_and_effort() {
        let mut builder = member_summary(
            "builder",
            "Builder",
            BackendKind::Codex,
            "impl",
            MemberStatus::Running,
        );
        builder.model = Some("gpt-5-codex".to_string());
        builder.effort = Some(Effort::High);
        let mut state = AppState::new(Vec::new());
        state.apply(RuntimeEvent::Ready {
            team: "t".to_string(),
            workspace: String::new(),
            default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
            workflow_runs: Vec::new(),
            members: vec![builder],
        });

        let mut terminal = Terminal::new(TestBackend::new(120, 12)).unwrap();
        terminal.draw(|frame| render(frame, &state)).unwrap();
        let view = format!("{}", terminal.backend());
        eprintln!("\n{view}");

        // The activity line spells the profile out; the header chip abbreviates.
        assert!(view.contains("model: gpt-5-codex"));
        assert!(view.contains("effort: high"));
        assert!(view.contains("·gpt-5-codex/high"));
    }

    #[test]
    fn pure_conversation_does_not_show_work_separator() {
        let state = AppState::new(vec![
            ChatItem::User {
                body: "explain this function".to_string(),
            },
            ChatItem::Agent {
                member: MemberId::new("builder"),
                display_name: "Builder".to_string(),
                backend: BackendKind::Codex,
                text: "It parses the request.".to_string(),
            },
        ]);
        let mut lines = Vec::new();

        render_chat_history(&state, 40, &mut lines);

        let text = plain_text(&lines);
        assert!(!text.iter().any(|line| is_separator_text(line)));
        assert!(text.iter().any(|line| line == "◆ You"));
        assert!(
            text.iter()
                .any(|line| line.contains("explain this function"))
        );
    }

    #[test]
    fn consecutive_agent_messages_suppress_repeated_header() {
        let state = AppState::new(vec![
            ChatItem::Agent {
                member: MemberId::new("builder"),
                display_name: "Builder".to_string(),
                backend: BackendKind::Codex,
                text: "first reply".to_string(),
            },
            ChatItem::Agent {
                member: MemberId::new("builder"),
                display_name: "Builder".to_string(),
                backend: BackendKind::Codex,
                text: "second reply".to_string(),
            },
            ChatItem::Agent {
                member: MemberId::new("reviewer"),
                display_name: "Reviewer".to_string(),
                backend: BackendKind::Claude,
                text: "review reply".to_string(),
            },
        ]);
        let mut lines = Vec::new();

        render_chat_history(&state, 60, &mut lines);

        let text = plain_text(&lines);
        let builder_headers = text
            .iter()
            .filter(|line| line.contains("Builder") && line.contains("codex"))
            .count();
        let reviewer_headers = text
            .iter()
            .filter(|line| line.contains("Reviewer") && line.contains("claude"))
            .count();
        assert_eq!(builder_headers, 1);
        assert_eq!(reviewer_headers, 1);
        assert!(text.iter().any(|line| line.contains("first reply")));
        assert!(text.iter().any(|line| line.contains("second reply")));
        let first = text
            .iter()
            .position(|line| line.contains("first reply"))
            .unwrap();
        assert!(text[first + 1].contains("second reply"));
    }

    #[test]
    fn member_activity_uses_one_full_height_unbroken_rail() {
        let member = MemberId::new("builder");
        let state = AppState::new(vec![
            ChatItem::Agent {
                member: member.clone(),
                display_name: "Builder".to_string(),
                backend: BackendKind::Codex,
                text: "checking now".to_string(),
            },
            ChatItem::Tool {
                member: member.clone(),
                name: "shell".to_string(),
                summary: "cargo test".to_string(),
                detail: "test result: ok".to_string(),
                ok: Some(true),
            },
            ChatItem::Diff {
                member: member.clone(),
                files: vec![("src/lib.rs".to_string(), "modify".to_string())],
            },
            ChatItem::Error {
                member: Some(member),
                message: "follow-up failed".to_string(),
            },
        ]);
        let mut lines = Vec::new();

        render_chat_history(&state, 70, &mut lines);

        let text = plain_text(&lines);
        let start = text
            .iter()
            .position(|line| line.contains("checking now"))
            .unwrap();
        let end = text
            .iter()
            .position(|line| line.contains("follow-up failed"))
            .unwrap();
        assert!(text[start..=end].iter().all(|line| !line.trim().is_empty()));
        assert!(lines[start..=end].iter().all(|line| {
            line.spans.first().is_some_and(|span| {
                span.content.as_ref() == " "
                    && span.style.bg == Some(theme::backend_color(BackendKind::Codex))
            })
        }));

        let rail_lines = lines[start..=end].to_vec();
        let height = u16::try_from(rail_lines.len()).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(70, height)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(Paragraph::new(rail_lines.clone()), frame.area());
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        for y in 0..height {
            assert_eq!(
                buffer.cell((0, y)).unwrap().bg,
                theme::backend_color(BackendKind::Codex),
                "rail cell at row {y} must have a full-cell background"
            );
        }
    }

    #[test]
    fn completed_work_turn_gets_separator_before_next_user_message() {
        use crate::domain::event::TurnId;

        let mut state = AppState::new(Vec::new());
        state.apply(RuntimeEvent::Ready {
            team: "t".to_string(),
            workspace: String::new(),
            default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
            workflow_runs: Vec::new(),
            members: vec![member_summary(
                "builder",
                "Builder",
                BackendKind::Codex,
                "impl",
                MemberStatus::Idle,
            )],
        });
        state.apply(RuntimeEvent::UserMessage {
            turn: TurnId(1),
            targets: vec![MemberId::new("builder")],
            body: "run tests".to_string(),
        });
        state.apply(RuntimeEvent::ToolStarted {
            member: MemberId::new("builder"),
            tool_id: "t1".to_string(),
            name: "shell".to_string(),
            summary: "cargo test".to_string(),
        });
        state.apply(RuntimeEvent::ToolCompleted {
            member: MemberId::new("builder"),
            tool_id: "t1".to_string(),
            ok: true,
            output: "test result: ok".to_string(),
        });
        state.apply(RuntimeEvent::UserMessage {
            turn: TurnId(2),
            targets: vec![MemberId::new("builder")],
            body: "now summarize".to_string(),
        });
        let mut lines = Vec::new();

        render_chat_history(&state, 40, &mut lines);

        let text = plain_text(&lines);
        let separators: Vec<_> = text
            .iter()
            .enumerate()
            .filter(|(_, line)| is_separator_text(line))
            .collect();
        assert_eq!(separators.len(), 1);
        let separator_index = separators[0].0;
        assert!(
            text[..separator_index]
                .iter()
                .any(|line| line.contains("shell"))
        );
        assert!(
            text[separator_index + 1..]
                .iter()
                .any(|line| line.contains("now summarize"))
        );
    }

    #[test]
    fn consecutive_tool_lines_stay_grouped() {
        use crate::domain::event::TurnId;

        let mut state = AppState::new(Vec::new());
        state.apply(RuntimeEvent::UserMessage {
            turn: TurnId(1),
            targets: vec![MemberId::new("builder")],
            body: "go".to_string(),
        });
        for (id, cmd) in [("t1", "cargo build"), ("t2", "cargo test")] {
            state.apply(RuntimeEvent::ToolStarted {
                member: MemberId::new("builder"),
                tool_id: id.to_string(),
                name: "shell".to_string(),
                summary: cmd.to_string(),
            });
            state.apply(RuntimeEvent::ToolCompleted {
                member: MemberId::new("builder"),
                tool_id: id.to_string(),
                ok: true,
                output: "ok".to_string(),
            });
        }
        let mut lines = Vec::new();

        render_chat_history(&state, 60, &mut lines);

        let text = plain_text(&lines);
        let build_idx = text
            .iter()
            .position(|line| line.contains("cargo build"))
            .unwrap();
        let test_idx = text
            .iter()
            .position(|line| line.contains("cargo test"))
            .unwrap();
        // Tool blocks (including their output lines) stay adjacent.
        assert!(test_idx > build_idx);
        assert!(
            text[build_idx + 1..test_idx]
                .iter()
                .all(|line| !line.trim().is_empty())
        );
    }

    #[test]
    fn renders_markdown_agent_message() {
        let chat = vec![ChatItem::Agent {
            member: MemberId::new("reviewer"),
            display_name: "Reviewer".to_string(),
            backend: BackendKind::Claude,
            text: "## Findings\n\nThe parser drops a **trailing newline**. Use `trim_end`.\n\n- check the lexer\n- add a test\n\n```rust\nlet x = 1;\n```"
                .to_string(),
        }];
        let state = AppState::new(chat);

        let mut terminal = Terminal::new(TestBackend::new(72, 18)).unwrap();
        terminal.draw(|frame| render(frame, &state)).unwrap();
        let view = format!("{}", terminal.backend());
        eprintln!("\n{view}");

        assert!(view.contains("Findings")); // heading, '##' stripped
        assert!(view.contains("• check the lexer")); // bullet marker
        assert!(view.contains("let x = 1;")); // code block body
        assert!(!view.contains("```")); // fences stripped
        assert!(!view.contains("**")); // bold markers consumed
    }

    #[test]
    fn renders_user_band_and_compact_tool() {
        use crate::domain::event::TurnId;

        let mut state = AppState::new(Vec::new());
        state.apply(RuntimeEvent::Ready {
            team: "t".to_string(),
            workspace: String::new(),
            default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
            workflow_runs: Vec::new(),
            members: vec![member_summary(
                "builder",
                "Builder",
                BackendKind::Codex,
                "impl",
                MemberStatus::Idle,
            )],
        });
        state.apply(RuntimeEvent::UserMessage {
            turn: TurnId(1),
            targets: vec![MemberId::new("builder")],
            body: "run the tests".to_string(),
        });
        let long = "/bin/zsh -lc \"rg -n 'Codex is OpenAIs coding agent' /var/folders/ym/abc/openai-docs-cache/codex-manual.md and a lot more text that used to wrap\"";
        state.apply(RuntimeEvent::ToolStarted {
            member: MemberId::new("builder"),
            tool_id: "t1".to_string(),
            name: "shell".to_string(),
            summary: long.to_string(),
        });
        state.apply(RuntimeEvent::ToolCompleted {
            member: MemberId::new("builder"),
            tool_id: "t1".to_string(),
            ok: true,
            output: "matches found".to_string(),
        });
        let mut terminal = Terminal::new(TestBackend::new(72, 14)).unwrap();
        terminal.draw(|frame| render(frame, &state)).unwrap();
        let view = format!("{}", terminal.backend());
        eprintln!("\n{view}");

        assert!(view.contains("◆ You"));
        assert!(view.contains("run the tests"));
        // The long command is truncated to a single line (ellipsis), not wrapped.
        assert!(view.contains('…'));
        assert!(view.contains("✓ shell"));
        assert!(view.contains("matches found"));
    }

    #[test]
    fn failed_tool_shows_error_output_without_expanding() {
        let state = AppState::new(vec![ChatItem::Tool {
            member: MemberId::new("builder"),
            name: "shell".to_string(),
            summary: "cargo test".to_string(),
            detail: "error: test parser failed\nexpected true, got false".to_string(),
            ok: Some(false),
        }]);
        let mut lines = Vec::new();

        render_chat_history(&state, 70, &mut lines);

        let text = plain_text(&lines).join("\n");
        assert!(text.contains("✕ shell"));
        assert!(text.contains("cargo test"));
        assert!(text.contains("error: test parser failed"));
        assert!(text.contains("expected true, got false"));
    }

    #[test]
    fn renders_verdict_card_with_title_and_summary() {
        let state = AppState::new(vec![
            ChatItem::Verdict {
                member: MemberId::new("reviewer"),
                approve: true,
                summary: "Looks good; ship it.".to_string(),
            },
            ChatItem::Verdict {
                member: MemberId::new("reviewer"),
                approve: false,
                summary: "Needs a regression test.".to_string(),
            },
        ]);
        let mut lines = Vec::new();
        render_chat_history(&state, 70, &mut lines);
        let text = plain_text(&lines).join("\n");
        assert!(
            text.contains("✓ review approved"),
            "missing approve title: {text}"
        );
        assert!(
            text.contains("Looks good; ship it."),
            "missing approve summary: {text}"
        );
        assert!(
            text.contains("✗ changes requested"),
            "missing reject title: {text}"
        );
        assert!(
            text.contains("Needs a regression test."),
            "missing reject summary: {text}"
        );
    }

    #[test]
    fn renders_scrollable_diff_drawer() {
        let mut state = AppState::new(Vec::new());
        state.set_diff(
            "diff --git a/src/lib.rs b/src/lib.rs\n@@ -1,3 +1,3 @@\n-old line\n+new line\n context"
                .to_string(),
        );
        state.toggle_drawer(Drawer::Diff);

        let mut terminal = Terminal::new(TestBackend::new(80, 16)).unwrap();
        terminal.draw(|frame| render(frame, &state)).unwrap();
        let view = format!("{}", terminal.backend());
        eprintln!("\n{view}");

        assert!(view.contains("Working-tree diff"));
        assert!(view.contains("scroll"));
        assert!(view.contains("+new line"));
        assert!(view.contains("-old line"));
    }

    fn ready_with_run(run: WorkflowRunSummary) -> AppState {
        let mut state = AppState::new(Vec::new());
        state.apply(RuntimeEvent::Ready {
            team: "t".to_string(),
            workspace: String::new(),
            default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
            workflow_runs: vec![run],
            members: vec![member_summary(
                "builder",
                "Builder",
                BackendKind::Codex,
                "impl",
                MemberStatus::Idle,
            )],
        });
        state
    }

    #[test]
    fn renders_workflow_footer_next_step() {
        let state = ready_with_run(WorkflowRunSummary {
            id: WorkflowRunId(7),
            goal: "ship parser".to_string(),
            status: WorkflowRunStatus::Done,
            coordinator: Some(MemberId::new("builder")),
            verification: None,
            created_at: "2026-06-28 10:00:00".to_string(),
            updated_at: "2026-06-28 10:00:00".to_string(),
            attempt: 1,
            events: Vec::new(),
            steps: Vec::new(),
            mode: None,
        });

        let mut terminal = Terminal::new(TestBackend::new(100, 16)).unwrap();
        terminal.draw(|frame| render(frame, &state)).unwrap();
        let view = format!("{}", terminal.backend());
        eprintln!("\n{view}");

        assert!(view.contains("run-7 done"));
        assert!(view.contains("/verify to check"));
        assert!(view.contains("/runs details"));
    }

    #[test]
    fn renders_workflow_footer_step_progress() {
        let state = ready_with_run(WorkflowRunSummary {
            id: WorkflowRunId(7),
            goal: "ship parser".to_string(),
            status: WorkflowRunStatus::Running,
            coordinator: Some(MemberId::new("builder")),
            verification: None,
            created_at: "2026-06-28 10:00:00".to_string(),
            updated_at: "2026-06-28 10:00:00".to_string(),
            attempt: 1,
            events: Vec::new(),
            steps: vec![
                WorkflowStepSummary {
                    number: 1,
                    status: WorkflowStepStatus::Done,
                    owner: None,
                    title: "Map parser states".to_string(),
                    note: None,
                    updated_at: "2026-06-28 10:05:00".to_string(),
                },
                WorkflowStepSummary {
                    number: 2,
                    status: WorkflowStepStatus::Doing,
                    owner: None,
                    title: "Wire checklist UI".to_string(),
                    note: None,
                    updated_at: "2026-06-28 10:10:00".to_string(),
                },
            ],
            mode: None,
        });

        let mut terminal = Terminal::new(TestBackend::new(100, 16)).unwrap();
        terminal.draw(|frame| render(frame, &state)).unwrap();
        let view = format!("{}", terminal.backend());
        eprintln!("\n{view}");

        assert!(view.contains("run-7 running"));
        assert!(view.contains("1/2 done"));
        assert!(view.contains("1 doing"));
        assert!(view.contains("/runs details"));
    }

    #[test]
    fn renders_workflow_runs_drawer() {
        let mut state = ready_with_run(WorkflowRunSummary {
            id: WorkflowRunId(1),
            goal: "ship parser".to_string(),
            status: WorkflowRunStatus::Done,
            coordinator: Some(MemberId::new("builder")),
            verification: Some(WorkflowVerification {
                command: "cargo test".to_string(),
                ok: true,
                summary: "ok".to_string(),
            }),
            created_at: "2026-06-28 10:00:00".to_string(),
            updated_at: "2026-06-28 10:15:00".to_string(),
            attempt: 1,
            events: vec![
                WorkflowRunEventSummary {
                    kind: "note".to_string(),
                    title: "User note".to_string(),
                    detail: Some("checkpoint saved".to_string()),
                    created_at: "2026-06-28 10:10:00".to_string(),
                    attempt: 1,
                },
                WorkflowRunEventSummary {
                    kind: "verification_passed".to_string(),
                    title: "Verification passed".to_string(),
                    detail: Some("cargo test\nok".to_string()),
                    created_at: "2026-06-28 10:15:00".to_string(),
                    attempt: 1,
                },
            ],
            steps: vec![
                WorkflowStepSummary {
                    number: 1,
                    status: WorkflowStepStatus::Done,
                    owner: Some(MemberId::new("builder")),
                    title: "Map parser states".to_string(),
                    note: None,
                    updated_at: "2026-06-28 10:05:00".to_string(),
                },
                WorkflowStepSummary {
                    number: 2,
                    status: WorkflowStepStatus::Blocked,
                    owner: None,
                    title: "Document edge cases".to_string(),
                    note: Some("waiting for reviewer".to_string()),
                    updated_at: "2026-06-28 10:12:00".to_string(),
                },
            ],
            mode: None,
        });
        state.toggle_drawer(Drawer::Runs);

        let mut terminal = Terminal::new(TestBackend::new(90, 34)).unwrap();
        terminal.draw(|frame| render(frame, &state)).unwrap();
        let view = format!("{}", terminal.backend());
        eprintln!("\n{view}");

        assert!(view.contains("Workflow Runs"));
        assert!(view.contains("Enter status"));
        assert!(view.contains("Tab dispatch"));
        assert!(view.contains("x details"));
        assert!(view.contains("←→ run"));
        assert!(view.contains("View: compact"));
        assert!(view.contains("Selected: run-1"));
        assert!(view.contains("Goal: ship parser"));
        assert!(view.contains("Progress:"));
        assert!(view.contains("Action: /plan"));
        assert!(view.contains("Steps:"));
        // Compact mode hides the deep-dive fields.
        assert!(!view.contains("Owners:"));
        assert!(!view.contains("Next:"));
        assert!(!view.contains("Outcome:"));
        assert!(!view.contains("Stages:"));
        assert!(!view.contains("Timeline:"));
        assert!(!view.contains("checkpoint saved"));

        assert!(state.toggle_workflow_runs_detail());
        terminal.draw(|frame| render(frame, &state)).unwrap();
        let view = format!("{}", terminal.backend());
        eprintln!("\n{view}");

        assert!(view.contains("x compact"));
        assert!(view.contains("History: 1 run"));
        assert!(view.contains("View: details"));
        assert!(view.contains("1 verified"));
        assert!(view.contains("Selected: run-1"));
        assert!(view.contains("Goal: ship parser"));
        assert!(view.contains("Owner: builder"));
        assert!(view.contains("Attempt: #1"));
        assert!(view.contains("Time: created 06-28 10:00"));
        assert!(view.contains("updated 06-28 10:15"));
        assert!(view.contains("Progress:"));
        assert!(view.contains("1/2 done"));
        assert!(view.contains("1 blocked"));
        assert!(view.contains("Owners:"));
        assert!(view.contains("@builder 0/1 done"));
        assert!(view.contains("unassigned 1/1 1 blocked"));
        assert!(view.contains("Outcome: verified by cargo test"));
        assert!(view.contains("Next: verified"));
        assert!(view.contains("Action: /plan"));
        assert!(view.contains("Stages:"));
        assert!(view.contains("Steps:"));
        assert!(view.contains("@builder"));
        assert!(view.contains("Map parser states"));
        assert!(view.contains("Document edge cases"));
        assert!(view.contains("waiting for reviewer"));
        assert!(view.contains("Timeline:"));
        assert!(view.contains("User note"));
        assert!(view.contains("checkpoint saved"));
        assert!(view.contains("Verification passed"));
        assert!(view.contains("plan done"));
        assert!(view.contains("work done"));
        assert!(view.contains("verify done"));
        assert!(view.contains("run-1"));
        assert!(view.contains("Try"));
        assert!(view.contains("Steps"));
        assert!(view.contains("#1"));
        assert!(view.contains("Updated"));
        assert!(view.contains("06-28 10:15"));
        assert!(view.contains("ship parser"));
        assert!(view.contains("cargo test"));
        assert!(view.contains("ok"));
    }

    #[test]
    fn renders_selected_workflow_step_action() {
        let mut state = ready_with_run(WorkflowRunSummary {
            id: WorkflowRunId(1),
            goal: "ship parser".to_string(),
            status: WorkflowRunStatus::Running,
            coordinator: Some(MemberId::new("builder")),
            verification: None,
            created_at: "2026-06-28 10:00:00".to_string(),
            updated_at: "2026-06-28 10:15:00".to_string(),
            attempt: 1,
            events: Vec::new(),
            steps: vec![WorkflowStepSummary {
                number: 1,
                status: WorkflowStepStatus::Doing,
                owner: Some(MemberId::new("builder")),
                title: "Wire checklist UI".to_string(),
                note: None,
                updated_at: "2026-06-28 10:05:00".to_string(),
            }],
            mode: None,
        });
        state.toggle_drawer(Drawer::Runs);
        state.select_next_workflow_step();

        let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
        terminal.draw(|frame| render(frame, &state)).unwrap();
        let view = format!("{}", terminal.backend());
        eprintln!("\n{view}");

        assert!(view.contains("Action: /step done run-1 1"));
        assert!(view.contains("Dispatch: @builder Continue run-1 step #1"));
        assert!(view.contains("@builder"));
        assert!(view.contains("› 1."));
        assert!(view.contains("Wire checklist UI"));
    }

    #[test]
    fn renders_failed_workflow_continue_action() {
        let mut state = ready_with_run(WorkflowRunSummary {
            id: WorkflowRunId(1),
            goal: "ship parser".to_string(),
            status: WorkflowRunStatus::Failed,
            coordinator: Some(MemberId::new("builder")),
            verification: Some(WorkflowVerification {
                command: "cargo test".to_string(),
                ok: false,
                summary: "tests failed".to_string(),
            }),
            created_at: "2026-06-28 10:00:00".to_string(),
            updated_at: "2026-06-28 10:15:00".to_string(),
            attempt: 2,
            events: vec![WorkflowRunEventSummary {
                kind: "verification_failed".to_string(),
                title: "Verification failed".to_string(),
                detail: Some("cargo test\ntests failed".to_string()),
                created_at: "2026-06-28 10:15:00".to_string(),
                attempt: 2,
            }],
            steps: Vec::new(),
            mode: None,
        });
        state.toggle_drawer(Drawer::Runs);

        let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
        terminal.draw(|frame| render(frame, &state)).unwrap();
        let view = format!("{}", terminal.backend());
        eprintln!("\n{view}");

        assert!(view.contains("Outcome: verification failed: cargo test"));
        assert!(view.contains("Timeline:"));
        assert!(view.contains("Verification failed"));
        assert!(view.contains("Attempt: #2"));
        assert!(view.contains("Next: run the Action command to continue fixes"));
        assert!(view.contains("Action: /continue run-1 fix failing verification"));
        assert!(view.contains("#2"));
    }

    #[test]
    fn renders_multiline_composer() {
        let mut state = AppState::new(Vec::new());
        for ch in "line one".chars() {
            state.insert_char(ch);
        }
        state.insert_newline();
        for ch in "line two".chars() {
            state.insert_char(ch);
        }

        let mut terminal = Terminal::new(TestBackend::new(60, 16)).unwrap();
        terminal.draw(|frame| render(frame, &state)).unwrap();
        let view = format!("{}", terminal.backend());
        eprintln!("\n{view}");

        // Both composer lines are visible (first with the prompt gutter).
        assert!(view.contains("> line one"));
        assert!(view.contains("line two"));
    }
}
