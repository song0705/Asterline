//! Local skill discovery for targeted member completion.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::domain::config;
use crate::domain::team::BackendKind;

const MAX_SKILL_DEPTH: usize = 8;
const MAX_SCAN_ENTRIES: usize = 20_000;
const MAX_SKILLS: usize = 512;
const MAX_SKILL_READ_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    /// The native CLI that can invoke this skill.
    pub backend: BackendKind,
    /// Exact text accepted by that backend, including any plugin namespace.
    pub invocation: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DiscoveryRoot {
    path: PathBuf,
    backends: &'static [BackendKind],
    invocation_prefix: Option<String>,
    kind: DiscoveryKind,
    /// Standalone Claude skills can be hidden or made user-invocable through
    /// Claude's `skillOverrides` setting. Plugin skills are intentionally
    /// excluded by Claude itself.
    apply_claude_skill_overrides: bool,
    /// Only direct user skill roots may contain normal symlinked skill
    /// directories. Workspace and plugin roots must never follow them.
    follow_symlinks: bool,
}

/// The on-disk forms that Claude exposes as slash-invocable content.
///
/// Skills are directory-based `SKILL.md` files. Legacy and plugin commands
/// are flat Markdown files whose filename is the command name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DiscoveryKind {
    SkillDirectories,
    CommandFiles,
    SingleSkillFile,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SkillMetadata {
    /// Human-facing metadata from the file, if supplied.
    name: String,
    /// The filename or directory component that the native CLI uses after the
    /// slash (or dollar) prefix.
    invocation_name: String,
    description: String,
    path: PathBuf,
    user_invocable: bool,
}

const CODEX_BACKENDS: &[BackendKind] = &[BackendKind::Codex];
const CLAUDE_BACKENDS: &[BackendKind] = &[BackendKind::Claude];
const GROK_BACKENDS: &[BackendKind] = &[BackendKind::Grok];
const AGY_BACKENDS: &[BackendKind] = &[BackendKind::Agy];
// Workspace `.agents` skills are recognized by Codex and Agy. Claude and
// Grok have their own project-level skill roots.
const WORKSPACE_AGENTS_BACKENDS: &[BackendKind] = &[BackendKind::Codex, BackendKind::Agy];
// Agy exposes workspace `.agents` plus its own global, shared, and builtin
// roots. The latter is listed by the local `skills.json` registry.
const USER_AGENTS_BACKENDS: &[BackendKind] = &[BackendKind::Codex, BackendKind::Grok];

pub fn discover(workspace: &Path) -> Vec<SkillInfo> {
    let user_home = config::user_home_dir();
    let codex_home = config::codex_home_dir();
    discover_with_homes(workspace, user_home.as_deref(), codex_home.as_deref())
}

fn discover_with_homes(
    workspace: &Path,
    user_home: Option<&Path>,
    codex_home: Option<&Path>,
) -> Vec<SkillInfo> {
    let mut roots = discovery_roots(workspace, user_home, codex_home);
    let claude_skill_overrides = claude_skill_overrides(workspace, user_home);
    if let Some(home) = user_home {
        roots.extend(claude_plugin_skill_roots(workspace, home));
    }

    let mut found = Vec::new();
    let mut names = HashSet::new();
    let mut budget = ScanBudget::new(MAX_SCAN_ENTRIES, MAX_SKILLS);
    for root in roots {
        collect_root_files(&root, &mut budget, &mut |path| {
            let Some(skill) = read_root_entry(path, root.kind, root.follow_symlinks) else {
                return;
            };
            if !root.is_user_invocable(&skill, &claude_skill_overrides) {
                return;
            }
            for &backend in root.backends {
                let invocation = root.invocation(backend, &skill.invocation_name);
                if names.insert((invocation.clone(), backend)) {
                    found.push(SkillInfo {
                        name: skill.name.clone(),
                        description: skill.description.clone(),
                        path: skill.path.clone(),
                        backend,
                        invocation,
                    });
                }
            }
        });
        if budget.exhausted() {
            break;
        }
    }
    found.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.backend.as_str().cmp(b.backend.as_str()))
            .then_with(|| a.invocation.cmp(&b.invocation))
    });
    found
}

fn collect_root_files(
    root: &DiscoveryRoot,
    budget: &mut ScanBudget,
    visit: &mut impl FnMut(&Path),
) {
    match root.kind {
        DiscoveryKind::SkillDirectories => {
            collect_skill_files(&root.path, 0, budget, root.follow_symlinks, visit)
        }
        DiscoveryKind::CommandFiles => {
            collect_command_files(&root.path, 0, budget, root.follow_symlinks, visit)
        }
        DiscoveryKind::SingleSkillFile => {
            if budget.skills_remaining == 0
                || !std::fs::metadata(&root.path)
                    .ok()
                    .is_some_and(|metadata| metadata.is_file())
            {
                return;
            }
            budget.skills_remaining -= 1;
            visit(&root.path);
        }
    }
}

fn read_root_entry(
    path: &Path,
    kind: DiscoveryKind,
    follow_symlinks: bool,
) -> Option<SkillMetadata> {
    match kind {
        DiscoveryKind::SkillDirectories => read_skill(path, follow_symlinks),
        DiscoveryKind::SingleSkillFile => {
            let mut skill = read_skill(path, follow_symlinks)?;
            skill.invocation_name = skill.name.clone();
            Some(skill)
        }
        DiscoveryKind::CommandFiles => read_command(path, follow_symlinks),
    }
}

