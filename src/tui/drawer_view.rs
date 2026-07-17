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
    let popup = drawer_rect(area);
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
        Drawer::Team => drawer_team(state, content.width as usize),
        Drawer::Runs => drawer_runs(state, content.width as usize),
        Drawer::Palette => drawer_palette(),
        Drawer::Diff => drawer_diff(state),
        Drawer::Skills => drawer_skills(state),
        Drawer::MemberLogs(member_id) => drawer_member_logs(state, member_id),
    };
    // Clamp the scroll offset so content can't be pushed entirely off-screen.
    // Account for line wrapping: the Paragraph widget wraps long lines, so the
    // visual line count can exceed `lines.len()`.
    let content_width = content.width.max(1) as usize;
    let visual_count: usize = lines
        .iter()
        .map(|line| {
            let w = line_width(line);
            if w == 0 {
                1
            } else {
                w.div_ceil(content_width).max(1)
            }
        })
        .sum();
    let max_scroll = visual_count.saturating_sub(content.height as usize);
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
            Drawer::Skills => "↑↓ choose · Enter/Tab use for next prompt · Esc close",
            Drawer::Team
                if state
                    .team_editor()
                    .is_some_and(|editor| editor.editing().is_some()) =>
            {
                "Enter save · Esc cancel · ←/→ move · Ctrl+U/W/K edit"
            }
            Drawer::Team
                if state
                    .team_editor()
                    .is_some_and(|editor| editor.field_mode()) =>
            {
                "↑↓ field · Enter edit/cycle · Esc members"
            }
            Drawer::Team => "↑↓ member · Enter fields · Esc close",
            _ => "↑↓ scroll · Esc close",
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(format!(" {hint}"), theme::muted()))),
            hint_row,
        );
    }

    if matches!(drawer, Drawer::Team)
        && let Some(edit) = state.team_editor().and_then(|editor| editor.editing())
    {
        crate::tui::team_builder::render_edit_box(frame, content, edit);
    }
}

/// Exact outer rectangle used by every drawer. Mouse selection uses the same
/// geometry so dragging inside an overlay cannot spill into the background UI.
pub(crate) fn drawer_rect(area: Rect) -> Rect {
    centered_rect(area, 86, 76)
}

/// Display width of all spans in a line (for scroll clamping with wrapping).
fn line_width(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|span| theme::display_width(span.content.as_ref()))
        .sum()
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
    let mut text = String::new();
    let mut rule = String::new();
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

