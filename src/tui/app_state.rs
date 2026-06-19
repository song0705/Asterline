//! The TUI model. Every field is driven by `RuntimeEvent`s applied through
//! [`AppState::apply`]; the renderer reads it and the key handler mutates the
//! composer / drawer / scroll. No state is inferred from matching strings.

use std::collections::HashMap;

use crate::adapter::parser::summarize;
use crate::domain::event::{ApprovalId, ChatItem, LogEntry, MemberStatus, MessageId, RuntimeEvent};
use crate::domain::team::{BackendKind, MemberId};
use crate::tui::composer::Composer;
use crate::tui::drawers::Drawer;

const MAX_LOGS: usize = 4000;
const REASONING_MAX: usize = 200;

/// Header view of one member.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemberView {
    pub id: MemberId,
    pub display_name: String,
    pub backend: BackendKind,
    pub role: String,
    pub status: MemberStatus,
    pub session: Option<String>,
}

/// A pending approval awaiting a decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingApproval {
    pub id: ApprovalId,
    pub action: String,
    pub body: String,
}

pub struct AppState {
    team: String,
    workspace: String,
    members: Vec<MemberView>,
    chat: Vec<ChatItem>,
    message_index: HashMap<MessageId, usize>,
    tool_index: HashMap<String, usize>,
    logs: Vec<LogEntry>,
    pending_approvals: Vec<PendingApproval>,
    paused_routes: usize,
    composer: Composer,
    drawer: Option<Drawer>,
    scroll: usize,
    should_quit: bool,
}

impl AppState {
    /// Create with replayed chat history (empty for a fresh session).
    pub fn new(chat: Vec<ChatItem>) -> Self {
        Self {
            team: "Asterline".to_string(),
            workspace: String::new(),
            members: Vec::new(),
            chat,
            message_index: HashMap::new(),
            tool_index: HashMap::new(),
            logs: Vec::new(),
            pending_approvals: Vec::new(),
            paused_routes: 0,
            composer: Composer::new(),
            drawer: None,
            scroll: 0,
            should_quit: false,
        }
    }

    // --- applying runtime events ----------------------------------------

    pub fn apply(&mut self, event: RuntimeEvent) {
        match event {
            RuntimeEvent::Ready {
                team,
                workspace,
                members,
            } => {
                self.team = team;
                self.workspace = workspace;
                self.members = members
                    .into_iter()
                    .map(|m| MemberView {
                        id: m.id,
                        display_name: m.display_name,
                        backend: m.backend,
                        role: m.role,
                        status: m.status,
                        session: None,
                    })
                    .collect();
            }
            RuntimeEvent::TurnStarted { .. } | RuntimeEvent::TurnFinished { .. } => {}
            RuntimeEvent::UserMessage { body, .. } => {
                self.push(ChatItem::User { body });
            }
            RuntimeEvent::MemberStatus { member, status } => self.set_status(&member, status),
            RuntimeEvent::MessageStarted { msg, member, .. } => {
                let (display_name, backend) = self.member_meta(&member);
                let idx = self.push(ChatItem::Agent {
                    member,
                    display_name,
                    backend,
                    text: String::new(),
                });
                self.message_index.insert(msg, idx);
            }
            RuntimeEvent::MessageDelta { msg, text } => {
                if let Some(&idx) = self.message_index.get(&msg)
                    && let Some(ChatItem::Agent { text: body, .. }) = self.chat.get_mut(idx)
                {
                    body.push_str(&text);
                }
            }
            RuntimeEvent::MessageCompleted { msg, text } => {
                if let Some(&idx) = self.message_index.get(&msg)
                    && let Some(ChatItem::Agent { text: body, .. }) = self.chat.get_mut(idx)
                {
                    *body = text;
                }
                self.message_index.remove(&msg);
            }
            RuntimeEvent::Reasoning { member, text } => {
                let display = self.member_display(&member);
                self.push(ChatItem::Notice {
                    text: format!("{display} · thinking: {}", summarize(&text, REASONING_MAX)),
                });
            }
            RuntimeEvent::ToolStarted {
                member,
                tool_id,
                name,
                summary,
            } => {
                let idx = self.push(ChatItem::Tool {
                    member,
                    name,
                    summary,
                    ok: None,
                });
                self.tool_index.insert(tool_id, idx);
            }
            RuntimeEvent::ToolCompleted {
                member,
                tool_id,
                ok,
                summary,
            } => {
                if let Some(idx) = self.tool_index.remove(&tool_id)
                    && let Some(ChatItem::Tool {
                        ok: cell_ok,
                        summary: cell_summary,
                        ..
                    }) = self.chat.get_mut(idx)
                {
                    *cell_ok = Some(ok);
                    if !summary.is_empty() {
                        *cell_summary = summary;
                    }
                } else {
                    self.push(ChatItem::Tool {
                        member,
                        name: "tool".to_string(),
                        summary,
                        ok: Some(ok),
                    });
                }
            }
            RuntimeEvent::Route { from, to, body, .. } => {
                self.push(ChatItem::Route { from, to, body });
            }
            RuntimeEvent::RouteError {
                from,
                target,
                reason,
                ..
            } => {
                self.push(ChatItem::Error {
                    member: Some(from),
                    message: format!("route to {target} failed: {reason}"),
                });
            }
            RuntimeEvent::RoutePaused {
                from,
                to,
                reason,
                queued,
                ..
            } => {
                self.paused_routes = queued;
                self.push(ChatItem::Notice {
                    text: format!(
                        "route paused {from} → {}: {reason} (queued {queued}; /retry to resume)",
                        to.join(", ")
                    ),
                });
            }
            RuntimeEvent::SessionUpdated { member, session } => {
                if let Some(view) = self.members.iter_mut().find(|m| m.id == member) {
                    view.session = Some(session.0);
                }
            }
            RuntimeEvent::ApprovalRequested {
                id, action, body, ..
            } => {
                self.pending_approvals.push(PendingApproval {
                    id,
                    action: action.clone(),
                    body: body.clone(),
                });
                self.push(ChatItem::Notice {
                    text: format!("approval needed [{action}]: {body} — /approve or /reject"),
                });
            }
            RuntimeEvent::ApprovalResolved { id, decision } => {
                self.pending_approvals.retain(|a| a.id != id);
                self.push(ChatItem::Notice {
                    text: format!("approval {}", decision.as_str()),
                });
            }
            RuntimeEvent::MemberError { member, message } => {
                self.push(ChatItem::Error {
                    member: Some(member),
                    message,
                });
            }
            RuntimeEvent::Log(entry) => {
                self.logs.push(entry);
                if self.logs.len() > MAX_LOGS {
                    self.logs.drain(0..self.logs.len() - MAX_LOGS);
                }
            }
            RuntimeEvent::Notice(text) => {
                self.push(ChatItem::Notice { text });
            }
        }
    }