/// Read only the install roots of plugins that Claude has enabled for this
/// workspace. The cache itself can contain disabled and orphaned versions, so
/// it must never be treated as a discovery root on its own.
fn claude_plugin_skill_roots(workspace: &Path, home: &Path) -> Vec<DiscoveryRoot> {
    let enabled = enabled_claude_plugins(workspace, home);
    if enabled.is_empty() {
        return Vec::new();
    }

    let registry = home.join(".claude/plugins/installed_plugins.json");
    let Ok(content) = std::fs::read_to_string(registry) else {
        return Vec::new();
    };
    let Ok(registry) = serde_json::from_str::<Value>(&content) else {
        return Vec::new();
    };
    let Some(plugins) = registry.get("plugins").and_then(Value::as_object) else {
        return Vec::new();
    };

    let mut roots = Vec::new();
    let mut seen = HashSet::new();
    for plugin in enabled {
        let Some(installs) = plugins.get(&plugin).and_then(Value::as_array) else {
            continue;
        };
        for install in installs {
            let Some(path) = install.get("installPath").and_then(Value::as_str) else {
                continue;
            };
            let path = PathBuf::from(path);
            if !path.is_absolute() || !seen.insert(path.clone()) {
                continue;
            }
            let manifest = match claude_plugin_manifest(&path) {
                Ok(manifest) => manifest,
                Err(()) => continue,
            };
            let Some(namespace) = claude_plugin_namespace(manifest.as_ref(), &plugin) else {
                continue;
            };
            // Claude plugins can expose directory skills and flat legacy
            // command files. A root SKILL.md is a fallback only when neither
            // default nor manifest-declared skills exist. Do not scan the rest
            // of the plugin: it can contain dependencies and unrelated files.
            roots.push(plugin_skill_root(path.join("skills"), namespace.clone()));
            roots.push(plugin_command_root(
                path.join("commands"),
                namespace.clone(),
            ));
            let default_skills = path.join("skills");
            let has_default_skills = std::fs::symlink_metadata(default_skills).is_ok();
            let has_custom_skills = manifest
                .as_ref()
                .is_some_and(|manifest| manifest.get("skills").is_some());
            if !has_default_skills && !has_custom_skills {
                roots.push(plugin_single_skill_root(path.join("SKILL.md"), namespace));
            }
        }
    }
    roots
}

