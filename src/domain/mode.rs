//! Collaboration mode domain types.
//!
//! Modes (`review`, `lead`, `roundtable`) are first-class run kinds resolved
//! from role heuristics, optional `team.json` bindings, and inline overrides.
//! This module stays dependency-free (no I/O) so the pure engine and tests can
//! share the same resolution rules.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::domain::team::{DefaultTarget, MemberId, TeamConfig};

/// Mode selected for the lifetime of the current terminal session.
///
/// Unlike [`CollabMode`], this includes ordinary chat and workflow dispatch.
/// A selection remains active until another `SetMode` command replaces it.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalMode {
    #[default]
    Normal,
    Review,
    Plan,
    Roundtable,
    Workflow,
}

impl TerminalMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Review => "review",
            Self::Plan => "plan",
            Self::Roundtable => "roundtable",
            Self::Workflow => "workflow",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "normal" | "chat" => Some(Self::Normal),
            "review" => Some(Self::Review),
            "plan" | "lead" => Some(Self::Plan),
            "roundtable" | "rt" => Some(Self::Roundtable),
            "workflow" => Some(Self::Workflow),
            _ => None,
        }
    }

    pub fn collab_mode(self) -> Option<CollabMode> {
        match self {
            Self::Review => Some(CollabMode::Review),
            Self::Plan => Some(CollabMode::Lead),
            Self::Roundtable => Some(CollabMode::Roundtable),
            Self::Normal | Self::Workflow => None,
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
    Lead,
    Roundtable,
}

impl CollabMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Review => "review",
            Self::Lead => "lead",
            Self::Roundtable => "roundtable",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "review" => Some(Self::Review),
            "lead" => Some(Self::Lead),
            "roundtable" => Some(Self::Roundtable),
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
pub struct ModesConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<ModeBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lead: Option<ModeBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roundtable: Option<ModeBinding>,
}

impl ModesConfig {
    pub fn is_default(&self) -> bool {
        self.review.is_none() && self.lead.is_none() && self.roundtable.is_none()
    }

    fn binding_for(&self, mode: CollabMode) -> Option<&ModeBinding> {
        match mode {
            CollabMode::Review => self.review.as_ref(),
            CollabMode::Lead => self.lead.as_ref(),
            CollabMode::Roundtable => self.roundtable.as_ref(),
        }
    }
}

/// Optional per-mode role and budget overrides stored in `team.json`.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModeBinding {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builder: Option<MemberId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<MemberId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leader: Option<MemberId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub participants: Option<Vec<MemberId>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moderator: Option<MemberId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_iterations: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rounds: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_verify: Option<bool>,
}

/// Fully resolved role bindings (every id verified against the roster).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedModeRoles {
    pub builder: MemberId,
    pub reviewer: MemberId,
    pub leader: MemberId,
    pub participants: Vec<MemberId>,
    pub moderator: Option<MemberId>,
}

/// Budget knobs for a mode run after override/config/default merge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModeLimits {
    pub max_iterations: u32,
    pub rounds: u32,
    pub auto_verify: bool,
}

impl Default for ModeLimits {
    fn default() -> Self {
        Self {
            max_iterations: 3,
            rounds: 2,
            auto_verify: true,
        }
    }
}

/// Structured review outcome from a `@@review` envelope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewVerdictKind {
    Approve,
    RequestChanges,
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
}

