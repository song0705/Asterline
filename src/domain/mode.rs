//! Collaboration mode domain types.
//!
//! Modes (`review`, `plan`, `brainstorm`, `team`) are first-class run kinds resolved
//! from role heuristics and mode-specific `team.json` bindings.
//! This module stays dependency-free (no I/O) so the pure engine and tests can
//! share the same resolution rules.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::domain::team::{DefaultTarget, MemberId, TeamConfig};

/// Mode selected for the lifetime of the current terminal session.
///
/// Unlike [`CollabMode`], this includes ordinary chat and team dispatch.
/// A selection remains active until another `SetMode` command replaces it.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalMode {
    #[default]
    Normal,
    Review,
    Plan,
    Brainstorm,
    Team,
}

impl TerminalMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Review => "review",
            Self::Plan => "plan",
            Self::Brainstorm => "brainstorm",
            Self::Team => "team",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "normal" => Some(Self::Normal),
            "review" => Some(Self::Review),
            "plan" => Some(Self::Plan),
            "brainstorm" => Some(Self::Brainstorm),
            "team" => Some(Self::Team),
            _ => None,
        }
    }

    pub fn collab_mode(self) -> Option<CollabMode> {
        match self {
            Self::Review => Some(CollabMode::Review),
            Self::Plan => Some(CollabMode::Plan),
            Self::Brainstorm => Some(CollabMode::Brainstorm),
            Self::Normal | Self::Team => None,
        }
    }
}

impl fmt::Display for TerminalMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which collaboration mode a run uses.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollabMode {
    Review,
    Plan,
    Brainstorm,
    Team,
}

impl CollabMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Review => "review",
            Self::Plan => "plan",
            Self::Brainstorm => "brainstorm",
            Self::Team => "team",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "review" => Some(Self::Review),
            "plan" => Some(Self::Plan),
            "brainstorm" => Some(Self::Brainstorm),
            "team" => Some(Self::Team),
            _ => None,
        }
    }
}

impl fmt::Display for CollabMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `team.json` `modes` section. All fields optional; defaults are derived from roles.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModesConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewModeConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<PlanModeConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brainstorm: Option<BrainstormModeConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<TeamModeConfig>,
}

impl ModesConfig {
    pub fn is_default(&self) -> bool {
        self.review.is_none()
            && self.plan.is_none()
            && self.brainstorm.is_none()
            && self.team.is_none()
    }
}

/// Review-only role and iteration settings stored in `team.json`.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewModeConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builder: Option<MemberId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<MemberId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_iterations: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_verify: Option<bool>,
    /// Shell command used for auto-verify after approve (e.g. `just check`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_command: Option<String>,
}

/// Plan-only leader, reviewer, and iteration settings stored in `team.json`.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanModeConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leader: Option<MemberId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<MemberId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_iterations: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_verify: Option<bool>,
    /// Shell command used for auto-verify after approve (e.g. `just check`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_command: Option<String>,
}

/// Brainstorm-only participant and phase settings stored in `team.json`.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrainstormModeConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub participants: Option<Vec<MemberId>>,
    /// Number of generation waves (default 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_rounds: Option<u32>,
    /// Requested idea cards per participant in each wave (default 4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ideas_per_round: Option<u32>,
}

/// Team-only coordinator and budget settings stored in `team.json`.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamModeConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator: Option<MemberId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_iterations: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_verify: Option<bool>,
    /// Shell command used for auto-verify after the coordinator finishes (e.g. `just check`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_command: Option<String>,
}

/// Resolved team-mode budget knobs (no `ModeSession`; stored on the run's mode_state).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TeamLimits {
    pub max_iterations: u32,
    pub auto_verify: bool,
    pub verify_command: Option<String>,
}

impl Default for TeamLimits {
    fn default() -> Self {
        Self {
            max_iterations: 3,
            auto_verify: true,
            verify_command: None,
        }
    }
}

/// Resolve team-mode limits from `modes.team` (defaults: max_iterations=3, auto_verify=true).
pub fn resolve_team_limits(config: &TeamConfig) -> Result<TeamLimits, String> {
    let mut limits = TeamLimits::default();
    if let Some(settings) = &config.modes.team {
        limits.max_iterations = positive_or_default("max_iterations", settings.max_iterations, 3)?;
        limits.auto_verify = settings.auto_verify.unwrap_or(true);
        limits.verify_command = settings
            .verify_command
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string);
    }
    Ok(limits)
}

