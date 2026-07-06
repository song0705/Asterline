//! Team roster domain types.
//!
//! A team is a generic roster of members. Each member binds a backend
//! (`claude`, `codex`, or `agy`) to a free-form role and a stable id. Roles are not
//! tied to a backend, so all-codex, all-claude, and mixed teams are all valid.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::de;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The default ceiling on consecutive automatic agent-to-agent relays within a
/// single turn before the runtime pauses and asks the user to continue.
pub const DEFAULT_MAX_AUTO_RELAYS: u32 = 6;

/// Which CLI backend drives a member.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    Claude,
    Codex,
    Agy,
}

impl BackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Agy => "agy",
        }
    }
}

impl fmt::Display for BackendKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl TryFrom<&str> for BackendKind {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            "agy" => Ok(Self::Agy),
            other => Err(format!("unknown backend: {other}")),
        }
    }
}

/// Stable identifier for a member within a team (e.g. `builder`, `reviewer`).
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MemberId(String);

impl MemberId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MemberId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for MemberId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl From<String> for MemberId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

/// Convert a user-facing name into the stable handle used in `@member`,
/// default targets, and routing. Explicit ids in config files are left intact;
/// this is only used when Asterline derives the handle from a display name.
pub fn normalize_member_id(value: &str, fallback: &str) -> String {
    let mut out = String::new();
    for ch in value.trim().chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch.to_ascii_lowercase());
        } else if ch.is_whitespace() {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        fallback.to_string()
    } else {
        out
    }
}

pub fn derived_member_id(display_name: &str, fallback: &str) -> MemberId {
    MemberId::new(normalize_member_id(display_name, fallback))
}

/// Codex sandbox policy. Serialized values match the `codex --sandbox` argument.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxPolicy {
    #[default]
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

impl SandboxPolicy {
    /// The value to pass to `codex exec --sandbox`.
    pub fn codex_arg(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::DangerFullAccess => "danger-full-access",
        }
    }
}

/// Claude permission mode. Serialized values match the `claude --permission-mode`
/// argument.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum PermissionMode {
    #[default]
    #[serde(rename = "default")]
    Default,
    #[serde(rename = "acceptEdits")]
    AcceptEdits,
    #[serde(rename = "plan")]
    Plan,
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "dontAsk")]
    DontAsk,
    #[serde(rename = "bypassPermissions")]
    BypassPermissions,
}

impl PermissionMode {
    /// The value to pass to `claude --permission-mode`.
    pub fn claude_arg(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::AcceptEdits => "acceptEdits",
            Self::Plan => "plan",
            Self::Auto => "auto",
            Self::DontAsk => "dontAsk",
            Self::BypassPermissions => "bypassPermissions",
        }
    }
}

/// Whether a member keeps a single resumable backend session or starts fresh
/// on every turn.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPolicy {
    /// Persist the backend session and resume it on later turns (product default).
    #[default]
    Resume,
    /// Start a new backend session for every turn.
    Fresh,
}

/// Reasoning effort for a member's backend model.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    /// The value for Claude's `--effort`.
    pub fn claude_arg(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }

    /// The value for Codex's `model_reasoning_effort` (clamped to its range).
    pub fn codex_value(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High | Self::Xhigh | Self::Max => "high",
        }
    }

    pub fn as_str(self) -> &'static str {
        self.claude_arg()
    }

    /// Cycle to the next level (wraps), for the UI.
    pub fn next(self) -> Self {
        match self {
            Self::Low => Self::Medium,
            Self::Medium => Self::High,
            Self::High => Self::Xhigh,
            Self::Xhigh => Self::Max,
            Self::Max => Self::Low,
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "low" => Some(Self::Low),
            "medium" | "med" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" => Some(Self::Xhigh),
            "max" => Some(Self::Max),
            _ => None,
        }
    }
}

/// A single team member: a backend bound to a role and a stable id.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TeamMember {
    pub id: MemberId,
    pub display_name: String,
    pub backend: BackendKind,
    pub role: String,
    pub cwd: Option<PathBuf>,
    pub model: Option<String>,
    pub system_prompt: Option<String>,
    pub sandbox: SandboxPolicy,
    pub permission_mode: Option<PermissionMode>,
    pub allowed_tools: Vec<String>,
    pub session_policy: SessionPolicy,
    pub effort: Option<Effort>,
}

