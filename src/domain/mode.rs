//! Collaboration mode domain types.
//!
//! Modes (`review`, `plan`, `brainstorm`, `team`) are first-class run kinds resolved
//! from role heuristics and mode-specific `team.json` bindings.
//! This module stays dependency-free (no I/O) so the pure engine and tests can
//! share the same resolution rules.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::domain::team::{DefaultTarget, MemberId, TeamConfig};

/// Mode selected for the lifetime of the current conversation.
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
    pub const ALL: [Self; 5] = [
        Self::Normal,
        Self::Review,
        Self::Plan,
        Self::Brainstorm,
        Self::Team,
    ];

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

/// Where a single mode knob currently comes from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModeValueSource {
    Default,
    TeamJson,
    Conversation,
}

impl ModeValueSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::TeamJson => "team.json",
            Self::Conversation => "this chat",
        }
    }
}

/// Field-level merge: each `Some` in `overrides` replaces that field only.
pub fn merge_modes(defaults: &ModesConfig, overrides: &ModesConfig) -> ModesConfig {
    ModesConfig {
        review: merge_review(&defaults.review, &overrides.review),
        plan: merge_plan(&defaults.plan, &overrides.plan),
        brainstorm: merge_brainstorm(&defaults.brainstorm, &overrides.brainstorm),
        team: merge_team(&defaults.team, &overrides.team),
    }
}

/// Copy `config` with `modes` replaced by the field-level merge of defaults and overrides.
pub fn apply_mode_overrides(config: &TeamConfig, overrides: &ModesConfig) -> TeamConfig {
    let mut merged = config.clone();
    merged.modes = merge_modes(&config.modes, overrides);
    merged
}

pub fn mode_field_source(overridden: bool, in_team_json: bool) -> ModeValueSource {
    if overridden {
        ModeValueSource::Conversation
    } else if in_team_json {
        ModeValueSource::TeamJson
    } else {
        ModeValueSource::Default
    }
}

/// Keep only the selected mode's override block.
pub fn mode_overrides_for(overrides: &ModesConfig, mode: TerminalMode) -> ModesConfig {
    match mode {
        TerminalMode::Normal => ModesConfig::default(),
        TerminalMode::Review => ModesConfig {
            review: overrides.review.clone(),
            ..ModesConfig::default()
        },
        TerminalMode::Plan => ModesConfig {
            plan: overrides.plan.clone(),
            ..ModesConfig::default()
        },
        TerminalMode::Brainstorm => ModesConfig {
            brainstorm: overrides.brainstorm.clone(),
            ..ModesConfig::default()
        },
        TerminalMode::Team => ModesConfig {
            team: overrides.team.clone(),
            ..ModesConfig::default()
        },
    }
}

pub fn clear_mode_overrides(overrides: &mut ModesConfig, mode: TerminalMode) {
    match mode {
        TerminalMode::Normal => {}
        TerminalMode::Review => overrides.review = None,
        TerminalMode::Plan => overrides.plan = None,
        TerminalMode::Brainstorm => overrides.brainstorm = None,
        TerminalMode::Team => overrides.team = None,
    }
}

pub fn prune_empty_mode_overrides(overrides: &mut ModesConfig) {
    if overrides
        .review
        .as_ref()
        .is_some_and(review_config_is_empty)
    {
        overrides.review = None;
    }
    if overrides.plan.as_ref().is_some_and(plan_config_is_empty) {
        overrides.plan = None;
    }
    if overrides
        .brainstorm
        .as_ref()
        .is_some_and(brainstorm_config_is_empty)
    {
        overrides.brainstorm = None;
    }
    if overrides.team.as_ref().is_some_and(team_config_is_empty) {
        overrides.team = None;
    }
}

fn review_config_is_empty(config: &ReviewModeConfig) -> bool {
    config.builder.is_none()
        && config.reviewer.is_none()
        && config.max_iterations.is_none()
        && config.reviewer_hint.is_none()
}

fn plan_config_is_empty(config: &PlanModeConfig) -> bool {
    config.leader.is_none()
        && config.builder.is_none()
        && config.reviewer.is_none()
        && config.max_iterations.is_none()
        && config.auto_execute.is_none()
}

