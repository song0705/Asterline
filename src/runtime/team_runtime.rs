//! The team runtime core.
//!
//! Pure orchestration logic: [`TeamRuntime::on_ui_command`] and
//! [`TeamRuntime::on_agent_event`] take an input and return the
//! [`RuntimeEvent`]s to emit plus the [`RunAction`]s to dispatch. All threading
//! and child-process work lives in the transport layer (`agent_runner` / the
//! `run` loop), so the core is fully unit-testable without spawning anything.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::domain::event::{
    AgentEvent, AgentSessionId, ApprovalDecision, ApprovalId, LogEntry, MemberStatus,
    MemberSummary, MessageId, MessageTarget, RuntimeEvent, TurnId, UiCommand,
};
use crate::domain::team::{BackendKind, MemberId, TeamConfig};
use crate::router::{self, RelayDecision, RelayGuard, parse_agent_output};
use crate::runtime::approval::risky_action_kind;
use crate::runtime::session_registry::SessionRegistry;
use crate::store::sqlite::SqliteStore;

/// What the core wants the transport layer to do after handling an input.
#[derive(Default)]
pub struct RuntimeStep {
    pub events: Vec<RuntimeEvent>,
    pub actions: Vec<RunAction>,
}

/// A run the transport layer should start for a member.
pub struct RunAction {
    pub member: MemberId,
    pub prompt: String,
    pub session: Option<AgentSessionId>,
    pub cancel: Arc<AtomicBool>,
}

struct RunningState {
    cancel: Arc<AtomicBool>,
    turn: TurnId,
    message: Option<MessageId>,
    text: String,
}

struct QueuedPrompt {
    turn: TurnId,
    prompt: String,
}

struct MemberState {
    status: MemberStatus,
    queue: VecDeque<QueuedPrompt>,
    running: Option<RunningState>,
    tools: HashMap<String, String>,
}

impl MemberState {
    fn new() -> Self {
        Self {
            status: MemberStatus::Idle,
            queue: VecDeque::new(),
            running: None,
            tools: HashMap::new(),
        }
    }
}

struct PausedRoute {
    turn: TurnId,
    from: MemberId,
    to_members: Vec<MemberId>,
    to_labels: Vec<String>,
    body: String,
}

struct HeldApproval {
    turn: TurnId,
    targets: Vec<MemberId>,
    body: String,
}

pub struct TeamRuntime {
    config: TeamConfig,
    store: SqliteStore,
    relay: RelayGuard,
    sessions: SessionRegistry,
    members: HashMap<MemberId, MemberState>,
    relay_paused: bool,
    paused_routes: VecDeque<PausedRoute>,
    held_approvals: HashMap<ApprovalId, HeldApproval>,
    last_user: Option<(MessageTarget, String)>,
    next_message_id: u64,
    approvals_enabled: bool,
}

impl TeamRuntime {
    pub fn new(config: TeamConfig, store: SqliteStore) -> Self {
        let _ = store.upsert_team(&config);
        let sessions = SessionRegistry::from_store(&store, &config.all_member_ids());
        let members = config
            .members
            .iter()
            .map(|m| (m.id.clone(), MemberState::new()))
            .collect();
        let relay = RelayGuard::new(config.max_auto_relays);
        Self {
            config,
            store,
            relay,
            sessions,
            members,
            relay_paused: false,
            paused_routes: VecDeque::new(),
            held_approvals: HashMap::new(),
            last_user: None,
            next_message_id: 0,
            approvals_enabled: true,
        }
    }

    /// Disable the risky-action approval gate (used in tests and by `--debug`).
    pub fn with_approvals(mut self, enabled: bool) -> Self {
        self.approvals_enabled = enabled;
        self
    }

    /// Snapshot for the TUI's initial `Ready` event.
    pub fn ready_event(&self) -> RuntimeEvent {
        let members = self
            .config
            .members
            .iter()
            .map(|m| MemberSummary {
                id: m.id.clone(),
                display_name: m.display_name.clone(),
                backend: m.backend,
                role: m.role.clone(),
                status: self
                    .members
                    .get(&m.id)
                    .map(|s| s.status)
                    .unwrap_or(MemberStatus::Idle),
                session: self.sessions.get(&m.id).map(|s| s.0.clone()),
            })
            .collect();
        RuntimeEvent::Ready {
            team: self.config.name.clone(),
            workspace: self.config.workspace.display().to_string(),
            members,
        }
    }

    // === command handling ===============================================