    fn push(&mut self, item: ChatItem) -> usize {
        self.chat.push(item);
        self.chat.len() - 1
    }

    fn set_status(&mut self, member: &MemberId, status: MemberStatus) {
        if let Some(view) = self.members.iter_mut().find(|m| &m.id == member) {
            view.status = status;
        }
    }

    fn member_meta(&self, member: &MemberId) -> (String, BackendKind) {
        self.members
            .iter()
            .find(|m| &m.id == member)
            .map(|m| (m.display_name.clone(), m.backend))
            .unwrap_or_else(|| (member.to_string(), BackendKind::Codex))
    }

    fn member_display(&self, member: &MemberId) -> String {
        self.members
            .iter()
            .find(|m| &m.id == member)
            .map(|m| m.display_name.clone())
            .unwrap_or_else(|| member.to_string())
    }

    // --- accessors for the renderer -------------------------------------

    pub fn team(&self) -> &str {
        &self.team
    }
    pub fn workspace(&self) -> &str {
        &self.workspace
    }
    pub fn members(&self) -> &[MemberView] {
        &self.members
    }
    pub fn chat(&self) -> &[ChatItem] {
        &self.chat
    }
    pub fn logs(&self) -> &[LogEntry] {
        &self.logs
    }
    pub fn pending_approvals(&self) -> &[PendingApproval] {
        &self.pending_approvals
    }
    pub fn paused_routes(&self) -> usize {
        self.paused_routes
    }
    pub fn drawer(&self) -> Option<Drawer> {
        self.drawer
    }
    pub fn scroll(&self) -> usize {
        self.scroll
    }
    pub fn composer(&self) -> &Composer {
        &self.composer
    }
    pub fn composer_mut(&mut self) -> &mut Composer {
        &mut self.composer
    }
    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    pub fn running_count(&self) -> usize {
        self.members
            .iter()
            .filter(|m| m.status == MemberStatus::Running)
            .count()
    }

    pub fn first_pending_approval(&self) -> Option<ApprovalId> {
        self.pending_approvals.first().map(|a| a.id)
    }

    // --- UI actions -----------------------------------------------------

    pub fn toggle_drawer(&mut self, drawer: Drawer) {
        self.drawer = if self.drawer == Some(drawer) {
            None
        } else {
            Some(drawer)
        };
    }

    pub fn close_drawer(&mut self) {
        self.drawer = None;
    }

