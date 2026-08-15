//! The team runtime core.
//!
//! Pure orchestration logic: [`TeamRuntime::on_ui_command`] and
//! [`TeamRuntime::on_agent_event`] take an input and return the
//! [`RuntimeEvent`]s to emit plus the [`RunAction`]s to dispatch. All threading
//! and child-process work lives in the transport layer (`agent_runner` / the
//! `run` loop), so the core is fully unit-testable without spawning anything.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::adapter::parser::{
    MAX_MESSAGE_TEXT_BYTES, MAX_TOOL_DETAIL_BYTES, append_bounded_text, bounded_text,
};
use crate::domain::config::{
    ASTERLINE_BRAINSTORM_SKILL_NAME, ASTERLINE_TEAM_SKILL_NAME, brainstorm_skill_text,
    inject_team_protocol, strip_team_protocol, strip_team_protocols, team_skill_hint,
};
use crate::domain::event::{
    AgentEvent, AgentSessionId, ApprovalDecision, ApprovalId, ImportedMessage, LogEntry,
    MemberStatus, MemberSummary, MessageId, MessageTarget, RunId, RunStatus, RunStepRequest,
    RunStepStatus, RunStepSummary, RunSummary, RuntimeEvent, TurnId, UiCommand,
};
use crate::domain::mode::{
    BrainstormCard, CollabMode, ModeStatusSummary, ReviewVerdict, ReviewVerdictKind, TerminalMode,
    resolve_mode_roles, resolve_team_coordinator, resolve_team_limits, resolve_verify_command,
};
use crate::domain::team::{
    ApprovalSurface, BackendKind, DefaultTarget, Effort, MemberId, SessionPolicy, TeamConfig,
    TeamMember,
};
use crate::router::{self, RelayDecision, RelayGuard, parse_agent_output};
use crate::run_support::suggested_verify_command;
use crate::runtime::approval::ApprovalMatcher;
use crate::runtime::mode_prompts::{
    brainstorm_build_prompt, brainstorm_propose_prompt, brainstorm_stretch_prompt,
    brainstorm_synthesis_prompt, brainstorm_vote_prompt, plan_iteration_prompt, plan_nudge_prompt,
    plan_plan_prompt, plan_progress_prompt, plan_review_prompt, plan_verify_failure_prompt,
    review_iteration_prompt, review_prompt, review_task_prompt, step_dispatch_prompt,
    verdict_nudge_prompt, verify_failure_prompt,
};
use crate::runtime::session_registry::SessionRegistry;
use crate::store::sqlite::{SqliteStore, StoredConversationSession};

pub(super) const MAX_IMPORTED_ITEMS: usize = 1_000;
pub(super) const MAX_IMPORTED_ITEM_BYTES: usize = 1024 * 1024;
pub(super) const MAX_IMPORTED_TOTAL_BYTES: usize = 8 * 1024 * 1024;
const MAX_ACTIVE_REASONING_BYTES: usize = 8 * 1024;

/// What the core wants the transport layer to do after handling an input.
#[derive(Default)]
pub struct RuntimeStep {
    pub events: Vec<RuntimeEvent>,
    pub actions: Vec<RunAction>,
    pub verify_actions: Vec<VerifyAction>,
    pub runner_changes: Vec<RunnerChange>,
    pub runner_controls: Vec<RunnerControl>,
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

/// A control message for a live backend runner. These are delivered by the
/// transport outside the pure runtime core, after the corresponding decision
/// has been durably recorded.
pub enum RunnerControl {
    ResolveNativeApproval {
        member: MemberId,
        request_id: u64,
        decision: ApprovalDecision,
    },
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
    pub run_id: RunId,
    pub command: String,
    pub workspace: PathBuf,
    pub cancel: Arc<AtomicBool>,
}

/// Result of a completed verification command.
pub struct VerifyOutput {
    pub run_id: RunId,
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
    reasoning: String,
    failed: bool,
    raw_persistence_failed: bool,
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
    prompt: String,
}

struct HeldApproval {
    turn: TurnId,
    targets: Vec<MemberId>,
    /// The prompt actually enqueued on approve (relay-wrapped for relays).
    prompt: String,
    /// The mode run to block if this dispatch is rejected (set by the M3 engine).
    mode_run: Option<RunId>,
    /// Agent-originated roster mutation to apply only after explicit approval.
    member_request: Option<(MemberId, TeamMember)>,
}

/// An approval emitted by a live backend while a turn is already running.
/// Unlike [`HeldApproval`], approving it resumes the same runner instead of
/// enqueueing a new prompt.
struct NativeApproval {
    member: MemberId,
    request_id: u64,
    turn: TurnId,
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
    native_approvals: HashMap<ApprovalId, NativeApproval>,
    run_turns: HashMap<TurnId, RunId>,
    failed_runs: HashSet<RunId>,
    mode_sessions: HashMap<RunId, ModeSession>,
    /// Selection for subsequent messages in the current chat. `/new` resets it
    /// to normal; another `/mode` selection replaces it within the chat.
    active_mode: TerminalMode,
    last_user: Option<(MessageTarget, String)>,
    next_message_id: u64,
    approvals_enabled: bool,
    matcher: ApprovalMatcher,
    startup_notices: Vec<String>,
    startup_reconciled: bool,
}

impl TeamRuntime {
    pub fn new(config: TeamConfig, store: SqliteStore) -> Self {
        let mut startup_notices = Vec::new();
        if let Err(err) = store.upsert_team(&config) {
            startup_notices.push(format!("could not save the initial team: {err}"));
        }
        // Bind to the latest conversation so records and replay agree.
        match store.current_conversation() {
            Ok(conversation) => {
                if let Err(err) = store.set_conversation(conversation) {
                    startup_notices.push(format!("could not select the current chat: {err}"));
                }
            }
            Err(err) => {
                startup_notices.push(format!("could not load the current chat: {err}"));
            }
        }
        match store.reject_pending_approvals_for_active_conversation() {
            Ok(0) => {}
            Ok(count) => startup_notices.push(format!(
                "rejected {count} approval request(s) interrupted by restart"
            )),
            Err(err) => startup_notices.push(format!(
                "could not reject approval requests interrupted by restart: {err}"
            )),
        }
        let active_mode = match store.conversation_snapshot(store.active_conversation()) {
            Ok(Some(snapshot)) => snapshot.mode,
            Ok(None) => TerminalMode::Normal,
            Err(err) => {
                startup_notices.push(format!("could not restore the current chat mode: {err}"));
                TerminalMode::Normal
            }
        };
        // In-flight mode runs cannot be resumed losslessly across process restarts.
        let mut startup_reconciled = true;
        match store.running_mode_runs() {
            Ok(ids) => {
                for id in ids {
                    if let Err(err) = store.block_run(id, "interrupted by restart") {
                        startup_reconciled = false;
                        startup_notices
                            .push(format!("could not block interrupted run {id}: {err}"));
                    }
                }
            }
            Err(err) => {
                startup_reconciled = false;
                startup_notices.push(format!("could not inspect interrupted runs: {err}"));
            }
        }
        let mut sessions = SessionRegistry::from_store(&store, &config.members);
        for member in &config.members {
            if let Some(id) = &member.session_id {
                let session = AgentSessionId(id.clone());
                sessions.set(member.id.clone(), session.clone());
                if let Err(err) = store.upsert_session(&member.id, member.backend, &session) {
                    startup_notices.push(format!(
                        "could not save configured session for {}: {err}",
                        member.id
                    ));
                }
            }
        }
        let members = config
            .members
            .iter()
            .map(|m| (m.id.clone(), MemberState::new(m.effort)))
            .collect();
        let relay = RelayGuard::new(config.max_auto_relays);
        let matcher = ApprovalMatcher::from_policy(&config.approvals);
        let runtime = Self {
            config,
            store,
            relay,
            sessions,
            members,
            relay_paused: false,
            paused_routes: VecDeque::new(),
            held_approvals: HashMap::new(),
            native_approvals: HashMap::new(),
            run_turns: HashMap::new(),
            failed_runs: HashSet::new(),
            mode_sessions: HashMap::new(),
            active_mode,
            last_user: None,
            next_message_id: 0,
            approvals_enabled: true,
            matcher,
            startup_notices,
            startup_reconciled,
        };
        let mut runtime = runtime;
        if let Err(err) = runtime.persist_conversation_snapshot() {
            runtime
                .startup_notices
                .push(format!("could not save the initial chat snapshot: {err}"));
        }
        runtime
    }