fn claude_plugin_manifest(path: &Path) -> Result<Option<Value>, ()> {
    match std::fs::read_to_string(path.join(".claude-plugin/plugin.json")) {
        Ok(content) => serde_json::from_str::<Value>(&content)
            .map(Some)
            .map_err(|_| ()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    }
}

fn claude_plugin_namespace(manifest: Option<&Value>, plugin: &str) -> Option<String> {
    let namespace = match manifest {
        Some(value) => value
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string)?,
        None => plugin.split('@').next().map(str::to_string)?,
    };
    (!namespace.is_empty()
        && namespace
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_')))
    .then_some(namespace)
}

/// Resolve standalone Claude skill visibility using the same user → project →
/// project-local setting order as plugin enablement. Plugin skills are not
/// subject to these overrides.
fn claude_skill_overrides(
    workspace: &Path,
    user_home: Option<&Path>,
) -> std::collections::HashMap<String, String> {
    let mut overrides = std::collections::HashMap::new();
    for path in claude_settings_paths(workspace, user_home) {
        apply_claude_skill_overrides(&mut overrides, &path);
    }
    overrides
}

fn apply_claude_skill_overrides(
    overrides: &mut std::collections::HashMap<String, String>,
    path: &Path,
) {
    let Ok(content) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(settings) = serde_json::from_str::<Value>(&content) else {
        return;
    };
    let Some(values) = settings.get("skillOverrides").and_then(Value::as_object) else {
        return;
    };
    for (skill, value) in values {
        if let Some(value) = value.as_str()
            && matches!(value, "on" | "name-only" | "user-invocable-only" | "off")
        {
            overrides.insert(skill.clone(), value.to_string());
        }
    }
}

/// Resolve `enabledPlugins` using Claude's documented precedence: user,
/// project, then project-local settings.
fn enabled_claude_plugins(workspace: &Path, home: &Path) -> Vec<String> {
    let mut enabled = std::collections::HashMap::new();
    for path in claude_settings_paths(workspace, Some(home)) {
        apply_claude_plugin_settings(&mut enabled, &path);
    }

    let mut enabled = enabled
        .into_iter()
        .filter_map(|(plugin, is_enabled)| is_enabled.then_some(plugin))
        .collect::<Vec<_>>();
    enabled.sort();
    enabled
}

/// Claude project settings belong to the repository root, not arbitrary
/// parent directories. The launch-directory local file remains a small
/// compatibility exception for older Claude releases.
fn claude_settings_paths(workspace: &Path, user_home: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = user_home {
        paths.push(home.join(".claude/settings.json"));
    }
    let project_root = claude_project_root(workspace);
    paths.push(project_root.join(".claude/settings.json"));
    paths.push(project_root.join(".claude/settings.local.json"));
    if project_root != workspace {
        paths.push(workspace.join(".claude/settings.local.json"));
    }
    paths
}

/// Avoid running `git` synchronously while the TUI is starting. A repository
/// marker is enough for the only purpose here: locating Claude's project
/// settings root.
fn claude_project_root(workspace: &Path) -> PathBuf {
    workspace
        .ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
        .and_then(|root| std::fs::canonicalize(root).ok())
        .unwrap_or_else(|| workspace.to_path_buf())
}

fn apply_claude_plugin_settings(
    enabled: &mut std::collections::HashMap<String, bool>,
    path: &Path,
) {
    let Ok(content) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(settings) = serde_json::from_str::<Value>(&content) else {
        return;
    };
    let Some(plugins) = settings.get("enabledPlugins").and_then(Value::as_object) else {
        return;
    };
    for (plugin, value) in plugins {
        if let Some(is_enabled) = value.as_bool() {
            enabled.insert(plugin.clone(), is_enabled);
        }
    }
}

fn discovery_roots(
    workspace: &Path,
    user_home: Option<&Path>,
    codex_home: Option<&Path>,
) -> Vec<DiscoveryRoot> {
    let mut roots = vec![
        skill_root(workspace.join(".codex/skills"), CODEX_BACKENDS),
        skill_root(workspace.join(".grok/skills"), GROK_BACKENDS),
        // Project skills must win over user-level skills of the same name.
        skill_root(workspace.join(".agents/skills"), WORKSPACE_AGENTS_BACKENDS),
    ];
    if let Some(home) = user_home {
        roots.extend([
            // Claude itself gives personal skills precedence over project
            // skills, so keep its two user roots ahead of their workspace
            // counterparts. Other backend-specific roots retain their own
            // ordering below.
            user_claude_skill_root(home.join(".claude/skills")),
            user_claude_command_root(home.join(".claude/commands")),
        ]);
    }
    roots.extend([
        claude_skill_root(workspace.join(".claude/skills")),
        claude_command_root(workspace.join(".claude/commands")),
    ]);
    if let Some(home) = user_home {
        roots.extend([
            user_skill_root(home.join(".grok/skills"), GROK_BACKENDS),
            user_skill_root(home.join(".grok/bundled/skills"), GROK_BACKENDS),
            user_skill_root(home.join(".gemini/antigravity-cli/skills"), AGY_BACKENDS),
            user_skill_root(home.join(".gemini/skills"), AGY_BACKENDS),
            user_skill_root(
                home.join(".gemini/antigravity-cli/builtin/skills"),
                AGY_BACKENDS,
            ),
        ]);
    }
    if let Some(home) = codex_home {
        roots.push(user_skill_root(home.join("skills"), CODEX_BACKENDS));
    }
    if let Some(home) = user_home {
        roots.push(user_skill_root(
            home.join(".agents/skills"),
            USER_AGENTS_BACKENDS,
        ));
    }
    roots
}

fn skill_root(path: PathBuf, backends: &'static [BackendKind]) -> DiscoveryRoot {
    DiscoveryRoot {
        path,
        backends,
        invocation_prefix: None,
        kind: DiscoveryKind::SkillDirectories,
        apply_claude_skill_overrides: false,
        follow_symlinks: false,
    }
}

fn user_skill_root(path: PathBuf, backends: &'static [BackendKind]) -> DiscoveryRoot {
    DiscoveryRoot {
        path,
        backends,
        invocation_prefix: None,
        kind: DiscoveryKind::SkillDirectories,
        apply_claude_skill_overrides: false,
        follow_symlinks: true,
    }
}

fn claude_skill_root(path: PathBuf) -> DiscoveryRoot {
    let mut root = skill_root(path, CLAUDE_BACKENDS);
    root.apply_claude_skill_overrides = true;
    root
}

fn user_claude_skill_root(path: PathBuf) -> DiscoveryRoot {
    let mut root = user_skill_root(path, CLAUDE_BACKENDS);
    root.apply_claude_skill_overrides = true;
    root
}

fn command_root(path: PathBuf, backends: &'static [BackendKind]) -> DiscoveryRoot {
    DiscoveryRoot {
        path,
        backends,
        invocation_prefix: None,
        kind: DiscoveryKind::CommandFiles,
        apply_claude_skill_overrides: false,
        follow_symlinks: false,
    }
}

fn user_command_root(path: PathBuf, backends: &'static [BackendKind]) -> DiscoveryRoot {
    DiscoveryRoot {
        path,
        backends,
        invocation_prefix: None,
        kind: DiscoveryKind::CommandFiles,
        apply_claude_skill_overrides: false,
        follow_symlinks: true,
    }
}

fn claude_command_root(path: PathBuf) -> DiscoveryRoot {
    let mut root = command_root(path, CLAUDE_BACKENDS);
    root.apply_claude_skill_overrides = true;
    root
}

fn user_claude_command_root(path: PathBuf) -> DiscoveryRoot {
    let mut root = user_command_root(path, CLAUDE_BACKENDS);
    root.apply_claude_skill_overrides = true;
    root
}

fn plugin_skill_root(path: PathBuf, namespace: String) -> DiscoveryRoot {
    DiscoveryRoot {
        path,
        backends: CLAUDE_BACKENDS,
        invocation_prefix: Some(format!("/{namespace}:")),
        kind: DiscoveryKind::SkillDirectories,
        apply_claude_skill_overrides: false,
        follow_symlinks: false,
    }
}

fn plugin_command_root(path: PathBuf, namespace: String) -> DiscoveryRoot {
    DiscoveryRoot {
        path,
        backends: CLAUDE_BACKENDS,
        invocation_prefix: Some(format!("/{namespace}:")),
        kind: DiscoveryKind::CommandFiles,
        apply_claude_skill_overrides: false,
        follow_symlinks: false,
    }
}

fn plugin_single_skill_root(path: PathBuf, namespace: String) -> DiscoveryRoot {
    DiscoveryRoot {
        path,
        backends: CLAUDE_BACKENDS,
        invocation_prefix: Some(format!("/{namespace}:")),
        kind: DiscoveryKind::SingleSkillFile,
        apply_claude_skill_overrides: false,
        follow_symlinks: false,
    }
}

impl DiscoveryRoot {
    fn invocation(&self, backend: BackendKind, skill: &str) -> String {
        if let Some(prefix) = &self.invocation_prefix {
            return format!("{prefix}{skill}");
        }
        match backend {
            BackendKind::Codex => format!("${skill}"),
            BackendKind::Claude | BackendKind::Grok | BackendKind::Agy => format!("/{skill}"),
        }
    }

    fn is_user_invocable(
        &self,
        skill: &SkillMetadata,
        claude_skill_overrides: &std::collections::HashMap<String, String>,
    ) -> bool {
        if !self.apply_claude_skill_overrides {
            return skill.user_invocable;
        }
        match claude_skill_overrides
            .get(&skill.invocation_name)
            .map(String::as_str)
        {
            Some("off") => false,
            Some("on" | "name-only" | "user-invocable-only") => true,
            _ => skill.user_invocable,
        }
    }
}

struct ScanBudget {
    entries_remaining: usize,
    skills_remaining: usize,
}

impl ScanBudget {
    fn new(entries: usize, skills: usize) -> Self {
        Self {
            entries_remaining: entries,
            skills_remaining: skills,
        }
    }

    fn exhausted(&self) -> bool {
        self.entries_remaining == 0 || self.skills_remaining == 0
    }
}

fn collect_skill_files(
    root: &Path,
    depth: usize,
    budget: &mut ScanBudget,
    follow_symlinks: bool,
    visit: &mut impl FnMut(&Path),
) {
    if depth > MAX_SKILL_DEPTH || budget.exhausted() {
        return;
    }
    if !follow_symlinks
        && std::fs::symlink_metadata(root)
            .ok()
            .is_some_and(|metadata| metadata.file_type().is_symlink())
    {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if budget.entries_remaining == 0 {
            break;
        }
        budget.entries_remaining -= 1;
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let is_symlink = file_type.is_symlink();
        if is_symlink && !follow_symlinks {
            continue;
        }
        let metadata = std::fs::metadata(&path).ok();
        let Some(metadata) = metadata else {
            continue;
        };
        if metadata.is_dir() {
            collect_skill_files(&path, depth + 1, budget, follow_symlinks, visit);
        } else if metadata.is_file()
            && path.file_name() == Some(OsStr::new("SKILL.md"))
            && budget.skills_remaining > 0
        {
            budget.skills_remaining -= 1;
            visit(&path);
        }
        if budget.exhausted() {
            break;
        }
    }
}

fn collect_command_files(
    root: &Path,
    depth: usize,
    budget: &mut ScanBudget,
    follow_symlinks: bool,
    visit: &mut impl FnMut(&Path),
) {
    if depth > MAX_SKILL_DEPTH || budget.exhausted() {
        return;
    }
    if !follow_symlinks
        && std::fs::symlink_metadata(root)
            .ok()
            .is_some_and(|metadata| metadata.file_type().is_symlink())
    {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if budget.entries_remaining == 0 {
            break;
        }
        budget.entries_remaining -= 1;
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() && !follow_symlinks {
            continue;
        }
        let Some(metadata) = std::fs::metadata(&path).ok() else {
            continue;
        };
        if metadata.is_dir() {
            collect_command_files(&path, depth + 1, budget, follow_symlinks, visit);
        } else if metadata.is_file()
            && path.extension() == Some(OsStr::new("md"))
            && budget.skills_remaining > 0
        {
            budget.skills_remaining -= 1;
            visit(&path);
        }
        if budget.exhausted() {
            break;
        }
    }
}

fn read_skill(path: &Path, follow_symlinks: bool) -> Option<SkillMetadata> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    let is_file =
        metadata.is_file() || (follow_symlinks && std::fs::metadata(path).ok()?.is_file());
    if (metadata.file_type().is_symlink() && !follow_symlinks) || !is_file {
        return None;
    }
    let mut content = String::new();
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        if !follow_symlinks {
            options.custom_flags(libc::O_NOFOLLOW);
        }
    }
    options
        .open(path)
        .ok()?
        .take(MAX_SKILL_READ_BYTES)
        .read_to_string(&mut content)
        .ok()?;
    let invocation_name = path
        .parent()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unnamed-skill".to_string());
    let name = frontmatter_value(&content, "name").unwrap_or_else(|| invocation_name.clone());
    let description = frontmatter_value(&content, "description").unwrap_or_else(|| {
        content
            .lines()
            .find(|line| !line.trim().is_empty() && !line.starts_with("---"))
            .unwrap_or("No description")
            .trim_start_matches('#')
            .trim()
            .to_string()
    });
    let user_invocable = frontmatter_bool(&content, "user-invocable").unwrap_or(true);
    Some(SkillMetadata {
        name,
        invocation_name,
        description,
        path: path.to_path_buf(),
        user_invocable,
    })
}

