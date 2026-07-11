//! The team runtime core.
//!
//! Pure orchestration logic: [`TeamRuntime::on_ui_command`] and
//! [`TeamRuntime::on_agent_event`] take an input and return the
//! [`RuntimeEvent`]s to emit plus the [`RunAction`]s to dispatch. All threading
//! and child-process work lives in the transport layer (`agent_runner` / the
//! `run` loop), so the core is fully unit-testable without spawning anything.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::domain::config::{
    ASTERLINE_TEAM_SKILL_NAME, inject_team_protocol, strip_team_protocol, strip_team_protocols,
    team_skill_hint,
};
use crate::domain::event::{
    AgentEvent, AgentSessionId, ApprovalDecision, ApprovalId, ImportedMessage, LogEntry,
    MemberStatus, MemberSummary, MessageId, MessageTarget, RuntimeEvent, TurnId, UiCommand,
    WorkflowRunId, WorkflowRunStatus, WorkflowRunSummary, WorkflowStepRequest, WorkflowStepStatus,
};
use crate::domain::team::{
    BackendKind, DefaultTarget, Effort, MemberId, SessionPolicy, TeamConfig, TeamMember,
};
use crate::router::{self, RelayDecision, RelayGuard, parse_agent_output};
use crate::runtime::approval::risky_action_kind;
use crate::runtime::session_registry::SessionRegistry;
use crate::store::sqlite::SqliteStore;
use crate::workflow::suggested_verify_command;

/// What the core wants the transport layer to do after handling an input.
#[derive(Default)]
pub struct RuntimeStep {
    pub events: Vec<RuntimeEvent>,
    pub actions: Vec<RunAction>,
    pub verify_actions: Vec<VerifyAction>,
    pub runner_changes: Vec<RunnerChange>,
    pub persist_team: Option<TeamConfig>,
}

/// A runner map mutation requested after a live roster edit.
pub enum RunnerChange {
    Upsert {
        member: TeamMember,
        workspace: PathBuf,
    },
    Remove(MemberId),
}

/// A run the transport layer should start for a member.
pub struct RunAction {
    pub member: MemberId,
    pub prompt: String,
    pub session: Option<AgentSessionId>,
    pub cancel: Arc<AtomicBool>,
    pub effort: Option<Effort>,
}

/// A verification command the transport layer should run outside the core loop.
pub struct VerifyAction {
    pub run_id: WorkflowRunId,
    pub command: String,
    pub workspace: PathBuf,
    pub cancel: Arc<AtomicBool>,
}

/// Result of a completed verification command.
pub struct VerifyOutput {
    pub run_id: WorkflowRunId,
    pub command: String,
    pub ok: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub start_error: Option<String>,
    pub cancelled: bool,
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
    tools: HashMap<String, ActiveTool>,
    effort: Option<Effort>,
}

struct ActiveTool {
    name: String,
    summary: String,
    detail: String,
}

impl MemberState {
    fn new(effort: Option<Effort>) -> Self {
        Self {
            status: MemberStatus::Idle,
            queue: VecDeque::new(),
            running: None,
            tools: HashMap::new(),
            effort,
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
    workflow_turns: HashMap<TurnId, WorkflowRunId>,
    failed_workflow_runs: HashSet<WorkflowRunId>,
    last_user: Option<(MessageTarget, String)>,
    next_message_id: u64,
    approvals_enabled: bool,
}

impl TeamRuntime {
    pub fn new(config: TeamConfig, store: SqliteStore) -> Self {
        let _ = store.upsert_team(&config);
        // Bind to the latest conversation so records and replay agree.
        if let Ok(conversation) = store.current_conversation() {
            store.set_conversation(conversation);
        }
        let sessions = SessionRegistry::from_store(&store, &config.all_member_ids());
        let members = config
            .members
            .iter()
            .map(|m| (m.id.clone(), MemberState::new(m.effort)))
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
            workflow_turns: HashMap::new(),
            failed_workflow_runs: HashSet::new(),
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
                cwd: m.resolved_cwd(&self.config.workspace).display().to_string(),
                model: m.model.clone(),
                effort: self.members.get(&m.id).and_then(|s| s.effort),
                sandbox: m.sandbox,
                permission_mode: m.permission_mode,
                session_policy: m.session_policy,
            })
            .collect();
        RuntimeEvent::Ready {
            team: self.config.name.clone(),
            workspace: self.config.workspace.display().to_string(),
            default_target: self.config.default_target.clone(),
            members,
            workflow_runs: self.store.recent_workflow_runs(50).unwrap_or_default(),
        }
    }

