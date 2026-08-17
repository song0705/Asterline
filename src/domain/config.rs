//! Loading and synthesizing [`TeamConfig`]: read a config file, detect which
//! backends are installed, and build a default in-memory roster.

use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;
use std::{env, fs, io};

use crate::adapter::models::run_with_timeout;
use crate::domain::team::{
    BackendKind, DefaultTarget, MemberId, PermissionMode, SandboxPolicy, TeamConfig, TeamMember,
};
use crate::fs_safety;

const TEAM_PROTOCOL_BEGIN: &str = "<!-- ASTERLINE_TEAM_PROTOCOL_BEGIN -->";
const TEAM_PROTOCOL_END: &str = "<!-- ASTERLINE_TEAM_PROTOCOL_END -->";
pub const ASTERLINE_TEAM_SKILL_NAME: &str = "asterline-team";
pub const ASTERLINE_TEAM_SKILL_PATH: &str = ".agents/skills/asterline-team/SKILL.md";
/// Live roster and member status. Rewritten whenever the team or a status changes.
pub const ASTERLINE_ROSTER_PATH: &str = ".asterline/roster.md";
/// Bump when the embedded skill protocol gains breaking agent-facing changes.
pub const ASTERLINE_TEAM_SKILL_VERSION: u32 = 19;
const ASTERLINE_TEAM_SKILL: &str = include_str!("../../.agents/skills/asterline-team/SKILL.md");
pub const ASTERLINE_BRAINSTORM_SKILL_NAME: &str = "asterline-brainstorm";
pub const ASTERLINE_BRAINSTORM_SKILL_PATH: &str = ".agents/skills/asterline-brainstorm/SKILL.md";
const ASTERLINE_BRAINSTORM_SKILL: &str =
    include_str!("../../.agents/skills/asterline-brainstorm/SKILL.md");
const MANAGED_SKILL_MARKER: &str =
    "<!-- managed-by: asterline (auto-upgraded; local edits will be overwritten) -->";
const MIN_AGY_VERSION: (u64, u64, u64) = (1, 1, 12);
// Backend discovery runs alongside other startup and test subprocesses. Keep
// the probe bounded, but leave enough headroom that a loaded machine does not
// incorrectly hide an installed Agy binary.
const BACKEND_CAPABILITY_TIMEOUT: Duration = Duration::from_secs(5);

/// Ensure the workspace skill file is present and, when Asterline manages it,
/// upgraded to the embedded protocol version. User-rewritten copies are left alone.
pub fn ensure_team_skill(workspace: &Path) -> io::Result<()> {
    let path = managed_skill_path(workspace, ASTERLINE_TEAM_SKILL_NAME)?;
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            let existing = fs_safety::read_regular_to_string(&path, "team skill")?;
            if is_managed_skill(&existing)
                && skill_version(&existing) < ASTERLINE_TEAM_SKILL_VERSION
            {
                fs_safety::write_regular_file(&path, "team skill", ASTERLINE_TEAM_SKILL)?;
            }
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs_safety::write_regular_file(&path, "team skill", ASTERLINE_TEAM_SKILL)
        }
        Err(error) => Err(error),
    }
}

/// Install the default brainstorm protocol once. Existing deployment-local
/// copies are always preserved so teams can customize the method and card text.
pub fn ensure_brainstorm_skill(workspace: &Path) -> io::Result<()> {
    let path = managed_skill_path(workspace, ASTERLINE_BRAINSTORM_SKILL_NAME)?;
    match fs::symlink_metadata(&path) {
        Ok(_) => fs_safety::read_regular_to_string(&path, "brainstorm skill").map(|_| ()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs_safety::write_regular_file(&path, "brainstorm skill", ASTERLINE_BRAINSTORM_SKILL)
        }
        Err(error) => Err(error),
    }
}

/// Load the deployment-local brainstorm protocol, falling back to the embedded
/// default for tests and workspaces that have not been initialized yet.
pub fn brainstorm_skill_text(workspace: &Path) -> String {
    managed_skill_path(workspace, ASTERLINE_BRAINSTORM_SKILL_NAME)
        .and_then(|path| fs_safety::read_regular_to_string(&path, "brainstorm skill"))
        .unwrap_or_else(|_| ASTERLINE_BRAINSTORM_SKILL.to_string())
}

