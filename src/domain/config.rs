//! Loading and synthesizing [`TeamConfig`]: read a config file, detect which
//! backends are installed, and build a default in-memory roster.

use std::path::{Path, PathBuf};
use std::{env, fs, io};

use crate::domain::team::{
    BackendKind, DefaultTarget, MemberId, PermissionMode, SandboxPolicy, TeamConfig, TeamMember,
};

const TEAM_PROTOCOL_BEGIN: &str = "<!-- ASTERLINE_TEAM_PROTOCOL_BEGIN -->";
const TEAM_PROTOCOL_END: &str = "<!-- ASTERLINE_TEAM_PROTOCOL_END -->";
pub const ASTERLINE_TEAM_SKILL_NAME: &str = "asterline-team";
pub const ASTERLINE_TEAM_SKILL_PATH: &str = ".agents/skills/asterline-team/SKILL.md";
/// Bump when the embedded skill protocol gains breaking agent-facing changes.
pub const ASTERLINE_TEAM_SKILL_VERSION: u32 = 2;
const ASTERLINE_TEAM_SKILL: &str = include_str!("../../.agents/skills/asterline-team/SKILL.md");
const MANAGED_SKILL_MARKER: &str =
    "<!-- managed-by: asterline (auto-upgraded; local edits will be overwritten) -->";

/// Ensure the workspace skill file is present and, when Asterline manages it,
/// upgraded to the embedded protocol version. User-rewritten copies are left alone.
pub fn ensure_team_skill(workspace: &Path) -> io::Result<()> {
    let path = workspace.join(ASTERLINE_TEAM_SKILL_PATH);
    if path.is_file() {
        let existing = fs::read_to_string(&path)?;
        if is_managed_skill(&existing) && skill_version(&existing) < ASTERLINE_TEAM_SKILL_VERSION {
            fs::write(&path, ASTERLINE_TEAM_SKILL)?;
        }
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, ASTERLINE_TEAM_SKILL)
}

/// Frontmatter `version:` value; missing or invalid values are treated as v1.
fn skill_version(text: &str) -> u32 {
    let mut in_frontmatter = false;
    for line in text.lines() {
        let line = line.trim();
        if line == "---" {
            if in_frontmatter {
                break;
            }
            in_frontmatter = true;
            continue;
        }
        if in_frontmatter && let Some(rest) = line.strip_prefix("version:") {
            return rest.trim().parse().unwrap_or(1);
        }
    }
    1
}

/// True for files Asterline wrote (managed marker or the historical name line).
fn is_managed_skill(text: &str) -> bool {
    text.contains(MANAGED_SKILL_MARKER) || text.contains("name: asterline-team")
}

pub fn team_skill_hint() -> String {
    format!(
        "Use ${ASTERLINE_TEAM_SKILL_NAME} for Asterline teammate messaging and roster changes. If skills are unavailable, read {ASTERLINE_TEAM_SKILL_PATH}."
    )
}

/// Read and validate a team config from a JSON file.
pub fn load_team_config(path: &Path) -> io::Result<TeamConfig> {
    let text = fs::read_to_string(path)?;
    let mut value: serde_json::Value =
        serde_json::from_str(&text).map_err(|err| invalid_config(path, err.to_string(), &text))?;
    let migrated = migrate_legacy_backends(&mut value);
    let config: TeamConfig = serde_json::from_value(value)
        .map_err(|err| invalid_config(path, err.to_string(), &text))?;
    config
        .validate()
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
    if migrated && should_rewrite_migrated_config(path) {
        let _ = fs::write(
            path,
            serde_json::to_string_pretty(&config).unwrap_or_default(),
        );
    }
    Ok(config)
}

fn invalid_config(path: &Path, err: String, text: &str) -> io::Error {
    let migration_hint = if text.contains("\"gemini\"") {
        " Legacy Gemini backend configs should be migrated to backend \"agy\"; re-run with --pick-team if automatic migration fails."
    } else {
        ""
    };
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "invalid team config {}: {err}.{migration_hint}",
            path.display()
        ),
    )
}

