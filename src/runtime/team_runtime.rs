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
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::adapter::parser::{
    MAX_MESSAGE_TEXT_BYTES, MAX_TOOL_DETAIL_BYTES, append_bounded_text, bounded_text,
};
use crate::domain::config::{
    ASTERLINE_BRAINSTORM_SKILL_NAME, ASTERLINE_ROSTER_PATH, brainstorm_skill_text,
    inject_team_protocol, strip_team_protocol, strip_team_protocols, team_skill_hint,
};
use crate::domain::event::{
    AgentEvent, AgentSessionId, ApprovalDecision, ApprovalId, ChatItem, ImportedMessage, LogEntry,
    MemberStatus, MemberSummary, MessageId, MessageTarget, RunId, RunStatus, RunStepRequest,
    RunStepStatus, RunStepSummary, RunSummary, RuntimeEvent, TurnId, UiCommand,
};
use crate::domain::mode::{
    BrainstormCard, CollabMode, ModeStatusSummary, ModesConfig, ReviewVerdict, ReviewVerdictKind,
    TerminalMode, apply_mode_overrides, clear_mode_overrides, format_mode_binding, merge_modes,
    mode_overrides_for, prune_empty_mode_overrides, resolve_mode_roles, resolve_plan_auto_execute,
    resolve_plan_builder, resolve_plan_reviewer, resolve_team_allow_add_members,
    resolve_team_coordinator, resolve_team_limits, validate_mode_overrides, validate_terminal_mode,
};
use crate::domain::team::{
    BackendKind, DefaultTarget, Effort, MemberId, SessionPolicy, TeamConfig, TeamMember,
};
use crate::fs_safety;
use crate::router::{self, RelayDecision, RelayGuard, parse_agent_output};
use crate::runtime::mode_prompts::{
    brainstorm_build_prompt, brainstorm_propose_prompt, brainstorm_stretch_prompt,
    brainstorm_synthesis_prompt, brainstorm_vote_prompt, plan_iteration_prompt, plan_nudge_prompt,
    plan_plan_prompt, plan_progress_prompt, plan_review_prompt, plan_step_nudge_prompt,
    plan_verify_failure_prompt, review_iteration_prompt, review_prompt, review_task_prompt,
    step_dispatch_prompt, verdict_nudge_prompt, verify_failure_prompt,
};
use crate::runtime::session_registry::SessionRegistry;
use crate::store::sqlite::{SqliteStore, StoredConversationSession};

pub(super) const MAX_IMPORTED_ITEMS: usize = 1_000;
pub(super) const MAX_IMPORTED_ITEM_BYTES: usize = 1024 * 1024;
pub(super) const MAX_IMPORTED_TOTAL_BYTES: usize = 8 * 1024 * 1024;

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
    reasoning_started: Option<Instant>,
    failed: bool,
    raw_persistence_failed: bool,
}

struct QueuedPrompt {
    turn: TurnId,
    prompt: String,
}

struct PendingUserMessage {
    targets: Vec<MemberId>,
    body: String,
    persisted: bool,
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
    /// Direct user messages waiting for their first actual member dispatch.
    pending_user_messages: HashMap<TurnId, PendingUserMessage>,
    run_turns: HashMap<TurnId, RunId>,
    failed_runs: HashSet<RunId>,
    mode_sessions: HashMap<RunId, ModeSession>,
    /// Selection for subsequent messages in the current chat. `/new` resets it
    /// to normal; another `/mode` selection replaces it within the chat.
    active_mode: TerminalMode,
    /// Field-level conversation overlay on `config.modes`.
    session_mode_overrides: ModesConfig,
    last_user: Option<(MessageTarget, String)>,
    next_message_id: u64,
    /// When false (default), Codex tool asks are approved automatically.
    manual_approvals: bool,
    startup_notices: Vec<String>,
    startup_events: Vec<RuntimeEvent>,
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
        let (active_mode, session_mode_overrides) =
            match store.conversation_snapshot(store.active_conversation()) {
                Ok(Some(snapshot)) => (snapshot.mode, snapshot.mode_overrides),
                Ok(None) => (TerminalMode::Normal, ModesConfig::default()),
                Err(err) => {
                    startup_notices.push(format!("could not restore the current chat mode: {err}"));
                    (TerminalMode::Normal, ModesConfig::default())
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
        let manual_approvals = config.approvals.manual;
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
            pending_user_messages: HashMap::new(),
            run_turns: HashMap::new(),
            failed_runs: HashSet::new(),
            mode_sessions: HashMap::new(),
            active_mode,
            session_mode_overrides,
            last_user: None,
            next_message_id: 0,
            manual_approvals,
            startup_notices,
            startup_events: Vec::new(),
            startup_reconciled,
        };
        let mut runtime = runtime;
        if let Err(err) = runtime.persist_conversation_snapshot() {
            runtime
                .startup_notices
                .push(format!("could not save the initial chat snapshot: {err}"));
        }
        if let Err(err) = runtime.write_roster_snapshot() {
            runtime
                .startup_notices
                .push(format!("could not write {ASTERLINE_ROSTER_PATH}: {err}"));
        }
        runtime.sync_resume_sessions();
        runtime
    }

