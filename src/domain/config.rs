//! Loading and synthesizing [`TeamConfig`]: read a config file, detect which
//! backends are installed, and build a default in-memory roster.

use std::path::{Path, PathBuf};
use std::{env, fs, io};

use crate::domain::team::{
    BackendKind, DefaultTarget, MemberId, PermissionMode, SandboxPolicy, TeamConfig, TeamMember,
};

/// Read and validate a team config from a JSON file.
pub fn load_team_config(path: &Path) -> io::Result<TeamConfig> {
    let text = fs::read_to_string(path)?;
    let config: TeamConfig = serde_json::from_str(&text).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid team config {}: {err}", path.display()),
        )
    })?;
    config
        .validate()
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
    Ok(config)
}

/// Which backend CLIs are available on the current `PATH`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DetectedBackends {
    pub codex: bool,
    pub claude: bool,
    pub gemini: bool,
}

impl DetectedBackends {
    pub fn any(self) -> bool {
        self.codex || self.claude || self.gemini
    }
}

/// Detect `codex` and `claude` on the current `PATH`.
pub fn detect_backends() -> DetectedBackends {
    let paths = env::var_os("PATH");
    let dirs: Vec<PathBuf> = paths
        .as_ref()
        .map(|value| env::split_paths(value).collect())
        .unwrap_or_default();
    DetectedBackends {
        codex: binary_in_dirs(&dirs, "codex"),
        claude: binary_in_dirs(&dirs, "claude"),
        gemini: binary_in_dirs(&dirs, "gemini"),
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
        (false, false) if detected.gemini => {
            let gemini = TeamMember::new("gemini", "Gemini", BackendKind::Gemini, "general");
            Some(TeamConfig::new("default-gemini", workspace).with_member(gemini))
        }
        (false, false) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_default_team_pairs_codex_builder_with_claude_reviewer() {
        let detected = DetectedBackends {
            codex: true,
            claude: true,
            gemini: false,
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
            gemini: false,
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
            gemini: false,
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
            gemini: false,
        };
        assert!(default_team("/tmp/ws", detected).is_none());
    }

    #[test]
    fn gemini_only_default_team_is_single_gemini() {
        let detected = DetectedBackends {
            codex: false,
            claude: false,
            gemini: true,
        };
        let config = default_team("/tmp/ws", detected).expect("gemini team");
        assert_eq!(config.members.len(), 1);
        assert_eq!(config.members[0].backend, BackendKind::Gemini);
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
    fn load_team_config_round_trips_via_file() {
        let dir = std::env::temp_dir().join(format!("asterline-cfg-load-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("team.json");
        let config = default_team(
            &dir,
            DetectedBackends {
                codex: true,
                claude: true,
                gemini: false,
            },
        )
        .unwrap();
        std::fs::write(&path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

        let loaded = load_team_config(&path).expect("config loads");
        assert_eq!(loaded, config);

        std::fs::remove_dir_all(&dir).ok();
    }
}