/// Resolve role bindings and limits for `mode`.
///
/// Precedence per field: **inline override > `config.modes` binding > role derivation**.
/// Override keys: `builder`, `reviewer`, `leader`, `moderator`, `participants`,
/// `max_iterations`, `rounds`, `auto_verify`. Member values may carry a leading `@`.
pub fn resolve_mode_roles(
    config: &TeamConfig,
    mode: CollabMode,
    overrides: &[(String, String)],
) -> Result<(ResolvedModeRoles, ModeLimits), String> {
    if config.members.is_empty() {
        return Err("team has no members".to_string());
    }

    let binding = config.modes.binding_for(mode);
    let override_map = collect_overrides(overrides)?;

    let derived = derive_roles(config);

    let builder = resolve_member_field(
        override_map.get("builder").map(String::as_str),
        binding.and_then(|b| b.builder.as_ref()),
        &derived.builder,
        config,
    )?;
    let reviewer = resolve_member_field(
        override_map.get("reviewer").map(String::as_str),
        binding.and_then(|b| b.reviewer.as_ref()),
        &derived.reviewer,
        config,
    )?;
    let leader = resolve_member_field(
        override_map.get("leader").map(String::as_str),
        binding.and_then(|b| b.leader.as_ref()),
        &derived.leader,
        config,
    )?;
    let moderator = resolve_optional_member_field(
        override_map.get("moderator").map(String::as_str),
        binding.and_then(|b| b.moderator.as_ref()),
        derived.moderator.as_ref(),
        config,
    )?;
    let participants = resolve_participants(
        override_map.get("participants").map(String::as_str),
        binding.and_then(|b| b.participants.as_ref()),
        &derived.participants,
        config,
    )?;

    let mut limits = ModeLimits::default();
    if let Some(value) = override_map.get("max_iterations") {
        limits.max_iterations = parse_positive_u32("max_iterations", value)?;
    } else if let Some(value) = binding.and_then(|b| b.max_iterations) {
        if value == 0 {
            return Err("max_iterations must be > 0".to_string());
        }
        limits.max_iterations = value;
    }
    if let Some(value) = override_map.get("rounds") {
        limits.rounds = parse_positive_u32("rounds", value)?;
    } else if let Some(value) = binding.and_then(|b| b.rounds) {
        if value == 0 {
            return Err("rounds must be > 0".to_string());
        }
        limits.rounds = value;
    }
    if let Some(value) = override_map.get("auto_verify") {
        limits.auto_verify = parse_bool("auto_verify", value)?;
    } else if let Some(value) = binding.and_then(|b| b.auto_verify) {
        limits.auto_verify = value;
    }

    if mode == CollabMode::Review && builder == reviewer {
        return Err("review mode needs two distinct members (builder and reviewer)".to_string());
    }

    Ok((
        ResolvedModeRoles {
            builder,
            reviewer,
            leader,
            participants,
            moderator,
        },
        limits,
    ))
}

fn collect_overrides(
    overrides: &[(String, String)],
) -> Result<std::collections::HashMap<String, String>, String> {
    let mut map = std::collections::HashMap::new();
    for (key, value) in overrides {
        match key.as_str() {
            "builder" | "reviewer" | "leader" | "moderator" | "participants" | "max_iterations"
            | "rounds" | "auto_verify" => {
                map.insert(key.clone(), value.clone());
            }
            other => return Err(format!("unknown mode option: {other}")),
        }
    }
    Ok(map)
}

fn strip_at(value: &str) -> &str {
    value.trim().trim_start_matches('@')
}

fn resolve_member_name(config: &TeamConfig, raw: &str) -> Result<MemberId, String> {
    let name = strip_at(raw);
    config
        .find(name)
        .map(|member| member.id.clone())
        .ok_or_else(|| format!("unknown member: {name}"))
}

fn resolve_bound_member(config: &TeamConfig, id: &MemberId) -> Result<MemberId, String> {
    config
        .member(id)
        .or_else(|| config.find(id.as_str()))
        .map(|member| member.id.clone())
        .ok_or_else(|| format!("unknown member: {id}"))
}

fn resolve_member_field(
    override_value: Option<&str>,
    binding: Option<&MemberId>,
    derived: &MemberId,
    config: &TeamConfig,
) -> Result<MemberId, String> {
    if let Some(raw) = override_value {
        return resolve_member_name(config, raw);
    }
    if let Some(id) = binding {
        return resolve_bound_member(config, id);
    }
    Ok(derived.clone())
}

fn resolve_optional_member_field(
    override_value: Option<&str>,
    binding: Option<&MemberId>,
    derived: Option<&MemberId>,
    config: &TeamConfig,
) -> Result<Option<MemberId>, String> {
    if let Some(raw) = override_value {
        return Ok(Some(resolve_member_name(config, raw)?));
    }
    if let Some(id) = binding {
        return Ok(Some(resolve_bound_member(config, id)?));
    }
    Ok(derived.cloned())
}