impl Serialize for TeamMember {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let derived_id = derived_member_id(&self.display_name, self.backend.as_str());
        let include_id = self.id != derived_id;
        let include_cwd = self.cwd.is_some();
        let include_model = self.model.is_some();
        let include_system_prompt = self.system_prompt.is_some();
        let include_sandbox = self.sandbox != SandboxPolicy::default();
        let include_permission = self.permission_mode.is_some();
        let include_allowed_tools = !self.allowed_tools.is_empty();
        let include_session = self.session_policy != SessionPolicy::default();
        let include_effort = self.effort.is_some();

        let field_count = 3
            + usize::from(include_id)
            + usize::from(include_cwd)
            + usize::from(include_model)
            + usize::from(include_system_prompt)
            + usize::from(include_sandbox)
            + usize::from(include_permission)
            + usize::from(include_allowed_tools)
            + usize::from(include_session)
            + usize::from(include_effort);
        let mut state = serializer.serialize_struct("TeamMember", field_count)?;
        if include_id {
            state.serialize_field("id", &self.id)?;
        }
        state.serialize_field("display_name", &self.display_name)?;
        state.serialize_field("backend", &self.backend)?;
        state.serialize_field("role", &self.role)?;
        if include_cwd {
            state.serialize_field("cwd", &self.cwd)?;
        }
        if include_model {
            state.serialize_field("model", &self.model)?;
        }
        if include_system_prompt {
            state.serialize_field("system_prompt", &self.system_prompt)?;
        }
        if include_sandbox {
            state.serialize_field("sandbox", &self.sandbox)?;
        }
        if include_permission {
            state.serialize_field("permission_mode", &self.permission_mode)?;
        }
        if include_allowed_tools {
            state.serialize_field("allowed_tools", &self.allowed_tools)?;
        }
        if include_session {
            state.serialize_field("session_policy", &self.session_policy)?;
        }
        if include_effort {
            state.serialize_field("effort", &self.effort)?;
        }
        state.end()
    }
}

impl<'de> Deserialize<'de> for TeamMember {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct TeamMemberInput {
            #[serde(default)]
            id: Option<MemberId>,
            #[serde(default)]
            display_name: Option<String>,
            backend: BackendKind,
            role: String,
            #[serde(default)]
            cwd: Option<PathBuf>,
            #[serde(default)]
            model: Option<String>,
            #[serde(default)]
            system_prompt: Option<String>,
            #[serde(default)]
            sandbox: SandboxPolicy,
            #[serde(default)]
            permission_mode: Option<PermissionMode>,
            #[serde(default)]
            allowed_tools: Vec<String>,
            #[serde(default)]
            session_policy: SessionPolicy,
            #[serde(default)]
            effort: Option<Effort>,
        }

        let input = TeamMemberInput::deserialize(deserializer)?;
        let display_name = input
            .display_name
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .or_else(|| input.id.as_ref().map(|id| id.as_str().to_string()))
            .ok_or_else(|| de::Error::custom("team member needs id or display_name"))?;
        let id = input
            .id
            .unwrap_or_else(|| derived_member_id(&display_name, input.backend.as_str()));

        Ok(Self {
            id,
            display_name,
            backend: input.backend,
            role: input.role,
            cwd: input.cwd,
            model: input.model,
            system_prompt: input.system_prompt,
            sandbox: input.sandbox,
            permission_mode: input.permission_mode,
            allowed_tools: input.allowed_tools,
            session_policy: input.session_policy,
            effort: input.effort,
        })
    }
}

impl TeamMember {
    /// Build a member with the common defaults for the given backend.
    pub fn new(
        id: impl Into<MemberId>,
        display_name: impl Into<String>,
        backend: BackendKind,
        role: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            display_name: display_name.into(),
            backend,
            role: role.into(),
            cwd: None,
            model: None,
            system_prompt: None,
            sandbox: SandboxPolicy::default(),
            permission_mode: None,
            allowed_tools: Vec::new(),
            session_policy: SessionPolicy::default(),
            effort: None,
        }
    }

    /// The member's working directory, defaulting to the team workspace.
    pub fn resolved_cwd(&self, workspace: &Path) -> PathBuf {
        self.cwd.clone().unwrap_or_else(|| workspace.to_path_buf())
    }
}

/// Where a user message with no explicit target is delivered.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DefaultTarget {
    Member(MemberId),
    All,
}

