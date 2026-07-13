//! Domain layer: dependency-free types shared across the runtime, adapters,
//! store, and TUI. Nothing here performs I/O beyond reading a config file.

pub mod config;
pub mod event;
pub mod mode;
pub mod team;

pub use event::{
    AgentEvent, AgentSessionId, ApprovalDecision, ApprovalId, ChatItem, LogEntry, LogLevel,
    MemberStatus, MemberSummary, MessageId, MessageTarget, ModeRunStatus, RouteTo, RuntimeEvent,
    TeamMessage, TurnId, UiCommand, WorkflowRunEventSummary, WorkflowRunId, WorkflowRunStatus,
    WorkflowRunSummary, WorkflowStepRequest, WorkflowStepStatus, WorkflowStepSummary,
    WorkflowVerification,
};
pub use mode::{
    CollabMode, ModeBinding, ModeLimits, ModeStatusSummary, ModesConfig, ResolvedModeRoles,
    ReviewVerdict, ReviewVerdictKind, resolve_mode_roles,
};
pub use team::{
    ApprovalPolicy, ApprovalSurface, BackendKind, DEFAULT_MAX_AUTO_RELAYS, DefaultTarget, MemberId,
    PermissionMode, SandboxPolicy, SessionPolicy, TeamConfig, TeamConfigError, TeamMember,
};
