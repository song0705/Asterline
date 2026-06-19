//! Renders the chat-first UI: a header with team + member status, the single
//! scrolling conversation column, the bottom composer, a footer hint line, and
//! an optional drawer overlay (logs / team / command palette).

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Wrap};

use crate::domain::event::{ChatItem, LogLevel, MemberStatus};
use crate::domain::team::BackendKind;
use crate::tui::app_state::AppState;
use crate::tui::completion::Completion;
use crate::tui::drawers::Drawer;
use crate::tui::markdown;

/// Subtle shaded background for user message bands.
const USER_BG: Color = Color::Rgb(38, 40, 48);

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
        render_drawer(frame, frame.area(), state, drawer);
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
    let mut title = vec![
        Span::styled(
            "Asterline",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  {}", state.team()),
            Style::default().fg(Color::DarkGray),
        ),
    ];
    if !state.workspace().is_empty() {
        title.push(Span::styled(
            format!("  ·  {}", state.workspace()),
            Style::default().fg(Color::DarkGray),
        ));
    }

    let mut chips = Vec::new();
    for (i, member) in state.members().iter().enumerate() {
        if i > 0 {
            chips.push(Span::raw("   "));
        }
        chips.push(Span::styled(
            member.display_name.clone(),
            Style::default()
                .fg(backend_color(member.backend))
                .add_modifier(Modifier::BOLD),
        ));
        chips.push(Span::styled(
            format!(" {}", status_glyph(member.status)),
            Style::default().fg(status_color(member.status)),
        ));
    }
    if chips.is_empty() {
        chips.push(Span::styled(
            "starting…",
            Style::default().fg(Color::DarkGray),
        ));
    }

    frame.render_widget(
        Paragraph::new(vec![Line::from(title), Line::from(chips)]),
        area,
    );
}

fn render_chat(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    // No box around the conversation — just a one-column side margin.
    let block = Block::default().padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let width = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    for item in state.chat() {
        render_item(item, width, &mut lines);
    }

    let height = inner.height as usize;
    let total = lines.len();
    let max_start = total.saturating_sub(height);
    let start = max_start.saturating_sub(state.scroll());
    let visible: Vec<Line> = lines.into_iter().skip(start).take(height).collect();

    frame.render_widget(Paragraph::new(visible), inner);
}

fn render_item(item: &ChatItem, width: usize, out: &mut Vec<Line<'static>>) {
    match item {
        ChatItem::User { body } => {
            out.push(band_line(
                vec![Span::styled(
                    "▌ You",
                    Style::default()
                        .fg(Color::Green)
                        .bg(USER_BG)
                        .add_modifier(Modifier::BOLD),
                )],
                width,
            ));
            for line in markdown::wrap(body, width.saturating_sub(2).max(1)) {
                out.push(band_line(
                    vec![Span::styled(
                        format!("▌ {line}"),
                        Style::default().fg(Color::Gray).bg(USER_BG),
                    )],
                    width,
                ));
            }
            out.push(Line::raw(""));
        }
        ChatItem::Agent {
            display_name,
            backend,
            text,
            ..
        } => {
            if text.is_empty() {
                return;
            }
            out.push(Line::from(vec![
                Span::styled(
                    display_name.clone(),
                    Style::default()
                        .fg(backend_color(*backend))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" · {}", backend.as_str()),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
            // Agent output is rendered as Markdown (headings, bold, code, lists).
            for line in markdown::render(text, width.saturating_sub(2).max(1)) {
                out.push(indent_line(line, "  "));
            }
            out.push(Line::raw(""));
        }
        ChatItem::Tool {
            name, summary, ok, ..
        } => {
            let (glyph, color) = match ok {
                None => ("⚙", Color::Yellow),
                Some(true) => ("✓", Color::Green),
                Some(false) => ("✗", Color::Red),
            };
            // One compact line: glyph + tool name + a truncated command/summary.
            let head_cols = glyph.chars().count() + 1 + name.chars().count() + 2;
            let cmd = truncate(summary, width.saturating_sub(head_cols).max(8));
            out.push(Line::from(vec![
                Span::styled(format!("{glyph} "), Style::default().fg(color)),
                Span::styled(name.clone(), Style::default().fg(color)),
                Span::raw("  "),
                Span::styled(cmd, Style::default().fg(Color::Gray)),
            ]));
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

/// Prepend a plain-text indent to an already-styled line.
fn indent_line(line: Line<'static>, indent: &str) -> Line<'static> {
    let mut spans = vec![Span::raw(indent.to_string())];
    spans.extend(line.spans);
    Line::from(spans)
}

/// Pad `spans` with a trailing background space run so the line reads as a
/// full-width shaded band (used for user messages).
fn band_line(spans: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let mut spans = spans;
    if used < width {
        spans.push(Span::styled(
            " ".repeat(width - used),
            Style::default().bg(USER_BG),
        ));
    }
    Line::from(spans)
}

/// Collapse whitespace and truncate to `max` columns with an ellipsis.
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
    let block = Block::default()
        .title(" message (@member · /command) ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Horizontally scroll a single input line so the cursor is always visible.
    let chars: Vec<char> = state.composer().text().chars().collect();
    let cursor = state.composer().cursor();
    let prompt_cols = 2usize; // "> "
    let avail = (inner.width as usize).saturating_sub(prompt_cols);
    let (visible, cursor_in_view) = if avail == 0 {
        (String::new(), 0)
    } else {
        let start = cursor.saturating_sub(avail.saturating_sub(1));
        let end = (start + avail).min(chars.len());
        (chars[start..end].iter().collect::<String>(), cursor - start)
    };

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::Cyan)),
            Span::raw(visible),
        ])),
        inner,
    );

    // Only show the cursor when the composer has focus (no drawer overlay).
    if state.drawer().is_none() {
        let col = inner.x + prompt_cols as u16 + cursor_in_view as u16;
        frame.set_cursor_position((col, inner.y));
    }
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let mut hints =
        "Enter send · Ctrl+L logs · Ctrl+R team · Ctrl+P commands · Ctrl+C cancel/quit".to_string();
    if state.paused_routes() > 0 {
        hints.push_str(&format!(
            "  ·  {} paused route(s): /retry",
            state.paused_routes()
        ));
    }
    if !state.pending_approvals().is_empty() {
        hints.push_str(&format!(
            "  ·  {} approval(s): /approve",
            state.pending_approvals().len()
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hints,
            Style::default().fg(Color::DarkGray),
        ))),
        area,
    );
}