fn resolve_participants(
    override_value: Option<&str>,
    binding: Option<&Vec<MemberId>>,
    derived: &[MemberId],
    config: &TeamConfig,
) -> Result<Vec<MemberId>, String> {
    if let Some(raw) = override_value {
        let raw = raw.trim();
        if raw.eq_ignore_ascii_case("all") {
            return Ok(config.all_member_ids());
        }
        let mut out = Vec::new();
        for part in raw.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            out.push(resolve_member_name(config, part)?);
        }
        if out.is_empty() {
            return Err("participants must list members or 'all'".to_string());
        }
        return Ok(out);
    }
    if let Some(ids) = binding {
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            out.push(resolve_bound_member(config, id)?);
        }
        return Ok(out);
    }
    Ok(derived.to_vec())
}

fn parse_positive_u32(field: &str, value: &str) -> Result<u32, String> {
    let parsed = value
        .trim()
        .parse::<u32>()
        .map_err(|_| format!("invalid {field}: {value}"))?;
    if parsed == 0 {
        return Err(format!("{field} must be > 0"));
    }
    Ok(parsed)
}

fn parse_bool(field: &str, value: &str) -> Result<bool, String> {
    match value.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(format!("invalid {field}: {other} (use true or false)")),
    }
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

    let moderator = config
        .members
        .iter()
        .find(|member| role_contains(&member.role, "plan") || role_contains(&member.role, "lead"))
        .or_else(|| {
            config
                .members
                .iter()
                .find(|member| role_contains(&member.role, "review"))
        })
        .map(|member| member.id.clone());

    ResolvedModeRoles {
        builder,
        reviewer,
        leader,
        participants: config.all_member_ids(),
        moderator,
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
        assert_eq!(TerminalMode::parse("lead"), Some(TerminalMode::Plan));
        assert_eq!(TerminalMode::parse("rt"), Some(TerminalMode::Roundtable));
        assert_eq!(TerminalMode::parse("unknown"), None);
        assert_eq!(TerminalMode::Plan.to_string(), "plan");
    }

    #[test]
    fn derives_roles_from_mixed_roster() {
        let config = mixed_roster();
        let (roles, limits) = resolve_mode_roles(&config, CollabMode::Review, &[]).unwrap();

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
        assert_eq!(roles.moderator, Some(MemberId::new("planner")));
        assert_eq!(limits, ModeLimits::default());
    }

    #[test]
    fn builder_default_target_selects_implementation_member() {
        let mut config = mixed_roster();
        config.default_target = Some(DefaultTarget::Member(MemberId::new("builder")));
        let (roles, _) = resolve_mode_roles(&config, CollabMode::Review, &[]).unwrap();
        assert_eq!(roles.builder, MemberId::new("builder"));
        assert_eq!(roles.reviewer, MemberId::new("reviewer"));
    }

    #[test]
    fn config_binding_wins_over_derivation() {
        let mut config = mixed_roster();
        config.modes.review = Some(ModeBinding {
            builder: Some(MemberId::new("planner")),
            reviewer: Some(MemberId::new("builder")),
            ..ModeBinding::default()
        });

        let (roles, _) = resolve_mode_roles(&config, CollabMode::Review, &[]).unwrap();
        assert_eq!(roles.builder, MemberId::new("planner"));
        assert_eq!(roles.reviewer, MemberId::new("builder"));
    }

    #[test]
    fn override_wins_over_config_and_derivation() {
        let mut config = mixed_roster();
        config.modes.review = Some(ModeBinding {
            builder: Some(MemberId::new("planner")),
            reviewer: Some(MemberId::new("builder")),
            max_iterations: Some(5),
            ..ModeBinding::default()
        });

        let overrides = vec![
            ("builder".to_string(), "builder".to_string()),
            ("reviewer".to_string(), "@reviewer".to_string()),
            ("max_iterations".to_string(), "7".to_string()),
        ];
        let (roles, limits) = resolve_mode_roles(&config, CollabMode::Review, &overrides).unwrap();
        assert_eq!(roles.builder, MemberId::new("builder"));
        assert_eq!(roles.reviewer, MemberId::new("reviewer"));
        assert_eq!(limits.max_iterations, 7);
    }

    #[test]
    fn resolves_display_name_and_at_prefix() {
        let mut named = member("b1", "implementation");
        named.display_name = "Builder Bot".to_string();
        let mut rev = member("r1", "review");
        rev.display_name = "Review Ace".to_string();
        let config = TeamConfig::new("t", "/tmp/ws")
            .with_member(named)
            .with_member(rev);

        let overrides = vec![
            ("builder".to_string(), "@Builder Bot".to_string()),
            ("reviewer".to_string(), "Review Ace".to_string()),
        ];
        let (roles, _) = resolve_mode_roles(&config, CollabMode::Review, &overrides).unwrap();
        assert_eq!(roles.builder, MemberId::new("b1"));
        assert_eq!(roles.reviewer, MemberId::new("r1"));
    }

    #[test]
    fn participants_all_and_csv() {
        let config = mixed_roster();
        let (roles_all, _) = resolve_mode_roles(
            &config,
            CollabMode::Roundtable,
            &[("participants".to_string(), "all".to_string())],
        )
        .unwrap();
        assert_eq!(roles_all.participants.len(), 3);

        let (roles_csv, _) = resolve_mode_roles(
            &config,
            CollabMode::Roundtable,
            &[("participants".to_string(), "builder,@reviewer".to_string())],
        )
        .unwrap();
        assert_eq!(
            roles_csv.participants,
            vec![MemberId::new("builder"), MemberId::new("reviewer")]
        );
    }

    #[test]
    fn unknown_key_errors() {
        let config = mixed_roster();
        let err = resolve_mode_roles(
            &config,
            CollabMode::Review,
            &[("bogus".to_string(), "1".to_string())],
        )
        .unwrap_err();
        assert_eq!(err, "unknown mode option: bogus");
    }

    #[test]
    fn unknown_member_errors() {
        let config = mixed_roster();
        let err = resolve_mode_roles(
            &config,
            CollabMode::Review,
            &[("builder".to_string(), "ghost".to_string())],
        )
        .unwrap_err();
        assert_eq!(err, "unknown member: ghost");
    }

    #[test]
    fn review_with_single_member_errors() {
        let config = TeamConfig::new("solo", "/tmp/ws").with_member(member("only", "review"));
        let err = resolve_mode_roles(&config, CollabMode::Review, &[]).unwrap_err();
        assert_eq!(
            err,
            "review mode needs two distinct members (builder and reviewer)"
        );
    }

    #[test]
    fn rounds_zero_errors() {
        let config = mixed_roster();
        let err = resolve_mode_roles(
            &config,
            CollabMode::Roundtable,
            &[("rounds".to_string(), "0".to_string())],
        )
        .unwrap_err();
        assert_eq!(err, "rounds must be > 0");
    }

    #[test]
    fn builder_prefers_default_target_when_distinct_from_reviewer() {
        let mut config = mixed_roster();
        config.default_target = Some(DefaultTarget::Member(MemberId::new("planner")));
        let (roles, _) = resolve_mode_roles(&config, CollabMode::Review, &[]).unwrap();
        assert_eq!(roles.builder, MemberId::new("planner"));
        assert_eq!(roles.reviewer, MemberId::new("reviewer"));
    }

    #[test]
    fn empty_team_errors() {
        let config = TeamConfig::new("empty", "/tmp/ws");
        let err = resolve_mode_roles(&config, CollabMode::Lead, &[]).unwrap_err();
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
        for mode in [CollabMode::Review, CollabMode::Lead, CollabMode::Roundtable] {
            assert_eq!(CollabMode::parse(mode.as_str()), Some(mode));
            assert_eq!(mode.to_string(), mode.as_str());
        }
        assert_eq!(CollabMode::parse("nope"), None);
    }
}