    /// Enable the composer approval card for Codex tool asks. Off by default.
    pub fn with_approvals(self, manual: bool) -> Self {
        self.with_manual_approvals(manual)
    }

    pub fn with_manual_approvals(mut self, enabled: bool) -> Self {
        self.manual_approvals = enabled;
        self
    }

    pub fn active_mode(&self) -> TerminalMode {
        self.active_mode
    }

    fn effective_config(&self) -> TeamConfig {
        apply_mode_overrides(&self.config, &self.session_mode_overrides)
    }

    /// Conversation-local `run-N` handle. Notices and chat must use this, not
    /// the raw SQLite id, or `/new` makes the footer and transcript disagree.
    fn run_label(&self, id: RunId) -> String {
        self.store
            .run(id)
            .map(|run| run.label())
            .unwrap_or_else(|_| format!("run-{}", id.0))
    }

    fn emit_modes_updated(&self) -> RuntimeEvent {
        RuntimeEvent::ModesUpdated {
            defaults: self.config.modes.clone(),
            overrides: self.session_mode_overrides.clone(),
        }
    }

    fn sync_resume_sessions(&mut self) {
        let existing = match self.store.replay_chat() {
            Ok(chat) => chat
                .iter()
                .filter_map(chat_item_fingerprint)
                .collect::<HashSet<_>>(),
            Err(err) => {
                self.startup_notices.push(format!(
                    "could not compare native sessions with chat: {err}"
                ));
                return;
            }
        };
        let members = self.config.members.clone();
        for member in members {
            if member.session_policy != SessionPolicy::Resume {
                continue;
            }
            let Some(session) = self.sessions.get(&member.id) else {
                continue;
            };
            let native = native_session_messages(
                member.backend,
                session.as_str(),
                &member.resolved_cwd(&self.config.workspace),
            );
            let cursor = match self.store.native_import_cursor(&member.id, &session) {
                Ok(cursor) => cursor.min(native.len()),
                Err(err) => {
                    self.startup_notices.push(format!(
                        "could not read the native import cursor for {}: {err}",
                        member.id
                    ));
                    continue;
                }
            };
            let incoming = native
                .iter()
                .skip(cursor)
                .filter(|item| !existing.contains(&import_fingerprint(&item.text)))
                .cloned()
                .collect::<Vec<_>>();
            if !incoming.is_empty() {
                let mut step = RuntimeStep::default();
                self.handle_import_transcript(member.id.clone(), incoming, &mut step);
                self.startup_events.extend(step.events);
            }
            if let Err(err) =
                self.store
                    .set_native_import_cursor(&member.id, &session, native.len())
            {
                self.startup_notices.push(format!(
                    "could not save the native import cursor for {}: {err}",
                    member.id
                ));
            }
        }
    }

    fn refresh_native_import_cursor(&self, member: &MemberId) {
        let Some(cfg) = self.config.member(member) else {
            return;
        };
        if cfg.session_policy != SessionPolicy::Resume {
            return;
        }
        let Some(session) = self.sessions.get(member) else {
            return;
        };
        let count = native_session_messages(
            cfg.backend,
            session.as_str(),
            &cfg.resolved_cwd(&self.config.workspace),
        )
        .len();
        let _ = self.store.set_native_import_cursor(member, &session, count);
    }

