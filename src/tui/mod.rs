pub mod input;
pub mod layout;
pub mod widgets;

use std::{io, time::Duration};

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::{
    runtime::workflow::{ApprovalDecision, FakeWorkflow},
    tui::{input::TargetSelector, widgets::AgentStatusRow},
    types::{AgentId, AgentStatus, Participant, RouteTarget},
};

#[derive(Debug)]
pub struct TuiState {
    target_selector: TargetSelector,
    input: String,
    events: Vec<String>,
    statuses: Vec<AgentStatusRow>,
    workflow: FakeWorkflow,
    view: TuiView,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TuiView {
    Events,
    Agents,
    Approvals,
    Help,
    Relays,
    Messages,
    Terminal,
}

impl TuiState {
    pub fn prototype() -> Self {
        Self::with_workflow(
            FakeWorkflow::in_memory().expect("in-memory workflow should initialize"),
            "codex=fake, claude=fake",
        )
    }

    pub fn with_workflow(workflow: FakeWorkflow, backend_label: impl Into<String>) -> Self {
        Self {
            target_selector: TargetSelector::new(),
            input: String::new(),
            events: vec![
                format!("System: runtime ready ({})", backend_label.into()),
                "Start: type a task in Composer and press Enter. Use Tab to change target, F1 for help, Ctrl-C to exit.".to_string(),
                "Codex -> Claude: Please review Codex next step for: build parser".to_string(),
            ],
            statuses: vec![
                AgentStatusRow {
                    participant: Participant::Team,
                    status: AgentStatus::Idle,
                },
                AgentStatusRow {
                    participant: Participant::Agent(AgentId::Codex),
                    status: AgentStatus::Idle,
                },
                AgentStatusRow {
                    participant: Participant::Agent(AgentId::Claude),
                    status: AgentStatus::Waiting,
                },
            ],
            workflow,
            view: TuiView::Events,
        }
    }

    pub fn current_target(&self) -> RouteTarget {
        self.target_selector.selected()
    }

    pub fn current_target_label(&self) -> String {
        self.target_selector.selected_label()
    }

    pub fn input(&self) -> &str {
        &self.input
    }

    pub fn events(&self) -> &[String] {
        &self.events
    }

    pub fn visible_events(&self) -> Vec<String> {
        match self.view {
            TuiView::Events => self.events.clone(),
            TuiView::Agents => self
                .statuses
                .iter()
                .map(|row| format!("{}: {}", row.participant, row.status))
                .collect(),
            TuiView::Approvals => self
                .workflow
                .pending_approval_summaries()
                .unwrap_or_else(|err| vec![format!("Approval queue error: {err}")]),
            TuiView::Help => help_lines(),
            TuiView::Relays => self.workflow.pending_relay_summaries(),
            TuiView::Messages => self
                .workflow
                .message_summaries()
                .unwrap_or_else(|err| vec![format!("Message log error: {err}")]),
            TuiView::Terminal => self
                .workflow
                .terminal_event_raw_log()
                .unwrap_or_else(|err| vec![format!("Terminal log error: {err}")]),
        }
    }

    pub fn log_pane_title(&self) -> &'static str {
        match self.view {
            TuiView::Events => "Events",
            TuiView::Agents => "Agents",
            TuiView::Approvals => "Approvals",
            TuiView::Help => "Help",
            TuiView::Relays => "Relays",
            TuiView::Messages => "Messages",
            TuiView::Terminal => "Terminal",
        }
    }

    pub fn statuses(&self) -> &[AgentStatusRow] {
        &self.statuses
    }

    pub fn next_target(&mut self) {
        self.target_selector.next();
    }

    pub fn toggle_messages(&mut self) {
        self.view = match self.view {
            TuiView::Messages => TuiView::Events,
            _ => TuiView::Messages,
        };
    }

    pub fn show_events(&mut self) {
        self.view = TuiView::Events;
    }

    pub fn show_agents(&mut self) {
        self.view = TuiView::Agents;
    }

    pub fn show_help(&mut self) {
        self.view = TuiView::Help;
    }

    pub fn show_terminal(&mut self) {
        self.view = TuiView::Terminal;
    }

    pub fn show_approvals(&mut self) {
        self.view = TuiView::Approvals;
    }