    pub fn on_ui_command(&mut self, cmd: UiCommand) -> RuntimeStep {
        let mut step = RuntimeStep::default();
        match cmd {
            UiCommand::UserMessage { target, body } => {
                self.handle_user_message(target, body, &mut step)
            }
            UiCommand::Cancel { member } => self.handle_cancel(member, &mut step),
            UiCommand::Retry => {
                if let Some((target, body)) = self.last_user.clone() {
                    self.handle_user_message(target, body, &mut step);
                } else {
                    step.events
                        .push(RuntimeEvent::Notice("nothing to retry".to_string()));
                }
            }
            UiCommand::Approve { id, decision } => self.handle_approval(id, decision, &mut step),
            UiCommand::SetRelayPaused(paused) => {
                self.relay_paused = paused;
                step.events.push(RuntimeEvent::Notice(if paused {
                    "automatic agent-to-agent relay paused".to_string()
                } else {
                    "automatic agent-to-agent relay resumed".to_string()
                }));
            }
            UiCommand::ResolvePausedRoute { resume } => {
                self.resolve_next_paused_route(resume, &mut step)
            }
            UiCommand::Shutdown => self.handle_cancel(None, &mut step),
        }
        step
    }

    fn handle_user_message(&mut self, target: MessageTarget, body: String, step: &mut RuntimeStep) {
        self.last_user = Some((target.clone(), body.clone()));
        let (targets, unknown) = self.resolve_message_target(&target);
        for name in unknown {
            step.events
                .push(RuntimeEvent::Notice(format!("unknown member: {name}")));
        }
        if targets.is_empty() {
            step.events.push(RuntimeEvent::Notice(
                "no matching member for message".to_string(),
            ));
            return;
        }

        let turn = match self.store.create_turn() {
            Ok(turn) => turn,
            Err(err) => {
                step.events
                    .push(RuntimeEvent::Notice(format!("store error: {err}")));
                return;
            }
        };
        let _ = self.store.record_user(turn, &targets, &body);
        step.events.push(RuntimeEvent::TurnStarted { turn });
        step.events.push(RuntimeEvent::UserMessage {
            turn,
            targets: targets.clone(),
            body: body.clone(),
        });

        let targets_str: Vec<String> = targets.iter().map(|t| t.to_string()).collect();
        if let Some(first_target) = targets.first() {
            self.log(
                first_target,
                LogEntry::info("user", format!("→ {}: {}", targets_str.join(", "), body)),
                step,
            );
        }

        if self.approvals_enabled
            && let Some(kind) = risky_action_kind(&body)
        {
            if let Ok(id) = self.store.insert_approval(Some(turn), None, kind, &body) {
                self.held_approvals.insert(
                    id,
                    HeldApproval {
                        turn,
                        targets,
                        body: body.clone(),
                    },
                );
                step.events.push(RuntimeEvent::ApprovalRequested {
                    id,
                    member: None,
                    action: kind.to_string(),
                    body,
                });
            }
            return;
        }

        for member in targets {
            self.enqueue_prompt(&member, turn, body.clone(), step);
        }
    }

    fn resolve_message_target(&self, target: &MessageTarget) -> (Vec<MemberId>, Vec<String>) {
        match target {
            MessageTarget::Default => (self.config.default_member_ids(), Vec::new()),
            MessageTarget::All => (self.config.all_member_ids(), Vec::new()),
            MessageTarget::Member(id) => self.resolve_named(std::slice::from_ref(id)),
            MessageTarget::Members(ids) => self.resolve_named(ids),
        }
    }

    fn resolve_named(&self, ids: &[MemberId]) -> (Vec<MemberId>, Vec<String>) {
        let mut resolved = Vec::new();
        let mut unknown = Vec::new();
        for id in ids {
            match self.config.find(id.as_str()) {
                Some(member) if !resolved.contains(&member.id) => resolved.push(member.id.clone()),
                Some(_) => {}
                None => unknown.push(id.to_string()),
            }
        }
        (resolved, unknown)
    }