fn render_drawer(frame: &mut Frame<'_>, area: Rect, state: &AppState, drawer: Drawer) {
    let popup = centered_rect(area, 80, 70);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .title(format!(" {} (Esc to close) ", drawer.title()))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let lines = match drawer {
        Drawer::Logs => drawer_logs(state),
        Drawer::Team => drawer_team(state),
        Drawer::Palette => drawer_palette(),
    };
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
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
                    format!("{:<5}", entry.level.as_str()),
                    Style::default().fg(log_color(entry.level)),
                ),
                Span::styled(
                    format!(" {} ", entry.source),
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw(entry.message.clone()),
            ])
        })
        .collect()
}

fn drawer_team(state: &AppState) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for member in state.members() {
        lines.push(Line::from(vec![
            Span::styled(
                member.display_name.clone(),
                Style::default()
                    .fg(backend_color(member.backend))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                "  {}  {}  [{}]",
                member.backend.as_str(),
                member.role,
                member.status
            )),
        ]));
        let session = member
            .session
            .clone()
            .unwrap_or_else(|| "no session yet".to_string());
        lines.push(Line::styled(
            format!("    session: {session}"),
            Style::default().fg(Color::DarkGray),
        ));
    }
    if !state.pending_approvals().is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "pending approvals:",
            Style::default().add_modifier(Modifier::BOLD),
        ));
        for approval in state.pending_approvals() {
            lines.push(Line::raw(format!(
                "  [{}] {} ({})",
                approval.id, approval.action, approval.body
            )));
        }
    }
    lines
}

fn drawer_palette() -> Vec<Line<'static>> {
    [
        "/ask <member> <message>   send to one member",
        "/all <message>            send to everyone",
        "@<member> <message>       send to one member",
        "/team                     show roster, sessions, approvals",
        "/logs                     show raw logs / stderr / warnings",
        "/retry                    resume a paused route or re-run last turn",
        "/abort                    cancel running members",
        "/approve · /reject        decide the first pending approval",
        "/help                     show this list",
    ]
    .iter()
    .map(|line| Line::raw(line.to_string()))
    .collect()
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
                },
                MemberSummary {
                    id: MemberId::new("reviewer"),
                    display_name: "Reviewer".to_string(),
                    backend: BackendKind::Claude,
                    role: "review".to_string(),
                    status: MemberStatus::Idle,
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

        let mut terminal = Terminal::new(TestBackend::new(72, 12)).unwrap();
        terminal.draw(|frame| render(frame, &state)).unwrap();
        let view = format!("{}", terminal.backend());
        eprintln!("\n{view}");

        assert!(view.contains("▌ You"));
        assert!(view.contains("run the tests"));
        // The long command is truncated to a single line (ellipsis), not wrapped.
        assert!(view.contains('…'));
        assert!(view.contains("✓ shell"));
    }
}
