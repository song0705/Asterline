//! Drawer overlays: logs, team roster/editor, command palette, diff, and
//! member logs. The `/runs` drawer body lives in `workflow_view`.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};

use crate::domain::event::LogEntry;
use crate::domain::team::MemberId;
use crate::tui::app_state::AppState;
use crate::tui::drawers::Drawer;
use crate::tui::markdown;
use crate::tui::team_builder::{Field, field_value};
use crate::tui::theme;
use crate::tui::theme::pad_width;
use crate::tui::workflow_view::drawer_runs;

pub(crate) fn render_drawer(frame: &mut Frame<'_>, area: Rect, state: &AppState, drawer: &Drawer) {
    let popup = centered_rect(area, 86, 76);
    frame.render_widget(Clear, popup);
    let title = match drawer {
        Drawer::MemberLogs(member) => format!("Logs: {member}"),
        _ => drawer.title().to_string(),
    };
    let block = Block::default()
        .title(Span::styled(format!(" {title} "), theme::accent_bold()))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::muted());
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    // Reserve the bottom row of the drawer for the key hint.
    let (content, hint_row) = if inner.height > 1 {
        (
            Rect::new(inner.x, inner.y, inner.width, inner.height - 1),
            Some(Rect::new(
                inner.x,
                inner.y + inner.height - 1,
                inner.width,
                1,
            )),
        )
    } else {
        (inner, None)
    };

    let lines = match drawer {
        Drawer::Logs => drawer_logs(state),
        Drawer::Team => drawer_team(state),
        Drawer::Runs => drawer_runs(state),
        Drawer::Palette => drawer_palette(),
        Drawer::Diff => drawer_diff(state),
        Drawer::MemberLogs(member_id) => drawer_member_logs(state, member_id),
    };
    // Clamp the scroll offset so content can't be pushed entirely off-screen.
    let max_scroll = lines.len().saturating_sub(content.height as usize);
    let offset = state.drawer_scroll().min(max_scroll) as u16;
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((offset, 0)),
        content,
    );

    if let Some(hint_row) = hint_row {
        let hint = match drawer {
            Drawer::Runs if state.workflow_runs_detail() => {
                "x compact · ↑↓ step · ←→ run · Enter status · Tab dispatch · Pg scroll · Esc close"
            }
            Drawer::Runs => {
                "x details · ↑↓ step · ←→ run · Enter status · Tab dispatch · Pg scroll · Esc close"
            }
            _ => "↑↓ scroll · Esc close",
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(format!(" {hint}"), theme::muted()))),
            hint_row,
        );
    }
}

fn log_lines<'a>(entries: impl Iterator<Item = &'a LogEntry>) -> Vec<Line<'static>> {
    entries
        .map(|entry| {
            Line::from(vec![
                Span::styled(
                    format!(" {:<5} ", entry.level.as_str()),
                    Style::default().fg(theme::log_color(entry.level)),
                ),
                Span::styled(format!("{} ", entry.source), theme::accent_bold()),
                Span::styled(entry.message.clone(), theme::text()),
            ])
        })
        .collect()
}

fn drawer_logs(state: &AppState) -> Vec<Line<'static>> {
    let logs = state.logs();
    if logs.is_empty() {
        return vec![Line::styled("no logs yet", theme::muted())];
    }
    log_lines(logs.iter().rev().take(200))
}

fn drawer_member_logs(state: &AppState, member_id: &MemberId) -> Vec<Line<'static>> {
    let member_str = member_id.as_str();
    let filtered: Vec<&LogEntry> = state
        .logs()
        .iter()
        .filter(|entry| entry.source == member_str)
        .collect();

    if filtered.is_empty() {
        return vec![Line::styled(
            format!("no logs yet for {member_id}"),
            theme::muted(),
        )];
    }
    log_lines(filtered.into_iter().rev().take(200))
}

/// Build a table header line and its matching rule from column widths.
fn table_header(cells: &[&str], widths: &[usize], tail: &str) -> (Line<'static>, Line<'static>) {
    let mut text = String::from(" ");
    let mut rule = String::from("─");
    for (cell, width) in cells.iter().zip(widths) {
        text.push_str(&pad_width(cell, *width));
        text.push_str("│ ");
        rule.push_str(&"─".repeat(*width));
        rule.push_str("┼─");
    }
    text.push_str(tail);
    rule.push_str(&"─".repeat(theme::display_width(tail).max(4)));
    (
        Line::from(Span::styled(text, theme::accent_bold())),
        Line::from(Span::styled(rule, theme::muted())),
    )
}

const TEAM_COLUMNS: [usize; 3] = [13, 9, 19];