fn drawer_team(state: &AppState, width: usize) -> Vec<Line<'static>> {
    if let Some(editor) = state.team_editor() {
        return drawer_team_editor(state, editor, width);
    }

    let mut lines = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(" Team", theme::accent_bold()),
        Span::styled(
            format!("  {} member(s)", state.members().len()),
            theme::muted(),
        ),
    ]));
    lines.push(Line::raw(""));

    for member in state.members() {
        let color = theme::backend_color(member.backend);
        let status_label = theme::status_label(member.status);
        // Header line: ● Name  backend · role  ·  status
        lines.push(Line::from(vec![
            Span::styled("● ", theme::bold(color)),
            Span::styled(member.display_name.clone(), theme::bold(color)),
            Span::styled(
                format!("   {} · {}", member.backend.as_str(), member.role),
                theme::muted(),
            ),
            Span::styled(format!("   {status_label}"), {
                Style::default().fg(theme::status_color(member.status))
            }),
        ]));
        // Detail line 1: session · model · effort
        let session = member.session.clone().unwrap_or_else(|| "—".to_string());
        let model = member.model.as_deref().unwrap_or("default");
        let effort = member
            .effort
            .map(|effort| effort.as_str().to_string())
            .unwrap_or_else(|| "default".to_string());
        lines.push(Line::from(vec![
            Span::styled("  session ", theme::muted()),
            Span::styled(session, theme::text()),
            Span::styled("  ·  model ", theme::muted()),
            Span::styled(model.to_string(), theme::text()),
            Span::styled("  ·  effort ", theme::muted()),
            Span::styled(effort, theme::text()),
        ]));
        // Detail line 2: cwd
        lines.push(Line::from(vec![
            Span::styled("  cwd ", theme::muted()),
            Span::styled(member.cwd.clone(), theme::text()),
        ]));
        lines.push(Line::raw(""));
    }

    if !state.pending_approvals().is_empty() {
        lines.push(Line::styled(" Pending approvals", theme::warning_bold()));
        for approval in state.pending_approvals() {
            lines.push(Line::from(vec![
                Span::styled(format!("  {} ", approval.id), theme::warning_bold()),
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
    width: usize,
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
        if editor.field_mode() {
            " ↑/↓ field · Enter edit/cycle/pick session · e manual model/session · s apply · Esc members"
        } else {
            " ↑/↓ member · Enter fields · a add · d delete · t target · * all · s apply · Esc close"
        },
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
        &["   Member", "Backend", "Role", "Model", "Target"],
        &EDITOR_COLUMNS,
        "Status",
    );
    lines.push(header);
    lines.push(rule);

    for (idx, member) in editor.members().iter().enumerate() {
        let selected = idx == editor.selected();
        let color = theme::backend_color(member.backend);
        let row_style = if selected {
            theme::bold(theme::emphasis_color())
        } else {
            Style::default().fg(color)
        };
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
            Span::raw(" "),
            Span::styled(
                if selected { "▶ " } else { "  " },
                if selected {
                    theme::warning_bold()
                } else {
                    theme::muted()
                },
            ),
            Span::styled(
                pad_width(&member.display_name, EDITOR_COLUMNS[0] - 3),
                row_style,
            ),
            sep.clone(),
            Span::styled(
                pad_width(member.backend.as_str(), EDITOR_COLUMNS[1]),
                row_style,
            ),
            sep.clone(),
            Span::styled(pad_width(&member.role, EDITOR_COLUMNS[2]), row_style),
            sep.clone(),
            Span::styled(pad_width(model, EDITOR_COLUMNS[3]), row_style),
            sep.clone(),
            Span::styled(pad_width(target, EDITOR_COLUMNS[4]), row_style),
            sep,
            Span::styled(status.to_string(), theme::muted()),
        ]));
    }

    if editor.model_picker().is_none()
        && editor.session_picker().is_none()
        && let Some(member) = editor.selected_member()
    {
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
            let selected = editor.field_mode() && idx == editor.field_index();
            let style = if selected {
                theme::editor_field_focus()
            } else {
                theme::text()
            };
            lines.push(Line::from(Span::styled(
                format!(
                    " {} {:>10}: {}",
                    if selected { "›" } else { " " },
                    field.label(),
                    field_value(member, *field)
                ),
                style,
            )));
        }
    }

    if let Some(picker) = editor.model_picker() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(" Model choices", theme::accent_bold()));
        let (start, options) = picker.window(8);
        if start > 0 {
            lines.push(Line::styled("    …", theme::muted()));
        }
        for (offset, model) in options.iter().enumerate() {
            let selected = start + offset == picker.selected();
            lines.push(Line::from(vec![
                Span::styled(
                    if selected { "  › " } else { "    " },
                    if selected {
                        theme::editor_focus()
                    } else {
                        theme::muted()
                    },
                ),
                Span::styled(
                    model.as_deref().unwrap_or("default").to_string(),
                    if selected {
                        theme::emphasis()
                    } else {
                        theme::text()
                    },
                ),
            ]));
        }
        if start + options.len() < picker.options().len() {
            lines.push(Line::styled("    …", theme::muted()));
        }
    }

    if let Some(picker) = editor.session_picker() {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} sessions ", picker.backend().as_str()),
                theme::accent_bold(),
            ),
            Span::styled(
                format!("{} match(es)", picker.visible_len()),
                theme::muted(),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled(" Search: ", theme::muted()),
            Span::styled(
                if picker.query().is_empty() {
                    "type title, project, or id…".to_string()
                } else {
                    picker.query().to_string()
                },
                if picker.query().is_empty() {
                    theme::muted_italic()
                } else {
                    theme::emphasis()
                },
            ),
        ]));
        if let Some(error) = picker.error() {
            lines.push(Line::styled(format!(" {error}"), theme::warning()));
        } else if picker.visible_len() == 0 {
            lines.push(Line::styled(" No matching sessions.", theme::muted()));
        } else {
            let available = width.saturating_sub(6).max(32);
            let session_columns = [available * 35 / 100, available * 30 / 100, 8];
            let id_width = available.saturating_sub(session_columns.iter().sum::<usize>());
            let (header, rule) = table_header(
                &["   Title", "Project", "Updated"],
                &session_columns,
                "Session ID",
            );
            lines.push(header);
            lines.push(rule);
            let (start, entries) = picker.window(8);
            for (offset, entry) in entries.iter().enumerate() {
                let selected = start + offset == picker.selected();
                let row_style = if selected {
                    theme::emphasis()
                } else {
                    theme::text()
                };
                let sep = Span::styled("│ ", theme::muted());
                lines.push(Line::from(vec![
                    Span::styled(
                        if selected { " ▶ " } else { "   " },
                        if selected {
                            theme::warning_bold()
                        } else {
                            theme::muted()
                        },
                    ),
                    Span::styled(pad_width(&entry.title, session_columns[0] - 3), row_style),
                    sep.clone(),
                    Span::styled(pad_width(&entry.project, session_columns[1]), row_style),
                    sep.clone(),
                    Span::styled(pad_width(&entry.age(), session_columns[2]), theme::muted()),
                    sep,
                    Span::styled(theme::clip_width(&entry.id, id_width.max(1)), row_style),
                ]));
            }
        }
        lines.push(Line::styled(
            " Type filter · ↑/↓ select · PgUp/PgDn · Enter bind · Esc cancel · Ctrl+U clear",
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
            " Start with @member, @all, /ask, or /all; later bare messages reuse the last target.",
            theme::muted(),
        ),
        Line::raw(""),
    ];

    for (name, hint, takes_arg) in crate::tui::completion::COMMANDS {
        let cmd = if *takes_arg {
            format!("/{name} …")
        } else {
            format!("/{name}")
        };
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<24} ", cmd), theme::accent_bold()),
            Span::styled(format!(" {hint}"), theme::text()),
        ]));
    }
    lines
}

