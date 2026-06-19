//! Team roster domain types.
//!
//! A team is a generic roster of members. Each member binds a backend
//! (`claude` or `codex`) to a free-form role and a stable id. Roles are not
//! tied to a backend, so all-codex, all-claude, and mixed teams are all valid.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The default ceiling on consecutive automatic agent-to-agent relays within a
/// single turn before the runtime pauses and asks the user to continue.
pub const DEFAULT_MAX_AUTO_RELAYS: u32 = 6;

/// Which CLI backend drives a member.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    Claude,
    Codex,
}

impl BackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
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

/// A single team member: a backend bound to a role and a stable id.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TeamMember {
    pub id: MemberId,
    pub display_name: String,
    pub backend: BackendKind,
    pub role: String,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub sandbox: SandboxPolicy,
    #[serde(default)]
    pub permission_mode: Option<PermissionMode>,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub session_policy: SessionPolicy,
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
        let mut member = TeamMember::new("builder", "Builder", BackendKind::Codex, "implementation");
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