fn drawer_team(state: &AppState) -> Vec<Line<'static>> {
    if let Some(editor) = state.team_editor() {
        return drawer_team_editor(state, editor);
    }

    let mut lines = Vec::new();
    let (header, rule) = table_header(&["Member", "Backend", "Role"], &TEAM_COLUMNS, "Status");
    lines.push(header);
    lines.push(rule);

    for member in state.members() {
        let color = theme::backend_color(member.backend);
        let sep = Span::styled("│ ", theme::muted());
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {}", pad_width(&member.display_name, TEAM_COLUMNS[0])),
                theme::bold(color),
            ),
            sep.clone(),
            Span::styled(
                pad_width(member.backend.as_str(), TEAM_COLUMNS[1]),
                Style::default().fg(color),
            ),
            sep.clone(),
            Span::styled(pad_width(&member.role, TEAM_COLUMNS[2]), theme::text()),
            sep,
            Span::styled(
                theme::status_label(member.status).to_string(),
                Style::default().fg(theme::status_color(member.status)),
            ),
        ]));
        let session = member
            .session
            .clone()
            .unwrap_or_else(|| "no session yet".to_string());
        let model = member.model.as_deref().unwrap_or("default");
        let effort = member
            .effort
            .map(|effort| effort.as_str().to_string())
            .unwrap_or_else(|| "default".to_string());
        lines.push(Line::styled(
            format!("   └─ session: {session}"),
            theme::muted(),
        ));
        lines.push(Line::styled(
            format!(
                "      model: {model} · effort: {effort} · cwd: {}",
                member.cwd
            ),
            theme::muted(),
        ));
    }

    if !state.pending_approvals().is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(" Pending approvals:", theme::warning_bold()));
        for approval in state.pending_approvals() {
            lines.push(Line::from(vec![
                Span::styled(format!("  [{}] ", approval.id), theme::warning_bold()),
                Span::styled(format!("{} (", approval.action), theme::warning()),
                Span::styled(
                    approval.body.clone(),
                    theme::text().add_modifier(ratatui::style::Modifier::ITALIC),
                ),
                Span::styled(")", theme::warning()),
            ]));
        }
    }
    lines
}

const EDITOR_COLUMNS: [usize; 5] = [17, 9, 15, 15, 7];

fn drawer_team_editor(
    state: &AppState,
    editor: &crate::tui::team_editor::TeamEditor,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let dirty = if editor.dirty() { "modified" } else { "saved" };
    lines.push(Line::from(vec![
        Span::styled(" Team editor ", theme::accent_bold()),
        Span::styled(
            format!("({dirty})"),
            if editor.dirty() {
                theme::warning()
            } else {
                theme::muted()
            },
        ),
    ]));
    lines.push(Line::styled(
        " ↑/↓ member · ←/→ field · Enter edit/cycle · a add · d delete · t target · * all · s apply · Esc close",
        theme::muted(),
    ));
    lines.push(Line::styled(
        format!(" Default target: {}", editor.default_label()),
        theme::text(),
    ));
    if let Some(notice) = editor.notice() {
        lines.push(Line::styled(format!(" {notice}"), theme::warning()));
    }
    lines.push(Line::raw(""));
    let (header, rule) = table_header(
        &["  Member", "Backend", "Role", "Model", "Target"],
        &EDITOR_COLUMNS,
        "Status",
    );
    lines.push(header);
    lines.push(rule);

    for (idx, member) in editor.members().iter().enumerate() {
        let selected = idx == editor.selected();
        let selected_field = editor.selected_field();
        let color = theme::backend_color(member.backend);
        let row_style = if selected {
            theme::selection()
        } else {
            Style::default().fg(color)
        };
        let cell_style = |field: Field, default_style: Style| {
            if selected && selected_field == field {
                theme::selection_cell()
            } else {
                default_style
            }
        };
        let marker = if selected { "›" } else { " " };
        let model = member.model.as_deref().unwrap_or("default");
        let target = editor.default_marker(member);
        let status = state
            .members()
            .iter()
            .find(|view| view.id == member.id)
            .map(|view| theme::status_label(view.status))
            .unwrap_or("new");
        let sep = Span::styled("│ ", theme::muted());
        lines.push(Line::from(vec![
            Span::styled(
                format!(
                    " {marker} {}",
                    pad_width(&member.display_name, EDITOR_COLUMNS[0] - 3)
                ),
                cell_style(Field::Name, row_style),
            ),
            sep.clone(),
            Span::styled(
                pad_width(member.backend.as_str(), EDITOR_COLUMNS[1]),
                cell_style(Field::Backend, row_style),
            ),
            sep.clone(),
            Span::styled(
                pad_width(&member.role, EDITOR_COLUMNS[2]),
                cell_style(Field::Role, row_style),
            ),
            sep.clone(),
            Span::styled(
                pad_width(model, EDITOR_COLUMNS[3]),
                cell_style(Field::Model, row_style),
            ),
            sep,
            Span::styled(pad_width(target, EDITOR_COLUMNS[4]), row_style),
            Span::styled(format!(" {status}"), theme::muted()),
        ]));
    }

    if let Some(member) = editor.selected_member() {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            " Selected member fields",
            theme::accent_bold(),
        )));
        lines.push(Line::from(vec![
            Span::styled("     handle: ", theme::muted()),
            Span::styled(format!("@{}", member.id), theme::accent()),
            Span::styled(" (auto)", theme::muted()),
        ]));
        for (idx, field) in Field::ALL.iter().enumerate() {
            let selected = idx == editor.field_index();
            let style = if selected {
                theme::selection_cell()
            } else {
                theme::text()
            };
            lines.push(Line::from(Span::styled(
                format!(" {:>10}: {}", field.label(), field_value(member, *field)),
                style,
            )));
        }
    }

    if let Some(edit) = editor.editing() {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled(
                format!(" editing {}: ", edit.field.label()),
                theme::warning_bold(),
            ),
            Span::styled(edit.buffer.clone(), theme::emphasis()),
        ]));
        lines.push(Line::styled(
            " Enter commit · Esc cancel · Ctrl+U clear",
            theme::muted_italic(),
        ));
    }

    if !state.pending_approvals().is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            " Pending approvals are still handled with /approve or /reject.",
            theme::muted(),
        ));
    }
    lines
}

