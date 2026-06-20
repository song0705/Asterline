//! Renders the chat-first UI: a header with team + member status, the single
//! scrolling conversation column, the bottom composer, a footer hint line, and
//! an optional drawer overlay (logs / team / command palette).

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Wrap};

use crate::domain::event::{ChatItem, LogEntry, LogLevel, MemberStatus};
use crate::domain::team::{BackendKind, MemberId};
use crate::tui::app_state::AppState;
use crate::tui::completion::Completion;
use crate::tui::drawers::Drawer;
use crate::tui::markdown;

pub fn render(frame: &mut Frame<'_>, state: &AppState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(frame.area());

    render_header(frame, chunks[0], state);
    render_chat(frame, chunks[1], state);
    render_composer(frame, chunks[2], state);
    render_footer(frame, chunks[3], state);

    // The completion popup floats just above the composer (hidden behind a drawer).
    if state.drawer().is_none()
        && let Some(completion) = state.completion()
    {
        render_popup(frame, chunks[2], &completion, state.popup_selected());
    }

    if let Some(drawer) = state.drawer() {
        render_drawer(frame, frame.area(), state, &drawer);
    }
}

fn render_popup(
    frame: &mut Frame<'_>,
    composer_area: Rect,
    completion: &Completion,
    selected: usize,
) {
    const MAX_ROWS: usize = 6;
    let count = completion.items.len();
    let shown = count.min(MAX_ROWS);
    let height = shown as u16 + 2;
    let width = composer_area.width.min(60);
    let area = Rect {
        x: composer_area.x,
        y: composer_area.y.saturating_sub(height),
        width,
        height,
    };
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(format!(" {} ", completion.title))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let selected = selected.min(count.saturating_sub(1));
    let start = if selected >= shown {
        selected + 1 - shown
    } else {
        0
    };
    let lines: Vec<Line> = completion
        .items
        .iter()
        .enumerate()
        .skip(start)
        .take(shown)
        .map(|(i, item)| {
            let style = if i == selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Line::from(Span::styled(format!(" {} ", item.label), style))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_header(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let width = area.width as usize;
    let project_name = format!(" Asterline · {} ", state.team());
    let workspace_info = if state.workspace().is_empty() {
        String::new()
    } else {
        format!(" {} ", state.workspace())
    };
    let remaining_dashes = width
        .saturating_sub(project_name.chars().count())
        .saturating_sub(workspace_info.chars().count())
        .saturating_sub(2);
    let dashes = "─".repeat(remaining_dashes);

    let header_line = Line::from(vec![
        Span::styled("┌", Style::default().fg(Color::DarkGray)),
        Span::styled(
            project_name,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(dashes, Style::default().fg(Color::DarkGray)),
        Span::styled(workspace_info, Style::default().fg(Color::DarkGray)),
        Span::styled("┐", Style::default().fg(Color::DarkGray)),
    ]);

    let mut chips = vec![Span::styled("│ ", Style::default().fg(Color::DarkGray))];
    for (i, member) in state.members().iter().enumerate() {
        if i > 0 {
            chips.push(Span::styled("  ·  ", Style::default().fg(Color::DarkGray)));
        }
        let color = backend_color(member.backend);
        let dot = match member.status {
            MemberStatus::Running => {
                let ms = std::time::SystemTime::now()
                    .duration_since(std::time::SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis();
                let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                let idx = ((ms / 80) % spinner.len() as u128) as usize;
                spinner[idx]
            }
            MemberStatus::NeedsApproval => "⚠",
            MemberStatus::Failed => "✘",
            _ => "○",
        };
        let is_selected = state.header_selected() == Some(i);
        let (name_style, status_style) = if is_selected {
            (
                Style::default()
                    .fg(Color::Black)
                    .bg(color)
                    .add_modifier(Modifier::BOLD),
                Style::default().fg(Color::Black).bg(color),
            )
        } else {
            (
                Style::default().fg(color).add_modifier(Modifier::BOLD),
                Style::default().fg(status_color(member.status)),
            )
        };
        chips.push(Span::styled(format!("{dot} "), status_style));
        chips.push(Span::styled(member.display_name.clone(), name_style));
        chips.push(Span::styled(
            format!(" ({})", status_glyph(member.status)),
            status_style,
        ));
        if let Some(effort) = member.effort {
            chips.push(Span::styled(
                format!(" ·{}", effort.as_str()),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }
    if state.members().is_empty() {
        chips.push(Span::styled(
            "starting…",
            Style::default().fg(Color::DarkGray),
        ));
    }

    let chips_len: usize = chips.iter().map(|s| s.content.chars().count()).sum();
    let right_padding = width.saturating_sub(chips_len).saturating_sub(1);
    chips.push(Span::raw(" ".repeat(right_padding)));
    chips.push(Span::styled("│", Style::default().fg(Color::DarkGray)));

    frame.render_widget(Paragraph::new(vec![header_line, Line::from(chips)]), area);
}

fn render_chat(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let block = Block::default().padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let width = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();

    if state.chat().is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "      _       _             _ _",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            "     / \\   __| |_ ___  _ __| (_)_ __   ___",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            "    / _ \\ / _` \\ __/ _ \\| '__| | | '_ \\ / _ \\",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            "   / ___ \\ (_| | ||  __/| |  | | | | | |  __/",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            "  /_/   \\_\\__,_|\\__\\___||_|  |_|_|_| |_|\\___|",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled("  │ ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "Welcome to Asterline — A chat-first multi-agent console.",
                Style::default().fg(Color::Gray),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled("  │ ", Style::default().fg(Color::DarkGray)),
            Span::styled("Press ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "Enter",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" to submit, ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "@member",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " to target a member, ",
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                "/help",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" for commands.", Style::default().fg(Color::DarkGray)),
        ]));
        lines.push(Line::raw(""));
    }

    for item in state.chat() {
        render_item(item, width, state, &mut lines);
    }

    // Append active member status lines
    let active_members: Vec<_> = state
        .members()
        .iter()
        .filter(|m| m.status != MemberStatus::Idle)
        .collect();

    if !active_members.is_empty() {
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let idx = ((ms / 80) % spinner.len() as u128) as usize;
        let spin_char = spinner[idx];

        for member in active_members {
            // Draw placeholder header if the member hasn't started their message yet
            if !state.has_active_message(&member.id) {
                let color = backend_color(member.backend);
                lines.push(Line::from(vec![
                    Span::styled(
                        "• ",
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        member.display_name.clone(),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!(" · {}", member.backend.as_str()),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));

                let has_reasoning = state
                    .active_reasoning()
                    .get(&member.id)
                    .map(|s| !s.is_empty())
                    .unwrap_or(false);
                let elapsed = state
                    .member_elapsed_secs(&member.id)
                    .map(fmt_elapsed_compact)
                    .unwrap_or_default();
                let line_text = if has_reasoning {
                    let reasoning = &state.active_reasoning()[&member.id];
                    format!("{spin_char} thinking {elapsed}: {reasoning}")
                } else {
                    match member.status {
                        MemberStatus::Running => {
                            format!("{spin_char} working {elapsed} · Ctrl+C to interrupt")
                        }
                        MemberStatus::Queued => format!("{spin_char} queued"),
                        MemberStatus::Waiting => format!("{spin_char} waiting"),
                        MemberStatus::NeedsApproval => {
                            format!("{spin_char} waiting for approval")
                        }
                        MemberStatus::Failed => format!("{spin_char} failed"),
                        MemberStatus::Idle => format!("{spin_char} idle"),
                    }
                };

                for wrapped in markdown::wrap(&line_text, width.saturating_sub(2).max(1)) {
                    lines.push(Line::from(vec![
                        Span::styled("  ", Style::default()),
                        Span::styled(
                            wrapped,
                            Style::default()
                                .fg(Color::DarkGray)
                                .add_modifier(Modifier::ITALIC),
                        ),
                    ]));
                }
                lines.push(Line::raw(""));
            } else {
                // Member has started their message; only show reasoning if present
                let has_reasoning = state
                    .active_reasoning()
                    .get(&member.id)
                    .map(|s| !s.is_empty())
                    .unwrap_or(false);
                if has_reasoning {
                    let reasoning = &state.active_reasoning()[&member.id];
                    let elapsed = state
                        .member_elapsed_secs(&member.id)
                        .map(fmt_elapsed_compact)
                        .unwrap_or_default();
                    let line_text = format!("{spin_char} thinking {elapsed}: {reasoning}");
                    for wrapped in markdown::wrap(&line_text, width.saturating_sub(2).max(1)) {
                        lines.push(Line::from(vec![
                            Span::styled("  ", Style::default()),
                            Span::styled(
                                wrapped,
                                Style::default()
                                    .fg(Color::DarkGray)
                                    .add_modifier(Modifier::ITALIC),
                            ),
                        ]));
                    }
                }
            }
        }
    }

    let height = inner.height as usize;
    let total = lines.len();
    let max_start = total.saturating_sub(height);
    let start = max_start.saturating_sub(state.scroll());
    let visible: Vec<Line> = lines.into_iter().skip(start).take(height).collect();

    frame.render_widget(Paragraph::new(visible), inner);
}

fn render_item(item: &ChatItem, width: usize, state: &AppState, out: &mut Vec<Line<'static>>) {
    match item {
        ChatItem::User { body } => {
            let mut first = true;
            for line in markdown::wrap(body, width.saturating_sub(2).max(1)) {
                let prefix = if first {
                    first = false;
                    Span::styled(
                        "› ",
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    Span::styled("  ", Style::default())
                };
                out.push(Line::from(vec![
                    prefix,
                    Span::styled(line, Style::default().fg(Color::Gray)),
                ]));
            }
            out.push(Line::raw(""));
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
            let color = backend_color(*backend);
            out.push(Line::from(vec![
                Span::styled(
                    "• ",
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    display_name.clone(),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" · {}", backend.as_str()),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
            for line in markdown::render(text, width.saturating_sub(2).max(1)) {
                let mut spans = vec![Span::styled("  ", Style::default())];
                spans.extend(line.spans);
                out.push(Line::from(spans));
            }
            out.push(Line::raw(""));
        }
        ChatItem::Tool {
            name, summary, ok, ..
        } => {
            let (dot, dot_color) = match ok {
                None => {
                    let ms = std::time::SystemTime::now()
                        .duration_since(std::time::SystemTime::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis();
                    let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                    let idx = ((ms / 80) % spinner.len() as u128) as usize;
                    (spinner[idx], Color::Yellow)
                }
                Some(true) => ("●", Color::Green),
                Some(false) => ("●", Color::Red),
            };

            let name_color = match ok {
                None => Color::Yellow,
                Some(_) => Color::DarkGray,
            };
            let name_modifier = match ok {
                None => Modifier::BOLD,
                Some(_) => Modifier::empty(),
            };

            let mut spans = vec![
                Span::styled("  ", Style::default()),
                Span::styled(format!("{dot} "), Style::default().fg(dot_color)),
                Span::styled(
                    name.clone(),
                    Style::default().fg(name_color).add_modifier(name_modifier),
                ),
            ];

            if state.tools_expanded() {
                let avail = width.saturating_sub(name.chars().count() + 28);
                let display_summary = truncate(summary, avail.max(10));
                spans.push(Span::styled(": ", Style::default().fg(Color::DarkGray)));
                spans.push(Span::styled(
                    display_summary,
                    Style::default().fg(Color::Gray),
                ));
                spans.push(Span::styled(" ", Style::default()));
                spans.push(Span::styled(
                    "(ctrl+g/t to collapse)",
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::ITALIC),
                ));
            } else {
                spans.push(Span::styled(" ", Style::default()));
                spans.push(Span::styled(
                    "(ctrl+g/t to expand)",
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::ITALIC),
                ));
            }

            out.push(Line::from(spans));
        }
        ChatItem::Diff { files, .. } => {
            out.push(Line::from(Span::styled(
                "  ✎ file changes",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
            for (path, kind) in files {
                let (sign, color) = match kind.as_str() {
                    "add" => ("+", Color::Green),
                    "delete" => ("-", Color::Red),
                    _ => ("~", Color::Yellow),
                };
                let shown = truncate(path, width.saturating_sub(6).max(10));
                out.push(Line::from(vec![
                    Span::styled(format!("    {sign} "), Style::default().fg(color)),
                    Span::styled(shown, Style::default().fg(color)),
                ]));
            }
        }
        ChatItem::Route { from, to, body } => {
            out.push(Line::from(Span::styled(
                format!("{from} → {}", to.join(", ")),
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            )));
            push_wrapped(body, width, "  ", Style::default().fg(Color::Magenta), out);
            out.push(Line::raw(""));
        }
        ChatItem::Notice { text } => {
            push_wrapped(
                &format!("• {text}"),
                width,
                "",
                Style::default().fg(Color::DarkGray),
                out,
            );
        }
        ChatItem::Error { member, message } => {
            let prefix = member
                .as_ref()
                .map(|m| format!("✗ {m}: "))
                .unwrap_or_else(|| "✗ ".to_string());
            push_wrapped(
                &format!("{prefix}{message}"),
                width,
                "",
                Style::default().fg(Color::Red),
                out,
            );
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

fn truncate(text: &str, max: usize) -> String {
    let max = max.max(1);
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max {
        collapsed
    } else {
        let mut out: String = collapsed.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn render_composer(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let (border_color, title_text) = if !state.pending_approvals().is_empty() {
        (
            Color::Magenta,
            format!(
                " Action Required ({} pending approvals: /approve) ",
                state.pending_approvals().len()
            ),
        )
    } else if state.paused_routes() > 0 {
        (
            Color::Yellow,
            format!(
                " Delivery Paused ({} routes paused: /retry) ",
                state.paused_routes()
            ),
        )
    } else if state.running_count() > 0 {
        (Color::Yellow, " Processing turn (running...) ".to_string())
    } else {
        (
            Color::Cyan,
            " Composer (Enter to send, @member, /command) ".to_string(),
        )
    };

    let block = Block::default()
        .title(title_text)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chars: Vec<char> = state.composer().text().chars().collect();
    let cursor = state.composer().cursor();
    let prompt_cols = 2usize; // "> "
    let avail = (inner.width as usize).saturating_sub(prompt_cols);
    let (visible, cursor_in_view_width) = if avail == 0 {
        (String::new(), 0)
    } else {
        let start = cursor.saturating_sub(avail.saturating_sub(1));
        let end = (start + avail).min(chars.len());
        let visible_str: String = chars[start..end].iter().collect();
        let cursor_relative_idx = cursor - start;
        let width_before_cursor: usize = chars[start..(start + cursor_relative_idx)]
            .iter()
            .map(|&c| {
                let val = c as u32;
                if (0x3000..=0x9FFF).contains(&val)
                    || (0xAC00..=0xD7AF).contains(&val)
                    || (0xFF00..=0xFFEF).contains(&val)
                {
                    2
                } else {
                    1
                }
            })
            .sum();
        (visible_str, width_before_cursor)
    };

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("> ", Style::default().fg(border_color)),
            Span::raw(visible),
        ])),
        inner,
    );

    if state.drawer().is_none() {
        let col = inner.x + prompt_cols as u16 + cursor_in_view_width as u16;
        frame.set_cursor_position((col, inner.y));
    }
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let mut parts = Vec::new();
    if state.paused_routes() > 0 {
        parts.push(Span::styled(
            format!(
                "● {} paused route(s) (type /retry to resume)",
                state.paused_routes()
            ),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if !state.pending_approvals().is_empty() {
        if !parts.is_empty() {
            parts.push(Span::raw("   "));
        }
        parts.push(Span::styled(
            format!(
                "● {} pending approval(s) (type /approve to decide)",
                state.pending_approvals().len()
            ),
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let running = state.running_count();
    if running > 0 {
        if !parts.is_empty() {
            parts.push(Span::raw("   "));
        }
        parts.push(Span::styled(
            format!("⏳ {running} working · Ctrl+C to interrupt"),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    } else if parts.is_empty() {
        // Idle: a faint, always-present key-hint line (codex-style).
        parts.push(Span::styled(
            "Enter send · ↑↓ history · @member · /help · Ctrl+R team · Ctrl+L logs",
            Style::default().fg(Color::DarkGray),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(parts)), area);
}

/// Format elapsed seconds compactly: `8s`, `1m 4s`, `1h 2m 3s` (mirrors codex's
/// `fmt_elapsed_compact`).
fn fmt_elapsed_compact(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m {}s", secs / 3600, (secs % 3600) / 60, secs % 60)
    }
}

fn render_drawer(frame: &mut Frame<'_>, area: Rect, state: &AppState, drawer: &Drawer) {
    let popup = centered_rect(area, 80, 70);
    frame.render_widget(Clear, popup);
    let title = match drawer {
        Drawer::MemberLogs(member) => format!("Logs: {member}"),
        _ => drawer.title().to_string(),
    };
    let block = Block::default()
        .title(format!(" {title} (↑↓ scroll · Esc to close) "))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let lines = match drawer {
        Drawer::Logs => drawer_logs(state),
        Drawer::Team => drawer_team(state),
        Drawer::Palette => drawer_palette(),
        Drawer::Diff => drawer_diff(state),
        Drawer::MemberLogs(member_id) => drawer_member_logs(state, member_id),
    };
    // Clamp the scroll offset so content can't be pushed entirely off-screen.
    let max_scroll = lines.len().saturating_sub(inner.height as usize);
    let offset = state.drawer_scroll().min(max_scroll) as u16;
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((offset, 0)),
        inner,
    );
}

/// Render the captured working-tree diff with +/- line coloring.
fn drawer_diff(state: &AppState) -> Vec<Line<'static>> {
    let Some(diff) = state.diff_text() else {
        return vec![Line::styled(
            "no diff captured — run /diff",
            Style::default().fg(Color::DarkGray),
        )];
    };
    if diff.trim().is_empty() {
        return vec![Line::styled(
            "working tree clean — no changes",
            Style::default().fg(Color::Green),
        )];
    }
    diff.lines()
        .map(|line| {
            let style = if line.starts_with("+++") || line.starts_with("---") {
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD)
            } else if line.starts_with("@@") {
                Style::default().fg(Color::Cyan)
            } else if line.starts_with("diff ")
                || line.starts_with("index ")
                || line.starts_with("Untracked")
            {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else if line.starts_with('+') {
                Style::default().fg(Color::Green)
            } else if line.starts_with('-') {
                Style::default().fg(Color::Red)
            } else {
                Style::default().fg(Color::Gray)
            };
            Line::from(Span::styled(line.to_string(), style))
        })
        .collect()
}

fn drawer_logs(state: &AppState) -> Vec<Line<'static>> {
    let logs = state.logs();
    if logs.is_empty() {
        return vec![Line::styled(
            "no logs yet",
            Style::default().fg(Color::DarkGray),
        )];
    }
    logs.iter()
        .rev()
        .take(200)
        .map(|entry| {
            Line::from(vec![
                Span::styled(
                    format!(" {:<5} ", entry.level.as_str()),
                    Style::default()
                        .fg(log_color(entry.level))
                        .bg(Color::Rgb(30, 30, 30)),
                ),
                Span::styled(
                    format!(" {} ", entry.source),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(entry.message.clone(), Style::default().fg(Color::Gray)),
            ])
        })
        .collect()
}

fn drawer_member_logs(state: &AppState, member_id: &MemberId) -> Vec<Line<'static>> {
    let logs = state.logs();
    let member_str = member_id.as_str();
    let filtered: Vec<&LogEntry> = logs
        .iter()
        .filter(|entry| entry.source == member_str)
        .collect();

    if filtered.is_empty() {
        return vec![Line::styled(
            format!("no logs yet for {member_id}"),
            Style::default().fg(Color::DarkGray),
        )];
    }

    filtered
        .into_iter()
        .rev()
        .take(200)
        .map(|entry| {
            Line::from(vec![
                Span::styled(
                    format!(" {:<5} ", entry.level.as_str()),
                    Style::default()
                        .fg(log_color(entry.level))
                        .bg(Color::Rgb(30, 30, 30)),
                ),
                Span::styled(
                    format!(" {} ", entry.source),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(entry.message.clone(), Style::default().fg(Color::Gray)),
            ])
        })
        .collect()
}

fn drawer_team(state: &AppState) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    lines.push(Line::from(vec![Span::styled(
        format!(
            " {:<12} │ {:<8} │ {:<18} │ {:<10} ",
            "Member", "Backend", "Role", "Status"
        ),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(vec![Span::styled(
        "──────────────┼──────────┼────────────────────┼────────────",
        Style::default().fg(Color::DarkGray),
    )]));

    for member in state.members() {
        let color = backend_color(member.backend);
        let status_color = status_color(member.status);
        let status_str = status_glyph(member.status);
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {:<12} ", member.display_name),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled("│ ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:<8} ", member.backend.as_str()),
                Style::default().fg(color),
            ),
            Span::styled("│ ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{:<18} ", member.role), Style::default()),
            Span::styled("│ ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:<10} ", status_str),
                Style::default().fg(status_color),
            ),
        ]));
        let session = member
            .session
            .clone()
            .unwrap_or_else(|| "no session yet".to_string());
        lines.push(Line::styled(
            format!("   └─ session: {session}"),
            Style::default().fg(Color::DarkGray),
        ));
    }

    if !state.pending_approvals().is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            " Pending approvals:",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ));
        for approval in state.pending_approvals() {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  [{}] ", approval.id),
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{} (", approval.action),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(
                    approval.body.clone(),
                    Style::default()
                        .fg(Color::Gray)
                        .add_modifier(Modifier::ITALIC),
                ),
                Span::styled(")", Style::default().fg(Color::Yellow)),
            ]));
        }
    }
    lines
}

fn drawer_palette() -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(Line::from(vec![Span::styled(
        " Command Palette",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(vec![Span::styled(
        "─────────────────",
        Style::default().fg(Color::DarkGray),
    )]));

    let items = [
        ("/ask <member> <msg>", "Send a message to a specific member"),
        ("@<member> <msg>", "Shortcut to message a specific member"),
        ("/all <msg>", "Broadcast a message to all members"),
        ("/team", "Open team roster, active sessions, and approvals"),
        ("/logs", "Open raw log stream, stderr, and warnings"),
        ("/diff", "Show the working-tree git diff"),
        ("/retry", "Resume paused routes or re-run the last turn"),
        ("/abort", "Cancel all running member executions"),
        (
            "/approve / /reject",
            "Approve or reject the first pending approval",
        ),
        ("/help", "Show this palette help drawer"),
    ];

    for (cmd, desc) in items {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {:<24} ", cmd),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" {desc}",), Style::default().fg(Color::Gray)),
        ]));
    }
    lines
}

fn centered_rect(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn backend_color(backend: BackendKind) -> Color {
    match backend {
        BackendKind::Claude => Color::Magenta,
        BackendKind::Codex => Color::Cyan,
        BackendKind::Gemini => Color::Blue,
    }
}

fn status_glyph(status: MemberStatus) -> &'static str {
    match status {
        MemberStatus::Idle => "idle",
        MemberStatus::Queued => "queued",
        MemberStatus::Running => "running",
        MemberStatus::Waiting => "waiting",
        MemberStatus::NeedsApproval => "approval",
        MemberStatus::Failed => "failed",
    }
}

fn status_color(status: MemberStatus) -> Color {
    match status {
        MemberStatus::Running => Color::Yellow,
        MemberStatus::Failed => Color::Red,
        MemberStatus::NeedsApproval => Color::Magenta,
        _ => Color::DarkGray,
    }
}

fn log_color(level: LogLevel) -> Color {
    match level {
        LogLevel::Error => Color::Red,
        LogLevel::Warn => Color::Yellow,
        LogLevel::Info => Color::Gray,
        LogLevel::Debug => Color::DarkGray,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_elapsed_compact_scales_units() {
        assert_eq!(fmt_elapsed_compact(8), "8s");
        assert_eq!(fmt_elapsed_compact(64), "1m 4s");
        assert_eq!(fmt_elapsed_compact(3723), "1h 2m 3s");
    }

    #[test]
    fn renders_a_clean_layout_snapshot() {
        use crate::domain::event::{MemberStatus, MemberSummary, RuntimeEvent};
        use crate::domain::team::MemberId;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut state = AppState::new(Vec::new());
        state.apply(RuntimeEvent::Ready {
            team: "default-mixed".to_string(),
            workspace: "/Users/me/proj".to_string(),
            members: vec![
                MemberSummary {
                    id: MemberId::new("builder"),
                    display_name: "Builder".to_string(),
                    backend: BackendKind::Codex,
                    role: "implementation".to_string(),
                    status: MemberStatus::Running,
                    session: None,
                    cwd: String::new(),
                    effort: None,
                },
                MemberSummary {
                    id: MemberId::new("reviewer"),
                    display_name: "Reviewer".to_string(),
                    backend: BackendKind::Claude,
                    role: "review".to_string(),
                    status: MemberStatus::Idle,
                    session: None,
                    cwd: String::new(),
                    effort: None,
                },
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
        assert!(view.contains("working"));
        assert!(view.contains("interrupt"));
        // The conversation is not wrapped in a box; the only border is the
        // rounded composer at the bottom.
        assert!(view.contains('╭') && view.contains('╰'));
    }

    #[test]
    fn renders_completion_popup() {
        use crate::domain::event::{MemberStatus, MemberSummary, RuntimeEvent};
        use crate::domain::team::MemberId;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut state = AppState::new(Vec::new());
        state.apply(RuntimeEvent::Ready {
            team: "t".to_string(),
            workspace: String::new(),
            members: vec![MemberSummary {
                id: MemberId::new("builder"),
                display_name: "Builder".to_string(),
                backend: BackendKind::Codex,
                role: "impl".to_string(),
                status: MemberStatus::Idle,
                session: None,
                cwd: String::new(),
                effort: None,
            }],
        });
        for ch in "/a".chars() {
            state.insert_char(ch);
        }

        let mut terminal = Terminal::new(TestBackend::new(70, 14)).unwrap();
        terminal.draw(|frame| render(frame, &state)).unwrap();
        let view = format!("{}", terminal.backend());
        eprintln!("\n{view}");

        assert!(view.contains("commands"));
        assert!(view.contains("/ask"));
        assert!(view.contains("/all"));
    }

    #[test]
    fn renders_markdown_agent_message() {
        use crate::domain::event::ChatItem;
        use crate::domain::team::MemberId;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

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
        use crate::domain::event::{MemberStatus, MemberSummary, RuntimeEvent, TurnId};
        use crate::domain::team::MemberId;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut state = AppState::new(Vec::new());
        state.apply(RuntimeEvent::Ready {
            team: "t".to_string(),
            workspace: String::new(),
            members: vec![MemberSummary {
                id: MemberId::new("builder"),
                display_name: "Builder".to_string(),
                backend: BackendKind::Codex,
                role: "impl".to_string(),
                status: MemberStatus::Idle,
                session: None,
                cwd: String::new(),
                effort: None,
            }],
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
            summary: long.to_string(),
        });
        state.toggle_tools_expansion();

        let mut terminal = Terminal::new(TestBackend::new(72, 12)).unwrap();
        terminal.draw(|frame| render(frame, &state)).unwrap();
        let view = format!("{}", terminal.backend());
        eprintln!("\n{view}");

        assert!(view.contains("› "));
        assert!(view.contains("run the tests"));
        // The long command is truncated to a single line (ellipsis), not wrapped.
        assert!(view.contains('…'));
        assert!(view.contains("● shell"));
    }

    #[test]
    fn renders_scrollable_diff_drawer() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

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
}
