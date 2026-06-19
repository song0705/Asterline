use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};

use crate::tui::TuiState;
use crate::types::{AgentStatus, Participant};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentStatusRow {
    pub participant: Participant,
    pub status: AgentStatus,
}

pub fn render(frame: &mut Frame<'_>, state: &TuiState) {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(3)])
        .split(frame.area());

    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(vertical[0]);

    let visible_events = state.visible_events();
    let event_items = visible_events
        .iter()
        .map(|event| ListItem::new(event.as_str()))
        .collect::<Vec<_>>();
    let events = List::new(event_items).block(
        Block::default()
            .title(state.log_pane_title())
            .borders(Borders::ALL),
    );
    frame.render_widget(events, main[0]);

    let status_items = state
        .statuses()
        .iter()
        .map(|row| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    row.participant.to_string(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::raw(row.status.to_string()),
            ]))
        })
        .collect::<Vec<_>>();
    let statuses =
        List::new(status_items).block(Block::default().title("Agents").borders(Borders::ALL));
    frame.render_widget(statuses, main[1]);

    let composer = Paragraph::new(state.input())
        .block(
            Block::default()
                .title(format!("Composer [{}]", state.current_target_label()))
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(composer, vertical[1]);
}
