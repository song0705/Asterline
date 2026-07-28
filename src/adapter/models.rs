//! Backend model catalog discovery.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

use crate::domain::team::{BackendKind, Effort};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredModel {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub default_effort: Option<Effort>,
    pub supported_efforts: Vec<Effort>,
    pub is_default: bool,
}

impl DiscoveredModel {
    pub fn simple(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            name: id.clone(),
            id,
            description: None,
            default_effort: None,
            supported_efforts: Vec::new(),
            is_default: false,
        }
    }
}

pub fn discover_models(backend: BackendKind, cwd: &Path) -> Result<Vec<DiscoveredModel>, String> {
    match backend {
        BackendKind::Codex => discover_codex_models(cwd),
        BackendKind::Claude => Ok(discover_claude_models(cwd)),
        BackendKind::Grok => discover_grok_models(cwd),
        BackendKind::Agy => discover_agy_models(cwd),
    }
}

fn discover_codex_models(cwd: &Path) -> Result<Vec<DiscoveredModel>, String> {
    let output = run("codex", &["debug", "models"], cwd)?;
    parse_codex_models(&output.stdout)
}

fn parse_codex_models(output: &[u8]) -> Result<Vec<DiscoveredModel>, String> {
    let value: Value = serde_json::from_slice(output)
        .map_err(|err| format!("invalid `codex debug models` JSON: {err}"))?;
    let models = value
        .get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|model| model.get("visibility").and_then(Value::as_str) == Some("list"))
        .filter_map(|model| {
            let id = model.get("slug").and_then(Value::as_str)?.to_string();
            let name = model
                .get("display_name")
                .and_then(Value::as_str)
                .unwrap_or(&id)
                .to_string();
            let supported_efforts = model
                .get("supported_reasoning_levels")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|level| level.get("effort").and_then(Value::as_str))
                .filter_map(Effort::parse)
                .collect();
            Some((
                model.get("priority").and_then(Value::as_i64),
                DiscoveredModel {
                    id,
                    name,
                    description: model
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    default_effort: model
                        .get("default_reasoning_level")
                        .and_then(Value::as_str)
                        .and_then(Effort::parse),
                    supported_efforts,
                    is_default: false,
                },
            ))
        })
        .collect::<Vec<_>>();
    let default_priority = models.iter().filter_map(|(priority, _)| *priority).min();
    let models = models
        .into_iter()
        .map(|(priority, mut model)| {
            model.is_default = priority.is_some() && priority == default_priority;
            model
        })
        .collect();
    non_empty("codex debug models", models)
}

fn discover_grok_models(cwd: &Path) -> Result<Vec<DiscoveredModel>, String> {
    let output = run("grok", &["models"], cwd)?;
    non_empty("grok models", parse_grok_models(&text(&output)))
}

fn discover_agy_models(cwd: &Path) -> Result<Vec<DiscoveredModel>, String> {
    let output = run("agy", &["models"], cwd)?;
    non_empty("agy models", parse_agy_models(&text(&output)))
}

fn discover_claude_models(cwd: &Path) -> Vec<DiscoveredModel> {
    let paths = claude_settings_paths(cwd);
    let custom = std::env::var("ANTHROPIC_CUSTOM_MODEL_OPTION").ok();
    claude_models_from_settings(&paths, custom.as_deref())
}

fn claude_models_from_settings(paths: &[PathBuf], custom: Option<&str>) -> Vec<DiscoveredModel> {
    const ALIASES: &[&str] = &[
        "best",
        "sonnet",
        "opus",
        "haiku",
        "sonnet[1m]",
        "opus[1m]",
        "opusplan",
    ];

    let mut configured = Vec::new();
    let mut restricted = false;
    for path in paths {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&content) else {
            continue;
        };
        let Some(models) = value.get("availableModels").and_then(Value::as_array) else {
            continue;
        };
        restricted = true;
        extend_unique(
            &mut configured,
            models.iter().filter_map(Value::as_str).map(str::to_string),
        );
    }

    let mut models = if restricted {
        configured
    } else {
        ALIASES.iter().map(|model| (*model).to_string()).collect()
    };
    if let Some(custom) = custom {
        let custom = custom.trim();
        if !custom.is_empty() {
            extend_unique(&mut models, [custom.to_string()]);
        }
    }
    models.into_iter().map(DiscoveredModel::simple).collect()
}

fn claude_settings_paths(cwd: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        paths.push(PathBuf::from(home).join(".claude/settings.json"));
    }
    paths.push(cwd.join(".claude/settings.json"));
    paths.push(cwd.join(".claude/settings.local.json"));
    paths
}

fn run(program: &str, args: &[&str], cwd: &Path) -> Result<Output, String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|err| format!("could not run `{}`: {err}", command_label(program, args)))?;
    if output.status.success() {
        return Ok(output);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr.trim();
    Err(if detail.is_empty() {
        format!(
            "`{}` exited with {}",
            command_label(program, args),
            output.status
        )
    } else {
        format!("`{}` failed: {detail}", command_label(program, args))
    })
}