    fn handle_cancel(&mut self, member: Option<MemberId>, step: &mut RuntimeStep) {
        let targets: Vec<MemberId> = match member {
            Some(m) => vec![m],
            None => {
                self.paused_routes.clear();
                self.members.keys().cloned().collect()
            }
        };
        for member in targets {
            let mut finished_turns = Vec::new();
            if let Some(state) = self.members.get_mut(&member) {
                for queued in state.queue.drain(..) {
                    finished_turns.push(queued.turn);
                }
                if let Some(running) = &state.running {
                    running.cancel.store(true, Ordering::Relaxed);
                    step.events
                        .push(RuntimeEvent::Notice(format!("cancelling {member}")));
                } else if state.status != MemberStatus::Idle {
                    state.status = MemberStatus::Idle;
                    step.events.push(RuntimeEvent::MemberStatus {
                        member: member.clone(),
                        status: MemberStatus::Idle,
                    });
                }
            }
            for turn in finished_turns {
                self.check_turn_complete(turn, step);
            }
        }
    }

    fn handle_approval(
        &mut self,
        id: ApprovalId,
        decision: ApprovalDecision,
        step: &mut RuntimeStep,
    ) {
        match self.store.resolve_approval(id, decision) {
            Ok(true) => step
                .events
                .push(RuntimeEvent::ApprovalResolved { id, decision }),
            _ => {
                step.events
                    .push(RuntimeEvent::Notice(format!("no pending approval {id}")));
                return;
            }
        }
        let Some(held) = self.held_approvals.remove(&id) else {
            return;
        };
        match decision {
            ApprovalDecision::Approve => {
                for member in held.targets {
                    self.enqueue_prompt(&member, held.turn, held.body.clone(), step);
                }
            }
            ApprovalDecision::Reject => {
                step.events
                    .push(RuntimeEvent::Notice("request rejected".to_string()));
                self.check_turn_complete(held.turn, step);
            }
        }
    }

    fn resolve_next_paused_route(&mut self, resume: bool, step: &mut RuntimeStep) {
        let Some(route) = self.paused_routes.pop_front() else {
            step.events
                .push(RuntimeEvent::Notice("no paused routes".to_string()));
            return;
        };
        if resume {
            let prompt = relay_prompt(&self.member_display(&route.from), &route.body);
            step.events.push(RuntimeEvent::Notice(format!(
                "resumed route {} -> {}",
                route.from,
                route.to_labels.join(", ")
            )));
            for member in route.to_members {
                self.enqueue_prompt(&member, route.turn, prompt.clone(), step);
            }
        } else {
            step.events.push(RuntimeEvent::Notice(format!(
                "dropped route {} -> {}",
                route.from,
                route.to_labels.join(", ")
            )));
            self.check_turn_complete(route.turn, step);
        }
    }

    // === agent event handling ===========================================

    pub fn on_agent_event(&mut self, member: &MemberId, event: AgentEvent) -> RuntimeStep {
        let mut step = RuntimeStep::default();
        // Ignore stream events for a member that is not currently running,
        // except the terminal Exited which we always honor.
        let running = self
            .members
            .get(member)
            .map(|s| s.running.is_some())
            .unwrap_or(false);
        if !running && !matches!(event, AgentEvent::Exited { .. }) {
            return step;
        }

        match event {
            AgentEvent::MessageStarted => self.start_message(member, &mut step),
            AgentEvent::TextDelta(text) => {
                self.ensure_message(member, &mut step);
                if let Some(msg) = self.message_id(member) {
                    self.append_text(member, &text);
                    step.events.push(RuntimeEvent::MessageDelta { msg, text });
                }
            }
            AgentEvent::Reasoning(text) => step.events.push(RuntimeEvent::Reasoning {
                member: member.clone(),
                text,
            }),
            AgentEvent::MessageCompleted(text) => self.complete_message(member, text, &mut step),
            AgentEvent::ToolStarted { id, name, summary } => {
                if let Some(state) = self.members.get_mut(member) {
                    state.tools.insert(id.clone(), name.clone());
                }
                step.events.push(RuntimeEvent::ToolStarted {
                    member: member.clone(),
                    tool_id: id,
                    name,
                    summary,
                });
            }
            AgentEvent::ToolProgress { .. } => {}
            AgentEvent::ToolCompleted { id, ok, summary } => {
                let name = self
                    .members
                    .get_mut(member)
                    .and_then(|s| s.tools.remove(&id))
                    .unwrap_or_else(|| "tool".to_string());
                if let Some(turn) = self.running_turn(member) {
                    let _ = self
                        .store
                        .record_tool(turn, member, &name, &summary, Some(ok));
                }
                step.events.push(RuntimeEvent::ToolCompleted {
                    member: member.clone(),
                    tool_id: id,
                    ok,
                    summary,
                });
            }
            AgentEvent::SessionDiscovered(session) => {
                // Backends may report the session id more than once per turn;
                // only persist and surface it when it actually changes.
                if self.sessions.get(member).as_ref() != Some(&session) {
                    let backend = self.member_backend(member);
                    self.sessions.set(member.clone(), session.clone());
                    let _ = self.store.upsert_session(member, backend, &session);
                    step.events.push(RuntimeEvent::SessionUpdated {
                        member: member.clone(),
                        session,
                    });
                }
            }
            AgentEvent::Raw(line) => {
                let _ = self.store.record_stream_event(member, &line);
            }
            AgentEvent::Stderr(line) => {
                self.log(member, LogEntry::warn(member.as_str(), line), &mut step)
            }
            AgentEvent::Log(message) => {
                self.log(member, LogEntry::info(member.as_str(), message), &mut step)
            }
            AgentEvent::ParseWarning(message) => {
                self.log(member, LogEntry::warn(member.as_str(), message), &mut step)
            }
            AgentEvent::Fatal(message) => {
                if let Some(turn) = self.running_turn(member) {
                    let _ = self.store.record_error(Some(turn), Some(member), &message);
                }
                step.events.push(RuntimeEvent::MemberError {
                    member: member.clone(),
                    message,
                });
            }
            AgentEvent::Exited { code, ok } => self.finalize_run(member, code, ok, &mut step),
        }
        step
    }