fn drawer_palette() -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::styled(
            " Messages require @member, @all, /ask, or /all. Bare drafts are kept.",
            theme::muted(),
        ),
        Line::raw(""),
    ];

    let items = [
        ("/ask <member> <msg>", "Send a message to a specific member"),
        ("@<member> <msg>", "Shortcut to message a specific member"),
        ("/all <msg>", "Broadcast a message to all members"),
        ("/team", "Edit team roster, sessions, and approvals"),
        ("/plan <goal>", "Start a tracked team workflow"),
        ("/workflow <goal>", "Alias for /plan"),
        ("/runs", "Open workflow status and next action"),
        (
            "/continue [run-id] [note]",
            "Resume latest or selected workflow run",
        ),
        (
            "/note [run-id] <note>",
            "Record a human checkpoint on a workflow run",
        ),
        (
            "/block [run-id] <reason>",
            "Mark a workflow run blocked with a reason",
        ),
        (
            "/step add|assign|done ...",
            "Manage workflow checklist steps and owners",
        ),
        (
            "/verify [run-id] [cmd]",
            "Run background verification for latest or selected workflow",
        ),
        ("/logs", "Open raw log stream, stderr, and warnings"),
        ("/diff", "Show the working-tree git diff"),
        ("/retry", "Resume paused routes or re-run the last turn"),
        ("/abort", "Cancel running members and active verification"),
        (
            "/approve / /reject",
            "Approve or reject the first pending approval",
        ),
        ("/help", "Show this palette help drawer"),
    ];

    for (cmd, desc) in items {
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<24} ", cmd), theme::accent_bold()),
            Span::styled(format!(" {desc}"), theme::text()),
        ]));
    }
    lines
}

/// Render the captured working-tree diff: structural lines stay colored by role,
/// while added/removed/context code is syntax-highlighted by the file's type.
fn drawer_diff(state: &AppState) -> Vec<Line<'static>> {
    let Some(diff) = state.diff_text() else {
        return vec![Line::styled("no diff captured — run /diff", theme::muted())];
    };
    if diff.trim().is_empty() {
        return vec![Line::styled(
            "working tree clean — no changes",
            theme::success(),
        )];
    }

    let mut out = Vec::new();
    let mut ext = String::new();
    for line in diff.lines() {
        // Track the current file so code lines highlight with the right syntax.
        if let Some(path) = line
            .strip_prefix("+++ b/")
            .or_else(|| line.strip_prefix("diff --git a/"))
        {
            ext = file_extension(path);
        }

        if line.starts_with("+++") || line.starts_with("---") {
            out.push(Line::styled(
                line.to_string(),
                theme::muted().add_modifier(ratatui::style::Modifier::BOLD),
            ));
        } else if line.starts_with("@@") {
            out.push(Line::styled(line.to_string(), theme::accent()));
        } else if line.starts_with("diff ")
            || line.starts_with("index ")
            || line.starts_with("Untracked")
        {
            out.push(Line::styled(line.to_string(), theme::warning_bold()));
        } else if let Some(rest) = line.strip_prefix('+') {
            out.push(diff_code_line('+', theme::SUCCESS, rest, &ext));
        } else if let Some(rest) = line.strip_prefix('-') {
            out.push(diff_code_line('-', theme::ERROR, rest, &ext));
        } else {
            let rest = line.strip_prefix(' ').unwrap_or(line);
            out.push(diff_code_line(' ', theme::MUTED, rest, &ext));
        }
    }
    out
}

/// One diff content line: a colored +/-/space gutter plus syntax-highlighted code.
fn diff_code_line(
    marker: char,
    gutter: ratatui::style::Color,
    code: &str,
    ext: &str,
) -> Line<'static> {
    let mut spans = vec![Span::styled(
        marker.to_string(),
        Style::default().fg(gutter),
    )];
    if code.is_empty() {
        return Line::from(spans);
    }
    if ext.is_empty() {
        spans.push(Span::styled(code.to_string(), Style::default().fg(gutter)));
    } else {
        spans.extend(markdown::highlight_code_line(code, ext));
    }
    Line::from(spans)
}

fn file_extension(path: &str) -> String {
    let path = path.trim();
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_string()
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