    pub fn show_relays(&mut self) {
        self.view = TuiView::Relays;
    }

    pub fn auto_relay_paused(&self) -> bool {
        self.workflow.auto_relay_paused()
    }

    pub fn toggle_auto_relay_pause(&mut self) {
        if self.workflow.toggle_auto_relay_pause() {
            self.events.push(
                "Relay paused by user: automatic agent-to-agent delivery disabled".to_string(),
            );
            self.set_status(Participant::Team, AgentStatus::Waiting);
        } else {
            self.events.push(
                "Relay resumed by user: automatic agent-to-agent delivery enabled".to_string(),
            );
            self.set_status(Participant::Team, AgentStatus::Idle);
        }
    }

    pub fn approve_next_pending(&mut self) {
        match self.view {
            TuiView::Relays => self.replay_next_pending_relay(),
            _ => self.decide_next_pending_approval(ApprovalDecision::Approve),
        }
    }

    pub fn reject_next_pending(&mut self) {
        match self.view {
            TuiView::Relays => self.reject_next_pending_relay(),
            _ => self.decide_next_pending_approval(ApprovalDecision::Reject),
        }
    }

    pub fn push_char(&mut self, ch: char) {
        self.input.push(ch);
    }

    pub fn backspace(&mut self) {
        self.input.pop();
    }

    pub fn submit(&mut self) {
        let body = self.input.trim().to_string();
        if body.is_empty() {
            return;
        }

        let target = self.current_target();
        self.mark_target_running(target);
        match self.workflow.handle_user_message(target, &body) {
            Ok(events) => {
                self.apply_status_events(&events);
                self.events.extend(events);
            }
            Err(err) => self.events.push(format!("Workflow error: {err}")),
        }

        self.input.clear();
    }

    fn mark_target_running(&mut self, target: RouteTarget) {
        match target.to {
            Participant::Team => {
                self.set_status(Participant::Team, AgentStatus::Running);
                self.set_status(Participant::Agent(AgentId::Claude), AgentStatus::Running);
            }
            Participant::Agent(agent) => {
                self.set_status(Participant::Agent(agent), AgentStatus::Running);
            }
            Participant::You => {}
        }
    }

    fn apply_status_events(&mut self, events: &[String]) {
        for event in events {
            if event.contains("Approval queued") {
                self.set_status(Participant::Team, AgentStatus::NeedsApproval);
            }

            if event.contains("Relay paused by user") {
                self.set_status(Participant::Team, AgentStatus::Waiting);
            }

            if event.starts_with("Codex error:") {
                self.set_status(
                    Participant::Agent(AgentId::Codex),
                    status_for_error_event(event),
                );
            } else if event.starts_with("Claude error:") {
                self.set_status(
                    Participant::Agent(AgentId::Claude),
                    status_for_error_event(event),
                );
            } else {
                if event.contains("Codex ->") {
                    self.set_status(Participant::Agent(AgentId::Codex), AgentStatus::Idle);
                }
                if event.contains("Claude ->") {
                    self.set_status(Participant::Agent(AgentId::Claude), AgentStatus::Idle);
                }
                if event.contains("Team ->") || event.starts_with("You -> Team:") {
                    self.set_status(Participant::Team, AgentStatus::Idle);
                }
            }
        }
    }

    fn set_status(&mut self, participant: Participant, status: AgentStatus) {
        if let Some(row) = self
            .statuses
            .iter_mut()
            .find(|row| row.participant == participant)
        {
            row.status = status;
        }
    }

    fn decide_next_pending_approval(&mut self, decision: ApprovalDecision) {
        match self.workflow.decide_next_pending_approval(decision) {
            Ok(Some(event)) => {
                self.events.push(event);
                match self.workflow.pending_approval_count() {
                    Ok(0) => self.set_status(Participant::Team, AgentStatus::Idle),
                    Ok(_) => self.set_status(Participant::Team, AgentStatus::NeedsApproval),
                    Err(err) => self.events.push(format!("Approval queue error: {err}")),
                }
            }
            Ok(None) => self.events.push("Approval queue empty".to_string()),
            Err(err) => self.events.push(format!("Approval decision error: {err}")),
        }
    }

