//! Structured event vocabulary that flows between the TUI, the runtime, and the
//! backend adapters. The TUI's state is driven entirely by [`RuntimeEvent`];
//! nothing infers state from matching free-form strings.

use std::fmt;

use crate::domain::mode::{CollabMode, ModeStatusSummary, TerminalMode};
use crate::domain::team::{
    BackendKind, DefaultTarget, Effort, MemberId, PermissionMode, SandboxPolicy, SessionPolicy,
    TeamMember,
};

/// A turn groups everything that happens after one user submission.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TurnId(pub u64);

/// A single chat message (user or agent) in the persisted conversation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MessageId(pub u64);

/// A pending approval request awaiting a user decision.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ApprovalId(pub u64);

/// A persisted run used by team and structured collaboration modes.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RunId(pub u64);

macro_rules! impl_id_display {
    ($($ty:ty => $prefix:literal),* $(,)?) => {
        $(impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}{}", $prefix, self.0)
            }
        })*
    };
}

impl_id_display! {
    TurnId => "turn-",
    MessageId => "msg-",
    ApprovalId => "approval-",
    RunId => "run-",
}

/// A backend session/thread id used to resume a member's conversation.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct AgentSessionId(pub String);

impl AgentSessionId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AgentSessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Coarse member lifecycle status surfaced in the header and team drawer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemberStatus {
    Idle,
    Queued,
    Running,
    Waiting,
    NeedsApproval,
    Failed,
}

impl MemberStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::NeedsApproval => "needs_approval",
            Self::Failed => "failed",
        }
    }
}

impl fmt::Display for MemberStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A user's decision on a pending approval.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApprovalDecision {
    Approve,
    Reject,
}

impl ApprovalDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approved",
            Self::Reject => "rejected",
        }
    }
}

/// High-level lifecycle for a user-visible run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunStatus {
    Planned,
    Running,
    Verifying,
    Done,
    Failed,
    Blocked,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Running => "running",
            Self::Verifying => "verifying",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Blocked => "blocked",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "running" => Self::Running,
            "verifying" => Self::Verifying,
            "done" => Self::Done,
            "failed" => Self::Failed,
            "blocked" => Self::Blocked,
            _ => Self::Planned,
        }
    }
}

impl fmt::Display for RunStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Per-run checklist item state shown in `/runs`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunStepStatus {
    Todo,
    Doing,
    Done,
    Blocked,
}

impl RunStepStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::Doing => "doing",
            Self::Done => "done",
            Self::Blocked => "blocked",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "doing" => Self::Doing,
            "done" => Self::Done,
            "blocked" => Self::Blocked,
            _ => Self::Todo,
        }
    }
}

impl fmt::Display for RunStepStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A destination for an agent-to-agent message, before resolution to concrete
/// member ids. `Member` holds either an id or a display name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RouteTo {
    Member(String),
    All,
}

impl fmt::Display for RouteTo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Member(name) => f.write_str(name),
            Self::All => f.write_str("all"),
        }
    }
}

/// A structured agent-to-agent message parsed from an `@@team_message` envelope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TeamMessage {
    pub to: Vec<RouteTo>,
    pub kind: Option<String>,
    pub body: String,
}

/// Who a user submission is addressed to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MessageTarget {
    /// The team default target.
    Default,
    /// A single named member (id or display name resolved by the runtime).
    Member(MemberId),
    /// An explicit set of members.
    Members(Vec<MemberId>),
    /// Every member.
    All,
}

/// One message imported from a member's native backend session transcript
/// (e.g. the codex rollout) after attaching to it interactively.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportedMessage {
    pub from_user: bool,
    pub text: String,
}