fn managed_skill_path(workspace: &Path, skill_name: &str) -> io::Result<PathBuf> {
    let directory = fs_safety::ensure_workspace_directory(
        workspace,
        &[".agents", "skills", skill_name],
        false,
    )?;
    Ok(directory.join("SKILL.md"))
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

/// True only for files that explicitly opt into Asterline-managed upgrades.
fn is_managed_skill(text: &str) -> bool {
    text.contains(MANAGED_SKILL_MARKER)
}

pub fn team_skill_hint() -> String {
    format!(
        "The Asterline team skill is available at {ASTERLINE_TEAM_SKILL_PATH}. Read this skill when you need its team controls. \
         If a tool or plan is waiting for the user to approve in Asterline, wait — do not retry or assume it ran."
    )
}

/// Read and validate a team config from a JSON file.
pub fn load_team_config(path: &Path) -> io::Result<TeamConfig> {
    let text = fs_safety::read_regular_to_string(path, "team config")?;
    let config: TeamConfig =
        serde_json::from_str(&text).map_err(|err| invalid_config(path, err.to_string()))?;
    config
        .validate()
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
    Ok(config)
}

fn invalid_config(path: &Path, err: String) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("invalid team config {}: {err}", path.display()),
    )
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

    pub fn contains(self, backend: BackendKind) -> bool {
        match backend {
            BackendKind::Codex => self.codex,
            BackendKind::Claude => self.claude,
            BackendKind::Grok => self.grok,
            BackendKind::Agy => self.agy,
        }
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
        agy: supported_agy_in_dirs(&dirs, env::var_os("PATHEXT").as_deref(), cfg!(windows)),
    }
}

fn binary_in_dirs(dirs: &[PathBuf], name: &str) -> bool {
    resolve_binary_in_dirs(dirs, name, env::var_os("PATHEXT").as_deref(), cfg!(windows)).is_some()
}

fn supported_agy_in_dirs(dirs: &[PathBuf], path_ext: Option<&OsStr>, windows: bool) -> bool {
    resolve_binary_in_dirs(dirs, "agy", path_ext, windows)
        .is_some_and(|binary| check_agy_version_path(&binary).is_ok())
}

pub(crate) fn check_agy_version(binary: &str) -> Result<(), String> {
    let resolved = resolve_binary_on_path(binary).unwrap_or_else(|| PathBuf::from(binary));
    check_agy_version_path(&resolved)
}

fn check_agy_version_path(binary: &Path) -> Result<(), String> {
    let program = binary.to_string_lossy();
    let output = run_with_timeout(
        &program,
        &["--version"],
        Path::new("."),
        BACKEND_CAPABILITY_TIMEOUT,
    )
    .map_err(|error| format!("Agy 1.1.12 or newer is required; {error}"))?;
    validate_agy_version(&String::from_utf8_lossy(&output.stdout))
}

pub(crate) fn validate_agy_version(raw: &str) -> Result<(), String> {
    let version = parse_agy_version(raw).ok_or_else(|| {
        format!(
            "Agy 1.1.12 or newer is required; could not parse version from `{}`",
            compact_version_output(raw, 80)
        )
    })?;
    let numeric = (version.major, version.minor, version.patch);
    if numeric < MIN_AGY_VERSION || (numeric == MIN_AGY_VERSION && version.prerelease) {
        return Err(format!(
            "Agy 1.1.12 or newer is required for structured streaming and reliable headless --mode enforcement; found {}",
            version.display
        ));
    }
    Ok(())
}

struct AgyVersion {
    major: u64,
    minor: u64,
    patch: u64,
    prerelease: bool,
    display: String,
}

fn parse_agy_version(raw: &str) -> Option<AgyVersion> {
    raw.split_whitespace().find_map(parse_agy_version_token)
}