fn migrate_legacy_backends(value: &mut serde_json::Value) -> bool {
    let mut migrated = false;
    let Some(members) = value
        .get_mut("members")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return false;
    };
    for member in members {
        let Some(backend) = member.get_mut("backend") else {
            continue;
        };
        if backend.as_str() == Some("gemini") {
            *backend = serde_json::Value::String("agy".to_string());
            migrated = true;
        }
    }
    migrated
}

fn should_rewrite_migrated_config(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()) == Some("team.json")
        && path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            == Some(".asterline")
}

/// Which backend CLIs are available on the current `PATH`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DetectedBackends {
    pub codex: bool,
    pub claude: bool,
    pub grok: bool,
    pub agy: bool,
}

impl DetectedBackends {
    pub fn any(self) -> bool {
        self.codex || self.claude || self.grok || self.agy
    }
}

/// Detect supported backend CLIs on the current `PATH`.
pub fn detect_backends() -> DetectedBackends {
    let paths = env::var_os("PATH");
    let dirs: Vec<PathBuf> = paths
        .as_ref()
        .map(|value| env::split_paths(value).collect())
        .unwrap_or_default();
    DetectedBackends {
        codex: binary_in_dirs(&dirs, "codex"),
        claude: binary_in_dirs(&dirs, "claude"),
        grok: binary_in_dirs(&dirs, "grok"),
        agy: binary_in_dirs(&dirs, "agy"),
    }
}

fn binary_in_dirs(dirs: &[PathBuf], name: &str) -> bool {
    dirs.iter().any(|dir| dir.join(name).is_file())
}

/// Build a default in-memory roster from the detected backends:
/// both -> mixed (codex builder + claude reviewer), one -> single-backend team,
/// none -> `None` (the caller should show a setup/error state).
pub fn default_team(
    workspace: impl Into<PathBuf>,
    detected: DetectedBackends,
) -> Option<TeamConfig> {
    let workspace = workspace.into();
    match (detected.codex, detected.claude) {
        (true, true) => {
            let mut builder =
                TeamMember::new("builder", "Builder", BackendKind::Codex, "implementation");
            builder.sandbox = SandboxPolicy::WorkspaceWrite;
            let mut reviewer =
                TeamMember::new("reviewer", "Reviewer", BackendKind::Claude, "review");
            reviewer.permission_mode = Some(PermissionMode::Plan);
            let mut config = TeamConfig::new("default-mixed", workspace)
                .with_member(builder)
                .with_member(reviewer);
            config.default_target = Some(DefaultTarget::Member(MemberId::new("builder")));
            Some(config)
        }
        (true, false) => {
            let mut codex = TeamMember::new("codex", "Codex", BackendKind::Codex, "general");
            codex.sandbox = SandboxPolicy::WorkspaceWrite;
            Some(TeamConfig::new("default-codex", workspace).with_member(codex))
        }
        (false, true) => {
            let claude = TeamMember::new("claude", "Claude", BackendKind::Claude, "general");
            Some(TeamConfig::new("default-claude", workspace).with_member(claude))
        }
        (false, false) if detected.grok => {
            let mut grok = TeamMember::new("grok", "Grok", BackendKind::Grok, "general");
            grok.sandbox = SandboxPolicy::WorkspaceWrite;
            grok.permission_mode = Some(PermissionMode::Auto);
            Some(TeamConfig::new("default-grok", workspace).with_member(grok))
        }
        (false, false) if detected.agy => {
            let agy = TeamMember::new("agy", "Agy", BackendKind::Agy, "general");
            Some(TeamConfig::new("default-agy", workspace).with_member(agy))
        }
        (false, false) => None,
    }
}

/// The canonical default member for a backend, used by the interactive team
/// builder. Custom rosters (roles, sandboxes, prompts) come via a config file.
pub fn default_member(backend: BackendKind) -> TeamMember {
    match backend {
        BackendKind::Codex => {
            let mut m = TeamMember::new("builder", "Builder", BackendKind::Codex, "implementation");
            m.sandbox = SandboxPolicy::WorkspaceWrite;
            m
        }
        BackendKind::Claude => {
            let mut m = TeamMember::new("reviewer", "Reviewer", BackendKind::Claude, "review");
            m.permission_mode = Some(PermissionMode::Plan);
            m
        }
        BackendKind::Grok => {
            let mut m = TeamMember::new("grok", "Grok", BackendKind::Grok, "implementation");
            m.sandbox = SandboxPolicy::WorkspaceWrite;
            m.permission_mode = Some(PermissionMode::Auto);
            m
        }
        BackendKind::Agy => {
            TeamMember::new("researcher", "Researcher", BackendKind::Agy, "research")
        }
    }
}