/// Commands sent from the TUI to the runtime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UiCommand {
    /// Select the mode used by subsequent messages in this terminal session.
    SetMode { mode: TerminalMode },
    /// Submit a user message to one or more members.
    UserMessage { target: MessageTarget, body: String },
    /// Cancel a specific member's run, or all running members when `None`.
    Cancel { member: Option<MemberId> },
    /// Re-run the most recent turn.
    Retry,
    /// Resolve a pending approval.
    Approve {
        id: ApprovalId,
        decision: ApprovalDecision,
    },
    /// Pause or resume automatic agent-to-agent relay.
    SetRelayPaused(bool),
    /// Continue (`true`) or drop (`false`) the next paused relay.
    ResolvePausedRoute { resume: bool },
    /// Replace the editable team roster while the TUI is running.
    ReplaceTeam {
        members: Vec<TeamMember>,
        default_target: Option<DefaultTarget>,
    },
    /// Start a fresh chat: a new conversation and new backend sessions, with the
    /// transcript cleared. (codex's `/new`.)
    NewSession,
    /// Ask the runtime for saved chats so the TUI can open the `/resume` picker.
    RequestResume,
    /// Restore one chat selected from the `/resume` picker.
    ResumeConversation { conversation: i64 },
    /// Import messages exchanged in a member's native session (after attaching),
    /// so they appear in the Asterline transcript and persist.
    ImportTranscript {
        member: MemberId,
        items: Vec<ImportedMessage>,
    },
    /// Ask the runtime to reserve exclusive access to one member's native
    /// interactive session. This travels through the ordered work queue so it
    /// cannot overtake earlier user commands.
    RequestAttach { member: MemberId },
    /// Atomically import messages from the native interactive CLI and release
    /// its attach reservation. This is control traffic so completion can be
    /// processed while ordinary work consumption is paused.
    AttachFinished {
        member: MemberId,
        /// A session identity recovered from the attached transcript, when it
        /// can be proven without guessing among concurrent native sessions.
        session: Option<AgentSessionId>,
        items: Vec<ImportedMessage>,
    },
    /// Continue an existing run, usually after a blocker or failed
    /// verification. Without a run id, the runtime targets the latest run.
    ContinueRun {
        run_id: Option<RunId>,
        note: Option<String>,
    },
    /// Append a human checkpoint note to a run without changing its
    /// lifecycle status. Without a run id, the runtime targets the latest run.
    NoteRun { run_id: Option<RunId>, note: String },
    /// Mark a run as blocked and record the reason in its timeline.
    /// Without a run id, the runtime targets the latest run.
    BlockRun {
        run_id: Option<RunId>,
        reason: String,
    },
    /// Run verification for the latest run, or a specific run when
    /// launched from `/runs`.
    VerifyRun {
        run_id: Option<RunId>,
        command: Option<String>,
    },
    /// Add one checklist step to the latest or specified run.
    AddRunStep {
        run_id: Option<RunId>,
        owner: Option<MemberId>,
        title: String,
    },
    /// Update a checklist step by its 1-based number in `/runs`.
    UpdateRunStep {
        run_id: Option<RunId>,
        step: u32,
        status: RunStepStatus,
        note: Option<String>,
    },
    /// Rename a checklist step by its 1-based number in `/runs`.
    RenameRunStep {
        run_id: Option<RunId>,
        step: u32,
        title: String,
    },
    /// Remove a checklist step by its 1-based number in `/runs`.
    RemoveRunStep { run_id: Option<RunId>, step: u32 },
    /// Assign or clear a checklist step owner by its 1-based number in `/runs`.
    AssignRunStep {
        run_id: Option<RunId>,
        step: u32,
        owner: Option<MemberId>,
    },
    /// Start a collaboration mode run.
    RunMode { mode: CollabMode, task: String },
    /// Begin a graceful shutdown.
    Shutdown,
}

/// Unified event emitted by a backend adapter while a member runs. The
/// backend stream adapters translate their output into this
/// single vocabulary; unknown backend output becomes [`AgentEvent::Log`] or
/// [`AgentEvent::ParseWarning`] so the TUI never crashes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentEvent {
    /// A new assistant message has begun.
    MessageStarted,
    /// Incremental assistant text for the current message.
    TextDelta(String),
    /// A reasoning/thinking summary (rendered faintly, not the final answer).
    Reasoning(String),
    /// The current assistant message is complete, with its full text.
    MessageCompleted(String),
    /// A tool/command invocation has started.
    ToolStarted {
        id: String,
        name: String,
        summary: String,
    },
    /// Incremental output for a running tool.
    ToolProgress { id: String, delta: String },
    /// A tool/command invocation finished.
    ToolCompleted {
        id: String,
        ok: bool,
        summary: String,
    },
    /// The backend session/thread id was discovered or updated.
    SessionDiscovered(AgentSessionId),
    /// Codex App Server needs an explicit user decision before it can continue
    /// the active turn. `request_id` is scoped to the currently live runner.
    NativeApprovalRequested {
        request_id: u64,
        action: String,
        body: String,
    },
    /// A raw, unparsed stdout line from the backend (persisted to `stream_events`
    /// for later parser fixes; not shown in the chat).
    Raw(String),
    /// A raw line from the backend's stderr.
    Stderr(String),
    /// A diagnostic line worth keeping in the logs drawer.
    Log(String),
    /// The backend emitted something the parser did not recognize.
    ParseWarning(String),
    /// A set of file changes the agent made (apply_patch / edits).
    FileChange {
        files: Vec<(String, String)>,
        ok: bool,
    },
    /// The backend process exited.
    Exited { code: Option<i32>, ok: bool },
    /// An unrecoverable error running the backend.
    Fatal(String),
}

/// Severity for a [`LogEntry`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A diagnostic entry shown only in the logs drawer (never the main chat).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogEntry {
    pub level: LogLevel,
    pub source: String,
    pub message: String,
}