fn parse_agy_version_token(token: &str) -> Option<AgyVersion> {
    let display = token.to_string();
    let token = token.trim_start_matches('v');
    let token = token.split_once('+').map_or(token, |(version, _)| version);
    let (core, prerelease) = token
        .split_once('-')
        .map_or((token, false), |(version, _)| (version, true));
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(AgyVersion {
        major,
        minor,
        patch,
        prerelease,
        display,
    })
}

fn compact_version_output(raw: &str, max_chars: usize) -> String {
    raw.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect()
}

/// Resolve a runnable program exactly as Asterline will launch it. On Windows,
/// bare names honor `PATHEXT`; returning the concrete path is important because
/// Rust only appends `.exe` when `Command` is given an extensionless name.
pub(crate) fn resolve_binary_on_path(name: &str) -> Option<PathBuf> {
    let requested = Path::new(name);
    let has_path = requested.is_absolute()
        || requested.components().count() != 1
        || requested
            .components()
            .any(|component| !matches!(component, Component::Normal(_)));
    let path_ext = env::var_os("PATHEXT");
    if has_path {
        return resolve_binary_candidate(requested, path_ext.as_deref(), cfg!(windows));
    }
    let paths = env::var_os("PATH")?;
    let dirs = env::split_paths(&paths).collect::<Vec<_>>();
    resolve_binary_in_dirs(&dirs, name, path_ext.as_deref(), cfg!(windows))
}

fn resolve_binary_in_dirs(
    dirs: &[PathBuf],
    name: &str,
    path_ext: Option<&OsStr>,
    windows: bool,
) -> Option<PathBuf> {
    dirs.iter()
        .find_map(|dir| resolve_binary_candidate(&dir.join(name), path_ext, windows))
}

fn resolve_binary_candidate(
    requested: &Path,
    path_ext: Option<&OsStr>,
    windows: bool,
) -> Option<PathBuf> {
    executable_candidates(requested, path_ext, windows)
        .into_iter()
        .find(|candidate| candidate_is_executable(candidate, windows))
}

fn executable_candidates(
    requested: &Path,
    path_ext: Option<&OsStr>,
    windows: bool,
) -> Vec<PathBuf> {
    if !windows || requested.extension().is_some() {
        return vec![requested.to_path_buf()];
    }
    windows_executable_extensions(path_ext)
        .into_iter()
        .map(|extension| requested.with_extension(extension))
        .collect()
}

fn windows_executable_extensions(path_ext: Option<&OsStr>) -> Vec<OsString> {
    let raw = path_ext
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| OsStr::new(".COM;.EXE;.BAT;.CMD"));
    let mut extensions = Vec::new();
    for extension in raw.to_string_lossy().split(';') {
        let extension = extension.trim();
        if extension.len() <= 1
            || !extension.starts_with('.')
            || extension.contains('/')
            || extension.contains('\\')
        {
            continue;
        }
        let extension = OsString::from(&extension[1..]);
        if !extensions.iter().any(|known: &OsString| {
            known
                .to_string_lossy()
                .eq_ignore_ascii_case(&extension.to_string_lossy())
        }) {
            extensions.push(extension);
        }
    }
    extensions
}

fn candidate_is_executable(path: &Path, windows: bool) -> bool {
    if windows {
        path.is_file()
    } else {
        is_executable(path)
    }
}

/// User profile used for backend history and global skills. Native Windows
/// shells expose `USERPROFILE`; Unix shells expose `HOME`. Each platform also
/// accepts the other variable as a compatibility fallback.
pub(crate) fn user_home_dir() -> Option<PathBuf> {
    user_home_dir_from_values(
        env::var_os("HOME"),
        env::var_os("USERPROFILE"),
        cfg!(windows),
    )
}

fn user_home_dir_from_values(
    home: Option<OsString>,
    user_profile: Option<OsString>,
    windows: bool,
) -> Option<PathBuf> {
    let usable = |value: Option<OsString>| value.filter(|value| !value.is_empty());
    let selected = if windows {
        usable(user_profile).or_else(|| usable(home))
    } else {
        usable(home).or_else(|| usable(user_profile))
    }?;
    Some(PathBuf::from(selected))
}

pub(crate) fn codex_home_dir() -> Option<PathBuf> {
    codex_home_dir_from_values(env::var_os("CODEX_HOME"), user_home_dir())
}