fn brainstorm_config_is_empty(config: &BrainstormModeConfig) -> bool {
    config.participants.is_none()
        && config.generation_rounds.is_none()
        && config.ideas_per_round.is_none()
}

fn team_config_is_empty(config: &TeamModeConfig) -> bool {
    config.coordinator.is_none()
        && config.max_iterations.is_none()
        && config.allow_add_members.is_none()
}

/// Validate merged knobs for modes that have any override fields set.
pub fn validate_mode_overrides(config: &TeamConfig, overrides: &ModesConfig) -> Result<(), String> {
    let merged = apply_mode_overrides(config, overrides);
    if overrides.review.is_some() {
        resolve_mode_roles(&merged, CollabMode::Review)?;
    }
    if overrides.plan.is_some() {
        resolve_mode_roles(&merged, CollabMode::Plan)?;
        resolve_plan_builder(&merged)?;
        resolve_plan_reviewer(&merged)?;
    }
    if overrides.brainstorm.is_some() {
        resolve_mode_roles(&merged, CollabMode::Brainstorm)?;
    }
    if overrides.team.is_some() {
        resolve_team_coordinator(&merged)?;
        resolve_team_limits(&merged)?;
    }
    Ok(())
}

pub fn validate_terminal_mode(config: &TeamConfig, mode: TerminalMode) -> Result<(), String> {
    match mode {
        TerminalMode::Normal => Ok(()),
        TerminalMode::Review => resolve_mode_roles(config, CollabMode::Review).map(|_| ()),
        TerminalMode::Plan => {
            resolve_mode_roles(config, CollabMode::Plan)?;
            resolve_plan_builder(config)?;
            resolve_plan_reviewer(config)?;
            Ok(())
        }
        TerminalMode::Brainstorm => resolve_mode_roles(config, CollabMode::Brainstorm).map(|_| ()),
        TerminalMode::Team => {
            resolve_team_coordinator(config)?;
            resolve_team_limits(config)?;
            Ok(())
        }
    }
}

fn merge_review(
    base: &Option<ReviewModeConfig>,
    over: &Option<ReviewModeConfig>,
) -> Option<ReviewModeConfig> {
    match (base, over) {
        (None, None) => None,
        (Some(base), None) => Some(base.clone()),
        (None, Some(over)) => Some(over.clone()),
        (Some(base), Some(over)) => Some(ReviewModeConfig {
            builder: over.builder.clone().or_else(|| base.builder.clone()),
            reviewer: over.reviewer.clone().or_else(|| base.reviewer.clone()),
            max_iterations: over.max_iterations.or(base.max_iterations),
            reviewer_hint: over
                .reviewer_hint
                .clone()
                .or_else(|| base.reviewer_hint.clone()),
            auto_verify: None,
        }),
    }
}

fn merge_plan(
    base: &Option<PlanModeConfig>,
    over: &Option<PlanModeConfig>,
) -> Option<PlanModeConfig> {
    match (base, over) {
        (None, None) => None,
        (Some(base), None) => Some(base.clone()),
        (None, Some(over)) => Some(over.clone()),
        (Some(base), Some(over)) => Some(PlanModeConfig {
            leader: over.leader.clone().or_else(|| base.leader.clone()),
            builder: over.builder.clone().or_else(|| base.builder.clone()),
            reviewer: over.reviewer.clone().or_else(|| base.reviewer.clone()),
            max_iterations: over.max_iterations.or(base.max_iterations),
            auto_execute: over.auto_execute.or(base.auto_execute),
            auto_verify: None,
            verify_command: None,
        }),
    }
}

fn merge_brainstorm(
    base: &Option<BrainstormModeConfig>,
    over: &Option<BrainstormModeConfig>,
) -> Option<BrainstormModeConfig> {
    match (base, over) {
        (None, None) => None,
        (Some(base), None) => Some(base.clone()),
        (None, Some(over)) => Some(over.clone()),
        (Some(base), Some(over)) => Some(BrainstormModeConfig {
            participants: over
                .participants
                .clone()
                .or_else(|| base.participants.clone()),
            generation_rounds: over.generation_rounds.or(base.generation_rounds),
            ideas_per_round: over.ideas_per_round.or(base.ideas_per_round),
        }),
    }
}

