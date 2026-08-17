//! Domain layer: dependency-free types shared across the runtime, adapters,
//! store, and TUI. Nothing here performs I/O beyond reading a config file.

pub mod config;
pub mod event;
pub mod mode;
pub mod team;

pub use event::{
    AgentEvent, AgentSessionId, ApprovalDecision, ApprovalId, ChatItem, ConversationSummary,
    FileChangeItem, LogEntry, LogLevel, MemberStatus, MemberSummary, MessageId, MessageTarget,
    ModeRunStatus, RouteTo, RunEventSummary, RunId, RunStatus, RunStepRequest, RunStepStatus,
    RunStepSummary, RunSummary, RunVerification, RuntimeEvent, TeamMessage, TurnId, UiCommand,
};
pub use mode::{
    BrainstormModeConfig, CollabMode, ModeLimits, ModeStatusSummary, ModeValueSource, ModesConfig,
    PlanModeConfig, ResolvedModeRoles, ReviewModeConfig, ReviewVerdict, ReviewVerdictKind,
    TeamLimits, TeamModeConfig, TerminalMode, apply_mode_overrides, clear_mode_overrides,
    format_mode_binding, merge_modes, mode_binding_is_error, mode_field_source, mode_overrides_for,
    prune_empty_mode_overrides, resolve_mode_roles, resolve_plan_auto_execute,
    resolve_plan_builder, resolve_plan_reviewer, resolve_team_coordinator, resolve_team_limits,
    resolve_verify_command, validate_mode_overrides, validate_terminal_mode,
};
pub use team::{
    ApprovalPolicy, ApprovalSurface, BackendKind, CodexApprovalsReviewer, CodexPermissionsPreset,
    DEFAULT_MAX_AUTO_RELAYS, DefaultTarget, MemberId, PermissionMode, SandboxPolicy, SessionPolicy,
    TeamConfig, TeamConfigError, TeamMember,
};