fn codex_home_dir_from_values(
    codex_home: Option<OsString>,
    user_home: Option<PathBuf>,
) -> Option<PathBuf> {
    codex_home
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| user_home.map(|home| home.join(".codex")))
}

pub(crate) fn paths_equivalent(left: &Path, right: &Path) -> bool {
    let left = fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    paths_equivalent_for_platform(&left, &right, cfg!(windows))
}

fn paths_equivalent_for_platform(left: &Path, right: &Path, windows: bool) -> bool {
    if windows {
        windows_path_key(left) == windows_path_key(right)
    } else {
        left == right
    }
}

fn windows_path_key(path: &Path) -> String {
    let mut value = path.as_os_str().to_string_lossy().replace('/', "\\");
    if let Some(rest) = value.strip_prefix("\\\\?\\UNC\\") {
        value = format!("\\\\{rest}");
    } else if let Some(rest) = value.strip_prefix("\\\\?\\") {
        value = rest.to_string();
    }
    while value.ends_with('\\') && value.len() > 3 {
        value.pop();
    }
    value.to_lowercase()
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
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
            builder.apply_codex_permissions_preset(
                crate::domain::team::CodexPermissionsPreset::AskForApproval,
            );
            let mut reviewer =
                TeamMember::new("reviewer", "Reviewer", BackendKind::Claude, "review");
            reviewer.permission_mode = Some(PermissionMode::AcceptEdits);
            let mut config = TeamConfig::new("default-mixed", workspace)
                .with_member(builder)
                .with_member(reviewer);
            config.default_target = Some(DefaultTarget::Member(MemberId::new("builder")));
            Some(config)
        }
        (true, false) => {
            let mut codex = TeamMember::new("codex", "Codex", BackendKind::Codex, "general");
            codex.apply_codex_permissions_preset(
                crate::domain::team::CodexPermissionsPreset::AskForApproval,
            );
            Some(TeamConfig::new("default-codex", workspace).with_member(codex))
        }
        (false, true) => {
            let mut claude = TeamMember::new("claude", "Claude", BackendKind::Claude, "general");
            claude.permission_mode = Some(PermissionMode::AcceptEdits);
            Some(TeamConfig::new("default-claude", workspace).with_member(claude))
        }
        (false, false) if detected.grok => {
            let mut grok = TeamMember::new("grok", "Grok", BackendKind::Grok, "general");
            grok.sandbox = SandboxPolicy::WorkspaceWrite;
            grok.permission_mode = Some(PermissionMode::Auto);
            Some(TeamConfig::new("default-grok", workspace).with_member(grok))
        }
        (false, false) if detected.agy => {
            let mut agy = TeamMember::new("agy", "Agy", BackendKind::Agy, "general");
            agy.sandbox = SandboxPolicy::WorkspaceWrite;
            agy.permission_mode = Some(PermissionMode::AcceptEdits);
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
            m.apply_codex_permissions_preset(
                crate::domain::team::CodexPermissionsPreset::AskForApproval,
            );
            m
        }
        BackendKind::Claude => {
            let mut m = TeamMember::new("reviewer", "Reviewer", BackendKind::Claude, "review");
            m.permission_mode = Some(PermissionMode::AcceptEdits);
            m
        }
        BackendKind::Grok => {
            let mut m = TeamMember::new("grok", "Grok", BackendKind::Grok, "implementation");
            m.sandbox = SandboxPolicy::WorkspaceWrite;
            m.permission_mode = Some(PermissionMode::Auto);
            m
        }
        BackendKind::Agy => {
            let mut m = TeamMember::new("researcher", "Researcher", BackendKind::Agy, "research");
            m.sandbox = SandboxPolicy::WorkspaceWrite;
            m.permission_mode = Some(PermissionMode::AcceptEdits);
            m
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
        protocol.push_str("You are the only member.\n");
    } else {
        protocol.push_str(&format!("Teammates: {}.\n", teammates.join(", ")));
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
        assert_eq!(config.members[0].sandbox, SandboxPolicy::WorkspaceWrite);
        assert_eq!(
            config.members[1].permission_mode,
            Some(PermissionMode::AcceptEdits)
        );
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
        assert_eq!(
            config.members[0].permission_mode,
            Some(PermissionMode::AcceptEdits)
        );
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
        assert_eq!(config.members[0].sandbox, SandboxPolicy::WorkspaceWrite);
        assert_eq!(
            config.members[0].permission_mode,
            Some(PermissionMode::AcceptEdits)
        );
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
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bin = if cfg!(windows) {
            dir.join("faux-backend.EXE")
        } else {
            dir.join("faux-backend")
        };
        std::fs::write(&bin, b"#!/bin/sh\n").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = std::fs::metadata(&bin).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&bin, permissions).unwrap();
        }

        let dirs = vec![dir.clone()];
        assert!(binary_in_dirs(&dirs, "faux-backend"));
        assert!(!binary_in_dirs(&dirs, "nope-backend"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn agy_capability_detection_rejects_versions_before_1_1_12() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "asterline-cfg-agy-capability-{}",
            std::process::id()
        ));
        let old_dir = root.join("old");
        let supported_dir = root.join("supported");
        let _ = std::fs::remove_dir_all(&root);
        for (dir, version) in [(&old_dir, "1.1.11"), (&supported_dir, "1.1.12")] {
            std::fs::create_dir_all(dir).unwrap();
            let binary = dir.join("agy");
            std::fs::write(&binary, format!("#!/bin/sh\nprintf '{version}\\r\\n'\n")).unwrap();
            let mut permissions = std::fs::metadata(&binary).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&binary, permissions).unwrap();
        }

        assert!(!supported_agy_in_dirs(&[old_dir], None, false));
        assert!(supported_agy_in_dirs(&[supported_dir], None, false));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn windows_pathext_resolves_script_shims_without_global_env_mutation() {
        let dir =
            std::env::temp_dir().join(format!("asterline-cfg-pathext-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let shim = dir.join("faux-backend.CMD");
        std::fs::write(&shim, b"@echo off\r\n").unwrap();

        let resolved = resolve_binary_in_dirs(
            std::slice::from_ref(&dir),
            "faux-backend",
            Some(OsStr::new(".EXE;.CMD;.cmd")),
            true,
        );

        assert_eq!(resolved, Some(shim));
        assert_eq!(
            windows_executable_extensions(Some(OsStr::new(".EXE;.CMD;.cmd"))),
            vec![OsString::from("EXE"), OsString::from("CMD")]
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn home_selection_covers_windows_userprofile_and_shell_fallbacks() {
        assert_eq!(
            user_home_dir_from_values(
                Some(OsString::from("/shell/home")),
                Some(OsString::from(r"C:\Users\Ada")),
                true,
            ),
            Some(PathBuf::from(r"C:\Users\Ada"))
        );
        assert_eq!(
            user_home_dir_from_values(
                Some(OsString::from("/shell/home")),
                Some(OsString::from(r"C:\Users\Ada")),
                false,
            ),
            Some(PathBuf::from("/shell/home"))
        );
        assert_eq!(
            user_home_dir_from_values(None, Some(OsString::from(r"C:\Users\Ada")), false),
            Some(PathBuf::from(r"C:\Users\Ada"))
        );
        assert_eq!(
            user_home_dir_from_values(Some(OsString::new()), Some(OsString::new()), true),
            None
        );
    }

    #[test]
    fn codex_home_prefers_explicit_override_without_requiring_user_home() {
        assert_eq!(
            codex_home_dir_from_values(Some(OsString::from("/custom/codex")), None),
            Some(PathBuf::from("/custom/codex"))
        );
        assert_eq!(
            codex_home_dir_from_values(Some(OsString::new()), Some(PathBuf::from("/shell/home")),),
            Some(PathBuf::from("/shell/home/.codex"))
        );
    }

    #[test]
    fn windows_project_paths_ignore_separator_case_and_verbatim_prefix() {
        assert!(paths_equivalent_for_platform(
            Path::new(r"C:\Work\Repo\"),
            Path::new("c:/work/repo"),
            true,
        ));
        assert!(paths_equivalent_for_platform(
            Path::new(r"\\?\C:\Work\Repo"),
            Path::new(r"c:\work\repo"),
            true,
        ));
        assert!(!paths_equivalent_for_platform(
            Path::new(r"C:\Work\Repo"),
            Path::new(r"C:\Work\Other"),
            true,
        ));
    }

    #[cfg(unix)]
    #[test]
    fn binary_in_dirs_rejects_non_executable_file() {
        let dir = std::env::temp_dir().join(format!(
            "asterline-cfg-non-executable-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("faux-backend"), b"#!/bin/sh\n").unwrap();

        assert!(!binary_in_dirs(std::slice::from_ref(&dir), "faux-backend"));

        std::fs::remove_dir_all(dir).ok();
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
    fn default_member_uses_permissive_backend_controls() {
        let codex = default_member(BackendKind::Codex);
        assert_eq!(codex.sandbox, SandboxPolicy::WorkspaceWrite);
        assert_eq!(codex.permission_mode, Some(PermissionMode::Auto));
        assert_eq!(
            codex.codex_permissions_preset(),
            Some(crate::domain::team::CodexPermissionsPreset::AskForApproval)
        );

        let claude = default_member(BackendKind::Claude);
        assert_eq!(claude.permission_mode, Some(PermissionMode::AcceptEdits));

        let grok = default_member(BackendKind::Grok);
        assert_eq!(grok.sandbox, SandboxPolicy::WorkspaceWrite);
        assert_eq!(grok.permission_mode, Some(PermissionMode::Auto));

        let agy = default_member(BackendKind::Agy);
        assert_eq!(agy.sandbox, SandboxPolicy::WorkspaceWrite);
        assert_eq!(agy.permission_mode, Some(PermissionMode::AcceptEdits));
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
        assert!(prompt.contains("Asterline team skill"));
        assert!(prompt.contains(ASTERLINE_TEAM_SKILL_PATH));
        assert!(!prompt.contains("$asterline-team"));
        assert!(!prompt.contains(ASTERLINE_ROSTER_PATH));
        assert!(!prompt.contains("@@team_message"));
        assert!(!prompt.contains("@@team_member"));
        assert!(prompt.contains("reviewer"));
        assert!(!prompt.contains("do not message"));
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
    fn ensure_brainstorm_skill_installs_default_and_preserves_custom_copy() {
        let dir =
            std::env::temp_dir().join(format!("asterline-brainstorm-skill-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_brainstorm_skill(&dir).unwrap();

        let path = dir.join(ASTERLINE_BRAINSTORM_SKILL_PATH);
        let installed = std::fs::read_to_string(&path).unwrap();
        assert!(installed.contains("name: asterline-brainstorm"));
        assert!(installed.contains("@@brainstorm_card"));
        assert!(installed.contains("@@brainstorm_vote"));

        let custom = installed.replace(
            "Use relevance, novelty,",
            "Use deployment-specific value, novelty,",
        );
        std::fs::write(&path, &custom).unwrap();
        ensure_brainstorm_skill(&dir).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), custom);
        assert_eq!(brainstorm_skill_text(&dir), custom);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ensure_team_skill_upgrades_managed_v4_file() {
        let dir =
            std::env::temp_dir().join(format!("asterline-skill-upgrade-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(ASTERLINE_TEAM_SKILL_PATH);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            format!(
                "---\nname: asterline-team\nmetadata:\n  version: 4\ndescription: old\n---\n{MANAGED_SKILL_MARKER}\n\n# Old protocol\n@@team_message\n"
            ),
        )
        .unwrap();

        ensure_team_skill(&dir).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("version: 19"));
        assert!(text.contains("@@review"));
        assert!(text.contains("Do not send `@@team_message` merely because teammates are listed"));
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
    fn ensure_team_skill_preserves_valid_custom_skill_without_managed_marker() {
        let dir = std::env::temp_dir().join(format!(
            "asterline-skill-custom-frontmatter-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(ASTERLINE_TEAM_SKILL_PATH);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let custom = "---\nname: asterline-team\nmetadata:\n  version: 1\n---\n# Custom protocol\n";
        std::fs::write(&path, custom).unwrap();

        ensure_team_skill(&dir).unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), custom);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn managed_skills_reject_symlinks_and_never_read_their_targets() {
        use std::os::unix::fs::symlink;

        let dir =
            std::env::temp_dir().join(format!("asterline-skill-symlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let victim = dir.join("outside.txt");
        std::fs::create_dir_all(&dir).unwrap();
        let managed_old = format!("---\nversion: 1\n---\n{MANAGED_SKILL_MARKER}\n");
        std::fs::write(&victim, &managed_old).unwrap();

        let team_path = dir.join(ASTERLINE_TEAM_SKILL_PATH);
        std::fs::create_dir_all(team_path.parent().unwrap()).unwrap();
        symlink(&victim, &team_path).unwrap();
        assert!(ensure_team_skill(&dir).is_err());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), managed_old);

        let brainstorm_path = dir.join(ASTERLINE_BRAINSTORM_SKILL_PATH);
        std::fs::create_dir_all(brainstorm_path.parent().unwrap()).unwrap();
        std::fs::write(&victim, "PRIVATE_LOCAL_CONTENT").unwrap();
        symlink(&victim, &brainstorm_path).unwrap();
        assert!(ensure_brainstorm_skill(&dir).is_err());
        assert!(!brainstorm_skill_text(&dir).contains("PRIVATE_LOCAL_CONTENT"));

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn embedded_team_skill_is_protocol_v19() {
        assert_eq!(skill_version(ASTERLINE_TEAM_SKILL), 19);
        assert!(
            ASTERLINE_TEAM_SKILL
                .lines()
                .any(|line| line.trim() == "version: 19")
        );
        assert!(ASTERLINE_TEAM_SKILL.contains(MANAGED_SKILL_MARKER));
        assert!(ASTERLINE_TEAM_SKILL.contains("@@review"));
        assert!(ASTERLINE_TEAM_SKILL.contains("Brainstorm Generation and Voting"));
        assert!(ASTERLINE_TEAM_SKILL.contains("$asterline-brainstorm"));
        assert!(ASTERLINE_TEAM_SKILL.contains("@@brainstorm_card"));
        assert!(ASTERLINE_TEAM_SKILL.contains("@@brainstorm_vote"));
        assert!(
            ASTERLINE_TEAM_SKILL
                .contains("Do not send `@@team_message` merely because teammates are listed")
        );
        assert!(
            ASTERLINE_TEAM_SKILL.contains("task involves search, research, review, or planning")
        );
        assert!(ASTERLINE_TEAM_SKILL.contains("`session_id`"));
        assert!(ASTERLINE_TEAM_SKILL.contains("`/mode plan`"));
        assert!(ASTERLINE_TEAM_SKILL.contains("@@run_step"));
        assert!(ASTERLINE_TEAM_SKILL.contains("Every Received Message Must Be Answered"));
        assert!(ASTERLINE_TEAM_SKILL.contains(r#""kind":"reply""#));
        assert!(
            ASTERLINE_TEAM_SKILL
                .contains("Writing the plan, review, or patch \"for the user\" is not delivery")
        );
        assert!(ASTERLINE_TEAM_SKILL.contains(ASTERLINE_ROSTER_PATH));
        assert_eq!(ASTERLINE_TEAM_SKILL_VERSION, 19);
        assert!(ASTERLINE_TEAM_SKILL.contains("Waiting For User Approval"));
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
    fn load_team_config_rejects_legacy_roundtable_modes_key() {
        let dir = std::env::temp_dir().join(format!(
            "asterline-cfg-legacy-roundtable-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("team.json");
        std::fs::write(
            &path,
            r#"{
              "name": "legacy",
              "workspace": "/tmp/ws",
              "members": [{
                "id": "builder",
                "display_name": "Builder",
                "backend": "codex",
                "role": "impl"
              }],
              "modes": { "roundtable": {} }
            }"#,
        )
        .unwrap();

        let err = load_team_config(&path).expect_err("legacy roundtable key must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("invalid team config") && msg.contains("roundtable"),
            "startup error should name the file and unknown field: {msg}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