fn merge_team(
    base: &Option<TeamModeConfig>,
    over: &Option<TeamModeConfig>,
) -> Option<TeamModeConfig> {
    match (base, over) {
        (None, None) => None,
        (Some(base), None) => Some(base.clone()),
        (None, Some(over)) => Some(over.clone()),
        (Some(base), Some(over)) => Some(TeamModeConfig {
            coordinator: over
                .coordinator
                .clone()
                .or_else(|| base.coordinator.clone()),
            max_iterations: over.max_iterations.or(base.max_iterations),
            allow_add_members: over.allow_add_members.or(base.allow_add_members),
            auto_verify: None,
            verify_command: None,
        }),
    }
}

/// Human binding line for a mode using merged config. Errors stay as the line
/// text so the panel can paint them in yellow.
pub fn format_mode_binding(config: &TeamConfig, mode: TerminalMode) -> String {
    match mode {
        TerminalMode::Normal => "plain text goes to the last @target".to_string(),
        TerminalMode::Review => match resolve_mode_roles(config, CollabMode::Review) {
            Ok((roles, limits)) => format!(
                "builder {} · reviewer {} · {} iterations{}",
                member_label(config, &roles.builder),
                member_label(config, &roles.reviewer),
                limits.max_iterations,
                limits
                    .reviewer_hint
                    .as_deref()
                    .map(str::trim)
                    .filter(|hint| !hint.is_empty())
                    .map(|_| " · hint")
                    .unwrap_or("")
            ),
            Err(err) => err,
        },
        TerminalMode::Plan => match (
            resolve_mode_roles(config, CollabMode::Plan),
            resolve_plan_builder(config),
            resolve_plan_reviewer(config),
        ) {
            (Ok((roles, limits)), Ok(builder), Ok(reviewer)) => format!(
                "leader {} · builder {} · reviewer {} · {} · {} iterations",
                member_label(config, &roles.leader),
                member_label(config, &builder),
                reviewer
                    .as_ref()
                    .map(|id| member_label(config, id))
                    .unwrap_or_else(|| "none".to_string()),
                if resolve_plan_auto_execute(config) {
                    "auto execute"
                } else {
                    "manual execute confirmation"
                },
                limits.max_iterations
            ),
            (Err(err), _, _) | (_, Err(err), _) | (_, _, Err(err)) => err,
        },
        TerminalMode::Brainstorm => match resolve_mode_roles(config, CollabMode::Brainstorm) {
            Ok((roles, limits)) => {
                let people = roles
                    .participants
                    .iter()
                    .map(|id| member_label(config, id))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "{people} · {} waves × {} cards",
                    limits.rounds, limits.ideas_per_round
                )
            }
            Err(err) => err,
        },
        TerminalMode::Team => match (
            resolve_team_coordinator(config),
            resolve_team_limits(config),
        ) {
            (Ok(coordinator), Ok(limits)) => format!(
                "coordinator {} · {} iterations · {}",
                member_label(config, &coordinator),
                limits.max_iterations,
                if limits.allow_add_members {
                    "free add"
                } else {
                    "roster only"
                }
            ),
            (Err(err), _) | (_, Err(err)) => err,
        },
    }
}

pub fn mode_binding_is_error(config: &TeamConfig, mode: TerminalMode) -> bool {
    match mode {
        TerminalMode::Normal => false,
        TerminalMode::Review => resolve_mode_roles(config, CollabMode::Review).is_err(),
        TerminalMode::Plan => {
            resolve_mode_roles(config, CollabMode::Plan).is_err()
                || resolve_plan_builder(config).is_err()
                || resolve_plan_reviewer(config).is_err()
        }
        TerminalMode::Brainstorm => resolve_mode_roles(config, CollabMode::Brainstorm).is_err(),
        TerminalMode::Team => {
            resolve_team_coordinator(config).is_err() || resolve_team_limits(config).is_err()
        }
    }
}