    fn replay_next_pending_relay(&mut self) {
        match self.workflow.replay_next_pending_relay() {
            Ok(Some(events)) => {
                self.apply_status_events(&events);
                self.events.extend(events);
                if self.workflow.pending_relay_count() > 0 {
                    self.set_status(Participant::Team, AgentStatus::Waiting);
                }
            }
            Ok(None) => self.events.push("Relay queue empty".to_string()),
            Err(err) => self.events.push(format!("Relay replay error: {err}")),
        }
    }

    fn reject_next_pending_relay(&mut self) {
        match self.workflow.reject_next_pending_relay() {
            Ok(Some(event)) => {
                self.events.push(event);
                if self.workflow.pending_relay_count() == 0 && !self.workflow.auto_relay_paused() {
                    self.set_status(Participant::Team, AgentStatus::Idle);
                }
            }
            Ok(None) => self.events.push("Relay queue empty".to_string()),
            Err(err) => self.events.push(format!("Relay reject error: {err}")),
        }
    }
}

fn help_lines() -> Vec<String> {
    vec![
        "First step: type a task in Composer, then press Enter.".to_string(),
        "Tab: cycle target route.".to_string(),
        "Ctrl-L: messages. Ctrl-T: terminal. Ctrl-A: agents.".to_string(),
        "Ctrl-R: pause/resume automatic relay. Ctrl-E: pending relays.".to_string(),
        "Ctrl-P: approvals. Ctrl-Y: approve/replay. Ctrl-N: reject.".to_string(),
        "Esc: back to events. Ctrl-C: exit.".to_string(),
    ]
}

fn status_for_error_event(event: &str) -> AgentStatus {
    let lower = event.to_ascii_lowercase();
    if lower.contains("login")
        || lower.contains("auth")
        || lower.contains("unauthorized")
        || lower.contains("token")
        || lower.contains("not authenticated")
    {
        AgentStatus::NeedsLogin
    } else {
        AgentStatus::Failed
    }
}