/// Build a team from an explicit list of backends chosen in the interactive
/// builder. Returns `None` when no backend is selected.
pub fn build_team(workspace: impl Into<PathBuf>, backends: &[BackendKind]) -> Option<TeamConfig> {
    if backends.is_empty() {
        return None;
    }
    let mut config = TeamConfig::new("custom", workspace);
    for &backend in backends {
        config = config.with_member(default_member(backend));
    }
    if let Some(first) = config.members.first().map(|m| m.id.clone()) {
        config.default_target = Some(DefaultTarget::Member(first));
    }
    Some(config)
}

/// Prepend a compact Asterline team hint to each member's system prompt.
/// Detailed protocol lives in the repo skill at `.agents/skills`.
pub fn inject_team_protocol(team: &mut TeamConfig) {
    let protocols: Vec<String> = team
        .members
        .iter()
        .map(|me| {
            let teammates: Vec<String> = team
                .members
                .iter()
                .filter(|other| other.id != me.id)
                .map(|other| format!("{} [{}]", other.id, other.role))
                .collect();
            build_protocol(me.id.as_str(), &teammates)
        })
        .collect();

    for (member, protocol) in team.members.iter_mut().zip(protocols) {
        let wrapped = format!("{TEAM_PROTOCOL_BEGIN}\n{protocol}\n{TEAM_PROTOCOL_END}");
        let existing = member.system_prompt.take().map(|prompt| {
            let stripped = strip_team_protocol(&prompt);
            stripped.trim().to_string()
        });
        member.system_prompt = match existing.filter(|prompt| !prompt.is_empty()) {
            Some(existing) => Some(format!("{wrapped}\n\n{existing}")),
            None => Some(wrapped),
        };
    }
}

/// Remove Asterline's injected protocol from system prompts before persisting a
/// user-editable team config.
pub fn strip_team_protocols(mut team: TeamConfig) -> TeamConfig {
    for member in &mut team.members {
        if let Some(prompt) = member.system_prompt.take() {
            let stripped = strip_team_protocol(&prompt);
            member.system_prompt = if stripped.trim().is_empty() {
                None
            } else {
                Some(stripped.trim().to_string())
            };
        }
    }
    team
}

pub fn strip_team_protocol(prompt: &str) -> String {
    let Some(begin) = prompt.find(TEAM_PROTOCOL_BEGIN) else {
        return prompt.to_string();
    };
    let after_begin = begin + TEAM_PROTOCOL_BEGIN.len();
    let Some(relative_end) = prompt[after_begin..].find(TEAM_PROTOCOL_END) else {
        return prompt.to_string();
    };
    let end = after_begin + relative_end + TEAM_PROTOCOL_END.len();
    let mut out = String::new();
    out.push_str(prompt[..begin].trim_end());
    if !out.is_empty() && !prompt[end..].trim_start().is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(prompt[end..].trim_start());
    out
}