    pub fn take_startup_events(&mut self) -> Vec<RuntimeEvent> {
        let mut events = std::mem::take(&mut self.startup_notices)
            .into_iter()
            .map(RuntimeEvent::Notice)
            .collect::<Vec<_>>();
        events.extend(std::mem::take(&mut self.startup_events));
        events
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
                approvals_reviewer: m.approvals_reviewer,
                session_policy: m.session_policy,
            })
            .collect();
        RuntimeEvent::Ready {
            modes: self.config.modes.clone(),
            mode_overrides: self.session_mode_overrides.clone(),
            suggested_verify: None,
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
                step.events
                    .push(RuntimeEvent::Notice(self.mode_switch_notice(mode)));
            }
            UiCommand::SetModeOverrides { overrides } => {
                self.handle_set_mode_overrides(overrides, &mut step);
            }
            UiCommand::SaveModeDefaults { mode } => {
                self.handle_save_mode_defaults(mode, &mut step);
            }
            UiCommand::UserMessage { target, body } => {
                self.handle_active_user_message(target, body, &mut step);
            }
            UiCommand::Cancel { member } => self.handle_cancel(member, &mut step),
            UiCommand::EditQueuedPrompt { member } => {
                self.handle_edit_queued_prompt(member, &mut step);
            }
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
            UiCommand::ImportSession { member, session_id } => {
                self.handle_import_session(member, session_id, &mut step)
            }
            UiCommand::ExportSession { format } => self.handle_export_session(format, &mut step),
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

    fn mode_switch_notice(&self, mode: TerminalMode) -> String {
        match mode {
            TerminalMode::Normal => {
                "mode → normal — next plain text uses the last @target".to_string()
            }
            _ => {
                let binding = format_mode_binding(&self.effective_config(), mode);
                format!(
                    "mode → {mode} — next plain text starts this run ({binding}); @member still sends directly"
                )
            }
        }
    }

    fn handle_set_mode_overrides(&mut self, overrides: ModesConfig, step: &mut RuntimeStep) {
        let mut overrides = overrides;
        prune_empty_mode_overrides(&mut overrides);
        if let Err(err) = validate_mode_overrides(&self.config, &overrides) {
            step.events.push(RuntimeEvent::Notice(err));
            return;
        }
        let previous = self.session_mode_overrides.clone();
        self.session_mode_overrides = overrides;
        if let Err(err) = self.persist_conversation_snapshot() {
            self.session_mode_overrides = previous;
            step.events.push(RuntimeEvent::Notice(format!(
                "could not save mode settings: {err}"
            )));
            return;
        }
        step.events.push(self.emit_modes_updated());
    }

    fn handle_save_mode_defaults(&mut self, mode: TerminalMode, step: &mut RuntimeStep) {
        if matches!(mode, TerminalMode::Normal) {
            step.events.push(RuntimeEvent::Notice(
                "normal has no team.json defaults to save".to_string(),
            ));
            return;
        }
        let extracted = mode_overrides_for(&self.session_mode_overrides, mode);
        if extracted.is_default() {
            step.events.push(RuntimeEvent::Notice(format!(
                "no this-chat overrides to save for {mode}"
            )));
            return;
        }
        let mut next_modes = merge_modes(&self.config.modes, &extracted);
        prune_empty_mode_overrides(&mut next_modes);
        let mut check = self.config.clone();
        check.modes = next_modes.clone();
        if let Err(err) = validate_terminal_mode(&check, mode) {
            step.events.push(RuntimeEvent::Notice(err));
            return;
        }

        let previous_modes = self.config.modes.clone();
        let previous_overrides = self.session_mode_overrides.clone();
        self.config.modes = next_modes;
        clear_mode_overrides(&mut self.session_mode_overrides, mode);
        prune_empty_mode_overrides(&mut self.session_mode_overrides);
        if let Err(err) = self.persist_conversation_snapshot() {
            self.config.modes = previous_modes;
            self.session_mode_overrides = previous_overrides;
            step.events.push(RuntimeEvent::Notice(format!(
                "could not save mode defaults: {err}"
            )));
            return;
        }
        step.persist_team = Some(strip_team_protocols(self.config.clone()));
        step.events.push(self.emit_modes_updated());
        step.events.push(RuntimeEvent::Notice(format!(
            "saved {mode} defaults to team.json"
        )));
    }

    fn handle_active_user_message(
        &mut self,
        target: MessageTarget,
        body: String,
        step: &mut RuntimeStep,
    ) {
        self.last_user = Some((target.clone(), body.clone()));
        // A collaboration run owns its orchestration task, but an explicit
        // member route is still a normal, one-to-one instruction.  Do not
        // reinterpret it as a second mode task while the run is active.
        if !matches!(self.active_mode, TerminalMode::Normal)
            && matches!(target, MessageTarget::Member(_))
            && !self.mode_sessions.is_empty()
        {
            self.handle_user_message(target, body, step);
            return;
        }
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
        let starts_immediately = targets.iter().any(|member| {
            self.members
                .get(member)
                .is_some_and(|state| state.running.is_none())
        });
        let defer_persistence = !starts_immediately;
        if !defer_persistence && let Err(err) = self.store.record_user(turn, &targets, &body) {
            self.report_store_error("save the user message", err, step);
            return None;
        }
        step.events.push(RuntimeEvent::TurnStarted { turn });

        let targets_str: Vec<String> = targets.iter().map(|t| t.to_string()).collect();
        if let Some(first_target) = targets.first() {
            self.log(
                first_target,
                LogEntry::info("user", format!("→ {}: {}", targets_str.join(", "), body)),
                step,
            );
        }

        self.pending_user_messages.insert(
            turn,
            PendingUserMessage {
                targets: targets.clone(),
                body: body.clone(),
                persisted: !defer_persistence,
            },
        );
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
        let mut roster_changed = false;
        for member in targets {
            let start_queued = if let Some(state) = self.members.get_mut(&member) {
                // Keep queued prompts. Esc interrupts the live run so the
                // next queued message can start as soon as the child exits.
                if let Some(running) = &state.running {
                    running.cancel.store(true, Ordering::Relaxed);
                    let queued = state.queue.len();
                    step.events.push(RuntimeEvent::Notice(if queued == 0 {
                        format!("cancelling {member}")
                    } else {
                        format!("cancelling {member} · {queued} queued message(s) will send next")
                    }));
                    None
                } else if !state.queue.is_empty() {
                    state.queue.pop_front()
                } else if state.status != MemberStatus::Idle {
                    state.status = MemberStatus::Idle;
                    step.events.push(RuntimeEvent::MemberStatus {
                        member: member.clone(),
                        status: MemberStatus::Idle,
                    });
                    roster_changed = true;
                    None
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(queued) = start_queued {
                self.emit_queue_updated(&member, step);
                self.start_run(&member, queued.turn, queued.prompt, step);
            }
        }
        if roster_changed {
            self.note_roster_write(step);
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
            member.session_policy = SessionPolicy::Fresh;
        }
        match self.store.create_fresh_conversation(
            &snapshot_team,
            TerminalMode::Normal,
            &self.session_mode_overrides,
        ) {
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
            member.session_policy = SessionPolicy::Fresh;
        }
        for state in self.members.values_mut() {
            state.status = MemberStatus::Idle;
            state.running = None;
            state.tools.clear();
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
        step.events.push(self.emit_modes_updated());
        self.note_roster_write(step);
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
        let mode_overrides = snapshot.mode_overrides;
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
            &mode_overrides,
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
        self.relay = RelayGuard::new(self.config.max_auto_relays);
        self.paused_routes.clear();
        self.held_approvals.clear();
        self.run_turns.clear();
        self.failed_runs.clear();
        self.mode_sessions.clear();
        self.active_mode = mode;
        self.session_mode_overrides = mode_overrides;
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
        self.note_roster_write(step);
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
        self.store.save_conversation_snapshot(
            &team,
            &sessions,
            self.active_mode,
            &self.session_mode_overrides,
        )
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
        let native_to_reject: Vec<ApprovalId> = self
            .native_approvals
            .iter()
            .filter(|(_, held)| removed_set.contains(&held.member))
            .map(|(id, _)| *id)
            .collect();
        let approval_ids: Vec<ApprovalId> = approvals_to_reject
            .iter()
            .map(|(id, _, _)| *id)
            .chain(native_to_reject.iter().copied())
            .collect();
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
            &self.session_mode_overrides,
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
        for id in native_to_reject {
            let Some(held) = self.native_approvals.remove(&id) else {
                continue;
            };
            cleanup_turns.insert(held.turn);
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
        self.manual_approvals |= self.config.approvals.manual;
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
        self.note_roster_write(step);
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
        let items = coalesce_imported_assistant_messages(items);
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
            "imported {count} message(s) from {id}'s session"
        )));
        if truncated || count < original_count {
            step.events.push(RuntimeEvent::Notice(format!(
                "attached transcript from {id} was truncated to {count} message(s) and {retained_bytes} bytes; skipped {oversized_items} oversized message(s)"
            )));
        }
        step.events.push(RuntimeEvent::TurnFinished { turn });
    }

    fn handle_import_session(
        &mut self,
        target_member: Option<MemberId>,
        session_id: String,
        step: &mut RuntimeStep,
    ) {
        let member_id = match target_member {
            Some(m) => m,
            None => match &self.config.default_target {
                Some(DefaultTarget::Member(id)) => id.clone(),
                _ => match self.config.members.first() {
                    Some(m) => m.id.clone(),
                    None => {
                        step.events.push(RuntimeEvent::Notice(
                            "team has no members to import into".to_string(),
                        ));
                        return;
                    }
                },
            },
        };
        let (backend, cwd) = match self.config.member(&member_id) {
            Some(member) => (member.backend, member.resolved_cwd(&self.config.workspace)),
            None => {
                step.events
                    .push(RuntimeEvent::Notice(format!("unknown member: {member_id}")));
                return;
            }
        };
        let native = native_session_messages(backend, &session_id, &cwd);
        if native.is_empty() {
            step.events.push(RuntimeEvent::Notice(format!(
                "no messages found in {backend} session '{session_id}'"
            )));
            return;
        }
        let count = native.len();
        let agent_session = AgentSessionId(session_id.clone());
        self.sessions.set(member_id.clone(), agent_session.clone());
        let _ = self
            .store
            .upsert_session(&member_id, backend, &agent_session);
        let _ = self
            .store
            .set_native_import_cursor(&member_id, &agent_session, count);

        self.handle_import_transcript(member_id.clone(), native, step);
        step.events.push(RuntimeEvent::SessionUpdated {
            member: member_id.clone(),
            session: agent_session,
        });
        step.events.push(RuntimeEvent::Notice(format!(
            "successfully imported {count} message(s) from {backend} session '{session_id}' into @{member_id}"
        )));
    }

    fn handle_export_session(&mut self, _format: Option<String>, step: &mut RuntimeStep) {
        let session_id = self
            .config
            .members
            .iter()
            .find_map(|m| self.sessions.get(&m.id).map(|s| s.0))
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        let chat_items = match self.store.replay_chat() {
            Ok(items) => items,
            Err(err) => {
                step.events.push(RuntimeEvent::Notice(format!(
                    "failed to load conversation for export: {err}"
                )));
                return;
            }
        };

        match crate::tui::claude_export::export_chat_items_to_claude_jsonl(
            &self.config.workspace,
            &session_id,
            &chat_items,
        ) {
            Ok(path) => {
                step.events.push(RuntimeEvent::Notice(format!(
                    "successfully exported session to Claude format: {}",
                    path.display()
                )));
            }
            Err(err) => {
                step.events.push(RuntimeEvent::Notice(format!(
                    "failed to export Claude session: {err}"
                )));
            }
        }
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
                if let Some(run_id) = held.mode_run
                    && self
                        .mode_sessions
                        .get(&run_id)
                        .is_some_and(|session| session.phase == ModePhase::AwaitingExecution)
                    && !self.mode_plan_confirm_execution(run_id, step)
                {
                    self.check_turn_complete(held.turn, step);
                    return;
                }
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
                if self.message_id(member).is_some() {
                    let pending = self
                        .members
                        .get(member)
                        .and_then(|state| state.running.as_ref())
                        .map(|running| running.text.clone())
                        .unwrap_or_default();
                    self.complete_message(member, pending, &mut step);
                }
                if let Some(text) = self.append_reasoning(member, &text) {
                    step.events.push(RuntimeEvent::Reasoning {
                        member: member.clone(),
                        text,
                    });
                }
            }
            AgentEvent::ReasoningSectionBreak | AgentEvent::ReasoningCompleted => {
                self.commit_open_thinking(member, &mut step);
            }
            AgentEvent::MessageCompleted(text) => self.complete_message(member, text, &mut step),
            AgentEvent::ToolStarted { id, name, summary } => {
                self.commit_open_thinking(member, &mut step);
                self.commit_open_message_before_tools(member, &mut step);
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
                if !self.manual_approvals {
                    self.log(
                        member,
                        LogEntry::info(member.as_str(), format!("auto-approved {action}")),
                        &mut step,
                    );
                    step.runner_controls
                        .push(RunnerControl::ResolveNativeApproval {
                            member: member.clone(),
                            request_id,
                            decision: ApprovalDecision::Approve,
                        });
                    return step;
                }
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
                if message.contains("agy quota exhausted") {
                    step.events.push(RuntimeEvent::Notice(message.clone()));
                }
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
        self.commit_open_thinking(member, step);
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

    /// Clear the current thinking progress before the next tool or answer.
    /// Thinking is transient status, not transcript content.
    fn commit_open_thinking(&mut self, member: &MemberId, step: &mut RuntimeStep) {
        if self.running_turn(member).is_none() {
            return;
        }
        let had_reasoning = self
            .members
            .get(member)
            .and_then(|state| state.running.as_ref())
            .is_some_and(|running| !running.reasoning.trim().is_empty());
        if let Some(state) = self.members.get_mut(member)
            && let Some(running) = &mut state.running
        {
            running.reasoning.clear();
            running.reasoning_started = None;
        }
        if !had_reasoning {
            return;
        }
        step.events.push(RuntimeEvent::ReasoningCompleted {
            member: member.clone(),
        });
    }

    /// Close any in-progress assistant cell before tools land after it.
    /// Otherwise MessageCompleted writes the final reply into the earlier
    /// cell and the UI shows the essay above the tools that produced it.
    fn commit_open_message_before_tools(&mut self, member: &MemberId, step: &mut RuntimeStep) {
        if self.message_id(member).is_none() {
            return;
        }
        let text = self
            .members
            .get(member)
            .and_then(|state| state.running.as_ref())
            .map(|running| running.text.clone())
            .unwrap_or_default();
        self.complete_message(member, text, step);
    }

    fn complete_message(&mut self, member: &MemberId, text: String, step: &mut RuntimeStep) {
        self.ensure_message(member, step);
        let Some(msg) = self.message_id(member) else {
            return;
        };
        let Some(turn) = self.running_turn(member) else {
            return;
        };

        let supplied_text = bounded_text(&text, MAX_MESSAGE_TEXT_BYTES);
        // Deltas are the only final text supplied by some otherwise-valid
        // streaming transports. Never replace a visible streamed answer with
        // an empty terminal event just because that transport omits its
        // canonical text field.
        let text = if supplied_text.is_empty() {
            self.members
                .get(member)
                .and_then(|state| state.running.as_ref())
                .map(|running| running.text.clone())
                .unwrap_or(supplied_text)
        } else {
            supplied_text
        };
        let mut parsed = parse_agent_output(&text);
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

        if let Some(limit) = self.brainstorm_ideas_cap(member) {
            let extra = parsed.brainstorm_cards.len().saturating_sub(limit);
            if extra > 0 {
                parsed.brainstorm_cards.truncate(limit);
                step.events.push(RuntimeEvent::Notice(format!(
                    "{member} emitted {extra} extra brainstorm card(s); kept {limit}"
                )));
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
        let mut checklist_run = None;
        for request in parsed.run_steps {
            if let Some(run) = self.apply_run_step_from_agent(member, turn, request, step) {
                checklist_run = Some(run);
            }
        }
        if let Some(run) = checklist_run {
            step.events.push(RuntimeEvent::Notice(format!(
                "{member} updated {} checklist",
                run.label()
            )));
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
        _turn: TurnId,
        member: TeamMember,
        step: &mut RuntimeStep,
    ) {
        if !self.validate_agent_team_member(from, &member, step) {
            return;
        }
        if self.active_mode == TerminalMode::Team && !self.team_allows_add_members() {
            step.events.push(RuntimeEvent::Notice(format!(
                "{from} could not add teammate {}: team mode is locked to the current roster",
                member.id
            )));
            return;
        }
        self.add_team_member_from_agent(from, member, step);
    }

    fn team_allows_add_members(&self) -> bool {
        resolve_team_allow_add_members(&self.effective_config())
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
    ) -> Option<crate::domain::event::RunSummary> {
        let Some(run_id) = self.run_turns.get(&turn).copied() else {
            step.events.push(RuntimeEvent::Notice(format!(
                "{from} ignored run step update: no active run"
            )));
            return None;
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
            return None;
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
                step.events
                    .push(RuntimeEvent::RunUpdated { run: run.clone() });
                Some(run)
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                step.events.push(RuntimeEvent::Notice(format!(
                    "{from} could not update run {run_id}: step was not found"
                )));
                None
            }
            Err(err) => {
                step.events.push(RuntimeEvent::Notice(format!(
                    "{from} could not update run {run_id}: {err}"
                )));
                None
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
        self.commit_open_thinking(member, step);
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
        self.refresh_native_import_cursor(member);
        self.note_roster_write(step);

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
            self.emit_queue_updated(member, step);
            self.start_run(member, queued.turn, queued.prompt, step);
        }
    }

    fn emit_queue_updated(&self, member: &MemberId, step: &mut RuntimeStep) {
        let prompts = self
            .members
            .get(member)
            .map(|state| {
                state
                    .queue
                    .iter()
                    .map(|queued| queued.prompt.clone())
                    .collect()
            })
            .unwrap_or_default();
        step.events.push(RuntimeEvent::QueueUpdated {
            member: member.clone(),
            prompts,
        });
    }

    fn handle_edit_queued_prompt(&mut self, member: Option<MemberId>, step: &mut RuntimeStep) {
        let member = member
            .or_else(|| {
                self.last_user.as_ref().and_then(|(target, _)| {
                    self.resolve_message_target(target)
                        .0
                        .into_iter()
                        .rev()
                        .find(|id| {
                            self.members
                                .get(id)
                                .is_some_and(|state| !state.queue.is_empty())
                        })
                })
            })
            .or_else(|| {
                self.members
                    .iter()
                    .find_map(|(id, state)| (!state.queue.is_empty()).then_some(id.clone()))
            });
        let Some(member) = member else {
            step.events.push(RuntimeEvent::Notice(
                "no queued message to edit".to_string(),
            ));
            return;
        };
        let Some(queued) = self
            .members
            .get_mut(&member)
            .and_then(|state| state.queue.pop_back())
        else {
            step.events.push(RuntimeEvent::Notice(format!(
                "{member} has no queued message"
            )));
            return;
        };
        self.check_turn_complete(queued.turn, step);
        if let Some(state) = self.members.get_mut(&member)
            && state.running.is_none()
            && state.queue.is_empty()
            && state.status == MemberStatus::Queued
        {
            state.status = MemberStatus::Idle;
            step.events.push(RuntimeEvent::MemberStatus {
                member: member.clone(),
                status: MemberStatus::Idle,
            });
            self.note_roster_write(step);
        }
        self.emit_queue_updated(&member, step);
        step.events.push(RuntimeEvent::QueuedPromptReturned {
            member,
            body: queued.prompt,
        });
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
            }
            self.emit_queue_updated(member, step);
            self.note_roster_write(step);
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
        if !self.publish_pending_user_message(turn, step) {
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
                reasoning_started: None,
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
        self.note_roster_write(step);
        let prompt = normalize_backend_command(self.member_backend(member), prompt);
        step.actions.push(RunAction {
            member: member.clone(),
            prompt,
            session,
            cancel,
            effort,
        });
    }

    fn publish_pending_user_message(&mut self, turn: TurnId, step: &mut RuntimeStep) -> bool {
        let Some(pending) = self.pending_user_messages.get(&turn) else {
            return true;
        };
        if !pending.persisted
            && let Err(err) = self
                .store
                .record_user(turn, &pending.targets, &pending.body)
        {
            self.report_store_error("save the queued user message", err, step);
            return false;
        }
        let pending = self
            .pending_user_messages
            .remove(&turn)
            .expect("pending user message exists");
        step.events.push(RuntimeEvent::UserMessage {
            turn,
            targets: pending.targets,
            body: pending.body,
        });
        true
    }

    fn roster_snapshot(&self) -> String {
        let mut lines = vec![
            "# Asterline roster".to_string(),
            String::new(),
            format!("Default target: {}", self.default_target_label()),
            String::new(),
            "## Members".to_string(),
        ];
        for member in &self.config.members {
            lines.push(format!("- {}", self.team_member_card(member)));
        }
        lines.push(String::new());
        lines.join("\n")
    }

    fn write_roster_snapshot(&self) -> std::io::Result<()> {
        let directory =
            fs_safety::ensure_workspace_directory(&self.config.workspace, &[".asterline"], true)?;
        fs_safety::write_regular_file(
            &directory.join("roster.md"),
            "team roster",
            &self.roster_snapshot(),
        )
    }

    fn note_roster_write(&self, step: &mut RuntimeStep) {
        if let Err(err) = self.write_roster_snapshot() {
            step.events.push(RuntimeEvent::Notice(format!(
                "could not update {ASTERLINE_ROSTER_PATH}: {err}"
            )));
        }
    }

    fn team_member_card(&self, member: &TeamMember) -> String {
        let status = self
            .members
            .get(&member.id)
            .map(|state| state.status)
            .unwrap_or(MemberStatus::Idle);
        let model = member.model.as_deref().unwrap_or("-");
        let effort = member.effort.map(Effort::as_str).unwrap_or("-");
        let permission = member.permission_native_label();
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
            member.sandbox_native_label(),
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
            self.pending_user_messages.remove(&turn);
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
        let text = bounded_text(text, MAX_MESSAGE_TEXT_BYTES);
        if text.is_empty() {
            return None;
        }
        self.members
            .get_mut(member)
            .and_then(|state| state.running.as_mut())
            .and_then(|running| {
                if running.reasoning_started.is_none() {
                    running.reasoning_started = Some(Instant::now());
                }
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
                append_bounded_text(&mut running.reasoning, &text, MAX_MESSAGE_TEXT_BYTES)
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
    format!(
        "[relay from {from_display} ({from})]\n{}\n{reply_instruction}\n\n{body}",
        team_skill_hint()
    )
}

fn session_policy_label(policy: SessionPolicy) -> &'static str {
    match policy {
        SessionPolicy::Resume => "resume",
        SessionPolicy::Fresh => "fresh",
    }
}

fn native_session_messages(
    backend: BackendKind,
    session_id: &str,
    cwd: &std::path::Path,
) -> Vec<ImportedMessage> {
    match backend {
        BackendKind::Grok => crate::tui::grok_import::messages_for_session(session_id),
        BackendKind::Codex => crate::tui::rollout_import::messages_for_session(session_id),
        BackendKind::Claude => {
            crate::tui::claude_import::messages_for_session(session_id, &cwd.display().to_string())
        }
        BackendKind::Agy => Vec::new(),
    }
}

/// Native transcripts omit tool calls from the Asterline chat. Their assistant
/// text therefore arrives in multiple adjacent records for one user turn.
fn coalesce_imported_assistant_messages(items: Vec<ImportedMessage>) -> Vec<ImportedMessage> {
    let mut merged: Vec<ImportedMessage> = Vec::with_capacity(items.len());
    for item in items {
        let can_merge = merged.last().is_some_and(|previous| {
            !previous.from_user
                && !item.from_user
                && previous
                    .text
                    .len()
                    .saturating_add(item.text.len())
                    .saturating_add(2)
                    <= MAX_IMPORTED_ITEM_BYTES
        });
        if !can_merge {
            merged.push(item);
            continue;
        }
        let previous = merged.last_mut().expect("checked above");
        previous.text.push_str("\n\n");
        previous.text.push_str(&item.text);
    }
    merged
}

fn chat_item_fingerprint(item: &ChatItem) -> Option<String> {
    let text = match item {
        ChatItem::User { body, .. } | ChatItem::Agent { text: body, .. } => body.as_str(),
        _ => return None,
    };
    Some(import_fingerprint(text))
}

fn import_fingerprint(text: &str) -> String {
    let extracted = extract_tagged(text, "user_query").unwrap_or(text);
    let trimmed = extracted.trim();
    let body = if let Some(rest) = trimmed.strip_prefix('@') {
        rest.split_once(char::is_whitespace)
            .map(|(_, rest)| rest)
            .unwrap_or(rest)
    } else {
        trimmed
    };
    body.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(240)
        .collect()
}

fn extract_tagged<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    let inner = text[start..end].trim();
    (!inner.is_empty()).then_some(inner)
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
#[path = "team_runtime_tests/mod.rs"]
mod tests;