fn read_command(path: &Path, follow_symlinks: bool) -> Option<SkillMetadata> {
    let mut command = read_skill(path, follow_symlinks)?;
    let name = path.file_stem()?.to_string_lossy().into_owned();
    command.name = name.clone();
    command.invocation_name = name;
    Some(command)
}

fn frontmatter_value(content: &str, key: &str) -> Option<String> {
    let mut lines = content.lines().peekable();
    if lines.next()?.trim() != "---" {
        return None;
    }
    while let Some(line) = lines.next() {
        if line.trim() == "---" {
            break;
        }
        let Some((candidate, value)) = line.split_once(':') else {
            continue;
        };
        if candidate.trim() == key {
            let value = value.trim();
            if matches!(value, ">" | ">-" | ">+" | "|" | "|-" | "|+") {
                let indentation = line.len() - line.trim_start().len();
                let mut block = Vec::new();
                while let Some(next) = lines.peek() {
                    if next.trim() == "---" {
                        break;
                    }
                    if !next.trim().is_empty()
                        && next.len() - next.trim_start().len() <= indentation
                    {
                        break;
                    }
                    let next = lines.next().expect("peeked frontmatter line");
                    block.push(next.trim());
                }
                let value = block.join(" ");
                return (!value.is_empty()).then_some(value);
            }
            return Some(value.trim_matches(['\'', '"']).to_string());
        }
    }
    None
}