/// Fully resolved role bindings (every id verified against the roster).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedModeRoles {
    pub builder: MemberId,
    pub reviewer: MemberId,
    pub leader: MemberId,
    pub participants: Vec<MemberId>,
}

/// Budget knobs for a mode run after config/default resolution.
///
/// Not `Copy` because `verify_command` is an owned string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModeLimits {
    pub max_iterations: u32,
    /// Brainstorm generation-wave budget (default 3).
    pub rounds: u32,
    /// Requested brainstorm idea cards per participant and wave (default 4).
    pub ideas_per_round: u32,
    pub auto_verify: bool,
    /// Optional explicit auto-verify command from mode config (review/plan).
    pub verify_command: Option<String>,
}

impl Default for ModeLimits {
    fn default() -> Self {
        Self {
            max_iterations: 3,
            rounds: 3,
            ideas_per_round: 4,
            auto_verify: true,
            verify_command: None,
        }
    }
}

/// Prefer a non-empty trimmed config command; otherwise `fallback` (e.g. workspace heuristic).
pub fn resolve_verify_command(configured: Option<&str>, fallback: Option<&str>) -> Option<String> {
    configured
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            fallback
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string)
        })
}

/// Structured review outcome from a `@@review` envelope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewVerdictKind {
    Approve,
    RequestChanges,
}

/// One structured idea emitted during a brainstorm generation wave.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BrainstormCard {
    pub title: String,
    pub proposal: String,
    pub mechanism: String,
    pub operation: String,
    pub sources: Vec<String>,
}

/// One participant's private ranked ballot for a brainstorm IdeaSet.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BrainstormVote {
    pub ranked: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// One reviewer verdict, optionally with a short summary of why.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewVerdict {
    pub verdict: ReviewVerdictKind,
    pub summary: Option<String>,
}

/// Display summary of a mode run, parsed from the persisted `mode_state` JSON.
///
/// serde must tolerate unknown fields and missing fields (all defaults) so newer
/// engines can persist richer state without breaking older readers.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModeStatusSummary {
    #[serde(default)]
    pub phase: String,
    #[serde(default)]
    pub iteration: u32,
    #[serde(default)]
    pub max_iterations: u32,
    #[serde(default)]
    pub round: u32,
    #[serde(default)]
    pub rounds: u32,
    #[serde(default)]
    pub idea_count: u32,
    #[serde(default)]
    pub vote_count: u32,
}

/// Resolve the fields relevant to `mode`; unrelated mode settings are never read.
pub fn resolve_mode_roles(
    config: &TeamConfig,
    mode: CollabMode,
) -> Result<(ResolvedModeRoles, ModeLimits), String> {
    if config.members.is_empty() {
        return Err("team has no members".to_string());
    }

    let mut roles = derive_roles(config);
    let mut limits = ModeLimits::default();
    match mode {
        CollabMode::Review => {
            if let Some(settings) = &config.modes.review {
                roles.builder =
                    resolve_or_derived(config, settings.builder.as_ref(), &roles.builder)?;
                roles.reviewer =
                    resolve_or_derived(config, settings.reviewer.as_ref(), &roles.reviewer)?;
                limits.max_iterations =
                    positive_or_default("max_iterations", settings.max_iterations, 3)?;
                limits.auto_verify = settings.auto_verify.unwrap_or(true);
                limits.verify_command = settings
                    .verify_command
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string);
            }
        }
        CollabMode::Plan => {
            if let Some(settings) = &config.modes.plan {
                roles.leader = resolve_or_derived(config, settings.leader.as_ref(), &roles.leader)?;
                roles.reviewer =
                    resolve_or_derived(config, settings.reviewer.as_ref(), &roles.reviewer)?;
                limits.max_iterations =
                    positive_or_default("max_iterations", settings.max_iterations, 3)?;
                limits.auto_verify = settings.auto_verify.unwrap_or(true);
                limits.verify_command = settings
                    .verify_command
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string);
            }
        }
        CollabMode::Brainstorm => {
            if let Some(settings) = &config.modes.brainstorm {
                roles.participants = resolve_bound_participants(
                    config,
                    settings.participants.as_ref(),
                    &roles.participants,
                )?;
                limits.rounds =
                    positive_or_default("generation_rounds", settings.generation_rounds, 3)?;
                limits.ideas_per_round =
                    positive_or_default("ideas_per_round", settings.ideas_per_round, 4)?;
                if limits.rounds < 2 {
                    return Err("generation_rounds must be >= 2".to_string());
                }
                if limits.ideas_per_round < 3 {
                    return Err("ideas_per_round must be >= 3".to_string());
                }
            }
        }
        CollabMode::Team => {}
    }

    if mode == CollabMode::Review && roles.builder == roles.reviewer {
        return Err("review mode needs two distinct members (builder and reviewer)".to_string());
    }
    Ok((roles, limits))
}