    pub fn scroll_up(&mut self) {
        self.scroll = self.scroll.saturating_add(1);
    }

    pub fn scroll_down(&mut self) {
        self.scroll = self.scroll.saturating_sub(1);
    }

    pub fn reset_scroll(&mut self) {
        self.scroll = 0;
    }

    pub fn quit(&mut self) {
        self.should_quit = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::event::{AgentSessionId, ApprovalDecision, MemberSummary, TurnId};

    fn ready() -> RuntimeEvent {
        RuntimeEvent::Ready {
            team: "mixed".to_string(),
            workspace: "/tmp/ws".to_string(),
            members: vec![MemberSummary {
                id: MemberId::new("builder"),
                display_name: "Builder".to_string(),
                backend: BackendKind::Codex,
                role: "impl".to_string(),
                status: MemberStatus::Idle,
            }],
        }
    }

    #[test]
    fn ready_populates_header() {
        let mut state = AppState::new(Vec::new());
        state.apply(ready());
        assert_eq!(state.team(), "mixed");
        assert_eq!(state.members().len(), 1);
    }

    #[test]
    fn streaming_message_builds_agent_cell() {
        let mut state = AppState::new(Vec::new());
        state.apply(ready());
        let builder = MemberId::new("builder");
        state.apply(RuntimeEvent::MessageStarted {
            msg: MessageId(1),
            turn: TurnId(1),
            member: builder.clone(),
        });
        state.apply(RuntimeEvent::MessageDelta {
            msg: MessageId(1),
            text: "Hel".to_string(),
        });
        state.apply(RuntimeEvent::MessageDelta {
            msg: MessageId(1),
            text: "lo".to_string(),
        });
        state.apply(RuntimeEvent::MessageCompleted {
            msg: MessageId(1),
            text: "Hello".to_string(),
        });

        assert!(matches!(
            state.chat().last(),
            Some(ChatItem::Agent { text, .. }) if text == "Hello"
        ));
    }

    #[test]
    fn tool_completion_updates_existing_cell() {
        let mut state = AppState::new(Vec::new());
        state.apply(ready());
        let builder = MemberId::new("builder");
        state.apply(RuntimeEvent::ToolStarted {
            member: builder.clone(),
            tool_id: "t1".to_string(),
            name: "shell".to_string(),
            summary: "cargo test".to_string(),
        });
        state.apply(RuntimeEvent::ToolCompleted {
            member: builder,
            tool_id: "t1".to_string(),
            ok: true,
            summary: "cargo test".to_string(),
        });
        // One tool cell, now marked ok.
        let tools: Vec<_> = state
            .chat()
            .iter()
            .filter(|i| matches!(i, ChatItem::Tool { .. }))
            .collect();
        assert_eq!(tools.len(), 1);
        assert!(matches!(tools[0], ChatItem::Tool { ok: Some(true), .. }));
    }

    #[test]
    fn logs_do_not_enter_chat() {
        let mut state = AppState::new(Vec::new());
        state.apply(RuntimeEvent::Log(LogEntry::warn("builder", "stderr noise")));
        assert!(state.chat().is_empty());
        assert_eq!(state.logs().len(), 1);
    }

    #[test]
    fn approvals_track_pending_and_resolve() {
        let mut state = AppState::new(Vec::new());
        state.apply(RuntimeEvent::ApprovalRequested {
            id: ApprovalId(1),
            member: None,
            action: "git".to_string(),
            body: "git push".to_string(),
        });
        assert_eq!(state.first_pending_approval(), Some(ApprovalId(1)));
        state.apply(RuntimeEvent::ApprovalResolved {
            id: ApprovalId(1),
            decision: ApprovalDecision::Approve,
        });
        assert!(state.first_pending_approval().is_none());
    }

    #[test]
    fn member_status_drives_running_count() {
        let mut state = AppState::new(Vec::new());
        state.apply(ready());
        let builder = MemberId::new("builder");
        state.apply(RuntimeEvent::MemberStatus {
            member: builder.clone(),
            status: MemberStatus::Running,
        });
        assert_eq!(state.running_count(), 1);
        state.apply(RuntimeEvent::MemberStatus {
            member: builder,
            status: MemberStatus::Idle,
        });
        assert_eq!(state.running_count(), 0);
    }

    #[test]
    fn drawer_toggles() {
        let mut state = AppState::new(Vec::new());
        state.toggle_drawer(Drawer::Logs);
        assert_eq!(state.drawer(), Some(Drawer::Logs));
        state.toggle_drawer(Drawer::Logs);
        assert_eq!(state.drawer(), None);
        let _ = AgentSessionId("x".to_string());
    }
}