fn member_label(config: &TeamConfig, id: &MemberId) -> String {
    config
        .member(id)
        .map(|member| member.display_name.clone())
        .unwrap_or_else(|| id.to_string())
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
    /// Extra instructions appended to the reviewer prompt. Not executed.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "verify_command"
    )]
    pub reviewer_hint: Option<String>,
    /// Older `team.json` files may still carry this. Review no longer runs a command.
    #[serde(default, skip_serializing)]
    pub auto_verify: Option<bool>,
}

/// Plan leader, required execution builder, optional reviewer, and execution settings stored in `team.json`.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanModeConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leader: Option<MemberId>,
    /// Required member that executes the finalized plan checklist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builder: Option<MemberId>,
    /// Optional plan-only reviewer. When omitted, a completed checklist goes
    /// straight to the execution setting below.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<MemberId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_iterations: Option<u32>,
    /// Dispatch the final checklist immediately (default), or require an
    /// explicit `/approve` before the Builder receives it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_execute: Option<bool>,
    /// Older team.json files may still carry this. Plan no longer runs a command.
    #[serde(default, skip_serializing)]
    pub auto_verify: Option<bool>,
    /// Older team.json files may still carry this. Plan no longer runs a command.
    #[serde(default, skip_serializing)]
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
    /// When true, `@@team_member` joins immediately. Default is the current roster only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_add_members: Option<bool>,
    /// Older team.json files may still carry this. Team no longer runs a command.
    #[serde(default, skip_serializing)]
    pub auto_verify: Option<bool>,
    /// Older team.json files may still carry this. Team no longer runs a command.
    #[serde(default, skip_serializing)]
    pub verify_command: Option<String>,
}

/// Resolved team-mode budget knobs (no `ModeSession`; stored on the run's mode_state).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TeamLimits {
    pub max_iterations: u32,
    pub allow_add_members: bool,
    pub auto_verify: bool,
    pub verify_command: Option<String>,
}

impl Default for TeamLimits {
    fn default() -> Self {
        Self {
            max_iterations: 3,
            allow_add_members: false,
            auto_verify: false,
            verify_command: None,
        }
    }
}

/// Resolve team-mode limits from `modes.team` (default max_iterations=3).
pub fn resolve_team_limits(config: &TeamConfig) -> Result<TeamLimits, String> {
    config
        .modes
        .team
        .as_ref()
        .map(|settings| {
            Ok(TeamLimits {
                max_iterations: positive_or_default("max_iterations", settings.max_iterations, 3)?,
                allow_add_members: settings.allow_add_members.unwrap_or(false),
                auto_verify: false,
                verify_command: None,
            })
        })
        .unwrap_or_else(|| Ok(TeamLimits::default()))
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
    /// Optional explicit auto-verify command from mode config (plan).
    pub verify_command: Option<String>,
    /// Optional extra text appended to the Review-mode reviewer prompt.
    pub reviewer_hint: Option<String>,
}

impl Default for ModeLimits {
    fn default() -> Self {
        Self {
            max_iterations: 3,
            rounds: 3,
            ideas_per_round: 4,
            auto_verify: false,
            verify_command: None,
            reviewer_hint: None,
        }
    }
}

