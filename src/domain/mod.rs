//! Domain layer: dependency-free types shared across the runtime, adapters,
//! store, and TUI. Nothing here performs I/O beyond reading a config file.

pub mod config;
pub mod event;
pub mod mode;
pub mod team;

pub use event::{
    AgentEvent, AgentSessionId, ApprovalDecision, ApprovalId, ChatItem, ConversationSummary,
    LogEntry, LogLevel, MemberStatus, MemberSummary, MessageId, MessageTarget, ModeRunStatus,
    RouteTo, RunEventSummary, RunId, RunStatus, RunStepRequest, RunStepStatus, RunStepSummary,
    RunSummary, RunVerification, RuntimeEvent, TeamMessage, TurnId, UiCommand,
};
pub use mode::{
    BrainstormModeConfig, CollabMode, ModeLimits, ModeStatusSummary, ModesConfig, PlanModeConfig,
    ResolvedModeRoles, ReviewModeConfig, ReviewVerdict, ReviewVerdictKind, TeamLimits,
    TeamModeConfig, TerminalMode, resolve_mode_roles, resolve_team_coordinator,
    resolve_team_limits, resolve_verify_command,
};
pub use team::{
    ApprovalPolicy, ApprovalSurface, BackendKind, DEFAULT_MAX_AUTO_RELAYS, DefaultTarget, MemberId,
    PermissionMode, SandboxPolicy, SessionPolicy, TeamConfig, TeamConfigError, TeamMember,
};