/// Full configuration for a team: workspace, roster, and routing defaults.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TeamConfig {
    pub name: String,
    pub workspace: PathBuf,
    pub members: Vec<TeamMember>,
    #[serde(default)]
    pub default_target: Option<DefaultTarget>,
    #[serde(default = "default_max_auto_relays")]
    pub max_auto_relays: u32,
}

fn default_max_auto_relays() -> u32 {
    DEFAULT_MAX_AUTO_RELAYS
}

/// Reasons a team config can be rejected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TeamConfigError {
    Empty,
    DuplicateMember(String),
    UnknownDefaultTarget(String),
}

impl fmt::Display for TeamConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("team has no members"),
            Self::DuplicateMember(id) => write!(f, "duplicate member id: {id}"),
            Self::UnknownDefaultTarget(id) => {
                write!(f, "default target refers to unknown member: {id}")
            }
        }
    }
}

impl std::error::Error for TeamConfigError {}

impl TeamConfig {
    pub fn new(name: impl Into<String>, workspace: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            workspace: workspace.into(),
            members: Vec::new(),
            default_target: None,
            max_auto_relays: DEFAULT_MAX_AUTO_RELAYS,
        }
    }

    pub fn with_member(mut self, member: TeamMember) -> Self {
        self.members.push(member);
        self
    }

    /// Validate roster invariants. All-codex, all-claude, and mixed rosters are
    /// all accepted; the only rules are non-empty, unique ids, and a resolvable
    /// default target.
    pub fn validate(&self) -> Result<(), TeamConfigError> {
        if self.members.is_empty() {
            return Err(TeamConfigError::Empty);
        }

        let mut seen = std::collections::HashSet::new();
        for member in &self.members {
            if !seen.insert(member.id.as_str()) {
                return Err(TeamConfigError::DuplicateMember(member.id.to_string()));
            }
        }

        if let Some(DefaultTarget::Member(id)) = &self.default_target
            && self.member(id).is_none()
        {
            return Err(TeamConfigError::UnknownDefaultTarget(id.to_string()));
        }

        Ok(())
    }

    pub fn member(&self, id: &MemberId) -> Option<&TeamMember> {
        self.members.iter().find(|member| &member.id == id)
    }

    /// Find a member by exact id or, failing that, by display name
    /// (case-insensitive). Used to resolve `/ask <name>` and `@name`.
    pub fn find(&self, id_or_name: &str) -> Option<&TeamMember> {
        self.members
            .iter()
            .find(|member| member.id.as_str() == id_or_name)
            .or_else(|| {
                self.members
                    .iter()
                    .find(|member| member.display_name.eq_ignore_ascii_case(id_or_name))
            })
    }

    /// Member ids that receive an untargeted (default) message.
    pub fn default_member_ids(&self) -> Vec<MemberId> {
        match &self.default_target {
            Some(DefaultTarget::All) => self.members.iter().map(|m| m.id.clone()).collect(),
            Some(DefaultTarget::Member(id)) => vec![id.clone()],
            None => self
                .members
                .first()
                .map(|m| vec![m.id.clone()])
                .unwrap_or_default(),
        }
    }

    pub fn all_member_ids(&self) -> Vec<MemberId> {
        self.members.iter().map(|m| m.id.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codex(id: &str, role: &str) -> TeamMember {
        TeamMember::new(id, id, BackendKind::Codex, role)
    }

    fn claude(id: &str, role: &str) -> TeamMember {
        TeamMember::new(id, id, BackendKind::Claude, role)
    }

    #[test]
    fn member_id_can_be_derived_from_display_name() {
        let member: TeamMember = serde_json::from_str(
            r#"{
                "display_name": " Lead Engineer ",
                "backend": "codex",
                "role": "implementation"
            }"#,
        )
        .unwrap();

        assert_eq!(member.id, MemberId::new("lead-engineer"));
        assert_eq!(member.display_name, "Lead Engineer");
    }

    #[test]
    fn member_serialization_omits_derived_id() {
        let derived = TeamMember::new(
            "lead-engineer",
            "Lead Engineer",
            BackendKind::Codex,
            "implementation",
        );
        let json = serde_json::to_string(&derived).unwrap();
        assert!(!json.contains("\"id\""));
        assert!(json.contains("\"display_name\""));

        let custom = TeamMember::new(
            "lead",
            "Lead Engineer",
            BackendKind::Codex,
            "implementation",
        );
        let json = serde_json::to_string(&custom).unwrap();
        assert!(json.contains("\"id\""));
    }

    #[test]
    fn all_codex_team_is_valid() {
        let config = TeamConfig::new("codex-team", "/tmp/ws")
            .with_member(codex("planner", "planning"))
            .with_member(codex("builder", "implementation"))
            .with_member(codex("reviewer", "review"));

        assert!(config.validate().is_ok());
    }

    #[test]
    fn all_claude_team_is_valid() {
        let config = TeamConfig::new("claude-team", "/tmp/ws")
            .with_member(claude("architect", "planning"))
            .with_member(claude("reviewer", "review"));

        assert!(config.validate().is_ok());
    }

    #[test]
    fn mixed_team_is_valid_and_role_is_not_tied_to_backend() {
        let config = TeamConfig::new("mixed", "/tmp/ws")
            .with_member(codex("reviewer", "review"))
            .with_member(claude("builder", "implementation"));

        assert!(config.validate().is_ok());
    }

    #[test]
    fn empty_team_is_rejected() {
        let config = TeamConfig::new("empty", "/tmp/ws");
        assert_eq!(config.validate(), Err(TeamConfigError::Empty));
    }

    #[test]
    fn duplicate_member_id_is_rejected() {
        let config = TeamConfig::new("dup", "/tmp/ws")
            .with_member(codex("builder", "implementation"))
            .with_member(claude("builder", "review"));

        assert_eq!(
            config.validate(),
            Err(TeamConfigError::DuplicateMember("builder".to_string()))
        );
    }

    #[test]
    fn unknown_default_target_is_rejected() {
        let mut config =
            TeamConfig::new("t", "/tmp/ws").with_member(codex("builder", "implementation"));
        config.default_target = Some(DefaultTarget::Member(MemberId::new("ghost")));

        assert_eq!(
            config.validate(),
            Err(TeamConfigError::UnknownDefaultTarget("ghost".to_string()))
        );
    }

    #[test]
    fn default_member_ids_falls_back_to_first_member() {
        let config = TeamConfig::new("t", "/tmp/ws")
            .with_member(codex("builder", "implementation"))
            .with_member(claude("reviewer", "review"));

        assert_eq!(config.default_member_ids(), vec![MemberId::new("builder")]);
    }

    #[test]
    fn default_target_all_returns_every_member() {
        let mut config = TeamConfig::new("t", "/tmp/ws")
            .with_member(codex("a", "r"))
            .with_member(claude("b", "r"));
        config.default_target = Some(DefaultTarget::All);

        assert_eq!(
            config.default_member_ids(),
            vec![MemberId::new("a"), MemberId::new("b")]
        );
    }

    #[test]
    fn find_matches_id_then_display_name() {
        let mut builder = codex("builder", "implementation");
        builder.display_name = "Builder Bot".to_string();
        let config = TeamConfig::new("t", "/tmp/ws").with_member(builder);

        assert_eq!(config.find("builder").unwrap().id, MemberId::new("builder"));
        assert_eq!(
            config.find("Builder Bot").unwrap().id,
            MemberId::new("builder")
        );
        assert!(config.find("missing").is_none());
    }

    #[test]
    fn config_round_trips_through_json() {
        let mut member =
            TeamMember::new("builder", "Builder", BackendKind::Codex, "implementation");
        member.sandbox = SandboxPolicy::WorkspaceWrite;
        member.allowed_tools = vec!["shell".to_string()];
        let config = TeamConfig::new("t", "/tmp/ws").with_member(member);

        let json = serde_json::to_string(&config).expect("serialize");
        let parsed: TeamConfig = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(parsed, config);
    }

    #[test]
    fn sandbox_and_permission_args_match_cli_values() {
        assert_eq!(SandboxPolicy::ReadOnly.codex_arg(), "read-only");
        assert_eq!(SandboxPolicy::WorkspaceWrite.codex_arg(), "workspace-write");
        assert_eq!(
            SandboxPolicy::DangerFullAccess.codex_arg(),
            "danger-full-access"
        );
        assert_eq!(PermissionMode::AcceptEdits.claude_arg(), "acceptEdits");
        assert_eq!(
            PermissionMode::BypassPermissions.claude_arg(),
            "bypassPermissions"
        );
    }
}