fn resolve_bound_member(config: &TeamConfig, id: &MemberId) -> Result<MemberId, String> {
    config
        .member(id)
        .or_else(|| config.find(id.as_str()))
        .map(|member| member.id.clone())
        .ok_or_else(|| format!("unknown member: {id}"))
}

fn resolve_or_derived(
    config: &TeamConfig,
    binding: Option<&MemberId>,
    derived: &MemberId,
) -> Result<MemberId, String> {
    if let Some(id) = binding {
        return resolve_bound_member(config, id);
    }
    Ok(derived.clone())
}

fn resolve_bound_participants(
    config: &TeamConfig,
    binding: Option<&Vec<MemberId>>,
    derived: &[MemberId],
) -> Result<Vec<MemberId>, String> {
    if let Some(ids) = binding {
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            out.push(resolve_bound_member(config, id)?);
        }
        return Ok(out);
    }
    Ok(derived.to_vec())
}

fn positive_or_default(field: &str, value: Option<u32>, default: u32) -> Result<u32, String> {
    let value = value.unwrap_or(default);
    if value == 0 {
        return Err(format!("{field} must be > 0"));
    }
    Ok(value)
}

pub fn resolve_team_coordinator(config: &TeamConfig) -> Result<MemberId, String> {
    if config.members.is_empty() {
        return Err("team has no members".to_string());
    }
    if let Some(id) = config
        .modes
        .team
        .as_ref()
        .and_then(|settings| settings.coordinator.as_ref())
    {
        return resolve_bound_member(config, id);
    }
    Ok(derive_roles(config).leader)
}

fn role_contains(role: &str, needle: &str) -> bool {
    role.to_ascii_lowercase().contains(needle)
}

/// Role-heuristic defaults used when neither override nor config binding sets a field.
fn derive_roles(config: &TeamConfig) -> ResolvedModeRoles {
    let first = config.members[0].id.clone();

    // Prefer a role-tagged reviewer so builder can avoid that seat; otherwise
    // pick builder first (default target / first member), then the last other member.
    let role_reviewer = config
        .members
        .iter()
        .find(|member| role_contains(&member.role, "review"))
        .map(|member| member.id.clone());

    let (builder, reviewer) = if let Some(reviewer) = role_reviewer {
        (derive_builder(config, &reviewer), reviewer)
    } else {
        let builder = provisional_builder(config);
        let reviewer = config
            .members
            .iter()
            .rev()
            .find(|member| member.id != builder)
            .map(|member| member.id.clone())
            .unwrap_or_else(|| first.clone());
        (builder, reviewer)
    };

    let leader = config
        .members
        .iter()
        .find(|member| role_contains(&member.role, "plan") || role_contains(&member.role, "lead"))
        .map(|member| member.id.clone())
        .unwrap_or_else(|| first.clone());

    ResolvedModeRoles {
        builder,
        reviewer,
        leader,
        participants: config.all_member_ids(),
    }
}

fn provisional_builder(config: &TeamConfig) -> MemberId {
    match &config.default_target {
        Some(DefaultTarget::Member(id)) if config.member(id).is_some() => id.clone(),
        _ => config.members[0].id.clone(),
    }
}