fn drawer_skills(state: &AppState) -> Vec<Line<'static>> {
    if state.skills().is_empty() {
        return vec![Line::styled(
            " No SKILL.md files found in workspace or user skill directories.",
            theme::muted(),
        )];
    }
    let mut lines = vec![Line::styled(
        " Select a skill; Asterline stages a targeted draft for one request.",
        theme::muted(),
    )];
    lines.push(Line::raw(""));
    for (idx, skill) in state.skills().iter().enumerate() {
        let selected = idx == state.selected_skill();
        lines.push(Line::from(vec![
            Span::styled(
                if selected { " › " } else { "   " },
                if selected {
                    theme::editor_focus()
                } else {
                    theme::muted()
                },
            ),
            Span::styled(
                skill.name.clone(),
                if selected {
                    theme::accent_bold()
                } else {
                    theme::emphasis()
                },
            ),
            Span::styled(format!("  {}", skill.description), theme::text()),
        ]));
        if selected {
            lines.push(Line::styled(
                format!("     {}", skill.path.display()),
                theme::muted(),
            ));
        }
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
            out.push(diff_code_line('+', theme::success_color(), rest, &ext));
        } else if let Some(rest) = line.strip_prefix('-') {
            out.push(diff_code_line('-', theme::error_color(), rest, &ext));
        } else {
            let rest = line.strip_prefix(' ').unwrap_or(line);
            out.push(diff_code_line(' ', theme::muted_color(), rest, &ext));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::event::{MemberStatus, MemberSummary, RuntimeEvent};
    use crate::domain::team::{
        BackendKind, DefaultTarget, MemberId, PermissionMode, SandboxPolicy, SessionPolicy,
    };
    use crate::tui::skills::SkillInfo;
    use std::path::PathBuf;

    fn ready_state() -> AppState {
        let mut state = AppState::new(Vec::new());
        state.apply(RuntimeEvent::Ready {
            team: "t".to_string(),
            workspace: "/tmp/ws".to_string(),
            default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
            workflow_runs: Vec::new(),
            members: vec![MemberSummary {
                id: MemberId::new("builder"),
                display_name: "Builder".to_string(),
                backend: BackendKind::Codex,
                role: "implementation".to_string(),
                status: MemberStatus::Idle,
                session: None,
                cwd: "/tmp/ws".to_string(),
                model: None,
                effort: None,
                sandbox: SandboxPolicy::WorkspaceWrite,
                permission_mode: Some(PermissionMode::Default),
                session_policy: SessionPolicy::Resume,
            }],
        });
        state
    }

    #[test]
    fn team_editor_shows_field_focus_only_after_enter() {
        let mut state = ready_state();
        state.toggle_drawer(Drawer::Team);

        let lines = drawer_team(&state, 100);
        let marker = lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content == "▶ ")
            .expect("selected member marker");
        let builder = lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content.trim() == "Builder")
            .expect("selected member row");

        assert_eq!(marker.style.fg, Some(theme::warning_color()));
        assert_eq!(builder.style.bg, None);
        assert_eq!(builder.style.fg, Some(theme::emphasis_color()));

        state.handle_team_editor_key(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        );
        let lines = drawer_team(&state, 100);
        let builder = lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content.trim() == "Builder")
            .expect("selected member row while a field is focused");
        let selected_field = lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content.starts_with(" ›") && span.content.contains("name:"))
            .expect("focused field in the details list");

        assert_eq!(builder.style.bg, None);
        assert_eq!(builder.style.fg, Some(theme::emphasis_color()));
        assert_eq!(selected_field.style.fg, Some(theme::warning_color()));
        assert!(
            !builder
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::UNDERLINED)
        );
    }

    #[test]
    fn team_editor_header_rule_and_rows_share_column_boundaries() {
        let mut state = ready_state();
        state.toggle_drawer(Drawer::Team);
        let lines = drawer_team(&state, 100);
        let text = |line: &Line<'_>| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        };
        let header = lines
            .iter()
            .map(&text)
            .find(|line| line.contains("Member") && line.contains("Backend"))
            .expect("team table header");
        let rule = lines
            .iter()
            .map(&text)
            .find(|line| line.contains('┼'))
            .expect("team table rule");
        let row = lines
            .iter()
            .map(&text)
            .find(|line| line.contains("Builder") && line.contains("codex"))
            .expect("team table row");
        let positions = |line: &str, separator: char| {
            line.match_indices(separator)
                .map(|(byte, _)| theme::display_width(&line[..byte]))
                .collect::<Vec<_>>()
        };

        let expected = vec![17, 28, 45, 62, 71];
        assert_eq!(positions(&header, '│'), expected);
        assert_eq!(positions(&rule, '┼'), expected);
        assert_eq!(positions(&row, '│'), expected);
    }

    #[test]
    fn skills_drawer_marks_selection_without_background_fill() {
        let mut state = ready_state();
        state.set_skills(vec![SkillInfo {
            name: "review".to_string(),
            description: "Review a patch".to_string(),
            path: PathBuf::from("/tmp/review/SKILL.md"),
        }]);

        let lines = drawer_skills(&state);
        let selected = lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content == "review")
            .expect("selected skill");

        assert_eq!(selected.style.bg, None);
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.contains("/tmp/review/SKILL.md"))
        }));
    }
}