fn frontmatter_bool(content: &str, key: &str) -> Option<bool> {
    frontmatter_value(content, key)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_workspace_skill_metadata() {
        let root = std::env::temp_dir().join(format!("asterline-skills-{}", std::process::id()));
        let skill_dir = root.join(".agents/skills/asterline-test-workspace-review");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: asterline-test-workspace-review\ndescription: Review a patch carefully.\n---\n",
        )
        .unwrap();

        let skills = discover(&root);

        assert!(skills.iter().any(|skill| {
            skill.name == "asterline-test-workspace-review"
                && skill.description == "Review a patch carefully."
                && skill.backend == BackendKind::Codex
        }));
        assert!(skills.iter().any(|skill| {
            skill.name == "asterline-test-workspace-review" && skill.backend == BackendKind::Agy
        }));
        assert!(!skills.iter().any(|skill| {
            skill.name == "asterline-test-workspace-review" && skill.backend == BackendKind::Grok
        }));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn folded_frontmatter_description_is_rendered_as_text() {
        let content = "---\nname: resume-cursor\ndescription: >\n  Resume or continue work from a recent Cursor session.\n  Accept a session name or native ID.\nmetadata:\n  short-description: Continue from Cursor\n---\n";

        assert_eq!(
            frontmatter_value(content, "description"),
            Some(
                "Resume or continue work from a recent Cursor session. Accept a session name or native ID."
                    .to_string()
            )
        );
    }

    #[test]
    fn scopes_backend_specific_skill_roots() {
        let root =
            std::env::temp_dir().join(format!("asterline-scoped-skills-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        for (directory, name) in [
            (".agents/skills/shared", "asterline-test-shared"),
            (".claude/skills/wake", "asterline-test-wake"),
            (".codex/skills/review", "asterline-test-review"),
        ] {
            let skill_dir = root.join(directory);
            std::fs::create_dir_all(&skill_dir).unwrap();
            std::fs::write(
                skill_dir.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: test\n---\n"),
            )
            .unwrap();
        }

        let skills = discover(&root);
        let backends_for = |name: &str| {
            skills
                .iter()
                .filter(|skill| skill.name == name)
                .map(|skill| skill.backend)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            backends_for("asterline-test-shared"),
            vec![BackendKind::Agy, BackendKind::Codex]
        );
        assert_eq!(
            backends_for("asterline-test-wake"),
            vec![BackendKind::Claude]
        );
        assert_eq!(
            backends_for("asterline-test-review"),
            vec![BackendKind::Codex]
        );

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn discovers_only_enabled_claude_plugin_skills_with_their_namespace() {
        let root = std::env::temp_dir().join(format!(
            "asterline-claude-plugin-skills-{}",
            std::process::id()
        ));
        let workspace = root.join("workspace");
        let home = root.join("home");
        let plugin = home.join(".claude/plugins/cache/example/fancy/1.0.0");
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(plugin.join("skills/review")).unwrap();
        std::fs::create_dir_all(plugin.join("commands")).unwrap();
        std::fs::create_dir_all(plugin.join(".claude-plugin")).unwrap();
        std::fs::create_dir_all(home.join(".claude/plugins")).unwrap();
        std::fs::write(
            plugin.join("skills/review/SKILL.md"),
            "---\nname: displayed-review\ndescription: Review with the enabled plugin.\n---\n",
        )
        .unwrap();
        std::fs::write(
            plugin.join("commands/diagnose.md"),
            "---\ndescription: Diagnose with the enabled plugin.\n---\n",
        )
        .unwrap();
        // Claude only treats this as a skill when the plugin has no skills
        // directory (and no manifest-declared skills).
        std::fs::write(
            plugin.join("SKILL.md"),
            "---\nname: quick-check\ndescription: Must not appear here.\n---\n",
        )
        .unwrap();
        std::fs::write(
            plugin.join(".claude-plugin/plugin.json"),
            r#"{"name":"fancy-tools"}"#,
        )
        .unwrap();
        std::fs::write(
            home.join(".claude/settings.json"),
            r#"{"enabledPlugins":{"fancy@marketplace":true,"stale@marketplace":false}}"#,
        )
        .unwrap();
        std::fs::write(
            home.join(".claude/plugins/installed_plugins.json"),
            serde_json::json!({
                "plugins": {
                    "fancy@marketplace": [{"installPath": plugin}],
                    "stale@marketplace": [{"installPath": root.join("stale-plugin")}]
                }
            })
            .to_string(),
        )
        .unwrap();

        let skills = discover_with_homes(&workspace, Some(&home), None);

        assert_eq!(
            skills
                .iter()
                .filter(|skill| skill.backend == BackendKind::Claude)
                .map(|skill| (skill.name.as_str(), skill.invocation.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("diagnose", "/fancy-tools:diagnose"),
                ("displayed-review", "/fancy-tools:review"),
            ]
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn discovers_a_root_plugin_skill_only_without_other_skill_locations() {
        let root = std::env::temp_dir().join(format!(
            "asterline-claude-root-plugin-skill-{}",
            std::process::id()
        ));
        let workspace = root.join("workspace");
        let home = root.join("home");
        let plugin = home.join(".claude/plugins/cache/example/root/1.0.0");
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(plugin.join(".claude-plugin")).unwrap();
        std::fs::create_dir_all(home.join(".claude/plugins")).unwrap();
        std::fs::write(
            plugin.join("SKILL.md"),
            "---\nname: quick-check\ndescription: Check from the plugin root.\n---\n",
        )
        .unwrap();
        std::fs::write(
            plugin.join(".claude-plugin/plugin.json"),
            r#"{"name":"root-tools"}"#,
        )
        .unwrap();
        std::fs::write(
            home.join(".claude/settings.json"),
            r#"{"enabledPlugins":{"root@marketplace":true}}"#,
        )
        .unwrap();
        std::fs::write(
            home.join(".claude/plugins/installed_plugins.json"),
            serde_json::json!({
                "plugins": {"root@marketplace": [{"installPath": plugin}]}
            })
            .to_string(),
        )
        .unwrap();

        assert!(
            discover_with_homes(&workspace, Some(&home), None)
                .iter()
                .any(|skill| {
                    skill.name == "quick-check" && skill.invocation == "/root-tools:quick-check"
                })
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn ignores_enabled_plugin_with_an_invalid_manifest() {
        let root = std::env::temp_dir().join(format!(
            "asterline-invalid-claude-plugin-manifest-{}",
            std::process::id()
        ));
        let workspace = root.join("workspace");
        let home = root.join("home");
        let plugin = home.join(".claude/plugins/cache/example/broken/1.0.0");
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(plugin.join("skills/review")).unwrap();
        std::fs::create_dir_all(plugin.join(".claude-plugin")).unwrap();
        std::fs::create_dir_all(home.join(".claude/plugins")).unwrap();
        std::fs::write(plugin.join("skills/review/SKILL.md"), "# Do not list\n").unwrap();
        std::fs::write(plugin.join(".claude-plugin/plugin.json"), "not json").unwrap();
        std::fs::write(
            home.join(".claude/settings.json"),
            r#"{"enabledPlugins":{"broken@marketplace":true}}"#,
        )
        .unwrap();
        std::fs::write(
            home.join(".claude/plugins/installed_plugins.json"),
            serde_json::json!({
                "plugins": {"broken@marketplace": [{"installPath": plugin}]}
            })
            .to_string(),
        )
        .unwrap();

        assert!(discover_with_homes(&workspace, Some(&home), None).is_empty());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn discovers_claude_legacy_commands_with_their_filename_invocation() {
        let root = std::env::temp_dir().join(format!(
            "asterline-claude-legacy-command-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&root).ok();
        let command = root.join(".claude/commands/release-note.md");
        std::fs::create_dir_all(command.parent().unwrap()).unwrap();
        std::fs::write(
            &command,
            "---\nname: ignored-by-claude\ndescription: Draft release notes.\n---\n",
        )
        .unwrap();

        assert!(discover_with_homes(&root, None, None).iter().any(|skill| {
            skill.name == "release-note"
                && skill.invocation == "/release-note"
                && skill.backend == BackendKind::Claude
        }));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn personal_claude_skill_wins_over_the_same_project_invocation() {
        let root = std::env::temp_dir().join(format!(
            "asterline-claude-personal-precedence-{}",
            std::process::id()
        ));
        let workspace = root.join("workspace");
        let home = root.join("home");
        std::fs::remove_dir_all(&root).ok();
        for (path, description) in [
            (
                workspace.join(".claude/skills/inspect/SKILL.md"),
                "project copy",
            ),
            (
                home.join(".claude/skills/inspect/SKILL.md"),
                "personal copy",
            ),
        ] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                path,
                format!("---\nname: inspect\ndescription: {description}\n---\n"),
            )
            .unwrap();
        }

        let matches = discover_with_homes(&workspace, Some(&home), None)
            .into_iter()
            .filter(|skill| skill.backend == BackendKind::Claude && skill.invocation == "/inspect")
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].description, "personal copy");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn claude_skill_overrides_control_standalone_menu_visibility() {
        let root = std::env::temp_dir().join(format!(
            "asterline-claude-skill-overrides-{}",
            std::process::id()
        ));
        let workspace = root.join("workspace");
        let home = root.join("home");
        std::fs::remove_dir_all(&root).ok();
        for (name, user_invocable) in [("hidden", true), ("forced", false)] {
            let skill = workspace.join(format!(".claude/skills/{name}/SKILL.md"));
            std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
            std::fs::write(
                skill,
                format!(
                    "---\nname: {name}\nuser-invocable: {user_invocable}\ndescription: test\n---\n"
                ),
            )
            .unwrap();
        }
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::create_dir_all(workspace.join(".claude")).unwrap();
        std::fs::write(
            home.join(".claude/settings.json"),
            r#"{"skillOverrides":{"hidden":"off"}}"#,
        )
        .unwrap();
        std::fs::write(
            workspace.join(".claude/settings.local.json"),
            r#"{"skillOverrides":{"forced":"on"}}"#,
        )
        .unwrap();

        let skills = discover_with_homes(&workspace, Some(&home), None);
        assert!(!skills.iter().any(|skill| skill.invocation == "/hidden"));
        assert!(skills.iter().any(|skill| skill.invocation == "/forced"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn claude_settings_ignore_unrelated_parent_directories() {
        let root = std::env::temp_dir().join(format!(
            "asterline-claude-settings-boundary-{}",
            std::process::id()
        ));
        let workspace = root.join("parent/workspace");
        let home = root.join("home");
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(root.join("parent/.claude")).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            root.join("parent/.claude/settings.json"),
            r#"{"enabledPlugins":{"wrong@marketplace":true},"skillOverrides":{"wrong":"off"}}"#,
        )
        .unwrap();

        assert!(enabled_claude_plugins(&workspace, &home).is_empty());
        assert!(claude_skill_overrides(&workspace, Some(&home)).is_empty());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn claude_settings_use_the_git_project_root_from_a_nested_workspace() {
        let root = std::env::temp_dir().join(format!(
            "asterline-claude-git-settings-root-{}",
            std::process::id()
        ));
        let project = root.join("project");
        let workspace = project.join("nested/workspace");
        let home = root.join("home");
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(project.join(".git")).unwrap();
        std::fs::create_dir_all(project.join(".claude")).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            project.join(".claude/settings.json"),
            r#"{"enabledPlugins":{"right@marketplace":true},"skillOverrides":{"visible":"on"}}"#,
        )
        .unwrap();

        assert_eq!(
            enabled_claude_plugins(&workspace, &home),
            vec!["right@marketplace".to_string()]
        );
        assert_eq!(
            claude_skill_overrides(&workspace, Some(&home)).get("visible"),
            Some(&"on".to_string())
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn local_claude_plugin_setting_can_disable_a_user_plugin() {
        let root = std::env::temp_dir().join(format!(
            "asterline-disabled-claude-plugin-{}",
            std::process::id()
        ));
        let workspace = root.join("workspace");
        let home = root.join("home");
        let plugin = home.join(".claude/plugins/cache/example/fancy/1.0.0");
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(plugin.join("skills/review")).unwrap();
        std::fs::create_dir_all(workspace.join(".claude")).unwrap();
        std::fs::create_dir_all(home.join(".claude/plugins")).unwrap();
        std::fs::write(
            plugin.join("skills/review/SKILL.md"),
            "---\nname: review\n---\n",
        )
        .unwrap();
        std::fs::write(
            home.join(".claude/settings.json"),
            r#"{"enabledPlugins":{"fancy@marketplace":true}}"#,
        )
        .unwrap();
        std::fs::write(
            workspace.join(".claude/settings.local.json"),
            r#"{"enabledPlugins":{"fancy@marketplace":false}}"#,
        )
        .unwrap();
        std::fs::write(
            home.join(".claude/plugins/installed_plugins.json"),
            serde_json::json!({
                "plugins": {"fancy@marketplace": [{"installPath": plugin}]}
            })
            .to_string(),
        )
        .unwrap();

        assert!(
            discover_with_homes(&workspace, Some(&home), None)
                .iter()
                .all(|skill| skill.backend != BackendKind::Claude)
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn hides_skills_marked_not_user_invocable() {
        let root =
            std::env::temp_dir().join(format!("asterline-hidden-skill-{}", std::process::id()));
        let skill = root.join(".claude/skills/background/SKILL.md");
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
        std::fs::write(
            &skill,
            "---\nname: background\nuser-invocable: false\n---\n",
        )
        .unwrap();

        assert!(discover_with_homes(&root, None, None).is_empty());
        std::fs::remove_dir_all(root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn user_skill_roots_follow_links_but_workspace_roots_do_not() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "asterline-symlinked-user-skill-{}",
            std::process::id()
        ));
        let workspace = root.join("workspace");
        let home = root.join("home");
        let target = root.join("target-skill");
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(home.join(".claude/skills")).unwrap();
        std::fs::create_dir_all(workspace.join(".claude/skills")).unwrap();
        std::fs::write(
            target.join("SKILL.md"),
            "---\nname: linked-user-skill\ndescription: A user link.\n---\n",
        )
        .unwrap();
        symlink(&target, home.join(".claude/skills/link")).unwrap();
        symlink(&target, workspace.join(".claude/skills/link")).unwrap();

        assert!(
            discover_with_homes(&workspace, Some(&home), None)
                .iter()
                .any(|skill| skill.name == "linked-user-skill"
                    && skill.backend == BackendKind::Claude)
        );
        assert!(discover_with_homes(&workspace, None, None).is_empty());
        std::fs::remove_dir_all(root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn enabled_plugin_skills_do_not_follow_links_outside_the_install() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "asterline-plugin-symlink-boundary-{}",
            std::process::id()
        ));
        let workspace = root.join("workspace");
        let home = root.join("home");
        let plugin = home.join(".claude/plugins/cache/example/safe/1.0.0");
        let outside = root.join("outside");
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(plugin.join("skills")).unwrap();
        std::fs::create_dir_all(plugin.join(".claude-plugin")).unwrap();
        std::fs::create_dir_all(home.join(".claude/plugins")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(
            outside.join("SKILL.md"),
            "---\nname: escaped\ndescription: must not load\n---\n",
        )
        .unwrap();
        symlink(&outside, plugin.join("skills/escaped")).unwrap();
        std::fs::write(
            plugin.join(".claude-plugin/plugin.json"),
            r#"{"name":"safe"}"#,
        )
        .unwrap();
        std::fs::write(
            home.join(".claude/settings.json"),
            r#"{"enabledPlugins":{"safe@marketplace":true}}"#,
        )
        .unwrap();
        std::fs::write(
            home.join(".claude/plugins/installed_plugins.json"),
            serde_json::json!({
                "plugins": {"safe@marketplace": [{"installPath": plugin}]}
            })
            .to_string(),
        )
        .unwrap();

        assert!(
            !discover_with_homes(&workspace, Some(&home), None)
                .iter()
                .any(|skill| skill.name == "escaped")
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn discovers_skill_at_plugin_cache_depth() {
        let root =
            std::env::temp_dir().join(format!("asterline-plugin-skills-{}", std::process::id()));
        let skill_dir = root.join("vendor/plugin/version/skills/review");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&skill_dir).unwrap();
        let skill_file = skill_dir.join("SKILL.md");
        std::fs::write(&skill_file, "---\nname: plugin-review\n---\n").unwrap();

        let mut found = Vec::new();
        let mut budget = ScanBudget::new(MAX_SCAN_ENTRIES, MAX_SKILLS);
        collect_skill_files(&root, 0, &mut budget, false, &mut |path| {
            found.push(path.to_path_buf())
        });

        assert_eq!(found, vec![skill_file]);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn custom_codex_home_roots_are_discovered_without_user_profile() {
        let roots = discovery_roots(
            Path::new("/workspace"),
            None,
            Some(Path::new("/custom/codex")),
        );

        assert!(roots.iter().any(|root| {
            root.path == Path::new("/custom/codex/skills") && root.backends == CODEX_BACKENDS
        }));
    }

    #[test]
    fn roots_keep_backends_separate_and_skip_unowned_caches() {
        let roots = discovery_roots(
            Path::new("/workspace"),
            Some(Path::new("/home/tester")),
            Some(Path::new("/home/tester/.codex")),
        );

        assert!(roots.iter().any(|root| {
            root.path == Path::new("/workspace/.agents/skills")
                && root.backends == WORKSPACE_AGENTS_BACKENDS
        }));
        for path in [
            "/home/tester/.gemini/antigravity-cli/skills",
            "/home/tester/.gemini/skills",
            "/home/tester/.gemini/antigravity-cli/builtin/skills",
        ] {
            assert!(
                roots
                    .iter()
                    .any(|root| { root.path == Path::new(path) && root.backends == AGY_BACKENDS })
            );
        }
        assert!(!roots.iter().any(|root| {
            root.path.to_string_lossy().contains(".augment")
                || root.path.to_string_lossy().contains("plugins/cache")
                || root.path.to_string_lossy().ends_with(".grok/plugins")
        }));
    }

    #[test]
    fn discovers_all_documented_agy_skill_roots() {
        let root =
            std::env::temp_dir().join(format!("asterline-agy-skill-roots-{}", std::process::id()));
        let workspace = root.join("workspace");
        let home = root.join("home");
        let locations = [
            (
                workspace.join(".agents/skills/workspace-skill"),
                "workspace-skill",
            ),
            (
                home.join(".gemini/antigravity-cli/skills/global-skill"),
                "global-skill",
            ),
            (home.join(".gemini/skills/shared-skill"), "shared-skill"),
            (
                home.join(".gemini/antigravity-cli/builtin/skills/builtin-skill"),
                "builtin-skill",
            ),
        ];
        std::fs::remove_dir_all(&root).ok();
        for (directory, name) in locations {
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(
                directory.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: Agy {name}.\n---\n"),
            )
            .unwrap();
        }

        let found = discover_with_homes(&workspace, Some(&home), None);
        for name in [
            "workspace-skill",
            "global-skill",
            "shared-skill",
            "builtin-skill",
        ] {
            assert!(found.iter().any(|skill| {
                skill.backend == BackendKind::Agy && skill.invocation == format!("/{name}")
            }));
        }
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn workspace_skill_roots_precede_global_skill_roots() {
        let roots = discovery_roots(
            Path::new("/workspace"),
            Some(Path::new("/home/tester")),
            Some(Path::new("/home/tester/.codex")),
        );
        let workspace_agents = roots
            .iter()
            .position(|root| root.path == Path::new("/workspace/.agents/skills"))
            .expect("workspace shared root");
        let agy_global = roots
            .iter()
            .position(|root| root.path == Path::new("/home/tester/.gemini/antigravity-cli/skills"))
            .expect("Agy global root");
        let codex_global = roots
            .iter()
            .position(|root| root.path == Path::new("/home/tester/.codex/skills"))
            .expect("Codex global root");

        assert!(workspace_agents < agy_global);
        assert!(workspace_agents < codex_global);
    }

    #[test]
    fn scan_budget_bounds_a_wide_plugin_tree() {
        let root = std::env::temp_dir().join(format!(
            "asterline-wide-plugin-skills-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();
        for index in 0..12 {
            let dir = root.join(format!("skill-{index}"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("SKILL.md"), format!("# Skill {index}")).unwrap();
        }

        let mut found = Vec::new();
        let mut budget = ScanBudget::new(5, 2);
        collect_skill_files(&root, 0, &mut budget, false, &mut |path| {
            found.push(path.to_path_buf())
        });

        assert!(found.len() <= 2);
        assert!(budget.exhausted());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn oversized_skill_reads_only_the_bounded_metadata_prefix() {
        let root =
            std::env::temp_dir().join(format!("asterline-large-skill-{}", std::process::id()));
        let dir = root.join("large");
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let mut content = "---\nname: bounded\ndescription: Small metadata.\n---\n".to_string();
        content.push_str(&"x".repeat(MAX_SKILL_READ_BYTES as usize * 2));
        let path = dir.join("SKILL.md");
        std::fs::write(&path, content).unwrap();

        let skill = read_skill(&path, false).expect("frontmatter is within the bounded prefix");

        assert_eq!(skill.name, "bounded");
        assert_eq!(skill.description, "Small metadata.");
        std::fs::remove_dir_all(root).ok();
    }

    // macOS sandboxed temp directories can reject non-UTF-8 path creation;
    // Linux CI exercises the Unix filename behavior directly.
    #[cfg(target_os = "linux")]
    #[test]
    fn frontmatter_skill_under_non_utf8_parent_is_not_skipped() {
        use std::os::unix::ffi::OsStringExt;

        let root =
            std::env::temp_dir().join(format!("asterline-nonutf-skill-{}", std::process::id()));
        let dir = root.join(std::ffi::OsString::from_vec(vec![b's', 0xff, b'k']));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("SKILL.md");
        std::fs::write(&path, "---\nname: portable\ndescription: Works.\n---\n").unwrap();

        assert_eq!(read_skill(&path, false).unwrap().name, "portable");
        std::fs::remove_dir_all(root).ok();
    }
}
