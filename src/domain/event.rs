//! Structured event vocabulary that flows between the TUI, the runtime, and the
//! backend adapters. The TUI's state is driven entirely by [`RuntimeEvent`];
//! nothing infers state from matching free-form strings.

use std::fmt;

use crate::domain::team::{BackendKind, Effort, MemberId};

/// A turn groups everything that happens after one user submission.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TurnId(pub u64);

/// A single chat message (user or agent) in the persisted conversation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MessageId(pub u64);

/// A pending approval request awaiting a user decision.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ApprovalId(pub u64);

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
    /// Set a member's reasoning effort.
    SetEffort { member: MemberId, effort: Effort },
    /// Start a fresh session: forget resumable session ids so the next turns
    /// begin new backend threads. `None` targets every member.
    NewSession { member: Option<MemberId> },
    /// Import messages exchanged in a member's native session (after attaching),
    /// so they appear in the Asterline transcript and persist.
    ImportTranscript {
        member: MemberId,
        items: Vec<ImportedMessage>,
    },
    /// Run a built-in coordinating workflow for a goal.
    RunWorkflow { goal: String },
    /// Begin a graceful shutdown.
    Shutdown,
}

/// Unified event emitted by a backend adapter while a member runs. Both the
/// Claude and Codex stream adapters translate their backend output into this
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
    pub effort: Option<Effort>,
}

/// Events sent from the runtime to the TUI. This is the single source of truth
/// for TUI state — the TUI never parses free-form strings to infer state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeEvent {
    /// Initial snapshot emitted once the runtime is ready.
    Ready {
        team: String,
        workspace: String,
        members: Vec<MemberSummary>,
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
    MemberEffort {
        member: MemberId,
        effort: Effort,
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
    ToolCompleted {
        member: MemberId,
        tool_id: String,
        ok: bool,
        summary: String,
    },
    /// A set of file changes the agent made (rendered as a diff card).
    FileChange {
        member: MemberId,
        files: Vec<(String, String)>,
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
    },
    /// Automatic relay was paused (limit hit or user paused); the route is queued.
    RoutePaused {
        turn: TurnId,
        from: MemberId,
        to: Vec<String>,
        reason: String,
        queued: usize,
    },
    SessionUpdated {
        member: MemberId,
        session: AgentSessionId,
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
    /// A diagnostic for the logs drawer only.
    Log(LogEntry),
    /// A human-readable system notice shown inline in the chat.
    Notice(String),
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
        ok: Option<bool>,
    },
    Diff {
        member: MemberId,
        files: Vec<(String, String)>,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_display_with_prefix() {
        assert_eq!(TurnId(3).to_string(), "turn-3");
        assert_eq!(MessageId(7).to_string(), "msg-7");
        assert_eq!(ApprovalId(1).to_string(), "approval-1");
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
