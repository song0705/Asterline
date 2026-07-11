//! Local skill discovery for the one-shot skill picker.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

pub fn discover(workspace: &Path) -> Vec<SkillInfo> {
    let mut roots = vec![
        workspace.join(".agents/skills"),
        workspace.join(".codex/skills"),
        workspace.join(".claude/skills"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        roots.extend([
            home.join(".agents/skills"),
            home.join(".codex/skills"),
            home.join(".codex/plugins/cache"),
            home.join(".claude/skills"),
            home.join(".claude/plugins/cache"),
        ]);
    }

    let mut found = Vec::new();
    let mut names = HashSet::new();
    for root in roots {
        collect_skill_files(&root, 0, &mut |path| {
            let Some(skill) = read_skill(path) else {
                return;
            };
            if names.insert(skill.name.clone()) {
                found.push(skill);
            }
        });
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

fn collect_skill_files(root: &Path, depth: usize, visit: &mut impl FnMut(&Path)) {
    if depth > 4 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_skill_files(&path, depth + 1, visit);
        } else if path.file_name().and_then(|name| name.to_str()) == Some("SKILL.md") {
            visit(&path);
        }
    }
}

fn read_skill(path: &Path) -> Option<SkillInfo> {
    let content = std::fs::read_to_string(path).ok()?;
    let fallback = path.parent()?.file_name()?.to_str()?.to_string();
    let name = frontmatter_value(&content, "name").unwrap_or(fallback);
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
}