fn derive_builder(config: &TeamConfig, reviewer: &MemberId) -> MemberId {
    if let Some(DefaultTarget::Member(id)) = &config.default_target
        && id != reviewer
        && config.member(id).is_some()
    {
        return id.clone();
    }
    config
        .members
        .iter()
        .find(|member| &member.id != reviewer)
        .map(|member| member.id.clone())
        .unwrap_or_else(|| config.members[0].id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::team::{BackendKind, TeamMember};

    fn member(id: &str, role: &str) -> TeamMember {
        TeamMember::new(id, id, BackendKind::Codex, role)
    }

    fn mixed_roster() -> TeamConfig {
        TeamConfig::new("mixed", "/tmp/ws")
            .with_member(member("planner", "planning lead"))
            .with_member(member("builder", "implementation"))
            .with_member(member("reviewer", "code review"))
    }

    #[test]
    fn terminal_mode_parses_user_facing_names() {
        assert_eq!(TerminalMode::parse("normal"), Some(TerminalMode::Normal));
        assert_eq!(TerminalMode::parse("plan"), Some(TerminalMode::Plan));
        assert_eq!(
            TerminalMode::parse("brainstorm"),
            Some(TerminalMode::Brainstorm)
        );
        assert_eq!(TerminalMode::parse("roundtable"), None);
        assert_eq!(TerminalMode::parse("lead"), None);
        assert_eq!(TerminalMode::parse("rt"), None);
        assert_eq!(TerminalMode::parse("team"), Some(TerminalMode::Team));
        assert_eq!(TerminalMode::parse("unknown"), None);
        assert_eq!(TerminalMode::Plan.to_string(), "plan");
    }

    #[test]
    fn derives_roles_from_mixed_roster() {
        let config = mixed_roster();
        let (roles, limits) = resolve_mode_roles(&config, CollabMode::Review).unwrap();

        // No default_target: builder is the first member that is not the reviewer.
        assert_eq!(roles.builder, MemberId::new("planner"));
        assert_eq!(roles.reviewer, MemberId::new("reviewer"));
        assert_eq!(roles.leader, MemberId::new("planner"));
        assert_eq!(
            roles.participants,
            vec![
                MemberId::new("planner"),
                MemberId::new("builder"),
                MemberId::new("reviewer"),
            ]
        );
        assert_eq!(limits, ModeLimits::default());
    }

    #[test]
    fn builder_default_target_selects_implementation_member() {
        let mut config = mixed_roster();
        config.default_target = Some(DefaultTarget::Member(MemberId::new("builder")));
        let (roles, _) = resolve_mode_roles(&config, CollabMode::Review).unwrap();
        assert_eq!(roles.builder, MemberId::new("builder"));
        assert_eq!(roles.reviewer, MemberId::new("reviewer"));
    }

    #[test]
    fn config_binding_wins_over_derivation() {
        let mut config = mixed_roster();
        config.modes.review = Some(ReviewModeConfig {
            builder: Some(MemberId::new("planner")),
            reviewer: Some(MemberId::new("builder")),
            ..ReviewModeConfig::default()
        });

        let (roles, _) = resolve_mode_roles(&config, CollabMode::Review).unwrap();
        assert_eq!(roles.builder, MemberId::new("planner"));
        assert_eq!(roles.reviewer, MemberId::new("builder"));
    }

    #[test]
    fn review_settings_apply_only_review_fields() {
        let mut config = mixed_roster();
        config.modes.review = Some(ReviewModeConfig {
            builder: Some(MemberId::new("builder")),
            reviewer: Some(MemberId::new("reviewer")),
            max_iterations: Some(5),
            auto_verify: Some(false),
            verify_command: Some("  just check  ".to_string()),
        });
        let (roles, limits) = resolve_mode_roles(&config, CollabMode::Review).unwrap();
        assert_eq!(roles.builder, MemberId::new("builder"));
        assert_eq!(roles.reviewer, MemberId::new("reviewer"));
        assert_eq!(limits.max_iterations, 5);
        assert!(!limits.auto_verify);
        assert_eq!(limits.verify_command.as_deref(), Some("just check"));
    }

    #[test]
    fn resolve_verify_command_prefers_config_over_fallback() {
        assert_eq!(
            resolve_verify_command(Some(" just check "), Some("cargo test")).as_deref(),
            Some("just check")
        );
        assert_eq!(
            resolve_verify_command(Some("  "), Some("cargo test")).as_deref(),
            Some("cargo test")
        );
        assert_eq!(resolve_verify_command(None, None), None);
    }

    #[test]
    fn brainstorm_settings_resolve_participants_and_generation_limits() {
        let mut config = mixed_roster();
        config.modes.brainstorm = Some(BrainstormModeConfig {
            participants: Some(vec![MemberId::new("builder"), MemberId::new("reviewer")]),
            generation_rounds: Some(4),
            ideas_per_round: Some(6),
        });
        let (roles, limits) = resolve_mode_roles(&config, CollabMode::Brainstorm).unwrap();
        assert_eq!(
            roles.participants,
            vec![MemberId::new("builder"), MemberId::new("reviewer")]
        );
        assert_eq!(limits.rounds, 4);
        assert_eq!(limits.ideas_per_round, 6);
    }

    #[test]
    fn unknown_configured_member_errors() {
        let mut config = mixed_roster();
        config.modes.review = Some(ReviewModeConfig {
            builder: Some(MemberId::new("ghost")),
            ..ReviewModeConfig::default()
        });
        let err = resolve_mode_roles(&config, CollabMode::Review).unwrap_err();
        assert_eq!(err, "unknown member: ghost");
    }

    #[test]
    fn review_with_single_member_errors() {
        let config = TeamConfig::new("solo", "/tmp/ws").with_member(member("only", "review"));
        let err = resolve_mode_roles(&config, CollabMode::Review).unwrap_err();
        assert_eq!(
            err,
            "review mode needs two distinct members (builder and reviewer)"
        );
    }

    #[test]
    fn generation_rounds_zero_errors() {
        let mut config = mixed_roster();
        config.modes.brainstorm = Some(BrainstormModeConfig {
            generation_rounds: Some(0),
            ..BrainstormModeConfig::default()
        });
        let err = resolve_mode_roles(&config, CollabMode::Brainstorm).unwrap_err();
        assert_eq!(err, "generation_rounds must be > 0");
    }

    #[test]
    fn team_coordinator_is_configurable() {
        let mut config = mixed_roster();
        config.modes.team = Some(TeamModeConfig {
            coordinator: Some(MemberId::new("builder")),
            ..TeamModeConfig::default()
        });
        assert_eq!(
            resolve_team_coordinator(&config).unwrap(),
            MemberId::new("builder")
        );
    }

    #[test]
    fn resolve_team_limits_defaults_and_bindings() {
        let config = mixed_roster();
        assert_eq!(resolve_team_limits(&config).unwrap(), TeamLimits::default());

        let mut config = mixed_roster();
        config.modes.team = Some(TeamModeConfig {
            max_iterations: Some(5),
            auto_verify: Some(false),
            verify_command: Some("  just check  ".to_string()),
            ..TeamModeConfig::default()
        });
        let limits = resolve_team_limits(&config).unwrap();
        assert_eq!(limits.max_iterations, 5);
        assert!(!limits.auto_verify);
        assert_eq!(limits.verify_command.as_deref(), Some("just check"));

        let mut config = mixed_roster();
        config.modes.team = Some(TeamModeConfig {
            max_iterations: Some(0),
            ..TeamModeConfig::default()
        });
        assert_eq!(
            resolve_team_limits(&config).unwrap_err(),
            "max_iterations must be > 0"
        );
    }

    #[test]
    fn mode_configs_reject_unrelated_fields() {
        let review = serde_json::from_str::<ModesConfig>(r#"{"review":{"rounds":2}}"#);
        assert!(review.is_err());
        let brainstorm =
            serde_json::from_str::<ModesConfig>(r#"{"brainstorm":{"builder":"builder"}}"#);
        assert!(brainstorm.is_err());
    }

    #[test]
    fn modes_config_rejects_legacy_roundtable_key() {
        let err = serde_json::from_str::<ModesConfig>(r#"{"roundtable":{}}"#).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("roundtable") || msg.contains("unknown field"),
            "expected unknown-field error mentioning roundtable, got: {msg}"
        );
    }

    #[test]
    fn builder_prefers_default_target_when_distinct_from_reviewer() {
        let mut config = mixed_roster();
        config.default_target = Some(DefaultTarget::Member(MemberId::new("planner")));
        let (roles, _) = resolve_mode_roles(&config, CollabMode::Review).unwrap();
        assert_eq!(roles.builder, MemberId::new("planner"));
        assert_eq!(roles.reviewer, MemberId::new("reviewer"));
    }

    #[test]
    fn empty_team_errors() {
        let config = TeamConfig::new("empty", "/tmp/ws");
        let err = resolve_mode_roles(&config, CollabMode::Plan).unwrap_err();
        assert_eq!(err, "team has no members");
    }

    #[test]
    fn mode_status_summary_tolerates_unknown_and_missing_fields() {
        let summary: ModeStatusSummary =
            serde_json::from_str(r#"{"phase":"build","iteration":1,"extra":true}"#).unwrap();
        assert_eq!(summary.phase, "build");
        assert_eq!(summary.iteration, 1);
        assert_eq!(summary.max_iterations, 0);
        assert_eq!(summary.round, 0);

        let empty: ModeStatusSummary = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, ModeStatusSummary::default());
    }

    #[test]
    fn collab_mode_round_trips_as_str() {
        for mode in [CollabMode::Review, CollabMode::Plan, CollabMode::Brainstorm] {
            assert_eq!(CollabMode::parse(mode.as_str()), Some(mode));
            assert_eq!(mode.to_string(), mode.as_str());
        }
        assert_eq!(CollabMode::parse("roundtable"), None);
        assert_eq!(CollabMode::parse("nope"), None);
    }
}
