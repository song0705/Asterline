//! Local skill discovery for the one-shot skill picker.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::domain::config;

const MAX_SKILL_DEPTH: usize = 8;
const MAX_SCAN_ENTRIES: usize = 20_000;
const MAX_SKILLS: usize = 512;
const MAX_SKILL_READ_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

pub fn discover(workspace: &Path) -> Vec<SkillInfo> {
    let user_home = config::user_home_dir();
    let codex_home = config::codex_home_dir();
    let roots = discovery_roots(workspace, user_home.as_deref(), codex_home.as_deref());

    let mut found = Vec::new();
    let mut names = HashSet::new();
    let mut budget = ScanBudget::new(MAX_SCAN_ENTRIES, MAX_SKILLS);
    for root in roots {
        collect_skill_files(&root, 0, &mut budget, &mut |path| {
            let Some(skill) = read_skill(path) else {
                return;
            };
            if names.insert(skill.name.clone()) {
                found.push(skill);
            }
        });
        if budget.exhausted() {
            break;
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

fn discovery_roots(
    workspace: &Path,
    user_home: Option<&Path>,
    codex_home: Option<&Path>,
) -> Vec<PathBuf> {
    let mut roots = vec![
        workspace.join(".agents/skills"),
        workspace.join(".codex/skills"),
        workspace.join(".claude/skills"),
        workspace.join(".grok/skills"),
        workspace.join(".augment/skills"),
    ];
    if let Some(home) = user_home {
        roots.extend([
            home.join(".agents/skills"),
            home.join(".claude/skills"),
            home.join(".claude/plugins/cache"),
            home.join(".grok/skills"),
            home.join(".grok/plugins"),
            home.join(".augment/skills"),
            home.join(".augment/plugins"),
        ]);
    }
    if let Some(home) = codex_home {
        roots.extend([home.join("skills"), home.join("plugins/cache")]);
    }
    roots
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
    visit: &mut impl FnMut(&Path),
) {
    if depth > MAX_SKILL_DEPTH || budget.exhausted() {
        return;
    }
    if std::fs::symlink_metadata(root)
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
        if file_type.is_dir() {
            collect_skill_files(&path, depth + 1, budget, visit);
        } else if file_type.is_file()
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

fn read_skill(path: &Path) -> Option<SkillInfo> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return None;
    }
    let mut content = String::new();
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.custom_flags(libc::O_NOFOLLOW);
    }
    options
        .open(path)
        .ok()?
        .take(MAX_SKILL_READ_BYTES)
        .read_to_string(&mut content)
        .ok()?;
    let name = frontmatter_value(&content, "name").unwrap_or_else(|| {
        path.parent()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unnamed-skill".to_string())
    });
    let description = frontmatter_value(&content, "description").unwrap_or_else(|| {
        content
            .lines()
            .find(|line| !line.trim().is_empty() && !line.starts_with("---"))
            .unwrap_or("No description")
            .trim_start_matches('#')
            .trim()
            .to_string()
    });
    Some(SkillInfo {
        name,
        description,
        path: path.to_path_buf(),
    })
}

fn frontmatter_value(content: &str, key: &str) -> Option<String> {
    let mut lines = content.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        let Some((candidate, value)) = line.split_once(':') else {
            continue;
        };
        if candidate.trim() == key {
            return Some(value.trim().trim_matches(['\'', '"']).to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_workspace_skill_metadata() {
        let root = std::env::temp_dir().join(format!("asterline-skills-{}", std::process::id()));
        let skill_dir = root.join(".agents/skills/review");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: review\ndescription: Review a patch carefully.\n---\n",
        )
        .unwrap();

        let skills = discover(&root);

        assert!(skills.iter().any(|skill| {
            skill.name == "review" && skill.description == "Review a patch carefully."
        }));
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
        collect_skill_files(&root, 0, &mut budget, &mut |path| {
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

        assert!(roots.contains(&PathBuf::from("/custom/codex/skills")));
        assert!(roots.contains(&PathBuf::from("/custom/codex/plugins/cache")));
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
        collect_skill_files(&root, 0, &mut budget, &mut |path| {
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

        let skill = read_skill(&path).expect("frontmatter is within the bounded prefix");

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

        assert_eq!(read_skill(&path).unwrap().name, "portable");
        std::fs::remove_dir_all(root).ok();
    }
}