    fn log(&self, _member: &MemberId, entry: LogEntry, step: &mut RuntimeStep) {
        let _ = self.store.record_log(&entry);
        step.events.push(RuntimeEvent::Log(entry));
    }

    fn start_message(&mut self, member: &MemberId, step: &mut RuntimeStep) {
        let msg = self.next_msg();
        if let Some(turn) = self.running_turn(member)
            && let Some(state) = self.members.get_mut(member)
            && let Some(running) = &mut state.running
        {
            running.message = Some(msg);
            running.text.clear();
            step.events.push(RuntimeEvent::MessageStarted {
                msg,
                turn,
                member: member.clone(),
            });
        }
    }

    fn ensure_message(&mut self, member: &MemberId, step: &mut RuntimeStep) {
        if self.message_id(member).is_none() {
            self.start_message(member, step);
        }
    }

    fn complete_message(&mut self, member: &MemberId, text: String, step: &mut RuntimeStep) {
        self.ensure_message(member, step);
        let Some(msg) = self.message_id(member) else {
            return;
        };
        let Some(turn) = self.running_turn(member) else {
            return;
        };

        let parsed = parse_agent_output(&text);
        for warning in &parsed.warnings {
            self.log(
                member,
                LogEntry::warn(member.as_str(), warning.clone()),
                step,
            );
        }

        if !parsed.visible_text.is_empty() {
            let display = self.member_display(member);
            let backend = self.member_backend(member);
            let _ = self
                .store
                .record_agent(turn, member, &display, backend, &parsed.visible_text);
        }
        step.events.push(RuntimeEvent::MessageCompleted {
            msg,
            text: parsed.visible_text,
        });

        if let Some(state) = self.members.get_mut(member)
            && let Some(running) = &mut state.running
        {
            running.message = None;
            running.text.clear();
        }

        for tmsg in parsed.messages {
            self.route_team_message(member, turn, tmsg, step);
        }
    }

