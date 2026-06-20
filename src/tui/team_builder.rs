//! Interactive startup team builder.
//!
//! When no `--team` config is given, Asterline detects which backend CLIs are
//! available and lets you choose which to include, instead of silently applying
//! a fixed default roster. Space toggles a backend, Enter starts the session.
//! On a non-interactive stdout it falls back to a team of all detected backends.

use std::io::{self, IsTerminal};
use std::path::Path;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use crate::domain::config::{DetectedBackends, build_team, default_member, default_team};
use crate::domain::team::{BackendKind, TeamConfig};

/// Pick a team interactively from the detected backends. Returns `None` if the
/// user cancels or nothing is available.
pub fn run(detected: DetectedBackends, workspace: &Path) -> io::Result<Option<TeamConfig>> {
    let available: Vec<BackendKind> =
        [BackendKind::Codex, BackendKind::Claude, BackendKind::Gemini]
            .into_iter()
            .filter(|b| is_detected(*b, detected))
            .collect();

    if available.is_empty() {
        return Ok(None);
    }
    // Non-interactive (piped/headless): keep the established default roster.
    if !io::stdout().is_terminal() {
        return Ok(default_team(workspace.to_path_buf(), detected));
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let outcome = select_loop(&mut terminal, &available);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(outcome?.and_then(|selected| build_team(workspace, &selected)))
}

fn is_detected(backend: BackendKind, detected: DetectedBackends) -> bool {
    match backend {
        BackendKind::Codex => detected.codex,
        BackendKind::Claude => detected.claude,
        BackendKind::Gemini => detected.gemini,
    }
}

fn select_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    available: &[BackendKind],
) -> io::Result<Option<Vec<BackendKind>>> {
    let mut checked = vec![true; available.len()];
    let mut cursor = 0usize;

    loop {
        terminal.draw(|frame| render(frame, available, &checked, cursor))?;

        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    cursor = cursor.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if cursor + 1 < available.len() {
                        cursor += 1;
                    }
                }
                KeyCode::Char(' ') => checked[cursor] = !checked[cursor],
                KeyCode::Enter => {
                    let selected: Vec<BackendKind> = available
                        .iter()
                        .zip(&checked)
                        .filter(|(_, on)| **on)
                        .map(|(b, _)| *b)
                        .collect();
                    if !selected.is_empty() {
                        return Ok(Some(selected));
                    }
                }
                KeyCode::Esc | KeyCode::Char('q') => return Ok(None),
                KeyCode::Char('c') if ctrl => return Ok(None),
                _ => {}
            }
        }
    }
}

fn render(
    frame: &mut ratatui::Frame<'_>,
    available: &[BackendKind],
    checked: &[bool],
    cursor: usize,
) {
    let area = centered(frame.area(), 64, available.len() as u16 + 8);
    let block = Block::default()
        .title(" Asterline · build your team ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines = vec![
        Line::from(Span::styled(
            "Detected backend CLIs — choose your team:",
            Style::default().fg(Color::Gray),
        )),
        Line::raw(""),
    ];
    for (i, backend) in available.iter().enumerate() {
        let member = default_member(*backend);
        let mark = if checked[i] { "[x]" } else { "[ ]" };
        let pointer = if i == cursor { "›" } else { " " };
        let row_style = if i == cursor {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else if checked[i] {
            Style::default().fg(Color::White)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        lines.push(Line::from(Span::styled(
            format!(
                " {pointer} {mark} {:<7} → {} ({})",
                backend.as_str(),
                member.display_name,
                member.role
            ),
            row_style,
        )));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "↑/↓ move · Space toggle · Enter start · Esc quit",
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    )));
    frame.render_widget(Paragraph::new(lines), inner);
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let x = area.x + (area.width - width) / 2;
    let y = area.y + (area.height - height) / 2;
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(y - area.y),
            Constraint::Length(height),
            Constraint::Min(0),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(x - area.x),
            Constraint::Length(width),
            Constraint::Min(0),
        ])
        .split(vertical[1])[1]
}