pub fn run(mut state: TuiState) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run_loop(&mut terminal, &mut state);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut TuiState,
) -> io::Result<()> {
    loop {
        terminal.draw(|frame| widgets::render(frame, state))?;

        if !event::poll(Duration::from_millis(200))? {
            continue;
        }

        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }

            match (key.code, key.modifiers) {
                (KeyCode::Char('c'), KeyModifiers::CONTROL) => return Ok(()),
                (KeyCode::Char('a'), KeyModifiers::CONTROL) => state.show_agents(),
                (KeyCode::Char('e'), KeyModifiers::CONTROL) => state.show_relays(),
                (KeyCode::Char('l'), KeyModifiers::CONTROL) => state.toggle_messages(),
                (KeyCode::Char('p'), KeyModifiers::CONTROL) => state.show_approvals(),
                (KeyCode::Char('r'), KeyModifiers::CONTROL) => state.toggle_auto_relay_pause(),
                (KeyCode::Char('t'), KeyModifiers::CONTROL) => state.show_terminal(),
                (KeyCode::Char('y'), KeyModifiers::CONTROL) => state.approve_next_pending(),
                (KeyCode::Char('n'), KeyModifiers::CONTROL) => state.reject_next_pending(),
                (KeyCode::F(1), _) => state.show_help(),
                (KeyCode::Esc, _) => state.show_events(),
                (KeyCode::Tab, _) => state.next_target(),
                (KeyCode::Enter, _) => state.submit(),
                (KeyCode::Backspace, _) => state.backspace(),
                (KeyCode::Char(ch), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                    state.push_char(ch);
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prototype_state_shows_fake_status_and_inter_agent_message() {
        let state = TuiState::prototype();

        assert_eq!(state.current_target_label(), "You -> Team");
        assert!(
            state
                .events()
                .iter()
                .any(|event| event.contains("type a task in Composer"))
        );
        assert!(
            state
                .events()
                .iter()
                .any(|event| event.contains("Codex -> Claude"))
        );
        assert!(state.statuses().iter().any(|row| {
            row.participant == Participant::Agent(AgentId::Codex) && row.status == AgentStatus::Idle
        }));
    }

    #[test]
    fn help_view_explains_first_step_and_can_return_to_events() {
        let mut state = TuiState::prototype();

        state.show_help();

        assert_eq!(state.log_pane_title(), "Help");
        assert!(
            state
                .visible_events()
                .iter()
                .any(|line| line == "First step: type a task in Composer, then press Enter.")
        );

        state.show_events();

        assert_eq!(state.log_pane_title(), "Events");
    }

    #[test]
    fn submit_to_codex_adds_visible_user_event_and_fake_relay() {
        let mut state = TuiState::prototype();
        state.next_target();
        state.push_char('h');
        state.push_char('i');

        state.submit();

        assert_eq!(state.input(), "");
        assert!(
            state
                .events()
                .iter()
                .any(|event| event == "You -> Codex: hi")
        );
        assert!(
            state
                .events()
                .iter()
                .any(|event| event.contains("Codex -> Claude"))
        );
        assert!(
            state
                .events()
                .iter()
                .any(|event| event.contains("Relay paused"))
        );
    }

    #[test]
    fn submit_to_team_routes_through_claude_and_codex() {
        let mut state = TuiState::prototype();
        state.push_char('p');
        state.push_char('l');
        state.push_char('a');
        state.push_char('n');

        state.submit();

        assert!(
            state
                .events()
                .iter()
                .any(|event| event == "You -> Team: plan")
        );
        assert!(
            state
                .events()
                .iter()
                .any(|event| event == "Team -> Claude: default planning route")
        );
        assert!(
            state
                .events()
                .iter()
                .any(|event| event.contains("Claude -> Codex"))
        );
        assert!(state.statuses().iter().any(|row| {
            row.participant == Participant::Agent(AgentId::Claude)
                && row.status == AgentStatus::Idle
        }));
    }

    #[test]
    fn terminal_view_shows_captured_terminal_events() {
        let mut state = TuiState::prototype();
        state.show_terminal();

        assert_eq!(state.log_pane_title(), "Terminal");
        assert_eq!(state.visible_events(), Vec::<String>::new());
        state.toggle_messages();
        assert_eq!(state.log_pane_title(), "Messages");
    }

    #[test]
    fn terminal_view_shows_raw_multiline_terminal_events() {
        let store = crate::store::sqlite::SqliteStore::in_memory().unwrap();
        store
            .insert_terminal_event(AgentId::Codex, "pty", "raw line 1\nraw line 2")
            .expect("terminal event should insert");
        let workflow =
            FakeWorkflow::with_store(store, crate::router::relay::DEFAULT_MAX_AUTO_RELAYS).unwrap();
        let mut state = TuiState::with_workflow(workflow, "codex=test, claude=test");

        state.show_terminal();

        assert_eq!(
            state.visible_events(),
            vec!["#1 Codex pty", "raw line 1", "raw line 2"]
        );
    }

    #[test]
    fn agents_view_lists_current_status_rows() {
        let mut state = TuiState::prototype();

        state.show_agents();

        assert_eq!(state.log_pane_title(), "Agents");
        assert_eq!(
            state.visible_events(),
            vec!["Team: idle", "Codex: idle", "Claude: waiting"]
        );
    }

    #[test]
    fn relay_pause_toggle_updates_state_and_team_status() {
        let mut state = TuiState::prototype();

        state.toggle_auto_relay_pause();

        assert!(state.auto_relay_paused());
        assert!(state.events().iter().any(|event| {
            event == "Relay paused by user: automatic agent-to-agent delivery disabled"
        }));
        assert!(state.statuses().iter().any(|row| {
            row.participant == Participant::Team && row.status == AgentStatus::Waiting
        }));

        state.toggle_auto_relay_pause();

        assert!(!state.auto_relay_paused());
        assert!(state.events().iter().any(|event| {
            event == "Relay resumed by user: automatic agent-to-agent delivery enabled"
        }));
        assert!(state.statuses().iter().any(|row| {
            row.participant == Participant::Team && row.status == AgentStatus::Idle
        }));
    }

    #[test]
    fn paused_relay_stops_automatic_agent_to_agent_delivery_in_tui() {
        let mut state = TuiState::prototype();
        state.toggle_auto_relay_pause();
        let event_count_before_submit = state.events().len();
        for ch in "plan".chars() {
            state.push_char(ch);
        }

        state.submit();

        let new_events = &state.events()[event_count_before_submit..];
        assert!(
            new_events
                .iter()
                .any(|event| event == "Team -> Claude: default planning route")
        );
        assert!(
            new_events
                .iter()
                .any(|event| event.contains("Claude -> Codex"))
        );
        assert!(new_events.iter().any(|event| {
            event == "Relay paused by user for thread-1: Claude -> Codex queued as #1"
        }));
        assert!(
            !new_events
                .iter()
                .any(|event| event.contains("Codex -> Claude"))
        );
    }

    #[test]
    fn relays_view_lists_pending_relays() {
        let mut state = TuiState::prototype();
        state.toggle_auto_relay_pause();
        for ch in "plan".chars() {
            state.push_char(ch);
        }
        state.submit();

        state.show_relays();

        assert_eq!(state.log_pane_title(), "Relays");
        assert_eq!(
            state.visible_events(),
            vec!["#1 [thread-1] Claude -> Codex: Implement the plan for: plan"]
        );
    }

    #[test]
    fn relays_view_replays_pending_relay_with_approve_key() {
        let mut state = TuiState::prototype();
        state.toggle_auto_relay_pause();
        for ch in "plan".chars() {
            state.push_char(ch);
        }
        state.submit();
        state.show_relays();

        state.approve_next_pending();

        assert!(state.events().iter().any(|event| {
            event == "Relay #1 replayed: Claude -> Codex: Implement the plan for: plan"
        }));
        assert!(
            state
                .events()
                .iter()
                .any(|event| { event.contains("Codex -> Claude") })
        );
        assert_eq!(
            state.visible_events(),
            vec![
                "#2 [thread-1] Codex -> Claude: Please review Codex next step for: Implement the plan for: plan"
            ]
        );
    }

    #[test]
    fn relays_view_rejects_pending_relay_with_reject_key() {
        let mut state = TuiState::prototype();
        state.toggle_auto_relay_pause();
        for ch in "plan".chars() {
            state.push_char(ch);
        }
        state.submit();
        state.show_relays();

        state.reject_next_pending();

        assert!(state.events().iter().any(|event| {
            event == "Relay #1 rejected: Claude -> Codex: Implement the plan for: plan"
        }));
        assert_eq!(state.visible_events(), Vec::<String>::new());
    }

    #[test]
    fn message_view_reads_persisted_messages() {
        let mut state = TuiState::prototype();
        for ch in "plan".chars() {
            state.push_char(ch);
        }
        state.submit();

        state.toggle_messages();

        assert_eq!(state.log_pane_title(), "Messages");
        assert!(
            state
                .visible_events()
                .iter()
                .any(|message| message == "You -> Team: plan")
        );
    }

    #[test]
    fn approval_event_marks_team_needs_approval() {
        let mut state = TuiState::prototype();
        for ch in "run git status".chars() {
            state.push_char(ch);
        }

        state.submit();

        assert!(state.statuses().iter().any(|row| {
            row.participant == Participant::Team && row.status == AgentStatus::NeedsApproval
        }));
    }

    #[test]
    fn approval_view_lists_and_decides_pending_approval() {
        let mut state = TuiState::prototype();
        for ch in "run git status".chars() {
            state.push_char(ch);
        }
        state.submit();

        state.show_approvals();

        assert_eq!(state.log_pane_title(), "Approvals");
        assert_eq!(
            state.visible_events(),
            vec!["#1 [thread-1] git: run git status"]
        );

        state.approve_next_pending();

        assert_eq!(state.visible_events(), Vec::<String>::new());
        assert!(
            state
                .events()
                .iter()
                .any(|event| { event == "Approval #1 approved for git: run git status" })
        );
        assert!(state.statuses().iter().any(|row| {
            row.participant == Participant::Team && row.status == AgentStatus::Idle
        }));
    }

    #[test]
    fn auth_error_event_maps_to_needs_login() {
        assert_eq!(
            status_for_error_event("Codex error: not authenticated, run codex login"),
            AgentStatus::NeedsLogin
        );
        assert_eq!(
            status_for_error_event("Claude error: process failed"),
            AgentStatus::Failed
        );
    }
}
