//! The TUI model. Every field is driven by `RuntimeEvent`s applied through
//! [`AppState::apply`]; the renderer reads it and the key handler mutates the
//! composer / drawer / scroll. No state is inferred from matching strings.

use std::collections::HashMap;

use crate::domain::event::{
    ApprovalId, ChatItem, LogEntry, MemberStatus, MessageId, MessageTarget, RuntimeEvent,
};
use crate::domain::team::{BackendKind, Effort, MemberId};
use crate::tui::attach::AttachRequest;
use crate::tui::completion::{self, Completion};
use crate::tui::composer::Composer;
use crate::tui::drawers::Drawer;

const MAX_LOGS: usize = 4000;

/// Header view of one member.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemberView {
    pub id: MemberId,
    pub display_name: String,
    pub backend: BackendKind,
    pub role: String,
    pub status: MemberStatus,
    pub session: Option<String>,
    pub cwd: String,
    pub effort: Option<Effort>,
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
    popup_selected: usize,
    popup_dismissed: bool,
    should_quit: bool,
    tools_expanded: bool,
    active_reasoning: HashMap<MemberId, String>,
    pending_user_messages: Vec<String>,
    header_selected: Option<usize>,
    attach_request: Option<AttachRequest>,
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
            popup_selected: 0,
            popup_dismissed: false,
            should_quit: false,
            tools_expanded: false,
            active_reasoning: HashMap::new(),
            pending_user_messages: Vec::new(),
            header_selected: None,
            attach_request: None,
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
                        session: m.session,
                        cwd: m.cwd,
                        effort: m.effort,
                    })
                    .collect();
            }
            RuntimeEvent::TurnStarted { .. } | RuntimeEvent::TurnFinished { .. } => {
                self.active_reasoning.clear();
            }
            RuntimeEvent::UserMessage { body, .. } => {
                if let Some(pos) = self.pending_user_messages.iter().position(|m| m == &body) {
                    self.pending_user_messages.remove(pos);
                } else {
                    self.push(ChatItem::User { body });
                }
            }
            RuntimeEvent::MemberStatus { member, status } => self.set_status(&member, status),
            RuntimeEvent::MemberEffort { member, effort } => {
                if let Some(view) = self.members.iter_mut().find(|m| m.id == member) {
                    view.effort = Some(effort);
                }
            }
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
                    && let Some(ChatItem::Agent {
                        text: body, member, ..
                    }) = self.chat.get_mut(idx)
                {
                    *body = text;
                    let m = member.clone();
                    self.active_reasoning.remove(&m);
                    self.set_status(&m, MemberStatus::Idle);
                }
                self.message_index.remove(&msg);
            }
            RuntimeEvent::Reasoning { member, text } => {
                self.active_reasoning.insert(member, text);
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
            RuntimeEvent::FileChange { member, files } => {
                self.push(ChatItem::Diff { member, files });
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
        if status == MemberStatus::Idle
            || status == MemberStatus::Failed
            || status == MemberStatus::NeedsApproval
        {
            self.active_reasoning.remove(member);
        }
    }

    fn member_meta(&self, member: &MemberId) -> (String, BackendKind) {
        self.members
            .iter()
            .find(|m| &m.id == member)
            .map(|m| (m.display_name.clone(), m.backend))
            .unwrap_or_else(|| (member.to_string(), BackendKind::Codex))
    }

    pub fn member_display(&self, member: &MemberId) -> String {
        self.members
            .iter()
            .find(|m| &m.id == member)
            .map(|m| m.display_name.clone())
            .unwrap_or_else(|| member.to_string())
    }

    pub fn has_active_message(&self, member_id: &MemberId) -> bool {
        self.message_index.values().any(|&idx| {
            if let Some(ChatItem::Agent { member, .. }) = self.chat.get(idx) {
                member == member_id
            } else {
                false
            }
        })
    }

    pub fn handle_user_message_submitted(&mut self, target: &MessageTarget, body: String) {
        self.pending_user_messages.push(body.clone());
        self.push(ChatItem::User { body });
        let targets = self.resolve_local_targets(target);
        for member_id in targets {
            self.set_status(&member_id, MemberStatus::Running);
        }
    }

    pub fn resolve_local_targets(&self, target: &MessageTarget) -> Vec<MemberId> {
        match target {
            MessageTarget::Default => self
                .members
                .first()
                .map(|m| vec![m.id.clone()])
                .unwrap_or_default(),
            MessageTarget::All => self.members.iter().map(|m| m.id.clone()).collect(),
            MessageTarget::Member(id) => self.resolve_local_named(std::slice::from_ref(id)),
            MessageTarget::Members(ids) => self.resolve_local_named(ids),
        }
    }

    fn resolve_local_named(&self, ids: &[MemberId]) -> Vec<MemberId> {
        let mut resolved = Vec::new();
        for id in ids {
            if let Some(member) = self
                .members
                .iter()
                .find(|m| m.id == *id || m.display_name.eq_ignore_ascii_case(id.as_str()))
                && !resolved.contains(&member.id)
            {
                resolved.push(member.id.clone());
            }
        }
        resolved
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
        self.drawer.clone()
    }
    pub fn header_selected(&self) -> Option<usize> {
        self.header_selected
    }
    pub fn scroll(&self) -> usize {
        self.scroll
    }
    pub fn composer(&self) -> &Composer {
        &self.composer
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

    /// Request attaching to the member at `idx`'s live backend session. Skipped
    /// (with a notice) while that member is running, to avoid two processes on
    /// one session.
    pub fn request_attach(&mut self, idx: usize) {
        let Some(member) = self.members.get(idx) else {
            self.header_selected = None;
            return;
        };
        if member.status == MemberStatus::Running {
            let name = member.display_name.clone();
            self.push(ChatItem::Notice {
                text: format!("{name} is running — /abort before attaching to its session"),
            });
            self.header_selected = None;
            return;
        }
        self.attach_request = Some(AttachRequest {
            display_name: member.display_name.clone(),
            backend: member.backend,
            session: member.session.clone(),
            cwd: member.cwd.clone(),
        });
        self.header_selected = None;
    }

    pub fn take_attach_request(&mut self) -> Option<AttachRequest> {
        self.attach_request.take()
    }

    // --- composer editing (each edit resets the completion popup) --------

    fn member_ids(&self) -> Vec<String> {
        self.members.iter().map(|m| m.id.to_string()).collect()
    }

    /// The active completion popup for the current composer text, if any.
    pub fn completion(&self) -> Option<Completion> {
        if self.popup_dismissed {
            return None;
        }
        completion::compute(&self.composer.head(), &self.member_ids())
    }

    pub fn popup_selected(&self) -> usize {
        self.popup_selected
    }

    fn reset_popup(&mut self) {
        self.popup_selected = 0;
        self.popup_dismissed = false;
    }

    pub fn insert_char(&mut self, ch: char) {
        self.header_selected = None;
        self.composer.insert(ch);
        self.reset_popup();
    }
    pub fn backspace(&mut self) {
        self.header_selected = None;
        self.composer.backspace();
        self.reset_popup();
    }
    pub fn delete_word(&mut self) {
        self.header_selected = None;
        self.composer.delete_word();
        self.reset_popup();
    }
    pub fn clear_composer(&mut self) {
        self.header_selected = None;
        self.composer.clear();
        self.reset_popup();
    }
    pub fn cursor_left(&mut self) {
        self.composer.left();
        self.reset_popup();
    }
    pub fn cursor_right(&mut self) {
        self.composer.right();
        self.reset_popup();
    }
    pub fn cursor_home(&mut self) {
        self.composer.home();
        self.reset_popup();
    }
    pub fn cursor_end(&mut self) {
        self.composer.end();
        self.reset_popup();
    }
    pub fn take_composer(&mut self) -> String {
        let text = self.composer.take();
        self.reset_popup();
        text
    }

    pub fn popup_up(&mut self) {
        self.popup_selected = self.popup_selected.saturating_sub(1);
    }
    pub fn popup_down(&mut self) {
        if let Some(completion) = self.completion()
            && self.popup_selected + 1 < completion.items.len()
        {
            self.popup_selected += 1;
        }
    }
    pub fn dismiss_popup(&mut self) {
        self.popup_dismissed = true;
    }

    /// Accept the highlighted completion. Returns true if the composer changed
    /// (false means the token already matched, so the caller should submit).
    pub fn accept_completion(&mut self) -> bool {
        let Some(completion) = self.completion() else {
            return false;
        };
        let index = self.popup_selected.min(completion.items.len() - 1);
        let insert = completion.items[index].insert.clone();
        let before = self.composer.text();
        self.composer.replace_token(completion.token_start, &insert);
        self.reset_popup();
        self.composer.text() != before
    }

    // --- UI actions -----------------------------------------------------

    pub fn toggle_drawer(&mut self, drawer: Drawer) {
        self.drawer = if self.drawer.as_ref() == Some(&drawer) {
            None
        } else {
            Some(drawer)
        };
    }

    pub fn close_drawer(&mut self) {
        self.drawer = None;
    }

    pub fn select_next_member(&mut self) {
        let len = self.members.len();
        if len == 0 {
            return;
        }
        let next_idx = match self.header_selected {
            None => 0,
            Some(idx) => (idx + 1) % len,
        };
        self.header_selected = Some(next_idx);
        self.reset_popup();
    }

    pub fn select_prev_member(&mut self) {
        let len = self.members.len();
        if len == 0 {
            return;
        }
        let prev_idx = match self.header_selected {
            None => len - 1,
            Some(idx) => (idx + len - 1) % len,
        };
        self.header_selected = Some(prev_idx);
        self.reset_popup();
    }

    pub fn clear_header_selection(&mut self) {
        self.header_selected = None;
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

    pub fn tools_expanded(&self) -> bool {
        self.tools_expanded
    }

    pub fn toggle_tools_expansion(&mut self) {
        self.tools_expanded = !self.tools_expanded;
    }

    pub fn active_reasoning(&self) -> &HashMap<MemberId, String> {
        &self.active_reasoning
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
                session: None,
                cwd: String::new(),
                effort: None,
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

    #[test]
    fn slash_opens_command_popup_and_accept_inserts() {
        let mut state = AppState::new(Vec::new());
        state.apply(ready());
        for ch in "/as".chars() {
            state.insert_char(ch);
        }
        let completion = state.completion().expect("command popup");
        assert_eq!(completion.items[0].insert, "/ask ");
        assert!(state.accept_completion());
        assert_eq!(state.composer().text(), "/ask ");
    }

    #[test]
    fn at_opens_member_popup_and_accepts() {
        let mut state = AppState::new(Vec::new());
        state.apply(ready());
        for ch in "@bu".chars() {
            state.insert_char(ch);
        }
        let completion = state.completion().expect("member popup");
        assert_eq!(completion.items[0].insert, "@builder ");
        state.accept_completion();
        assert_eq!(state.composer().text(), "@builder ");
    }

    #[test]
    fn dismiss_hides_popup_until_text_changes() {
        let mut state = AppState::new(Vec::new());
        state.apply(ready());
        state.insert_char('/');
        assert!(state.completion().is_some());
        state.dismiss_popup();
        assert!(state.completion().is_none());
        state.insert_char('a');
        assert!(state.completion().is_some());
    }

    #[test]
    fn header_roster_selection() {
        let mut state = AppState::new(Vec::new());
        state.members = vec![
            MemberView {
                id: MemberId::new("builder"),
                display_name: "Builder".to_string(),
                backend: BackendKind::Codex,
                role: "impl".to_string(),
                status: MemberStatus::Idle,
                session: None,
                cwd: String::new(),
                effort: None,
            },
            MemberView {
                id: MemberId::new("reviewer"),
                display_name: "Reviewer".to_string(),
                backend: BackendKind::Claude,
                role: "review".to_string(),
                status: MemberStatus::Idle,
                session: None,
                cwd: String::new(),
                effort: None,
            },
        ];
        assert_eq!(state.header_selected(), None);

        state.select_next_member();
        assert_eq!(state.header_selected(), Some(0)); // builder

        state.select_next_member();
        assert_eq!(state.header_selected(), Some(1)); // reviewer

        state.select_prev_member();
        assert_eq!(state.header_selected(), Some(0)); // builder

        state.insert_char('x');
        assert_eq!(state.header_selected(), None); // cleared on typing
    }
}