pub const MAX_LOG_SOURCE_BYTES: usize = 256;
pub const MAX_LOG_MESSAGE_BYTES: usize = 16 * 1024;

impl LogEntry {
    pub fn new(level: LogLevel, source: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            level,
            source: source.into(),
            message: message.into(),
        }
    }

    pub fn info(source: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(LogLevel::Info, source, message)
    }

    pub fn warn(source: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(LogLevel::Warn, source, message)
    }

    pub fn error(source: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(LogLevel::Error, source, message)
    }

    /// Bound untrusted backend diagnostics before they are persisted or
    /// rendered. A single stderr line must not consume the entire log drawer.
    pub fn bounded(mut self) -> Self {
        truncate_log_text(&mut self.source, MAX_LOG_SOURCE_BYTES);
        truncate_log_text(&mut self.message, MAX_LOG_MESSAGE_BYTES);
        self
    }

    pub fn byte_len(&self) -> usize {
        self.source.len().saturating_add(self.message.len())
    }
}

fn truncate_log_text(text: &mut String, limit: usize) {
    const TRUNCATION: &str = "… [truncated]";
    if text.len() <= limit {
        return;
    }
    let mut end = limit.saturating_sub(TRUNCATION.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    if limit >= TRUNCATION.len() {
        text.push_str(TRUNCATION);
    }
}

/// A short summary of one member for the header and team drawer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemberSummary {
    pub id: MemberId,
    pub display_name: String,
    pub backend: BackendKind,
    pub role: String,
    pub status: MemberStatus,
    pub session: Option<String>,
    pub cwd: String,
    pub model: Option<String>,
    pub effort: Option<Effort>,
    pub sandbox: SandboxPolicy,
    pub permission_mode: Option<PermissionMode>,
    pub session_policy: SessionPolicy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunVerification {
    pub command: String,
    pub ok: bool,
    pub summary: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunEventSummary {
    pub kind: String,
    pub title: String,
    pub detail: Option<String>,
    pub created_at: String,
    pub attempt: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunStepSummary {
    pub number: u32,
    pub status: RunStepStatus,
    pub owner: Option<MemberId>,
    pub title: String,
    pub note: Option<String>,
    pub updated_at: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RunStepRequest {
    Add {
        owner: Option<MemberId>,
        title: String,
    },
    Update {
        step: u32,
        status: RunStepStatus,
        note: Option<String>,
    },
    Rename {
        step: u32,
        title: String,
    },
    Remove {
        step: u32,
    },
    Assign {
        step: u32,
        owner: Option<MemberId>,
    },
}

/// Collaboration-mode status attached to a run when `mode` is set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModeRunStatus {
    pub mode: CollabMode,
    pub state: ModeStatusSummary,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunSummary {
    pub id: RunId,
    pub goal: String,
    pub status: RunStatus,
    pub coordinator: Option<MemberId>,
    pub verification: Option<RunVerification>,
    pub created_at: String,
    pub updated_at: String,
    pub attempt: u32,
    pub events: Vec<RunEventSummary>,
    pub steps: Vec<RunStepSummary>,
    /// Present when this run was started as a collaboration mode (`/mode review`, …).
    pub mode: Option<ModeRunStatus>,
    /// Raw `runs.mode` string when it is non-null but not a known [`CollabMode`].
    ///
    /// Used so legacy rows (e.g. `"roundtable"`) still appear in `/runs` and
    /// `/continue` can refuse them with a clear notice instead of treating them
    /// as ordinary TEAM runs.
    pub legacy_mode: Option<String>,
}

/// Events sent from the runtime to the TUI. This is the single source of truth
/// for TUI state — the TUI never parses free-form strings to infer state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeEvent {
    /// Initial snapshot emitted once the runtime is ready.
    Ready {
        team: String,
        workspace: String,
        default_target: Option<DefaultTarget>,
        members: Vec<MemberSummary>,
        runs: Vec<RunSummary>,
    },
    /// The terminal-scoped message dispatch mode changed.
    ModeChanged {
        mode: TerminalMode,
    },
    TurnStarted {
        turn: TurnId,
    },
    TurnFinished {
        turn: TurnId,
    },
    /// A user message was accepted and routed to `targets`.
    UserMessage {
        turn: TurnId,
        targets: Vec<MemberId>,
        body: String,
    },
    MemberStatus {
        member: MemberId,
        status: MemberStatus,
    },
    /// A new agent message cell begins.
    MessageStarted {
        msg: MessageId,
        turn: TurnId,
        member: MemberId,
    },
    /// Streaming text appended to the current agent message cell.
    MessageDelta {
        msg: MessageId,
        text: String,
    },
    /// The agent message cell is finalized with its canonical text.
    MessageCompleted {
        msg: MessageId,
        text: String,
    },
    Reasoning {
        member: MemberId,
        text: String,
    },
    ToolStarted {
        member: MemberId,
        tool_id: String,
        name: String,
        summary: String,
    },
    ToolProgress {
        member: MemberId,
        tool_id: String,
        delta: String,
    },
    ToolCompleted {
        member: MemberId,
        tool_id: String,
        ok: bool,
        output: String,
    },
    /// A set of file changes the agent made (rendered as a diff card).
    FileChange {
        member: MemberId,
        files: Vec<(String, String)>,
        ok: bool,
    },
    /// An agent-to-agent message was routed (shown inline in the chat).
    Route {
        turn: TurnId,
        from: MemberId,
        to: Vec<String>,
        body: String,
    },
    /// A route referenced an unknown target.
    RouteError {
        turn: TurnId,
        from: MemberId,
        target: String,
        reason: String,
        body: String,
    },
    /// Automatic relay was paused (limit hit or user paused); the route is queued.
    RoutePaused {
        turn: TurnId,
        from: MemberId,
        to: Vec<String>,
        reason: String,
        queued: usize,
    },
    /// Authoritative count after paused routes are resolved or cancelled.
    RouteQueueUpdated {
        queued: usize,
    },
    SessionUpdated {
        member: MemberId,
        session: AgentSessionId,
    },
    /// The runtime is globally quiescent and has reserved exclusive native
    /// session access for `member` until [`UiCommand::AttachFinished`].
    AttachGranted {
        member: MemberId,
    },
    /// An attach reservation was not created. `reason` is display-ready but
    /// the event remains structured so the TUI can clear pending handshake
    /// state without parsing a notice.
    AttachDenied {
        member: MemberId,
        reason: String,
    },
    ApprovalRequested {
        id: ApprovalId,
        member: Option<MemberId>,
        action: String,
        body: String,
    },
    ApprovalResolved {
        id: ApprovalId,
        decision: ApprovalDecision,
    },
    MemberError {
        member: MemberId,
        message: String,
    },
    RunUpdated {
        run: RunSummary,
    },
    /// A reviewer verdict was recorded for a mode run.
    Verdict {
        run: RunId,
        member: MemberId,
        approve: bool,
        summary: String,
    },
    /// A diagnostic for the logs drawer only.
    Log(LogEntry),
    /// A human-readable system notice shown inline in the chat.
    Notice(String),
    /// Clear the transcript to begin a fresh chat (from `/new`). Members, logs,
    /// and prompt history are kept.
    SessionReset,
    /// Saved chats available to the `/resume` picker.
    ResumeChoices {
        conversations: Vec<ConversationSummary>,
    },
    /// Replace the visible transcript after restoring a saved chat.
    ConversationResumed {
        conversation: i64,
        chat: Vec<ChatItem>,
    },
}

/// One row in the `/resume` saved-chat picker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConversationSummary {
    pub id: i64,
    pub created_at: String,
    pub preview: String,
    pub message_count: usize,
    pub member_count: usize,
}

/// A rendered conversation block in the single-column chat. The TUI builds these
/// from [`RuntimeEvent`]s, and the store replays them from persisted rows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChatItem {
    User {
        body: String,
    },
    Agent {
        member: MemberId,
        display_name: String,
        backend: BackendKind,
        text: String,
    },
    Tool {
        member: MemberId,
        name: String,
        summary: String,
        detail: String,
        ok: Option<bool>,
    },
    Diff {
        member: MemberId,
        files: Vec<(String, String)>,
        ok: bool,
    },
    Route {
        from: MemberId,
        to: Vec<String>,
        body: String,
    },
    Notice {
        text: String,
    },
    Error {
        member: Option<MemberId>,
        message: String,
    },
    Verdict {
        member: MemberId,
        approve: bool,
        summary: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_display_with_prefix() {
        assert_eq!(TurnId(3).to_string(), "turn-3");
        assert_eq!(MessageId(7).to_string(), "msg-7");
        assert_eq!(ApprovalId(1).to_string(), "approval-1");
        assert_eq!(RunId(2).to_string(), "run-2");
    }

    #[test]
    fn member_status_strings_are_stable() {
        assert_eq!(MemberStatus::NeedsApproval.as_str(), "needs_approval");
        assert_eq!(MemberStatus::Running.to_string(), "running");
    }

    #[test]
    fn log_entry_helpers_set_level() {
        assert_eq!(LogEntry::warn("runtime", "x").level, LogLevel::Warn);
        assert_eq!(LogEntry::error("builder", "y").level, LogLevel::Error);
    }

    #[test]
    fn route_to_display() {
        assert_eq!(
            RouteTo::Member("reviewer".to_string()).to_string(),
            "reviewer"
        );
        assert_eq!(RouteTo::All.to_string(), "all");
    }
}