/// Prefer a non-empty trimmed config command; otherwise `fallback`.
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
            limits.auto_verify = false;
            limits.verify_command = None;
            if let Some(settings) = &config.modes.review {
                roles.builder =
                    resolve_or_derived(config, settings.builder.as_ref(), &roles.builder)?;
                roles.reviewer =
                    resolve_or_derived(config, settings.reviewer.as_ref(), &roles.reviewer)?;
                limits.max_iterations =
                    positive_or_default("max_iterations", settings.max_iterations, 3)?;
                limits.reviewer_hint = settings
                    .reviewer_hint
                    .as_deref()
                    .map(str::trim)
                    .filter(|hint| !hint.is_empty())
                    .map(ToString::to_string);
            }
        }
        CollabMode::Plan => {
            limits.auto_verify = false;
            limits.verify_command = None;
            if let Some(settings) = &config.modes.plan {
                roles.leader = resolve_or_derived(config, settings.leader.as_ref(), &roles.leader)?;
                limits.max_iterations =
                    positive_or_default("max_iterations", settings.max_iterations, 3)?;
            }
        }
        CollabMode::Brainstorm => {
            if let Some(settings) = &config.modes.brainstorm {
                roles.participants = resolve_bound_participants(
                    config,
                    settings.participants.as_ref(),
                    &roles.participants,
                )?;
                limits.rounds = settings.generation_rounds.unwrap_or(3);
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
    if mode == CollabMode::Brainstorm && roles.participants.len() < 2 {
        return Err(
            "brainstorm mode needs at least two participants with distinct member identities"
                .to_string(),
        );
    }
    Ok((roles, limits))
}

/// Resolve the required Plan-mode execution builder. Plan has no derived
/// fallback so a configuration cannot silently dispatch implementation work.
pub fn resolve_plan_builder(config: &TeamConfig) -> Result<MemberId, String> {
    config
        .modes
        .plan
        .as_ref()
        .and_then(|settings| settings.builder.as_ref())
        .map(|id| resolve_bound_member(config, id))
        .ok_or_else(|| "plan mode needs a builder".to_string())?
}

/// Resolve the optional Plan reviewer. Unlike Review mode, an omitted Plan
/// reviewer deliberately skips the review phase.
pub fn resolve_plan_reviewer(config: &TeamConfig) -> Result<Option<MemberId>, String> {
    config
        .modes
        .plan
        .as_ref()
        .and_then(|settings| settings.reviewer.as_ref())
        .map(|id| resolve_bound_member(config, id))
        .transpose()
}

/// Whether Plan dispatches a final checklist immediately. The default keeps
/// existing configurations non-interactive after a checklist is ready.
pub fn resolve_plan_auto_execute(config: &TeamConfig) -> bool {
    config
        .modes
        .plan
        .as_ref()
        .and_then(|settings| settings.auto_execute)
        .unwrap_or(true)
}

/// Whether team mode may grow the live roster via `@@team_member`.
///
/// The default keeps the current roster. When enabled, a requested teammate
/// joins immediately and is not held for approval.
pub fn resolve_team_allow_add_members(config: &TeamConfig) -> bool {
    config
        .modes
        .team
        .as_ref()
        .and_then(|settings| settings.allow_add_members)
        .unwrap_or(false)
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
        let mut seen = std::collections::HashSet::with_capacity(ids.len());
        for id in ids {
            let resolved = resolve_bound_member(config, id)?;
            if !seen.insert(resolved.clone()) {
                return Err(format!(
                    "brainstorm participant resolves more than once: {resolved}"
                ));
            }
            out.push(resolved);
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
        assert_eq!(
            limits,
            ModeLimits {
                auto_verify: false,
                ..ModeLimits::default()
            }
        );
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
            reviewer_hint: Some("  look at the parser tests  ".to_string()),
            ..ReviewModeConfig::default()
        });
        let (roles, limits) = resolve_mode_roles(&config, CollabMode::Review).unwrap();
        assert_eq!(roles.builder, MemberId::new("builder"));
        assert_eq!(roles.reviewer, MemberId::new("reviewer"));
        assert_eq!(limits.max_iterations, 5);
        assert!(!limits.auto_verify);
        assert_eq!(limits.verify_command, None);
        assert_eq!(
            limits.reviewer_hint.as_deref(),
            Some("look at the parser tests")
        );
    }

    #[test]
    fn review_accepts_legacy_verify_command_as_hint() {
        let cfg: ReviewModeConfig =
            serde_json::from_str(r#"{"verify_command":"just check","auto_verify":false}"#).unwrap();
        assert_eq!(cfg.reviewer_hint.as_deref(), Some("just check"));
        let (roles, limits) = {
            let mut config = mixed_roster();
            config.modes.review = Some(cfg);
            resolve_mode_roles(&config, CollabMode::Review).unwrap()
        };
        assert_eq!(roles.reviewer, MemberId::new("reviewer"));
        assert_eq!(limits.reviewer_hint.as_deref(), Some("just check"));
        assert!(!limits.auto_verify);
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
    fn brainstorm_rejects_duplicate_and_single_participants() {
        let mut duplicate = mixed_roster();
        duplicate.members[1].display_name = "Builder Bot".to_string();
        duplicate.modes.brainstorm = Some(BrainstormModeConfig {
            participants: Some(vec![MemberId::new("builder"), MemberId::new("Builder Bot")]),
            ..BrainstormModeConfig::default()
        });
        assert_eq!(
            resolve_mode_roles(&duplicate, CollabMode::Brainstorm).unwrap_err(),
            "brainstorm participant resolves more than once: builder"
        );

        let single =
            TeamConfig::new("solo", "/tmp/ws").with_member(member("builder", "implementation"));
        assert_eq!(
            resolve_mode_roles(&single, CollabMode::Brainstorm).unwrap_err(),
            "brainstorm mode needs at least two participants with distinct member identities"
        );
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
    fn plan_builder_is_required_and_reviewer_is_optional() {
        let config = mixed_roster();
        assert_eq!(
            resolve_plan_builder(&config).unwrap_err(),
            "plan mode needs a builder"
        );
        assert_eq!(resolve_plan_reviewer(&config).unwrap(), None);

        let mut configured = mixed_roster();
        configured.modes.plan = Some(PlanModeConfig {
            builder: Some(MemberId::new("builder")),
            reviewer: Some(MemberId::new("reviewer")),
            ..PlanModeConfig::default()
        });
        assert_eq!(
            resolve_plan_builder(&configured).unwrap(),
            MemberId::new("builder")
        );
        assert_eq!(
            resolve_plan_reviewer(&configured).unwrap(),
            Some(MemberId::new("reviewer"))
        );

        configured.modes.plan.as_mut().unwrap().builder = Some(MemberId::new("ghost"));
        assert_eq!(
            resolve_plan_builder(&configured).unwrap_err(),
            "unknown member: ghost"
        );
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
        assert_eq!(err, "generation_rounds must be >= 2");
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
        assert!(!limits.allow_add_members);
        assert!(!limits.auto_verify);
        assert_eq!(limits.verify_command, None);

        config.modes.team = Some(TeamModeConfig {
            allow_add_members: Some(true),
            ..TeamModeConfig::default()
        });
        assert!(resolve_team_allow_add_members(&config));
        assert!(resolve_team_limits(&config).unwrap().allow_add_members);

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
    fn merge_modes_overrides_single_fields() {
        let defaults = ModesConfig {
            review: Some(ReviewModeConfig {
                builder: Some(MemberId::new("builder")),
                reviewer: Some(MemberId::new("reviewer")),
                max_iterations: Some(3),
                ..ReviewModeConfig::default()
            }),
            plan: Some(PlanModeConfig {
                leader: Some(MemberId::new("planner")),
                builder: Some(MemberId::new("builder")),
                max_iterations: Some(4),
                ..PlanModeConfig::default()
            }),
            ..ModesConfig::default()
        };
        let overrides = ModesConfig {
            review: Some(ReviewModeConfig {
                max_iterations: Some(5),
                ..ReviewModeConfig::default()
            }),
            ..ModesConfig::default()
        };
        let merged = merge_modes(&defaults, &overrides);
        let review = merged.review.unwrap();
        assert_eq!(review.builder, Some(MemberId::new("builder")));
        assert_eq!(review.reviewer, Some(MemberId::new("reviewer")));
        assert_eq!(review.max_iterations, Some(5));
        let plan = merged.plan.unwrap();
        assert_eq!(plan.leader, Some(MemberId::new("planner")));
        assert_eq!(plan.builder, Some(MemberId::new("builder")));
        assert_eq!(
            mode_overrides_for(&overrides, TerminalMode::Review).review,
            overrides.review
        );
        assert!(
            mode_overrides_for(&overrides, TerminalMode::Plan)
                .plan
                .is_none()
        );
        assert_eq!(mode_field_source(true, true), ModeValueSource::Conversation);
        assert_eq!(mode_field_source(false, true), ModeValueSource::TeamJson);
        assert_eq!(mode_field_source(false, false), ModeValueSource::Default);
        assert_eq!(ModeValueSource::Conversation.label(), "this chat");
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
