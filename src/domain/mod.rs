//! Domain layer: dependency-free types shared across the runtime, adapters,
//! store, and TUI. Nothing here performs I/O beyond reading a config file.

pub mod config;
pub mod event;
pub mod team;

pub use event::{
    AgentEvent, AgentSessionId, ApprovalDecision, ApprovalId, ChatItem, LogEntry, LogLevel,
    MemberStatus, MemberSummary, MessageId, MessageTarget, RouteTo, RuntimeEvent, TeamMessage,
    TurnId, UiCommand, WorkflowRunEventSummary, WorkflowRunId, WorkflowRunStatus,
    WorkflowRunSummary, WorkflowStepRequest, WorkflowStepStatus, WorkflowStepSummary,
    WorkflowVerification,
};
pub use team::{
    BackendKind, DEFAULT_MAX_AUTO_RELAYS, DefaultTarget, MemberId, PermissionMode, SandboxPolicy,
    SessionPolicy, TeamConfig, TeamConfigError, TeamMember,
};