    // === command handling ===============================================

    pub fn on_ui_command(&mut self, cmd: UiCommand) -> RuntimeStep {
        let mut step = RuntimeStep::default();
        match cmd {
            UiCommand::UserMessage { target, body } => {
                self.handle_user_message(target, body, &mut step);
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
            UiCommand::SetEffort { member, effort } => {
                match self.config.find(member.as_str()).map(|m| m.id.clone()) {
                    Some(id) => {
                        if let Some(state) = self.members.get_mut(&id) {
                            state.effort = Some(effort);
                        }
                        step.events.push(RuntimeEvent::MemberEffort {
                            member: id.clone(),
                            effort,
                        });
                        step.events.push(RuntimeEvent::Notice(format!(
                            "{id} reasoning effort → {}",
                            effort.as_str()
                        )));
                    }
                    None => step
                        .events
                        .push(RuntimeEvent::Notice(format!("unknown member: {member}"))),
                }
            }
            UiCommand::ReplaceTeam {
                members,
                default_target,
            } => self.handle_replace_team(members, default_target, &mut step),
            UiCommand::NewSession => self.handle_new_session(&mut step),
            UiCommand::ImportTranscript { member, items } => {
                self.handle_import_transcript(member, items, &mut step)
            }
            UiCommand::RunWorkflow { goal } => {
                self.handle_run_workflow(goal, &mut step);
            }
            UiCommand::ContinueWorkflow { run_id, note } => {
                self.handle_continue_workflow(run_id, note, &mut step)
            }
            UiCommand::NoteWorkflow { run_id, note } => {
                self.handle_note_workflow(run_id, note, &mut step)
            }
            UiCommand::BlockWorkflow { run_id, reason } => {
                self.handle_block_workflow(run_id, reason, &mut step)
            }
            UiCommand::VerifyWorkflow { run_id, command } => {
                self.handle_verify_workflow(run_id, command, &mut step)
            }
            UiCommand::AddWorkflowStep {
                run_id,
                owner,
                title,
            } => self.handle_add_workflow_step(run_id, owner, title, &mut step),
            UiCommand::UpdateWorkflowStep {
                run_id,
                step: step_number,
                status,
                note,
            } => self.handle_update_workflow_step(run_id, step_number, status, note, &mut step),
            UiCommand::RenameWorkflowStep {
                run_id,
                step: step_number,
                title,
            } => self.handle_rename_workflow_step(run_id, step_number, title, &mut step),
            UiCommand::RemoveWorkflowStep {
                run_id,
                step: step_number,
            } => self.handle_remove_workflow_step(run_id, step_number, &mut step),
            UiCommand::AssignWorkflowStep {
                run_id,
                step: step_number,
                owner,
            } => self.handle_assign_workflow_step(run_id, step_number, owner, &mut step),
            UiCommand::Shutdown => self.handle_cancel(None, &mut step),
        }
        step
    }

    fn handle_user_message(
        &mut self,
        target: MessageTarget,
        body: String,
        step: &mut RuntimeStep,
    ) -> Option<TurnId> {
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
            return None;
        }

        let turn = match self.store.create_turn() {
            Ok(turn) => turn,
            Err(err) => {
                step.events
                    .push(RuntimeEvent::Notice(format!("store error: {err}")));
                return None;
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
            return Some(turn);
        }

        for member in targets {
            self.enqueue_prompt(&member, turn, body.clone(), step);
        }
        Some(turn)
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

    fn handle_new_session(&mut self, step: &mut RuntimeStep) {
        // A fresh chat: a new conversation (so the transcript starts clean and
        // restart shows only this chat) plus new backend sessions for everyone.
        if let Ok(conversation) = self.store.create_conversation() {
            self.store.set_conversation(conversation);
        }
        for id in self.config.all_member_ids() {
            self.sessions.clear(&id);
            let _ = self.store.delete_session(&id);
        }
        // Drop any in-flight turn state from the previous chat.
        self.paused_routes.clear();
        self.held_approvals.clear();
        step.events.push(RuntimeEvent::SessionReset);
        step.events.push(RuntimeEvent::Notice(
            "started a new chat — fresh session for all members".to_string(),
        ));
    }

    fn handle_replace_team(
        &mut self,
        members: Vec<TeamMember>,
        default_target: Option<DefaultTarget>,
        step: &mut RuntimeStep,
    ) {
        let mut raw_config = self.config.clone();
        raw_config.members = self.merge_member_config(members);
        raw_config.default_target = default_target.or_else(|| {
            raw_config
                .members
                .first()
                .map(|member| DefaultTarget::Member(member.id.clone()))
        });
        raw_config = strip_team_protocols(raw_config);

        if let Err(err) = raw_config.validate() {
            step.events
                .push(RuntimeEvent::Notice(format!("team update rejected: {err}")));
            return;
        }

        let old_ids: HashSet<MemberId> = self.members.keys().cloned().collect();
        let new_ids: HashSet<MemberId> = raw_config.members.iter().map(|m| m.id.clone()).collect();
        for removed in old_ids.difference(&new_ids) {
            if let Some(state) = self.members.get(removed)
                && (state.status != MemberStatus::Idle
                    || state.running.is_some()
                    || !state.queue.is_empty())
            {
                step.events.push(RuntimeEvent::Notice(format!(
                    "cannot remove {removed} while it is active"
                )));
                return;
            }
        }

        let old_backends: HashMap<MemberId, BackendKind> = self
            .config
            .members
            .iter()
            .map(|member| (member.id.clone(), member.backend))
            .collect();
        let changed_backend_ids: Vec<MemberId> = raw_config
            .members
            .iter()
            .filter(|member| {
                old_backends
                    .get(&member.id)
                    .is_some_and(|backend| *backend != member.backend)
            })
            .map(|member| member.id.clone())
            .collect();

        let removed_ids: Vec<MemberId> = old_ids.difference(&new_ids).cloned().collect();
        for id in &removed_ids {
            self.members.remove(id);
            self.sessions.clear(id);
            let _ = self.store.delete_session(id);
            self.paused_routes
                .retain(|route| route.from != *id && !route.to_members.contains(id));
            self.held_approvals
                .retain(|_, held| !held.targets.contains(id));
            step.runner_changes.push(RunnerChange::Remove(id.clone()));
        }

        for id in &changed_backend_ids {
            self.sessions.clear(id);
            let _ = self.store.delete_session(id);
        }

        for member in &raw_config.members {
            self.members
                .entry(member.id.clone())
                .or_insert_with(|| MemberState::new(member.effort))
                .effort = member.effort;
        }

        let mut operational_config = raw_config.clone();
        inject_team_protocol(&mut operational_config);
        self.config = operational_config;
        let _ = self.store.upsert_team(&self.config);

        for member in self.config.members.clone() {
            step.runner_changes.push(RunnerChange::Upsert {
                member,
                workspace: self.config.workspace.clone(),
            });
        }
        step.persist_team = Some(raw_config);
        step.events.push(self.ready_event());
        step.events.push(RuntimeEvent::Notice(format!(
            "team updated: {} member(s)",
            self.config.members.len()
        )));
    }

    fn merge_member_config(&self, members: Vec<TeamMember>) -> Vec<TeamMember> {
        let previous: HashMap<MemberId, TeamMember> = self
            .config
            .members
            .iter()
            .cloned()
            .map(|member| (member.id.clone(), member))
            .collect();
        members
            .into_iter()
            .map(|mut member| {
                if let Some(old) = previous.get(&member.id) {
                    if member.system_prompt.is_none()
                        && let Some(prompt) = &old.system_prompt
                    {
                        let prompt = strip_team_protocol(prompt);
                        if !prompt.trim().is_empty() {
                            member.system_prompt = Some(prompt.trim().to_string());
                        }
                    }
                    if member.allowed_tools.is_empty() && !old.allowed_tools.is_empty() {
                        member.allowed_tools = old.allowed_tools.clone();
                    }
                }
                member
            })
            .collect()
    }

    /// Persist and surface messages exchanged in a member's native session
    /// (imported after an interactive attach), as one synthetic turn.
    fn handle_import_transcript(
        &mut self,
        member: MemberId,
        items: Vec<ImportedMessage>,
        step: &mut RuntimeStep,
    ) {
        let Some(id) = self.config.find(member.as_str()).map(|m| m.id.clone()) else {
            step.events
                .push(RuntimeEvent::Notice(format!("unknown member: {member}")));
            return;
        };
        if items.is_empty() {
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
        let display = self.member_display(&id);
        let backend = self.member_backend(&id);
        let count = items.len();
        step.events.push(RuntimeEvent::TurnStarted { turn });
        for item in items {
            if item.from_user {
                let _ = self
                    .store
                    .record_user(turn, std::slice::from_ref(&id), &item.text);
                step.events.push(RuntimeEvent::UserMessage {
                    turn,
                    targets: vec![id.clone()],
                    body: item.text,
                });
            } else {
                let _ = self
                    .store
                    .record_agent(turn, &id, &display, backend, &item.text);
                let msg = self.next_msg();
                step.events.push(RuntimeEvent::MessageStarted {
                    msg,
                    turn,
                    member: id.clone(),
                });
                step.events.push(RuntimeEvent::MessageCompleted {
                    msg,
                    text: item.text,
                });
            }
        }
        step.events.push(RuntimeEvent::Notice(format!(
            "imported {count} message(s) from {id}'s attached session"
        )));
        step.events.push(RuntimeEvent::TurnFinished { turn });
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
                    state.tools.insert(
                        id.clone(),
                        ActiveTool {
                            name: name.clone(),
                            summary: summary.clone(),
                            detail: String::new(),
                        },
                    );
                }
                step.events.push(RuntimeEvent::ToolStarted {
                    member: member.clone(),
                    tool_id: id,
                    name,
                    summary,
                });
            }
            AgentEvent::ToolProgress { id, delta } => {
                if let Some(tool) = self
                    .members
                    .get_mut(member)
                    .and_then(|state| state.tools.get_mut(&id))
                {
                    tool.detail.push_str(&delta);
                }
                step.events.push(RuntimeEvent::ToolProgress {
                    member: member.clone(),
                    tool_id: id,
                    delta,
                });
            }
            AgentEvent::ToolCompleted { id, ok, summary } => {
                let tool = self
                    .members
                    .get_mut(member)
                    .and_then(|state| state.tools.remove(&id));
                let (name, input, mut output) = match tool {
                    Some(tool) => (tool.name, tool.summary, tool.detail),
                    None => ("tool".to_string(), String::new(), String::new()),
                };
                if !summary.is_empty()
                    && summary.trim() != input.trim()
                    && output.trim_end() != summary.trim()
                {
                    if !output.is_empty() && !output.ends_with('\n') {
                        output.push('\n');
                    }
                    output.push_str(&summary);
                }
                if let Some(turn) = self.running_turn(member) {
                    let _ = self
                        .store
                        .record_tool(turn, member, &name, &input, &output, Some(ok));
                }
                step.events.push(RuntimeEvent::ToolCompleted {
                    member: member.clone(),
                    tool_id: id,
                    ok,
                    output,
                });
            }
            AgentEvent::FileChange { files, ok: _ } => {
                if let Some(turn) = self.running_turn(member) {
                    let _ = self.store.record_diff(turn, member, &files);
                }
                step.events.push(RuntimeEvent::FileChange {
                    member: member.clone(),
                    files,
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

        for member_request in parsed.members {
            self.add_team_member_from_agent(member, member_request, step);
        }
        for request in parsed.workflow_steps {
            self.apply_workflow_step_from_agent(member, turn, request, step);
        }
        for tmsg in parsed.messages {
            self.route_team_message(member, turn, tmsg, step);
        }
    }

    fn add_team_member_from_agent(
        &mut self,
        from: &MemberId,
        member: TeamMember,
        step: &mut RuntimeStep,
    ) {
        if self.config.find(member.id.as_str()).is_some() {
            step.events.push(RuntimeEvent::Notice(format!(
                "{from} could not add teammate {}: member already exists",
                member.id
            )));
            return;
        }
        if self.config.find(&member.display_name).is_some() {
            step.events.push(RuntimeEvent::Notice(format!(
                "{from} could not add teammate {}: display name already exists",
                member.display_name
            )));
            return;
        }

        let id = member.id.clone();
        let backend = member.backend;
        let role = member.role.clone();
        let mut members = self.config.members.clone();
        members.push(member);
        let default_target = self.config.default_target.clone();
        self.handle_replace_team(members, default_target, step);

        if self.config.member(&id).is_some() {
            step.events.push(RuntimeEvent::Notice(format!(
                "{from} added teammate {id} ({backend}, {role})"
            )));
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

        if !to_labels.is_empty() {
            let _ = self.store.record_route(turn, from, &to_labels, &tmsg.body);
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
            let err_msg = format!(
                "route to {unknown} failed: unknown member — message: {}",
                tmsg.body
            );
            let _ = self.store.record_error(Some(turn), Some(from), &err_msg);
            step.events.push(RuntimeEvent::RouteError {
                turn,
                from: from.clone(),
                target: unknown.clone(),
                reason: "unknown member".to_string(),
                body: tmsg.body.clone(),
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

    fn apply_workflow_step_from_agent(
        &mut self,
        from: &MemberId,
        turn: TurnId,
        request: WorkflowStepRequest,
        step: &mut RuntimeStep,
    ) {
        let Some(run_id) = self.workflow_turns.get(&turn).copied() else {
            step.events.push(RuntimeEvent::Notice(format!(
                "{from} ignored workflow step update: no active workflow run"
            )));
            return;
        };

        let result = match request {
            WorkflowStepRequest::Add { owner, title } => {
                self.store.add_workflow_step(run_id, owner.as_ref(), &title)
            }
            WorkflowStepRequest::Update {
                step: step_number,
                status,
                note,
            } => self
                .store
                .update_workflow_step(run_id, step_number, status, note.as_deref()),
            WorkflowStepRequest::Rename {
                step: step_number,
                title,
            } => self.store.rename_workflow_step(run_id, step_number, &title),
            WorkflowStepRequest::Remove { step: step_number } => {
                self.store.remove_workflow_step(run_id, step_number)
            }
            WorkflowStepRequest::Assign {
                step: step_number,
                owner,
            } => self
                .store
                .assign_workflow_step(run_id, step_number, owner.as_ref()),
        };

        match result {
            Ok(run) => {
                let id = run.id;
                step.events.push(RuntimeEvent::WorkflowRunUpdated { run });
                step.events.push(RuntimeEvent::Notice(format!(
                    "{from} updated workflow {id} checklist"
                )));
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                step.events.push(RuntimeEvent::Notice(format!(
                    "{from} could not update workflow {run_id}: step was not found"
                )));
            }
            Err(err) => step.events.push(RuntimeEvent::Notice(format!(
                "{from} could not update workflow {run_id}: {err}"
            ))),
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
            self.mark_workflow_turn(turn, WorkflowRunStatus::Failed, step);
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
        let effort = self.members.get(member).and_then(|s| s.effort);
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
        let prompt = self.prompt_for_member(member, prompt);
        step.actions.push(RunAction {
            member: member.clone(),
            prompt,
            session,
            cancel,
            effort,
        });
    }

    fn prompt_for_member(&self, member: &MemberId, prompt: String) -> String {
        let Some(member) = self.config.member(member) else {
            return prompt;
        };
        if member.backend != BackendKind::Codex {
            return prompt;
        }
        let marker = format!("${ASTERLINE_TEAM_SKILL_NAME}");
        let team_context = self.team_context_for(member);
        if prompt.contains(&marker) {
            format!("{team_context}\n\n{prompt}")
        } else {
            format!("{team_context}\n\n{}\n\n{prompt}", team_skill_hint())
        }
    }

    fn team_context_for(&self, current: &TeamMember) -> String {
        let mut lines = vec![
            "Current Asterline team roster. Use member ids when routing teammate messages."
                .to_string(),
            format!("You are: {}", self.team_member_card(current)),
            format!("Default target: {}", self.default_target_label()),
            "Members:".to_string(),
        ];
        for member in &self.config.members {
            lines.push(format!("- {}", self.team_member_card(member)));
        }
        lines.join("\n")
    }

    fn team_member_card(&self, member: &TeamMember) -> String {
        let status = self
            .members
            .get(&member.id)
            .map(|state| state.status)
            .unwrap_or(MemberStatus::Idle);
        let model = member.model.as_deref().unwrap_or("-");
        let effort = member.effort.map(Effort::as_str).unwrap_or("-");
        let permission = member
            .permission_mode
            .map(|mode| mode.claude_arg())
            .unwrap_or("-");
        let allowed_tools = if member.allowed_tools.is_empty() {
            "-".to_string()
        } else {
            member.allowed_tools.join(",")
        };
        format!(
            "id={} display_name={:?} backend={} role={:?} status={} model={} effort={} cwd={:?} sandbox={} permission_mode={} session_policy={} allowed_tools={}",
            member.id,
            member.display_name,
            member.backend.as_str(),
            member.role,
            status.as_str(),
            model,
            effort,
            member
                .resolved_cwd(&self.config.workspace)
                .display()
                .to_string(),
            member.sandbox.codex_arg(),
            permission,
            session_policy_label(member.session_policy),
            allowed_tools,
        )
    }

    fn default_target_label(&self) -> String {
        match &self.config.default_target {
            Some(DefaultTarget::All) => "all".to_string(),
            Some(DefaultTarget::Member(id)) => id.to_string(),
            None => self
                .config
                .members
                .first()
                .map(|member| member.id.to_string())
                .unwrap_or_else(|| "-".to_string()),
        }
    }

    fn check_turn_complete(&mut self, turn: TurnId, step: &mut RuntimeStep) {
        if !self.turn_active(turn) {
            self.relay.reset_turn(turn);
            if let Some(run_id) = self.workflow_turns.remove(&turn)
                && !self.failed_workflow_runs.contains(&run_id)
                && let Ok(run) = self
                    .store
                    .update_workflow_status(run_id, WorkflowRunStatus::Done)
            {
                step.events.push(RuntimeEvent::WorkflowRunUpdated { run });
            }
            step.events.push(RuntimeEvent::TurnFinished { turn });
        }
    }

    fn mark_workflow_turn(
        &mut self,
        turn: TurnId,
        status: WorkflowRunStatus,
        step: &mut RuntimeStep,
    ) {
        let Some(run_id) = self.workflow_turns.get(&turn).copied() else {
            return;
        };
        if status == WorkflowRunStatus::Failed {
            self.failed_workflow_runs.insert(run_id);
        }
        if let Ok(run) = self.store.update_workflow_status(run_id, status) {
            step.events.push(RuntimeEvent::WorkflowRunUpdated { run });
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

// Workflow handlers (handle_run_workflow … on_verify_output), split out for
// readability. Still inside this module so private fields are accessible.
include!("team_runtime_workflow.inc.rs");

fn relay_prompt(from_display: &str, body: &str) -> String {
    format!("[relay from {from_display}]\n{body}")
}

fn session_policy_label(policy: SessionPolicy) -> &'static str {
    match policy {
        SessionPolicy::Resume => "resume",
        SessionPolicy::Fresh => "fresh",
    }
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

fn summarize_verify_output(stdout: &[u8], stderr: &[u8]) -> String {
    let mut text = String::new();
    let stdout = String::from_utf8_lossy(stdout);
    let stderr = String::from_utf8_lossy(stderr);
    if !stdout.trim().is_empty() {
        text.push_str(stdout.trim());
    }
    if !stderr.trim().is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(stderr.trim());
    }
    let text = text.lines().rev().take(12).collect::<Vec<_>>();
    let mut summary = text.into_iter().rev().collect::<Vec<_>>().join("\n");
    if summary.chars().count() > 1200 {
        summary = summary.chars().take(1199).collect::<String>() + "…";
    }
    if summary.is_empty() {
        "verification produced no output".to_string()
    } else {
        summary
    }
}

#[cfg(test)]
#[path = "team_runtime_tests.rs"]
mod tests;