fn build_protocol(me: &str, teammates: &[String]) -> String {
    let mut protocol = format!(
        "You are \"{me}\", a member of an Asterline multi-agent team.\n\
         {}\n",
        team_skill_hint()
    );
    if teammates.is_empty() {
        protocol.push_str("You are the only member; there are no teammates to message.\n");
    } else {
        protocol.push_str(&format!(
            "Teammates you can message: {}.\n",
            teammates.join(", ")
        ));
    }
    protocol.push_str("All other text you write is shown to the user.");
    protocol
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_default_team_pairs_codex_builder_with_claude_reviewer() {
        let detected = DetectedBackends {
            codex: true,
            claude: true,
            grok: false,
            agy: false,
        };
        let config = default_team("/tmp/ws", detected).expect("mixed team");

        assert!(config.validate().is_ok());
        assert_eq!(config.members.len(), 2);
        assert_eq!(config.members[0].backend, BackendKind::Codex);
        assert_eq!(config.members[1].backend, BackendKind::Claude);
        assert_eq!(config.default_member_ids(), vec![MemberId::new("builder")]);
    }

    #[test]
    fn codex_only_default_team_is_single_codex() {
        let detected = DetectedBackends {
            codex: true,
            claude: false,
            grok: false,
            agy: false,
        };
        let config = default_team("/tmp/ws", detected).expect("codex team");

        assert_eq!(config.members.len(), 1);
        assert_eq!(config.members[0].backend, BackendKind::Codex);
    }

    #[test]
    fn claude_only_default_team_is_single_claude() {
        let detected = DetectedBackends {
            codex: false,
            claude: true,
            grok: false,
            agy: false,
        };
        let config = default_team("/tmp/ws", detected).expect("claude team");

        assert_eq!(config.members.len(), 1);
        assert_eq!(config.members[0].backend, BackendKind::Claude);
    }

    #[test]
    fn no_backends_yields_no_default_team() {
        let detected = DetectedBackends {
            codex: false,
            claude: false,
            grok: false,
            agy: false,
        };
        assert!(default_team("/tmp/ws", detected).is_none());
    }

    #[test]
    fn agy_only_default_team_is_single_agy() {
        let detected = DetectedBackends {
            codex: false,
            claude: false,
            grok: false,
            agy: true,
        };
        let config = default_team("/tmp/ws", detected).expect("agy team");
        assert_eq!(config.members.len(), 1);
        assert_eq!(config.members[0].backend, BackendKind::Agy);
    }

    #[test]
    fn grok_only_default_team_is_single_grok() {
        let detected = DetectedBackends {
            codex: false,
            claude: false,
            grok: true,
            agy: false,
        };
        let config = default_team("/tmp/ws", detected).expect("grok team");
        assert_eq!(config.members.len(), 1);
        assert_eq!(config.members[0].backend, BackendKind::Grok);
        assert_eq!(config.members[0].sandbox, SandboxPolicy::WorkspaceWrite);
        assert_eq!(
            config.members[0].permission_mode,
            Some(PermissionMode::Auto)
        );
    }

    #[test]
    fn binary_in_dirs_finds_existing_file() {
        let dir = std::env::temp_dir().join(format!("asterline-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("faux-backend");
        std::fs::write(&bin, b"#!/bin/sh\n").unwrap();

        let dirs = vec![dir.clone()];
        assert!(binary_in_dirs(&dirs, "faux-backend"));
        assert!(!binary_in_dirs(&dirs, "nope-backend"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn build_team_from_selected_backends() {
        assert!(build_team("/tmp/ws", &[]).is_none());

        let config = build_team("/tmp/ws", &[BackendKind::Codex, BackendKind::Agy]).unwrap();
        assert!(config.validate().is_ok());
        assert_eq!(config.members.len(), 2);
        assert_eq!(config.members[0].backend, BackendKind::Codex);
        assert_eq!(config.members[1].backend, BackendKind::Agy);
        // The first selected member is the default target.
        assert_eq!(config.default_member_ids(), vec![MemberId::new("builder")]);
    }

    #[test]
    fn default_member_maps_backend_to_role() {
        assert_eq!(default_member(BackendKind::Codex).role, "implementation");
        assert_eq!(default_member(BackendKind::Claude).role, "review");
        assert_eq!(default_member(BackendKind::Grok).backend, BackendKind::Grok);
        assert_eq!(default_member(BackendKind::Agy).backend, BackendKind::Agy);
    }

    #[test]
    fn team_protocol_is_injected_and_stripped_for_persistence() {
        let mut member = TeamMember::new("builder", "Builder", BackendKind::Codex, "impl");
        member.system_prompt = Some("custom prompt".to_string());
        let mut config = TeamConfig::new("t", "/tmp/ws")
            .with_member(member)
            .with_member(TeamMember::new(
                "reviewer",
                "Reviewer",
                BackendKind::Claude,
                "review",
            ));

        inject_team_protocol(&mut config);
        let prompt = config.members[0].system_prompt.as_ref().unwrap();
        assert!(prompt.contains("$asterline-team"));
        assert!(prompt.contains(ASTERLINE_TEAM_SKILL_PATH));
        assert!(!prompt.contains("@@team_message"));
        assert!(!prompt.contains("@@team_member"));
        assert!(prompt.contains("reviewer"));
        assert!(prompt.contains("custom prompt"));

        let stripped = strip_team_protocols(config);
        assert_eq!(
            stripped.members[0].system_prompt.as_deref(),
            Some("custom prompt")
        );
        assert_eq!(stripped.members[1].system_prompt, None);
    }

    #[test]
    fn ensure_team_skill_writes_repo_skill_when_missing() {
        let dir = std::env::temp_dir().join(format!("asterline-skill-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_team_skill(&dir).unwrap();

        let path = dir.join(ASTERLINE_TEAM_SKILL_PATH);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("name: asterline-team"));
        assert!(text.contains("@@team_message"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ensure_team_skill_upgrades_managed_v1_file() {
        let dir =
            std::env::temp_dir().join(format!("asterline-skill-upgrade-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(ASTERLINE_TEAM_SKILL_PATH);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // v1 managed files had the name line but no version / managed marker.
        std::fs::write(
            &path,
            "---\nname: asterline-team\ndescription: old\n---\n\n# Old protocol\n@@team_message\n",
        )
        .unwrap();

        ensure_team_skill(&dir).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("version: 2"));
        assert!(text.contains("@@review"));
        assert!(text.contains(MANAGED_SKILL_MARKER));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ensure_team_skill_leaves_user_rewritten_file_alone() {
        let dir =
            std::env::temp_dir().join(format!("asterline-skill-custom-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(ASTERLINE_TEAM_SKILL_PATH);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let custom = "# My custom team notes\nDo not overwrite me.\n";
        std::fs::write(&path, custom).unwrap();

        ensure_team_skill(&dir).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, custom);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn embedded_team_skill_is_protocol_v2() {
        assert!(ASTERLINE_TEAM_SKILL.contains("version: 2"));
        assert!(ASTERLINE_TEAM_SKILL.contains(MANAGED_SKILL_MARKER));
        assert!(ASTERLINE_TEAM_SKILL.contains("@@review"));
        assert_eq!(ASTERLINE_TEAM_SKILL_VERSION, 2);
    }

    #[test]
    fn load_team_config_round_trips_via_file() {
        let dir = std::env::temp_dir().join(format!("asterline-cfg-load-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("team.json");
        let config = default_team(
            &dir,
            DetectedBackends {
                codex: true,
                claude: true,
                grok: false,
                agy: false,
            },
        )
        .unwrap();
        std::fs::write(&path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

        let loaded = load_team_config(&path).expect("config loads");
        assert_eq!(loaded, config);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_team_config_derives_missing_member_ids() {
        let dir =
            std::env::temp_dir().join(format!("asterline-cfg-derived-id-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("team.json");
        std::fs::write(
            &path,
            r#"{
              "name": "manual",
              "workspace": "/tmp/ws",
              "default_target": { "member": "lead-engineer" },
              "members": [{
                "display_name": "Lead Engineer",
                "backend": "codex",
                "role": "implementation"
              }]
            }"#,
        )
        .unwrap();

        let config = load_team_config(&path).expect("config derives id from display_name");
        assert_eq!(config.members[0].id, MemberId::new("lead-engineer"));
        assert_eq!(
            config.default_member_ids(),
            vec![MemberId::new("lead-engineer")]
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_team_config_migrates_old_gemini_backend() {
        let dir = std::env::temp_dir().join(format!("asterline-cfg-gemini-{}", std::process::id()));
        let config_dir = dir.join(".asterline");
        std::fs::create_dir_all(&config_dir).unwrap();
        let path = config_dir.join("team.json");
        std::fs::write(
            &path,
            r#"{
              "name": "old",
              "workspace": "/tmp/ws",
              "members": [{
                "id": "researcher",
                "display_name": "Researcher",
                "backend": "gemini",
                "role": "research"
              }]
            }"#,
        )
        .unwrap();

        let config = load_team_config(&path).expect("old gemini backend is migrated");
        assert_eq!(config.members[0].backend, BackendKind::Agy);
        let rewritten = std::fs::read_to_string(&path).unwrap();
        assert!(rewritten.contains("\"backend\": \"agy\""));
        assert!(!rewritten.contains("\"gemini\""));

        std::fs::remove_dir_all(&dir).ok();
    }
}