fn command_label(program: &str, args: &[&str]) -> String {
    std::iter::once(program)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ")
}

fn text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn non_empty(command: &str, models: Vec<DiscoveredModel>) -> Result<Vec<DiscoveredModel>, String> {
    if models.is_empty() {
        Err(format!("`{command}` returned no available models"))
    } else {
        Ok(models)
    }
}

fn parse_grok_models(output: &str) -> Vec<DiscoveredModel> {
    let mut models = Vec::new();
    let mut default_model = None;
    for line in output.lines() {
        let trimmed = line.trim();
        let candidate = trimmed
            .strip_prefix("Default model:")
            .and_then(|rest| rest.split_whitespace().next())
            .or_else(|| {
                trimmed
                    .strip_prefix("* ")
                    .or_else(|| trimmed.strip_prefix("- "))
                    .and_then(|rest| rest.split_whitespace().next())
            });
        if let Some(model) = candidate {
            if trimmed.starts_with("Default model:") {
                default_model = Some(model.to_string());
            }
            extend_unique(&mut models, [model.to_string()]);
        }
    }
    models
        .into_iter()
        .map(|id| {
            let mut model = DiscoveredModel::simple(id);
            model.is_default = default_model.as_deref() == Some(&model.id);
            model
        })
        .collect()
}

fn parse_agy_models(output: &str) -> Vec<DiscoveredModel> {
    let mut models = Vec::new();
    extend_unique(
        &mut models,
        output
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string),
    );
    models
        .into_iter()
        .map(|id| {
            let mut model = DiscoveredModel::simple(id);
            if let Some(effort) = effort_suffix(&model.id) {
                model.default_effort = Some(effort);
                model.supported_efforts = vec![effort];
            }
            model
        })
        .collect()
}

fn effort_suffix(model: &str) -> Option<Effort> {
    let (_, suffix) = model.rsplit_once('-')?;
    Effort::parse(suffix)
}

fn extend_unique(models: &mut Vec<String>, values: impl IntoIterator<Item = String>) {
    for value in values {
        if !value.is_empty() && !models.contains(&value) {
            models.push(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(models: &[DiscoveredModel]) -> Vec<&str> {
        models.iter().map(|model| model.id.as_str()).collect()
    }

    #[test]
    fn parses_and_deduplicates_grok_models_output() {
        let models = parse_grok_models(
            "Default model: grok-build\nAvailable models:\n  * grok-build (default)\n  - grok-4.5\n",
        );
        assert_eq!(ids(&models), vec!["grok-build", "grok-4.5"]);
        assert!(models[0].is_default);
    }

    #[test]
    fn parses_agy_effort_qualified_slugs() {
        let models =
            parse_agy_models("gemini-3.6-flash-high\ngemini-3.6-flash-low\nclaude-sonnet-4-6\n");
        assert_eq!(
            ids(&models),
            vec![
                "gemini-3.6-flash-high",
                "gemini-3.6-flash-low",
                "claude-sonnet-4-6"
            ]
        );
        assert_eq!(models[0].default_effort, Some(Effort::High));
        assert_eq!(models[0].supported_efforts, vec![Effort::High]);
        assert_eq!(models[1].default_effort, Some(Effort::Low));
        assert!(models[2].supported_efforts.is_empty());
    }

    #[test]
    fn codex_catalog_keeps_only_listed_slugs() {
        let models = parse_codex_models(
            br#"{"models":[{"slug":"gpt-a","display_name":"GPT A","description":"Agent model","visibility":"list","priority":1,"default_reasoning_level":"medium","supported_reasoning_levels":[{"effort":"low"},{"effort":"medium"}]},{"slug":"hidden","visibility":"hide"}]}"#,
        )
        .unwrap();
        assert_eq!(ids(&models), vec!["gpt-a"]);
        assert_eq!(models[0].name, "GPT A");
        assert_eq!(models[0].default_effort, Some(Effort::Medium));
        assert_eq!(
            models[0].supported_efforts,
            vec![Effort::Low, Effort::Medium]
        );
        assert!(models[0].is_default);
    }

    #[test]
    fn claude_uses_documented_aliases_without_restrictions() {
        let models = claude_models_from_settings(&[], None);
        assert!(ids(&models).contains(&"sonnet"));
        assert!(ids(&models).contains(&"opusplan"));
    }

    #[test]
    fn claude_honors_available_models_and_custom_option() {
        let dir = std::env::temp_dir().join(format!(
            "asterline-claude-model-settings-{}",
            std::process::id()
        ));
        let path = dir.join("settings.json");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &path,
            r#"{"availableModels":["company-sonnet","company-opus"]}"#,
        )
        .unwrap();

        let models = claude_models_from_settings(&[path], Some(" company-custom "));

        assert_eq!(
            ids(&models),
            vec!["company-sonnet", "company-opus", "company-custom"]
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