    fn route_team_message(
        &mut self,
        from: &MemberId,
        turn: TurnId,
        tmsg: crate::domain::event::TeamMessage,
        step: &mut RuntimeStep,
    ) {
        let resolved = router::resolve_targets(&self.config, &tmsg.to, Some(from));
        let to_labels: Vec<String> = resolved.members.iter().map(|m| m.to_string()).collect();

        let _ = self.store.record_route(turn, from, &to_labels, &tmsg.body);
        if !to_labels.is_empty() {
            step.events.push(RuntimeEvent::Route {
                turn,
                from: from.clone(),
                to: to_labels.clone(),
                body: tmsg.body.clone(),
            });

            self.log(
                from,
                LogEntry::info(
                    from.as_str(),
                    format!("→ {}: {}", to_labels.join(", "), tmsg.body),
                ),
                step,
            );
        }
        for unknown in &resolved.unknown {
            step.events.push(RuntimeEvent::RouteError {
                turn,
                from: from.clone(),
                target: unknown.clone(),
                reason: "unknown member".to_string(),
            });
        }
        if resolved.members.is_empty() {
            return;
        }

        let prompt = relay_prompt(&self.member_display(from), &tmsg.body);
        if self.relay_paused {
            self.pause_route(
                turn,
                from,
                resolved.members,
                to_labels,
                tmsg.body,
                "relay paused by user",
                step,
            );
            return;
        }
        match self.relay.record_auto_relay(turn, from) {
            RelayDecision::Continue { .. } => {
                for member in resolved.members {
                    self.enqueue_prompt(&member, turn, prompt.clone(), step);
                }
            }
            RelayDecision::Pause { count } => {
                self.pause_route(
                    turn,
                    from,
                    resolved.members,
                    to_labels,
                    tmsg.body,
                    &format!("auto-relay limit reached ({count})"),
                    step,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn pause_route(
        &mut self,
        turn: TurnId,
        from: &MemberId,
        to_members: Vec<MemberId>,
        to_labels: Vec<String>,
        body: String,
        reason: &str,
        step: &mut RuntimeStep,
    ) {
        self.paused_routes.push_back(PausedRoute {
            turn,
            from: from.clone(),
            to_members,
            to_labels: to_labels.clone(),
            body,
        });
        step.events.push(RuntimeEvent::RoutePaused {
            turn,
            from: from.clone(),
            to: to_labels,
            reason: reason.to_string(),
            queued: self.paused_routes.len(),
        });
    }

    fn finalize_run(
        &mut self,
        member: &MemberId,
        code: Option<i32>,
        ok: bool,
        step: &mut RuntimeStep,
    ) {
        // Flush an unterminated streaming message.
        let pending_text = self.members.get(member).and_then(|s| {
            s.running
                .as_ref()
                .filter(|r| r.message.is_some())
                .map(|r| r.text.clone())
        });
        if let Some(text) = pending_text {
            self.complete_message(member, text, step);
        }

        let (turn, cancelled) = match self.members.get_mut(member).and_then(|s| s.running.take()) {
            Some(running) => (running.turn, running.cancel.load(Ordering::Relaxed)),
            None => return,
        };

        if cancelled {
            // A user-requested cancel kills the process (no exit code); that is
            // expected, not an error.
            step.events
                .push(RuntimeEvent::Notice(format!("{member} cancelled")));
        } else if !ok {
            let message = format!(
                "{} exited without success (code {})",
                self.member_backend(member),
                code.map(|c| c.to_string())
                    .unwrap_or_else(|| "unknown".to_string())
            );
            let _ = self.store.record_error(Some(turn), Some(member), &message);
            step.events.push(RuntimeEvent::MemberError {
                member: member.clone(),
                message,
            });
        }

        if let Some(state) = self.members.get_mut(member) {
            state.tools.clear();
            state.status = MemberStatus::Idle;
        }
        step.events.push(RuntimeEvent::MemberStatus {
            member: member.clone(),
            status: MemberStatus::Idle,
        });

        // Start the next queued prompt for this member, if any.
        let next = self
            .members
            .get_mut(member)
            .and_then(|s| s.queue.pop_front());
        if let Some(queued) = next {
            self.start_run(member, queued.turn, queued.prompt, step);
        }

        self.check_turn_complete(turn, step);
    }

    // === queueing / dispatch ============================================

    fn enqueue_prompt(
        &mut self,
        member: &MemberId,
        turn: TurnId,
        prompt: String,
        step: &mut RuntimeStep,
    ) {
        let stripped_prompt = strip_routing_prefix(&prompt);
        let busy = self
            .members
            .get(member)
            .map(|s| s.running.is_some())
            .unwrap_or(false);
        if busy {
            if let Some(state) = self.members.get_mut(member) {
                state.queue.push_back(QueuedPrompt {
                    turn,
                    prompt: stripped_prompt,
                });
                state.status = MemberStatus::Queued;
            }
            step.events.push(RuntimeEvent::MemberStatus {
                member: member.clone(),
                status: MemberStatus::Queued,
            });
        } else {
            self.start_run(member, turn, stripped_prompt, step);
        }
    }

    fn start_run(
        &mut self,
        member: &MemberId,
        turn: TurnId,
        prompt: String,
        step: &mut RuntimeStep,
    ) {
        let session = if self.member_uses_resume(member) {
            self.sessions.get(member)
        } else {
            None
        };
        let cancel = Arc::new(AtomicBool::new(false));
        if let Some(state) = self.members.get_mut(member) {
            state.running = Some(RunningState {
                cancel: cancel.clone(),
                turn,
                message: None,
                text: String::new(),
            });
            state.status = MemberStatus::Running;
            state.tools.clear();
        }
        step.events.push(RuntimeEvent::MemberStatus {
            member: member.clone(),
            status: MemberStatus::Running,
        });
        step.actions.push(RunAction {
            member: member.clone(),
            prompt,
            session,
            cancel,
        });
    }

    fn check_turn_complete(&mut self, turn: TurnId, step: &mut RuntimeStep) {
        if !self.turn_active(turn) {
            self.relay.reset_turn(turn);
            step.events.push(RuntimeEvent::TurnFinished { turn });
        }
    }

    fn turn_active(&self, turn: TurnId) -> bool {
        let in_members = self.members.values().any(|state| {
            state.running.as_ref().map(|r| r.turn) == Some(turn)
                || state.queue.iter().any(|q| q.turn == turn)
        });
        in_members
            || self.paused_routes.iter().any(|r| r.turn == turn)
            || self.held_approvals.values().any(|h| h.turn == turn)
    }

    // === small helpers ==================================================

    fn append_text(&mut self, member: &MemberId, text: &str) {
        if let Some(state) = self.members.get_mut(member)
            && let Some(running) = &mut state.running
        {
            running.text.push_str(text);
        }
    }

    fn message_id(&self, member: &MemberId) -> Option<MessageId> {
        self.members
            .get(member)
            .and_then(|s| s.running.as_ref())
            .and_then(|r| r.message)
    }

    fn running_turn(&self, member: &MemberId) -> Option<TurnId> {
        self.members
            .get(member)
            .and_then(|s| s.running.as_ref())
            .map(|r| r.turn)
    }

    fn next_msg(&mut self) -> MessageId {
        self.next_message_id += 1;
        MessageId(self.next_message_id)
    }

    fn member_display(&self, member: &MemberId) -> String {
        self.config
            .member(member)
            .map(|m| m.display_name.clone())
            .unwrap_or_else(|| member.to_string())
    }

    fn member_backend(&self, member: &MemberId) -> BackendKind {
        self.config
            .member(member)
            .map(|m| m.backend)
            .unwrap_or(BackendKind::Codex)
    }

    fn member_uses_resume(&self, member: &MemberId) -> bool {
        use crate::domain::team::SessionPolicy;
        self.config
            .member(member)
            .map(|m| m.session_policy == SessionPolicy::Resume)
            .unwrap_or(true)
    }
}

fn relay_prompt(from_display: &str, body: &str) -> String {
    format!("[relay from {from_display}]\n{body}")
}

fn strip_routing_prefix(prompt: &str) -> String {
    let trimmed = prompt.trim();
    if let Some(rest) = trimmed.strip_prefix('@')
        && let Some(idx) = rest.find(char::is_whitespace)
    {
        return rest[idx..].trim().to_string();
    }
    prompt.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::team::{BackendKind, DefaultTarget, TeamMember};

    fn team() -> TeamConfig {
        let mut config = TeamConfig::new("mixed", "/tmp/ws")
            .with_member(TeamMember::new(
                "builder",
                "Builder",
                BackendKind::Codex,
                "impl",
            ))
            .with_member(TeamMember::new(
                "reviewer",
                "Reviewer",
                BackendKind::Claude,
                "review",
            ));
        config.default_target = Some(DefaultTarget::Member(MemberId::new("builder")));
        config
    }

    fn runtime() -> TeamRuntime {
        TeamRuntime::new(team(), SqliteStore::in_memory().unwrap()).with_approvals(false)
    }

    fn user(body: &str) -> UiCommand {
        UiCommand::UserMessage {
            target: MessageTarget::Default,
            body: body.to_string(),
        }
    }

    #[test]
    fn user_message_starts_a_run_for_default_member() {
        let mut rt = runtime();
        let step = rt.on_ui_command(user("build it"));

        assert_eq!(step.actions.len(), 1);
        assert_eq!(step.actions[0].member, MemberId::new("builder"));
        assert!(
            step.events
                .iter()
                .any(|e| matches!(e, RuntimeEvent::TurnStarted { .. }))
        );
        assert!(step.events.iter().any(|e| matches!(
            e,
            RuntimeEvent::MemberStatus {
                status: MemberStatus::Running,
                ..
            }
        )));
    }

    #[test]
    fn completed_message_is_emitted_and_persisted_then_turn_finishes() {
        let mut rt = runtime();
        rt.on_ui_command(user("build it"));
        let builder = MemberId::new("builder");

        let step = rt.on_agent_event(&builder, AgentEvent::MessageCompleted("done".to_string()));
        assert!(step.events.iter().any(|e| matches!(
            e,
            RuntimeEvent::MessageCompleted { text, .. } if text == "done"
        )));

        let step = rt.on_agent_event(
            &builder,
            AgentEvent::Exited {
                code: Some(0),
                ok: true,
            },
        );
        assert!(
            step.events
                .iter()
                .any(|e| matches!(e, RuntimeEvent::TurnFinished { .. }))
        );

        let items = rt.store.replay_chat().unwrap();
        assert!(items.iter().any(|i| matches!(
            i,
            crate::domain::event::ChatItem::Agent { text, .. } if text == "done"
        )));
    }

    #[test]
    fn team_message_routes_to_another_member() {
        let mut rt = runtime();
        rt.on_ui_command(user("plan it"));
        let builder = MemberId::new("builder");

        let step = rt.on_agent_event(
            &builder,
            AgentEvent::MessageCompleted(
                r#"@@team_message {"to":"reviewer","body":"please review"}"#.to_string(),
            ),
        );

        assert!(step.events.iter().any(|e| matches!(
            e,
            RuntimeEvent::Route { to, .. } if to == &vec!["reviewer".to_string()]
        )));
        // The relay is dispatched to the reviewer.
        assert!(
            step.actions
                .iter()
                .any(|a| a.member == MemberId::new("reviewer"))
        );
        assert!(step.actions[0].prompt.contains("please review"));
    }

    #[test]
    fn unknown_route_target_reports_error() {
        let mut rt = runtime();
        rt.on_ui_command(user("plan it"));
        let builder = MemberId::new("builder");

        let step = rt.on_agent_event(
            &builder,
            AgentEvent::MessageCompleted(
                r#"@@team_message {"to":"ghost","body":"hi"}"#.to_string(),
            ),
        );
        assert!(step.events.iter().any(|e| matches!(
            e,
            RuntimeEvent::RouteError { target, .. } if target == "ghost"
        )));
        assert!(step.actions.is_empty());
    }

    #[test]
    fn second_message_to_busy_member_is_queued_then_runs() {
        let mut rt = runtime();
        let builder = MemberId::new("builder");
        rt.on_ui_command(UiCommand::UserMessage {
            target: MessageTarget::Member(builder.clone()),
            body: "first".to_string(),
        });
        let step = rt.on_ui_command(UiCommand::UserMessage {
            target: MessageTarget::Member(builder.clone()),
            body: "second".to_string(),
        });
        assert!(step.events.iter().any(|e| matches!(
            e,
            RuntimeEvent::MemberStatus {
                status: MemberStatus::Queued,
                ..
            }
        )));
        assert!(
            step.actions.is_empty(),
            "busy member does not start a second run"
        );

        // Finishing the first run starts the queued prompt.
        rt.on_agent_event(&builder, AgentEvent::MessageCompleted("a".to_string()));
        let step = rt.on_agent_event(
            &builder,
            AgentEvent::Exited {
                code: Some(0),
                ok: true,
            },
        );
        assert!(step.actions.iter().any(|a| a.prompt == "second"));
    }

    #[test]
    fn relay_can_be_paused_by_user() {
        let mut rt = runtime();
        rt.on_ui_command(UiCommand::SetRelayPaused(true));
        rt.on_ui_command(user("plan"));
        let builder = MemberId::new("builder");

        let step = rt.on_agent_event(
            &builder,
            AgentEvent::MessageCompleted(
                r#"@@team_message {"to":"reviewer","body":"check"}"#.to_string(),
            ),
        );
        assert!(
            step.events
                .iter()
                .any(|e| matches!(e, RuntimeEvent::RoutePaused { .. }))
        );
        assert!(
            !step
                .actions
                .iter()
                .any(|a| a.member == MemberId::new("reviewer"))
        );

        // Resolving with resume delivers it.
        let step = rt.on_ui_command(UiCommand::ResolvePausedRoute { resume: true });
        assert!(
            step.actions
                .iter()
                .any(|a| a.member == MemberId::new("reviewer"))
        );
    }

    #[test]
    fn relay_guard_pauses_after_limit() {
        let mut config = team();
        config.max_auto_relays = 1;
        let mut rt =
            TeamRuntime::new(config, SqliteStore::in_memory().unwrap()).with_approvals(false);
        rt.on_ui_command(user("go"));
        let builder = MemberId::new("builder");

        // First relay: delivered.
        let step = rt.on_agent_event(
            &builder,
            AgentEvent::MessageCompleted(
                r#"@@team_message {"to":"reviewer","body":"1"}"#.to_string(),
            ),
        );
        assert!(
            step.actions
                .iter()
                .any(|a| a.member == MemberId::new("reviewer"))
        );

        // Second relay from the same member in the same turn: paused.
        let step = rt.on_agent_event(
            &builder,
            AgentEvent::MessageCompleted(
                r#"@@team_message {"to":"reviewer","body":"2"}"#.to_string(),
            ),
        );
        assert!(
            step.events
                .iter()
                .any(|e| matches!(e, RuntimeEvent::RoutePaused { .. }))
        );
    }

    #[test]
    fn session_discovered_is_persisted_and_emitted() {
        let mut rt = runtime();
        rt.on_ui_command(user("hi"));
        let builder = MemberId::new("builder");

        let step = rt.on_agent_event(
            &builder,
            AgentEvent::SessionDiscovered(AgentSessionId("thread-1".to_string())),
        );
        assert!(
            step.events
                .iter()
                .any(|e| matches!(e, RuntimeEvent::SessionUpdated { .. }))
        );
        assert_eq!(
            rt.store.session_for(&builder).unwrap(),
            Some(AgentSessionId("thread-1".to_string()))
        );
    }

    #[test]
    fn risky_request_is_gated_until_approved() {
        let mut rt = TeamRuntime::new(team(), SqliteStore::in_memory().unwrap()); // approvals on
        let step = rt.on_ui_command(user("run git push origin main"));

        let approval_id = step.events.iter().find_map(|e| match e {
            RuntimeEvent::ApprovalRequested { id, .. } => Some(*id),
            _ => None,
        });
        let id = approval_id.expect("approval requested");
        assert!(step.actions.is_empty(), "gated request does not run yet");

        let step = rt.on_ui_command(UiCommand::Approve {
            id,
            decision: ApprovalDecision::Approve,
        });
        assert!(
            step.actions
                .iter()
                .any(|a| a.member == MemberId::new("builder"))
        );
    }

    #[test]
    fn streaming_text_deltas_build_a_message() {
        let mut rt = runtime();
        rt.on_ui_command(user("hi"));
        let reviewer_unused = MemberId::new("builder");

        rt.on_agent_event(&reviewer_unused, AgentEvent::MessageStarted);
        let step = rt.on_agent_event(&reviewer_unused, AgentEvent::TextDelta("Hel".to_string()));
        assert!(
            step.events
                .iter()
                .any(|e| matches!(e, RuntimeEvent::MessageDelta { .. }))
        );
        rt.on_agent_event(&reviewer_unused, AgentEvent::TextDelta("lo".to_string()));
        let step = rt.on_agent_event(
            &reviewer_unused,
            AgentEvent::MessageCompleted("Hello".to_string()),
        );
        assert!(step.events.iter().any(|e| matches!(
            e,
            RuntimeEvent::MessageCompleted { text, .. } if text == "Hello"
        )));
    }

    #[test]
    fn cancelled_run_is_not_reported_as_error() {
        let mut rt = runtime();
        rt.on_ui_command(user("build it"));
        let builder = MemberId::new("builder");

        rt.on_ui_command(UiCommand::Cancel {
            member: Some(builder.clone()),
        });
        // The killed process exits unsuccessfully with no exit code.
        let step = rt.on_agent_event(
            &builder,
            AgentEvent::Exited {
                code: None,
                ok: false,
            },
        );

        assert!(
            !step
                .events
                .iter()
                .any(|e| matches!(e, RuntimeEvent::MemberError { .. })),
            "a cancelled run must not surface as an error"
        );
        assert!(step.events.iter().any(|e| matches!(
            e,
            RuntimeEvent::Notice(text) if text.contains("cancelled")
        )));
    }

    #[test]
    fn user_message_with_at_prefix_strips_prefix_for_agent_run() {
        let mut rt = runtime();
        let builder = MemberId::new("builder");
        let step = rt.on_ui_command(UiCommand::UserMessage {
            target: MessageTarget::Member(builder.clone()),
            body: "@builder nihao".to_string(),
        });

        assert_eq!(step.actions.len(), 1);
        assert_eq!(step.actions[0].member, builder);
        assert_eq!(step.actions[0].prompt, "nihao");
    }
}