    /// Disable the risky-action approval gate (used in tests and by `--debug`).
    pub fn with_approvals(mut self, enabled: bool) -> Self {
        self.approvals_enabled = enabled;
        self
    }

    pub fn active_mode(&self) -> TerminalMode {
        self.active_mode
    }

    pub fn take_startup_events(&mut self) -> Vec<RuntimeEvent> {
        std::mem::take(&mut self.startup_notices)
            .into_iter()
            .map(RuntimeEvent::Notice)
            .collect()
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
            runs: self.store.recent_runs(50).unwrap_or_default(),
        }
    }

    // === command handling ===============================================

    pub fn on_ui_command(&mut self, cmd: UiCommand) -> RuntimeStep {
        let mut step = RuntimeStep::default();
        let requires_reconciled_startup = matches!(
            &cmd,
            UiCommand::UserMessage { .. }
                | UiCommand::Retry
                | UiCommand::NewSession
                | UiCommand::ResumeConversation { .. }
                | UiCommand::RequestAttach { .. }
                | UiCommand::Approve {
                    decision: ApprovalDecision::Approve,
                    ..
                }
                | UiCommand::ResolvePausedRoute { resume: true }
                | UiCommand::ContinueRun { .. }
                | UiCommand::VerifyRun { .. }
                | UiCommand::RunMode { .. }
        );
        if requires_reconciled_startup && !self.startup_reconciled {
            let reason = "cannot change chats or dispatch work because interrupted runs were not reconciled; restart after fixing SQLite persistence"
                .to_string();
            match &cmd {
                UiCommand::RequestAttach { member } => {
                    step.events.push(RuntimeEvent::AttachDenied {
                        member: member.clone(),
                        reason,
                    });
                }
                _ => step.events.push(RuntimeEvent::Notice(reason)),
            }
            return step;
        }
        match cmd {
            UiCommand::SetMode { mode } => {
                let previous = self.active_mode;
                self.active_mode = mode;
                if let Err(err) = self.persist_conversation_snapshot() {
                    self.active_mode = previous;
                    step.events.push(RuntimeEvent::Notice(format!(
                        "could not save terminal mode: {err}"
                    )));
                    return step;
                }
                step.events.push(RuntimeEvent::ModeChanged { mode });
                step.events.push(RuntimeEvent::Notice(format!(
                    "terminal mode → {mode} (applies until changed)"
                )));
            }
            UiCommand::UserMessage { target, body } => {
                self.handle_active_user_message(target, body, &mut step);
            }
            UiCommand::Cancel { member } => self.handle_cancel(member, &mut step),
            UiCommand::Retry => {
                if let Some((target, body)) = self.last_user.clone() {
                    self.handle_active_user_message(target, body, &mut step);
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
            UiCommand::ReplaceTeam {
                members,
                default_target,
            } => self.handle_replace_team(members, default_target, &mut step),
            UiCommand::NewSession => self.handle_new_session(&mut step),
            UiCommand::RequestResume => self.handle_request_resume(&mut step),
            UiCommand::ResumeConversation { conversation } => {
                self.handle_resume_conversation(conversation, &mut step)
            }
            // Transcript import is valid only through the transport's matching
            // AttachFinished reservation. Keep the public legacy command from
            // bypassing that trust boundary when the core is used directly.
            UiCommand::ImportTranscript { .. } => step.events.push(RuntimeEvent::Notice(
                "ignored direct transcript import without an attach reservation".to_string(),
            )),
            UiCommand::RequestAttach { member } => self.handle_request_attach(member, &mut step),
            // The transport owns the attach reservation. A stray completion
            // delivered directly to the synchronous core is intentionally a
            // no-op.
            UiCommand::AttachFinished { .. } => {}
            UiCommand::ContinueRun { run_id, note } => {
                self.handle_continue_run(run_id, note, &mut step)
            }
            UiCommand::NoteRun { run_id, note } => self.handle_note_run(run_id, note, &mut step),
            UiCommand::BlockRun { run_id, reason } => {
                self.handle_block_run(run_id, reason, &mut step)
            }
            UiCommand::VerifyRun { run_id, command } => {
                self.handle_verify_run(run_id, command, &mut step)
            }
            UiCommand::AddRunStep {
                run_id,
                owner,
                title,
            } => self.handle_add_run_step(run_id, owner, title, &mut step),
            UiCommand::UpdateRunStep {
                run_id,
                step: step_number,
                status,
                note,
            } => self.handle_update_run_step(run_id, step_number, status, note, &mut step),
            UiCommand::RenameRunStep {
                run_id,
                step: step_number,
                title,
            } => self.handle_rename_run_step(run_id, step_number, title, &mut step),
            UiCommand::RemoveRunStep {
                run_id,
                step: step_number,
            } => self.handle_remove_run_step(run_id, step_number, &mut step),
            UiCommand::AssignRunStep {
                run_id,
                step: step_number,
                owner,
            } => self.handle_assign_run_step(run_id, step_number, owner, &mut step),
            UiCommand::RunMode { mode, task } => self.handle_run_mode(mode, task, &mut step),
            UiCommand::Shutdown => self.handle_cancel(None, &mut step),
        }
        step
    }

    /// Import the transcript owned by a transport-validated attach completion.
    /// The transport calls this only while the matching reservation is held.
    pub(super) fn import_attached_transcript(
        &mut self,
        member: MemberId,
        session: Option<AgentSessionId>,
        items: Vec<ImportedMessage>,
    ) -> RuntimeStep {
        let mut step = RuntimeStep::default();
        if let Some(session) = session {
            self.record_member_session(&member, session, &mut step);
        }
        self.handle_import_transcript(member, items, &mut step);
        step
    }

    fn handle_active_user_message(
        &mut self,
        target: MessageTarget,
        body: String,
        step: &mut RuntimeStep,
    ) {
        self.last_user = Some((target.clone(), body.clone()));
        let task = strip_routing_prefix(&body);
        match self.active_mode {
            TerminalMode::Normal => {
                self.handle_user_message(target, body, step);
            }
            TerminalMode::Team => self.handle_run_team(task, step),
            mode => self.handle_run_mode(
                mode.collab_mode().expect("collaboration terminal mode"),
                task,
                step,
            ),
        }
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
        if let Err(err) = self.store.record_user(turn, &targets, &body) {
            self.report_store_error("save the user message", err, step);
            return None;
        }
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
            && self.matcher.applies_to(ApprovalSurface::User)
            && let Some(kind) = self.matcher.classify(&body)
        {
            match self.store.insert_approval(Some(turn), None, &kind, &body) {
                Ok(id) => {
                    self.held_approvals.insert(
                        id,
                        HeldApproval {
                            turn,
                            targets,
                            prompt: body.clone(),
                            mode_run: None,
                            member_request: None,
                        },
                    );
                    step.events.push(RuntimeEvent::ApprovalRequested {
                        id,
                        member: None,
                        action: kind,
                        body,
                    });
                }
                Err(err) => {
                    self.report_store_error("save an approval request", err, step);
                    step.events.push(RuntimeEvent::TurnFinished { turn });
                }
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
        let mut cancelled_approval_turns = Vec::new();
        let mut cancelled_route_turns = HashSet::new();
        let native_approval_ids: Vec<ApprovalId> = self
            .native_approvals
            .iter()
            .filter(|(_, held)| member.as_ref().is_none_or(|target| target == &held.member))
            .map(|(id, _)| *id)
            .collect();
        for id in native_approval_ids {
            let Some(held) = self.native_approvals.remove(&id) else {
                continue;
            };
            if let Err(err) = self.store.resolve_approval(id, ApprovalDecision::Reject) {
                self.report_store_error("cancel a native approval", err, step);
            }
            step.events.push(RuntimeEvent::ApprovalResolved {
                id,
                decision: ApprovalDecision::Reject,
            });
            step.runner_controls
                .push(RunnerControl::ResolveNativeApproval {
                    member: held.member,
                    request_id: held.request_id,
                    decision: ApprovalDecision::Reject,
                });
        }
        let targets: Vec<MemberId> = match member {
            Some(m) => vec![m],
            None => {
                self.block_all_mode_sessions("aborted by user", step);
                let team_runs: HashSet<RunId> = self
                    .run_turns
                    .values()
                    .copied()
                    .filter(|run_id| {
                        self.store.run(*run_id).is_ok_and(|run| {
                            run.mode
                                .as_ref()
                                .is_some_and(|mode| mode.mode == CollabMode::Team)
                        })
                    })
                    .collect();
                for run_id in team_runs {
                    self.block_mode_run(run_id, "aborted by user", step);
                }
                cancelled_route_turns.extend(self.paused_routes.drain(..).map(|route| route.turn));
                step.events
                    .push(RuntimeEvent::RouteQueueUpdated { queued: 0 });
                for (id, held) in std::mem::take(&mut self.held_approvals) {
                    cancelled_approval_turns.push(held.turn);
                    if let Err(err) = self.store.resolve_approval(id, ApprovalDecision::Reject) {
                        self.report_store_error("cancel a pending approval", err, step);
                    }
                    step.events.push(RuntimeEvent::ApprovalResolved {
                        id,
                        decision: ApprovalDecision::Reject,
                    });
                }
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
        for turn in cancelled_approval_turns {
            cancelled_route_turns.insert(turn);
        }
        for turn in cancelled_route_turns {
            if !step.events.iter().any(
                |event| matches!(event, RuntimeEvent::TurnFinished { turn: done } if *done == turn),
            ) {
                self.check_turn_complete(turn, step);
            }
        }
    }

    fn handle_new_session(&mut self, step: &mut RuntimeStep) {
        if self.has_active_work() {
            step.events.push(RuntimeEvent::Notice(
                "cannot start a new chat while members or runs are active; press Esc to cancel work first"
                    .to_string(),
            ));
            return;
        }
        if let Err(err) = self.persist_conversation_snapshot() {
            step.events.push(RuntimeEvent::Notice(format!(
                "could not save the current chat before /new: {err}"
            )));
            return;
        }
        // A fresh chat: a new conversation (so the transcript starts clean and
        // restart shows only this chat) plus new backend sessions for everyone.
        let mut snapshot_team = strip_team_protocols(self.config.clone());
        for member in &mut snapshot_team.members {
            member.session_id = None;
        }
        match self
            .store
            .create_fresh_conversation(&snapshot_team, TerminalMode::Normal)
        {
            Ok(_) => {}
            Err(err) => {
                step.events.push(RuntimeEvent::Notice(format!(
                    "could not start a new chat: {err}"
                )));
                return;
            }
        }
        let previous_mode = self.active_mode;
        self.active_mode = TerminalMode::Normal;
        self.sessions = SessionRegistry::new();
        for member in &mut self.config.members {
            member.session_id = None;
        }
        // Drop any in-flight turn state from the previous chat.
        self.paused_routes.clear();
        self.held_approvals.clear();
        self.last_user = None;
        if previous_mode != TerminalMode::Normal {
            step.events.push(RuntimeEvent::ModeChanged {
                mode: TerminalMode::Normal,
            });
        }
        step.events.push(RuntimeEvent::SessionReset);
        step.events.push(RuntimeEvent::Notice(
            "started a new chat in normal mode — fresh session for all members".to_string(),
        ));
    }

    fn handle_request_resume(&self, step: &mut RuntimeStep) {
        self.persist_snapshot_or_notice("save the current chat", step);
        match self.store.resumable_conversations() {
            Ok(conversations) => step
                .events
                .push(RuntimeEvent::ResumeChoices { conversations }),
            Err(err) => step.events.push(RuntimeEvent::Notice(format!(
                "could not list saved chats: {err}"
            ))),
        }
    }

    fn handle_resume_conversation(&mut self, conversation: i64, step: &mut RuntimeStep) {
        if self.has_active_work() {
            step.events.push(RuntimeEvent::Notice(
                "cannot resume another chat while members or runs are active; press Esc to cancel work first"
                    .to_string(),
            ));
            return;
        }

        if let Err(err) = self.persist_conversation_snapshot() {
            step.events.push(RuntimeEvent::Notice(format!(
                "could not save the current chat before /resume: {err}"
            )));
            return;
        }
        let snapshot = match self.store.conversation_snapshot(conversation) {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) => {
                step.events.push(RuntimeEvent::Notice(format!(
                    "saved chat {conversation} is no longer available"
                )));
                return;
            }
            Err(err) => {
                step.events.push(RuntimeEvent::Notice(format!(
                    "could not restore saved chat {conversation}: {err}"
                )));
                return;
            }
        };
        let mode = snapshot.mode;
        let raw_config = strip_team_protocols(snapshot.team);
        if let Err(err) = raw_config.validate() {
            step.events.push(RuntimeEvent::Notice(format!(
                "saved chat {conversation} has an invalid team: {err}"
            )));
            return;
        }
        let chat = match self.store.replay_chat_for(conversation) {
            Ok(chat) => chat,
            Err(err) => {
                step.events.push(RuntimeEvent::Notice(format!(
                    "could not replay saved chat {conversation}: {err}"
                )));
                return;
            }
        };
        let restored_sessions: Vec<StoredConversationSession> = snapshot
            .sessions
            .into_iter()
            .filter(|saved| {
                raw_config
                    .find(saved.member.as_str())
                    .is_some_and(|member| member.backend == saved.backend)
            })
            .collect();
        let mut operational_config = raw_config.clone();
        inject_team_protocol(&mut operational_config);
        let rejected = match self.store.activate_runtime_team_state(
            conversation,
            &operational_config,
            &raw_config,
            &restored_sessions,
            mode,
        ) {
            Ok(rejected) => rejected,
            Err(err) => {
                self.report_store_error("restore the selected chat atomically", err, step);
                return;
            }
        };
        if rejected > 0 {
            step.events.push(RuntimeEvent::Notice(format!(
                "rejected {rejected} approval request(s) interrupted before this chat was resumed"
            )));
        }

        let old_ids: Vec<MemberId> = self.members.keys().cloned().collect();
        for id in old_ids {
            step.runner_changes.push(RunnerChange::Remove(id));
        }

        self.config = operational_config;
        self.members = self
            .config
            .members
            .iter()
            .map(|member| (member.id.clone(), MemberState::new(member.effort)))
            .collect();
        self.sessions = SessionRegistry::new();
        for saved in restored_sessions {
            self.sessions
                .set(saved.member, AgentSessionId(saved.session_id));
        }
        self.matcher = ApprovalMatcher::from_policy(&self.config.approvals);
        self.relay = RelayGuard::new(self.config.max_auto_relays);
        self.paused_routes.clear();
        self.held_approvals.clear();
        self.run_turns.clear();
        self.failed_runs.clear();
        self.mode_sessions.clear();
        self.active_mode = mode;
        self.last_user = None;
        for member in self.config.members.clone() {
            step.runner_changes.push(RunnerChange::Upsert {
                member,
                workspace: self.config.workspace.clone(),
            });
        }
        step.persist_team = Some(raw_config);
        step.events
            .push(RuntimeEvent::ConversationResumed { conversation, chat });
        step.events.push(self.ready_event());
        step.events.push(RuntimeEvent::ModeChanged { mode });
        step.events.push(RuntimeEvent::Notice(format!(
            "resumed saved chat {conversation}"
        )));
    }

    fn persist_conversation_snapshot(&self) -> crate::store::sqlite::Result<()> {
        self.persist_conversation_snapshot_for(&self.config)
    }

    fn persist_conversation_snapshot_for(
        &self,
        config: &TeamConfig,
    ) -> crate::store::sqlite::Result<()> {
        if self.store.active_conversation() <= 0 {
            return Ok(());
        }
        let team = strip_team_protocols(config.clone());
        let sessions = config
            .members
            .iter()
            .filter_map(|member| {
                self.sessions
                    .get(&member.id)
                    .map(|session| StoredConversationSession {
                        member: member.id.clone(),
                        backend: member.backend,
                        session_id: session.0,
                    })
            })
            .collect::<Vec<_>>();
        self.store
            .save_conversation_snapshot(&team, &sessions, self.active_mode)
    }

    fn persist_snapshot_or_notice(&self, context: &str, step: &mut RuntimeStep) {
        if let Err(err) = self.persist_conversation_snapshot() {
            self.report_store_error(context, err, step);
        }
    }

    fn report_store_error(&self, context: &str, err: rusqlite::Error, step: &mut RuntimeStep) {
        step.events
            .push(RuntimeEvent::Notice(format!("could not {context}: {err}")));
    }

    fn has_active_work(&self) -> bool {
        self.members
            .values()
            .any(|state| state.running.is_some() || !state.queue.is_empty())
            || !self.paused_routes.is_empty()
            || !self.held_approvals.is_empty()
            || !self.native_approvals.is_empty()
            || !self.run_turns.is_empty()
            || !self.mode_sessions.is_empty()
    }

    fn handle_request_attach(&self, member: MemberId, step: &mut RuntimeStep) {
        if self.config.member(&member).is_none() {
            let reason = format!("cannot attach: unknown member {member}");
            step.events
                .push(RuntimeEvent::AttachDenied { member, reason });
            return;
        }
        if self.has_active_work() {
            step.events.push(RuntimeEvent::AttachDenied {
                member,
                reason: "cannot attach while member work, verification, routing, approval, or a run is active; press Esc to cancel it or resolve it first"
                    .to_string(),
            });
            return;
        }
        step.events.push(RuntimeEvent::AttachGranted { member });
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

        let previous_raw = strip_team_protocols(self.config.clone());
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

        for member in &raw_config.members {
            let changed = previous_raw
                .member(&member.id)
                .is_some_and(|old| old != member);
            let active = self.members.get(&member.id).is_some_and(|state| {
                state.status != MemberStatus::Idle
                    || state.running.is_some()
                    || !state.queue.is_empty()
            });
            if changed && active {
                step.events.push(RuntimeEvent::Notice(format!(
                    "cannot update {} while it is active",
                    member.id
                )));
                return;
            }
        }

        let old_members: HashMap<MemberId, TeamMember> = self
            .config
            .members
            .iter()
            .cloned()
            .map(|member| (member.id.clone(), member))
            .collect();
        let reset_session_ids: Vec<MemberId> = raw_config
            .members
            .iter()
            .filter(|member| {
                old_members.get(&member.id).is_some_and(|old| {
                    old.backend != member.backend
                        || (old.session_policy != SessionPolicy::Fresh
                            && member.session_policy == SessionPolicy::Fresh)
                        || old.session_id != member.session_id
                })
            })
            .map(|member| member.id.clone())
            .collect();

        let removed_ids: Vec<MemberId> = old_ids.difference(&new_ids).cloned().collect();
        let removed_set: HashSet<MemberId> = removed_ids.iter().cloned().collect();
        let approvals_to_reject: Vec<(ApprovalId, TurnId, Option<RunId>)> = self
            .held_approvals
            .iter()
            .filter(|(_, held)| {
                held.targets.iter().any(|id| removed_set.contains(id))
                    || held.member_request.as_ref().is_some_and(|(from, member)| {
                        removed_set.contains(from) || removed_set.contains(&member.id)
                    })
            })
            .map(|(id, held)| (*id, held.turn, held.mode_run))
            .collect();
        let approval_ids: Vec<ApprovalId> =
            approvals_to_reject.iter().map(|(id, _, _)| *id).collect();
        let next_sessions: Vec<StoredConversationSession> = raw_config
            .members
            .iter()
            .filter_map(|member| {
                let session_id = member.session_id.clone().or_else(|| {
                    (!reset_session_ids.contains(&member.id))
                        .then(|| {
                            self.sessions
                                .get(&member.id)
                                .map(|session| session.0.clone())
                        })
                        .flatten()
                })?;
                Some(StoredConversationSession {
                    member: member.id.clone(),
                    backend: member.backend,
                    session_id,
                })
            })
            .collect();
        let mut operational_config = raw_config.clone();
        inject_team_protocol(&mut operational_config);
        if let Err(err) = self.store.replace_runtime_team_state(
            &operational_config,
            &raw_config,
            &next_sessions,
            self.active_mode,
            &approval_ids,
        ) {
            self.report_store_error("save the updated team atomically", err, step);
            return;
        }

        let mut cleanup_turns: HashSet<TurnId> = approvals_to_reject
            .iter()
            .map(|(_, turn, _)| *turn)
            .collect();
        let mut affected_runs: HashSet<RunId> = approvals_to_reject
            .iter()
            .filter_map(|(_, _, run_id)| *run_id)
            .collect();
        for (id, _, _) in approvals_to_reject {
            self.held_approvals.remove(&id);
            step.events.push(RuntimeEvent::ApprovalResolved {
                id,
                decision: ApprovalDecision::Reject,
            });
        }

        self.paused_routes.retain(|route| {
            let keep = !removed_set.contains(&route.from)
                && !route.to_members.iter().any(|id| removed_set.contains(id));
            if !keep {
                cleanup_turns.insert(route.turn);
                if let Some(run_id) = self.run_turns.get(&route.turn) {
                    affected_runs.insert(*run_id);
                }
            }
            keep
        });
        for run_id in affected_runs {
            self.block_mode_run(run_id, "member removed while dispatch was pending", step);
        }

        for id in &removed_ids {
            self.members.remove(id);
            step.runner_changes.push(RunnerChange::Remove(id.clone()));
        }

        self.sessions = SessionRegistry::new();
        for saved in &next_sessions {
            self.sessions.set(
                saved.member.clone(),
                AgentSessionId(saved.session_id.clone()),
            );
        }

        for member in &raw_config.members {
            self.members
                .entry(member.id.clone())
                .or_insert_with(|| MemberState::new(member.effort))
                .effort = member.effort;
        }

        self.config = operational_config;
        // Rebuild even though ReplaceTeam does not carry approvals yet, so a
        // future path that mutates config.approvals stays correct.
        self.matcher = ApprovalMatcher::from_policy(&self.config.approvals);
        for member in self.config.members.clone() {
            step.runner_changes.push(RunnerChange::Upsert {
                member,
                workspace: self.config.workspace.clone(),
            });
        }
        for turn in cleanup_turns {
            self.check_turn_complete(turn, step);
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
        let original_count = items.len();
        let mut retained_bytes = 0_usize;
        let mut truncated = false;
        let mut oversized_items = 0_usize;
        let mut retained = Vec::with_capacity(original_count.min(MAX_IMPORTED_ITEMS));
        for (index, item) in items.into_iter().enumerate() {
            if index == MAX_IMPORTED_ITEMS {
                truncated = true;
                break;
            }
            if item.text.len() > MAX_IMPORTED_ITEM_BYTES {
                truncated = true;
                oversized_items = oversized_items.saturating_add(1);
                continue;
            }
            let remaining = MAX_IMPORTED_TOTAL_BYTES.saturating_sub(retained_bytes);
            if remaining == 0 || item.text.len() > remaining {
                truncated = true;
                break;
            }
            retained_bytes = retained_bytes.saturating_add(item.text.len());
            retained.push(item);
            if retained_bytes == MAX_IMPORTED_TOTAL_BYTES {
                truncated |= retained.len() < original_count;
                break;
            }
        }
        if retained.is_empty() {
            if truncated {
                step.events.push(RuntimeEvent::Notice(format!(
                    "attached transcript from {id} exceeded import limits and contained no retainable messages (skipped {oversized_items} oversized message(s))"
                )));
            }
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
        let count = retained.len();
        step.events.push(RuntimeEvent::TurnStarted { turn });
        for item in retained {
            if item.from_user {
                if let Err(err) =
                    self.store
                        .record_user(turn, std::slice::from_ref(&id), &item.text)
                {
                    self.report_store_error("save an imported user message", err, step);
                    continue;
                }
                step.events.push(RuntimeEvent::UserMessage {
                    turn,
                    targets: vec![id.clone()],
                    body: item.text,
                });
            } else {
                if let Err(err) = self
                    .store
                    .record_agent(turn, &id, &display, backend, &item.text)
                {
                    self.report_store_error("save an imported agent message", err, step);
                    continue;
                }
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
        if truncated || count < original_count {
            step.events.push(RuntimeEvent::Notice(format!(
                "attached transcript from {id} was truncated to {count} message(s) and {retained_bytes} bytes; skipped {oversized_items} oversized message(s)"
            )));
        }
        step.events.push(RuntimeEvent::TurnFinished { turn });
    }

    fn handle_approval(
        &mut self,
        id: ApprovalId,
        decision: ApprovalDecision,
        step: &mut RuntimeStep,
    ) {
        if let Some(held) = self.native_approvals.remove(&id) {
            match self.store.resolve_approval(id, decision) {
                Ok(true) => step
                    .events
                    .push(RuntimeEvent::ApprovalResolved { id, decision }),
                Ok(false) => {
                    step.events
                        .push(RuntimeEvent::Notice(format!("no pending approval {id}")));
                    return;
                }
                Err(err) => {
                    // Keep the in-memory request alive if its durable row
                    // could not be updated. The runner remains paused, so the
                    // user can safely retry rather than silently continuing.
                    self.native_approvals.insert(id, held);
                    self.report_store_error("resolve a native approval", err, step);
                    return;
                }
            }
            step.runner_controls
                .push(RunnerControl::ResolveNativeApproval {
                    member: held.member,
                    request_id: held.request_id,
                    decision,
                });
            return;
        }
        if decision == ApprovalDecision::Approve
            && let Some(run_id) = self.held_approvals.get(&id).and_then(|held| held.mode_run)
        {
            match self.store.active_run(run_id) {
                Ok(run) if matches!(run.status, RunStatus::Running | RunStatus::Verifying) => {}
                Ok(_) | Err(rusqlite::Error::QueryReturnedNoRows) => {
                    match self.store.resolve_approval(id, ApprovalDecision::Reject) {
                        Ok(true) => step.events.push(RuntimeEvent::ApprovalResolved {
                            id,
                            decision: ApprovalDecision::Reject,
                        }),
                        Ok(false) => {
                            step.events
                                .push(RuntimeEvent::Notice(format!("no pending approval {id}")));
                            return;
                        }
                        Err(err) => {
                            self.report_store_error("reject a stale mode approval", err, step);
                            return;
                        }
                    }
                    let held = self
                        .held_approvals
                        .remove(&id)
                        .expect("approval was inspected above");
                    let mut affected_turns = self.reject_sibling_mode_approvals(run_id, step);
                    affected_turns.insert(held.turn);
                    self.failed_runs.insert(run_id);
                    self.mode_sessions.remove(&run_id);
                    step.events.push(RuntimeEvent::Notice(format!(
                        "approval {id} rejected because run {run_id} is no longer active"
                    )));
                    for turn in affected_turns {
                        self.check_turn_complete(turn, step);
                    }
                    return;
                }
                Err(err) => {
                    self.report_store_error("confirm an approval run is active", err, step);
                    return;
                }
            }
        }
        match self.store.resolve_approval(id, decision) {
            Ok(true) => step
                .events
                .push(RuntimeEvent::ApprovalResolved { id, decision }),
            Ok(false) => {
                step.events
                    .push(RuntimeEvent::Notice(format!("no pending approval {id}")));
                return;
            }
            Err(err) => {
                self.report_store_error("resolve an approval", err, step);
                return;
            }
        }
        let Some(held) = self.held_approvals.remove(&id) else {
            return;
        };
        if let Some((from, member)) = held.member_request {
            match decision {
                ApprovalDecision::Approve => {
                    self.add_team_member_from_agent(&from, member, step);
                }
                ApprovalDecision::Reject => {
                    step.events.push(RuntimeEvent::Notice(
                        "teammate addition rejected".to_string(),
                    ));
                }
            }
            self.check_turn_complete(held.turn, step);
            return;
        }
        match decision {
            ApprovalDecision::Approve => {
                for member in held.targets {
                    self.enqueue_prompt(&member, held.turn, held.prompt.clone(), step);
                }
            }
            ApprovalDecision::Reject => {
                step.events
                    .push(RuntimeEvent::Notice("request rejected".to_string()));
                if let Some(run_id) = held.mode_run {
                    self.block_mode_run(run_id, "dispatch rejected by user", step);
                    for turn in self.reject_sibling_mode_approvals(run_id, step) {
                        self.check_turn_complete(turn, step);
                    }
                }
                self.check_turn_complete(held.turn, step);
            }
        }
    }

    fn reject_sibling_mode_approvals(
        &mut self,
        run_id: RunId,
        step: &mut RuntimeStep,
    ) -> HashSet<TurnId> {
        let siblings: Vec<(ApprovalId, TurnId)> = self
            .held_approvals
            .iter()
            .filter(|(_, held)| held.mode_run == Some(run_id))
            .map(|(id, held)| (*id, held.turn))
            .collect();
        let ids: Vec<ApprovalId> = siblings.iter().map(|(id, _)| *id).collect();
        if let Err(err) = self.store.reject_pending_approvals(&ids) {
            self.report_store_error("reject sibling mode approvals", err, step);
            return HashSet::new();
        }
        let mut turns = HashSet::new();
        for (id, turn) in siblings {
            self.held_approvals.remove(&id);
            turns.insert(turn);
            step.events.push(RuntimeEvent::ApprovalResolved {
                id,
                decision: ApprovalDecision::Reject,
            });
        }
        turns
    }

    fn resolve_next_paused_route(&mut self, resume: bool, step: &mut RuntimeStep) {
        let Some(route) = self.paused_routes.pop_front() else {
            step.events
                .push(RuntimeEvent::RouteQueueUpdated { queued: 0 });
            step.events
                .push(RuntimeEvent::Notice("no paused routes".to_string()));
            return;
        };
        step.events.push(RuntimeEvent::RouteQueueUpdated {
            queued: self.paused_routes.len(),
        });
        if resume {
            step.events.push(RuntimeEvent::Notice(format!(
                "resumed route {} -> {}",
                route.from,
                route.to_labels.join(", ")
            )));
            for member in route.to_members {
                self.enqueue_prompt(&member, route.turn, route.prompt.clone(), step);
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
                if let Some(msg) = self.message_id(member)
                    && let Some(text) = self.append_text(member, &text)
                {
                    step.events.push(RuntimeEvent::MessageDelta { msg, text });
                }
            }
            AgentEvent::Reasoning(text) => {
                if let Some(text) = self.append_reasoning(member, &text) {
                    step.events.push(RuntimeEvent::Reasoning {
                        member: member.clone(),
                        text,
                    });
                }
            }
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
                let retained_delta = if let Some(tool) = self
                    .members
                    .get_mut(member)
                    .and_then(|state| state.tools.get_mut(&id))
                {
                    append_bounded_text(&mut tool.detail, &delta, MAX_TOOL_DETAIL_BYTES)
                } else {
                    Some(bounded_text(&delta, MAX_TOOL_DETAIL_BYTES))
                };
                if let Some(delta) = retained_delta {
                    step.events.push(RuntimeEvent::ToolProgress {
                        member: member.clone(),
                        tool_id: id,
                        delta,
                    });
                }
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
                    let separator = if !output.is_empty() && !output.ends_with('\n') {
                        "\n"
                    } else {
                        ""
                    };
                    let _ = append_bounded_text(
                        &mut output,
                        &format!("{separator}{summary}"),
                        MAX_TOOL_DETAIL_BYTES,
                    );
                }
                if let Some(turn) = self.running_turn(member)
                    && let Err(err) =
                        self.store
                            .record_tool(turn, member, &name, &input, &output, Some(ok))
                {
                    self.report_store_error("save a tool result", err, &mut step);
                }
                step.events.push(RuntimeEvent::ToolCompleted {
                    member: member.clone(),
                    tool_id: id,
                    ok,
                    output,
                });
            }
            AgentEvent::FileChange { files, ok } => {
                if let Some(turn) = self.running_turn(member)
                    && let Err(err) = self.store.record_diff(turn, member, &files, ok)
                {
                    self.report_store_error("save a file change", err, &mut step);
                }
                step.events.push(RuntimeEvent::FileChange {
                    member: member.clone(),
                    files,
                    ok,
                });
            }
            AgentEvent::SessionDiscovered(session) => {
                self.record_member_session(member, session, &mut step);
            }
            AgentEvent::NativeApprovalRequested {
                request_id,
                action,
                body,
            } => {
                let Some(turn) = self.running_turn(member) else {
                    step.runner_controls
                        .push(RunnerControl::ResolveNativeApproval {
                            member: member.clone(),
                            request_id,
                            decision: ApprovalDecision::Reject,
                        });
                    return step;
                };
                match self
                    .store
                    .insert_approval(Some(turn), Some(member), &action, &body)
                {
                    Ok(id) => {
                        self.native_approvals.insert(
                            id,
                            NativeApproval {
                                member: member.clone(),
                                request_id,
                                turn,
                            },
                        );
                        step.events.push(RuntimeEvent::ApprovalRequested {
                            id,
                            member: Some(member.clone()),
                            action,
                            body,
                        });
                    }
                    Err(err) => {
                        self.report_store_error("save a native approval request", err, &mut step);
                        step.runner_controls
                            .push(RunnerControl::ResolveNativeApproval {
                                member: member.clone(),
                                request_id,
                                decision: ApprovalDecision::Reject,
                            });
                    }
                }
            }
            AgentEvent::Raw(line) => {
                let persistence_disabled = self
                    .members
                    .get(member)
                    .and_then(|state| state.running.as_ref())
                    .is_some_and(|running| running.raw_persistence_failed);
                if persistence_disabled {
                    return step;
                }
                if let Err(err) = self.store.record_stream_event(member, &line) {
                    if let Some(running) = self
                        .members
                        .get_mut(member)
                        .and_then(|state| state.running.as_mut())
                    {
                        running.raw_persistence_failed = true;
                    }
                    self.report_store_error("save a raw stream event", err, &mut step);
                }
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
                let (turn, cancelled) = self
                    .members
                    .get_mut(member)
                    .and_then(|state| state.running.as_mut())
                    .map(|running| {
                        let cancelled = running.cancel.load(Ordering::Relaxed);
                        if !cancelled {
                            running.failed = true;
                        }
                        (running.turn, cancelled)
                    })
                    .unzip();
                if cancelled == Some(true) {
                    self.log(
                        member,
                        LogEntry::info(
                            member.as_str(),
                            format!("backend stopped during cancellation: {message}"),
                        ),
                        &mut step,
                    );
                } else {
                    if let Some(turn) = turn
                        && let Err(err) =
                            self.store.record_error(Some(turn), Some(member), &message)
                    {
                        self.report_store_error("save a member error", err, &mut step);
                    }
                    step.events.push(RuntimeEvent::MemberError {
                        member: member.clone(),
                        message,
                    });
                }
            }
            AgentEvent::Exited { code, ok } => self.finalize_run(member, code, ok, &mut step),
        }
        step
    }

    /// Persist and surface a backend session only when it changed. Both stream
    /// events and a transcript-proven native attach use this path.
    fn record_member_session(
        &mut self,
        member: &MemberId,
        session: AgentSessionId,
        step: &mut RuntimeStep,
    ) {
        if self.sessions.get(member).as_ref() == Some(&session) {
            return;
        }
        let backend = self.member_backend(member);
        self.sessions.set(member.clone(), session.clone());
        if let Err(err) = self.store.upsert_session(member, backend, &session) {
            self.report_store_error("save a member session", err, step);
        }
        self.persist_snapshot_or_notice("save the chat session", step);
        step.events.push(RuntimeEvent::SessionUpdated {
            member: member.clone(),
            session,
        });
    }

    fn log(&self, _member: &MemberId, entry: LogEntry, step: &mut RuntimeStep) {
        let entry = entry.bounded();
        if let Err(err) = self.store.record_log(&entry) {
            step.events.push(RuntimeEvent::Log(LogEntry::error(
                "store",
                format!("could not save a log entry: {err}"),
            )));
        }
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

        let text = bounded_text(&text, MAX_MESSAGE_TEXT_BYTES);
        let parsed = parse_agent_output(&text);
        for warning in &parsed.warnings {
            self.log(
                member,
                LogEntry::warn(member.as_str(), warning.clone()),
                step,
            );
        }

        let has_controls = !parsed.messages.is_empty()
            || !parsed.members.is_empty()
            || !parsed.run_steps.is_empty()
            || !parsed.reviews.is_empty()
            || !parsed.brainstorm_votes.is_empty()
            || !parsed.brainstorm_cards.is_empty();
        if has_controls {
            let raw_persistence_failed = self
                .members
                .get(member)
                .and_then(|state| state.running.as_ref())
                .is_some_and(|running| running.raw_persistence_failed);
            if raw_persistence_failed {
                step.events.push(RuntimeEvent::Notice(format!(
                    "ignored controls from {member} because their raw source could not be saved"
                )));
                self.fail_completed_message(member, msg, step);
                return;
            }
            if let Err(err) = self.store.record_agent_control_source(turn, member, &text) {
                self.report_store_error("save an agent control source", err, step);
                self.fail_completed_message(member, msg, step);
                return;
            }
        }

        let visible_text =
            render_brainstorm_response(&parsed.visible_text, &parsed.brainstorm_cards);
        if !visible_text.is_empty() {
            let display = self.member_display(member);
            let backend = self.member_backend(member);
            if let Err(err) =
                self.store
                    .record_agent(turn, member, &display, backend, &visible_text)
            {
                self.report_store_error("save an agent message", err, step);
                // Streaming deltas are provisional until the canonical message
                // is durable. Clear them and do not execute controls from an
                // output that is absent from the audit trail.
                self.fail_completed_message(member, msg, step);
                return;
            }
        }
        step.events.push(RuntimeEvent::MessageCompleted {
            msg,
            text: visible_text.clone(),
        });

        if let Some(state) = self.members.get_mut(member)
            && let Some(running) = &mut state.running
        {
            running.message = None;
            running.text.clear();
        }

        self.mode_record_message(member, turn, &visible_text, &parsed, step);

        for member_request in parsed.members {
            self.request_team_member_from_agent(member, turn, member_request, step);
        }
        for request in parsed.run_steps {
            self.apply_run_step_from_agent(member, turn, request, step);
        }
        for tmsg in parsed.messages {
            self.route_team_message(member, turn, tmsg, step);
        }
    }

    fn fail_completed_message(
        &mut self,
        member: &MemberId,
        msg: MessageId,
        step: &mut RuntimeStep,
    ) {
        if let Some(state) = self.members.get_mut(member)
            && let Some(running) = &mut state.running
        {
            running.failed = true;
            running.message = None;
            running.text.clear();
        }
        step.events.push(RuntimeEvent::MessageCompleted {
            msg,
            text: String::new(),
        });
    }

    fn request_team_member_from_agent(
        &mut self,
        from: &MemberId,
        turn: TurnId,
        member: TeamMember,
        step: &mut RuntimeStep,
    ) {
        if !self.validate_agent_team_member(from, &member, step) {
            return;
        }
        if self.approvals_enabled && self.matcher.applies_to(ApprovalSurface::Relay) {
            let body = match serde_json::to_string(&member) {
                Ok(body) => body,
                Err(err) => {
                    step.events.push(RuntimeEvent::Notice(format!(
                        "{from} could not request teammate {}: {err}",
                        member.id
                    )));
                    return;
                }
            };
            match self
                .store
                .insert_approval(Some(turn), Some(from), "team_member", &body)
            {
                Ok(id) => {
                    self.held_approvals.insert(
                        id,
                        HeldApproval {
                            turn,
                            targets: Vec::new(),
                            prompt: String::new(),
                            mode_run: None,
                            member_request: Some((from.clone(), member)),
                        },
                    );
                    step.events.push(RuntimeEvent::ApprovalRequested {
                        id,
                        member: Some(from.clone()),
                        action: "team_member".to_string(),
                        body,
                    });
                }
                Err(err) => {
                    self.report_store_error("save a teammate approval request", err, step);
                }
            }
            return;
        }
        self.add_team_member_from_agent(from, member, step);
    }

    fn validate_agent_team_member(
        &self,
        from: &MemberId,
        member: &TeamMember,
        step: &mut RuntimeStep,
    ) -> bool {
        if self.config.find(member.id.as_str()).is_some() {
            step.events.push(RuntimeEvent::Notice(format!(
                "{from} could not add teammate {}: member already exists",
                member.id
            )));
            return false;
        }
        if self.config.find(&member.display_name).is_some() {
            step.events.push(RuntimeEvent::Notice(format!(
                "{from} could not add teammate {}: display name already exists",
                member.display_name
            )));
            return false;
        }
        true
    }

    fn add_team_member_from_agent(
        &mut self,
        from: &MemberId,
        member: TeamMember,
        step: &mut RuntimeStep,
    ) {
        if !self.validate_agent_team_member(from, &member, step) {
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
            if let Err(err) = self.store.record_route(turn, from, &to_labels, &tmsg.body) {
                self.report_store_error("save an agent route", err, step);
                return;
            }
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
            if let Err(err) = self.store.record_error(Some(turn), Some(from), &err_msg) {
                self.report_store_error("save a route error", err, step);
            }
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

        let prompt = relay_prompt(
            from,
            &self.member_display(from),
            tmsg.kind.as_deref(),
            &tmsg.body,
        );
        if self.relay_paused {
            self.pause_route(
                turn,
                from,
                resolved.members,
                to_labels,
                prompt,
                "relay paused by user",
                step,
            );
            return;
        }
        match self.relay.record_auto_relay(turn, from) {
            RelayDecision::Continue { .. } => {
                // Gate risky relay bodies the same way as user messages. A
                // user-resumed paused route (/retry) is intentionally left
                // ungated below / in resolve_next_paused_route — that path is
                // itself an explicit human decision.
                if self.approvals_enabled
                    && self.matcher.applies_to(ApprovalSurface::Relay)
                    && let Some(kind) = self.matcher.classify(&tmsg.body)
                {
                    match self
                        .store
                        .insert_approval(Some(turn), Some(from), &kind, &tmsg.body)
                    {
                        Ok(id) => {
                            self.held_approvals.insert(
                                id,
                                HeldApproval {
                                    turn,
                                    targets: resolved.members,
                                    prompt,
                                    mode_run: None,
                                    member_request: None,
                                },
                            );
                            step.events.push(RuntimeEvent::ApprovalRequested {
                                id,
                                member: Some(from.clone()),
                                action: kind,
                                body: tmsg.body,
                            });
                        }
                        Err(err) => {
                            self.report_store_error("save a relay approval request", err, step);
                            self.check_turn_complete(turn, step);
                        }
                    }
                    return;
                }
                for member in resolved.members {
                    self.enqueue_prompt(&member, turn, prompt.clone(), step);
                }
            }
            RelayDecision::Pause { count } => {
                // Ungated: resume via /retry is an explicit human decision.
                self.pause_route(
                    turn,
                    from,
                    resolved.members,
                    to_labels,
                    prompt,
                    &format!("auto-relay limit reached ({count})"),
                    step,
                );
            }
        }
    }

    fn apply_run_step_from_agent(
        &mut self,
        from: &MemberId,
        turn: TurnId,
        request: RunStepRequest,
        step: &mut RuntimeStep,
    ) {
        let Some(run_id) = self.run_turns.get(&turn).copied() else {
            step.events.push(RuntimeEvent::Notice(format!(
                "{from} ignored run step update: no active run"
            )));
            return;
        };

        let requested_owner = match &request {
            RunStepRequest::Add { owner, .. } | RunStepRequest::Assign { owner, .. } => {
                owner.as_ref()
            }
            _ => None,
        };
        if let Some(owner) = requested_owner
            && self.config.member(owner).is_none()
        {
            step.events.push(RuntimeEvent::Notice(format!(
                "{from} could not update run {run_id}: unknown step owner {owner}"
            )));
            return;
        }

        let result = match request {
            RunStepRequest::Add { owner, title } => {
                self.store.add_run_step(run_id, owner.as_ref(), &title)
            }
            RunStepRequest::Update {
                step: step_number,
                status,
                note,
            } => self
                .store
                .update_run_step(run_id, step_number, status, note.as_deref()),
            RunStepRequest::Rename {
                step: step_number,
                title,
            } => self.store.rename_run_step(run_id, step_number, &title),
            RunStepRequest::Remove { step: step_number } => {
                self.store.remove_run_step(run_id, step_number)
            }
            RunStepRequest::Assign {
                step: step_number,
                owner,
            } => self
                .store
                .assign_run_step(run_id, step_number, owner.as_ref()),
        };

        match result {
            Ok(run) => {
                let id = run.id;
                step.events.push(RuntimeEvent::RunUpdated { run });
                step.events.push(RuntimeEvent::Notice(format!(
                    "{from} updated run {id} checklist"
                )));
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                step.events.push(RuntimeEvent::Notice(format!(
                    "{from} could not update run {run_id}: step was not found"
                )));
            }
            Err(err) => step.events.push(RuntimeEvent::Notice(format!(
                "{from} could not update run {run_id}: {err}"
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
        prompt: String,
        reason: &str,
        step: &mut RuntimeStep,
    ) {
        self.paused_routes.push_back(PausedRoute {
            turn,
            from: from.clone(),
            to_members,
            to_labels: to_labels.clone(),
            prompt,
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

        let (turn, cancelled, failed) =
            match self.members.get_mut(member).and_then(|s| s.running.take()) {
                Some(running) => (
                    running.turn,
                    running.cancel.load(Ordering::Relaxed),
                    running.failed,
                ),
                None => return,
            };

        let terminal_state_saved = if cancelled {
            // A user-requested cancel kills the process (no exit code); that is
            // expected, not an error.
            self.mode_mark_turn_cancelled(turn);
            step.events
                .push(RuntimeEvent::Notice(format!("{member} cancelled")));
            true
        } else if failed {
            // A structured backend failure remains authoritative even when the
            // child process subsequently exits with status 0.
            self.mark_run_turn(turn, RunStatus::Failed, step)
        } else if !ok {
            let message = format!(
                "{} exited without success (code {})",
                self.member_backend(member),
                code.map(|c| c.to_string())
                    .unwrap_or_else(|| "unknown".to_string())
            );
            if let Err(err) = self.store.record_error(Some(turn), Some(member), &message) {
                self.report_store_error("save a member exit error", err, step);
            }
            step.events.push(RuntimeEvent::MemberError {
                member: member.clone(),
                message,
            });
            self.mark_run_turn(turn, RunStatus::Failed, step)
        } else {
            true
        };

        if let Some(state) = self.members.get_mut(member) {
            state.tools.clear();
            state.status = MemberStatus::Idle;
        }
        step.events.push(RuntimeEvent::MemberStatus {
            member: member.clone(),
            status: MemberStatus::Idle,
        });

        if !terminal_state_saved {
            return;
        }

        // Finalize the completed turn before starting unrelated queued work.
        // If the terminal transition could not be persisted, run_turns keeps
        // ownership and the queue remains stopped for an explicit recovery.
        let finishing_turn = !self.turn_active(turn);
        self.check_turn_complete(turn, step);
        if finishing_turn && self.run_turns.contains_key(&turn) {
            return;
        }

        // Start the next queued prompt for this member, if any.
        let next = self
            .members
            .get_mut(member)
            .and_then(|s| s.queue.pop_front());
        if let Some(queued) = next {
            self.start_run(member, queued.turn, queued.prompt, step);
        }
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
        if !self.members.contains_key(member) {
            let message = format!("cannot dispatch to unknown member {member}");
            if let Err(err) = self.store.record_error(Some(turn), Some(member), &message) {
                self.report_store_error("save an unknown-member dispatch error", err, step);
            }
            step.events.push(RuntimeEvent::MemberError {
                member: member.clone(),
                message: message.clone(),
            });
            if let Some(run_id) = self.run_turns.get(&turn).copied() {
                self.block_mode_run(run_id, &message, step);
            }
            self.check_turn_complete(turn, step);
            return;
        }
        // Both policies pin the first session id reported by the backend.
        // `fresh` only controls whether an older id is discarded when that
        // policy is selected; it must not create a new session every turn.
        let session = self.sessions.get(member);
        let cancel = Arc::new(AtomicBool::new(false));
        let effort = self.members.get(member).and_then(|s| s.effort);
        if let Some(state) = self.members.get_mut(member) {
            state.running = Some(RunningState {
                cancel: cancel.clone(),
                turn,
                message: None,
                text: String::new(),
                reasoning: String::new(),
                failed: false,
                raw_persistence_failed: false,
            });
            state.status = MemberStatus::Running;
            state.tools.clear();
        }
        step.events.push(RuntimeEvent::MemberStatus {
            member: member.clone(),
            status: MemberStatus::Running,
        });
        let prompt = normalize_backend_command(self.member_backend(member), prompt);
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
            "Current Asterline team roster. This lists available members only; do not message them unless collaboration is necessary or explicitly requested. If routing is needed, use member ids."
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
            let run_id = self.run_turns.get(&turn).copied();
            let completion_saved = match run_id {
                Some(run_id) if self.mode_sessions.contains_key(&run_id) => {
                    self.run_turns.remove(&turn);
                    step.events.push(RuntimeEvent::TurnFinished { turn });
                    self.mode_on_turn_complete(run_id, step);
                    return;
                }
                Some(run_id) if !self.failed_runs.contains(&run_id) => {
                    // Team runs may auto-verify; plain/team Done otherwise.
                    self.finish_plain_or_team_run(run_id, step)
                }
                _ => true,
            };
            if !completion_saved {
                return;
            }
            self.run_turns.remove(&turn);
            step.events.push(RuntimeEvent::TurnFinished { turn });
        }
    }

    fn mark_run_turn(&mut self, turn: TurnId, status: RunStatus, step: &mut RuntimeStep) -> bool {
        let Some(run_id) = self.run_turns.get(&turn).copied() else {
            return true;
        };
        match self.store.update_run_status(run_id, status) {
            Ok(run) => {
                if status == RunStatus::Failed {
                    self.failed_runs.insert(run_id);
                }
                step.events.push(RuntimeEvent::RunUpdated { run });
                true
            }
            Err(err) => {
                self.report_store_error("save a run status", err, step);
                false
            }
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
            || self.native_approvals.values().any(|h| h.turn == turn)
    }

    // === small helpers ==================================================

    fn append_text(&mut self, member: &MemberId, text: &str) -> Option<String> {
        self.members
            .get_mut(member)
            .and_then(|state| state.running.as_mut())
            .and_then(|running| {
                append_bounded_text(&mut running.text, text, MAX_MESSAGE_TEXT_BYTES)
            })
    }

    fn append_reasoning(&mut self, member: &MemberId, text: &str) -> Option<String> {
        let text = bounded_text(text, MAX_ACTIVE_REASONING_BYTES);
        if text.is_empty() {
            return None;
        }
        self.members
            .get_mut(member)
            .and_then(|state| state.running.as_mut())
            .and_then(|running| {
                if text.starts_with(running.reasoning.as_str()) {
                    if text == running.reasoning {
                        return None;
                    }
                    running.reasoning = text;
                    return Some(running.reasoning.clone());
                }
                if running.reasoning.ends_with(&text) {
                    return None;
                }
                append_bounded_text(&mut running.reasoning, &text, MAX_ACTIVE_REASONING_BYTES)
                    .map(|_| running.reasoning.clone())
            })
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
}

// Team-run and shared run handlers (handle_run_team … on_verify_output), split out for
// readability. Still inside this module so private fields are accessible.
include!("team_runtime_runs.inc.rs");

// Collaboration-mode engine (review, plan, brainstorm, and team).
include!("team_runtime_modes.inc.rs");

fn relay_prompt(from: &MemberId, from_display: &str, kind: Option<&str>, body: &str) -> String {
    let reply_instruction = if kind == Some("reply") {
        "This message is marked as a reply. Do not send another acknowledgement unless it contains a new question, request, correction, or blocker that requires an answer."
            .to_string()
    } else {
        format!(
            "Before ending your turn, you MUST answer the sender with exactly one control line:\n\
             @@team_message {{\"to\":\"{from}\",\"kind\":\"reply\",\"body\":\"your substantive response\"}}\n\
             Visible response text and run-step updates do not replace this reply."
        )
    };
    format!("[relay from {from_display} ({from})]\n{reply_instruction}\n\n{body}")
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

/// Do not rewrite a member's text based only on its first character. Native
/// controls and skills have different grammars; the TUI inserts an exact skill
/// invocation (for example `$review` for Codex) when it knows one.
fn normalize_backend_command(_backend: BackendKind, prompt: String) -> String {
    prompt
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
