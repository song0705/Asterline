//! Backend model catalog discovery.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

use crate::domain::team::BackendKind;

pub fn discover_models(backend: BackendKind, cwd: &Path) -> Result<Vec<String>, String> {
    match backend {
        BackendKind::Codex => discover_codex_models(cwd),
        BackendKind::Claude => Ok(discover_claude_models(cwd)),
        BackendKind::Grok => discover_grok_models(cwd),
        BackendKind::Agy => discover_agy_models(cwd),
    }
}

fn discover_codex_models(cwd: &Path) -> Result<Vec<String>, String> {
    let output = run("codex", &["debug", "models"], cwd)?;
    parse_codex_models(&output.stdout)
}

fn parse_codex_models(output: &[u8]) -> Result<Vec<String>, String> {
    let value: Value = serde_json::from_slice(output)
        .map_err(|err| format!("invalid `codex debug models` JSON: {err}"))?;
    let models = value
        .get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|model| model.get("visibility").and_then(Value::as_str) == Some("list"))
        .filter_map(|model| model.get("slug").and_then(Value::as_str))
        .map(str::to_string)
        .collect::<Vec<_>>();
    non_empty("codex debug models", models)
}

fn discover_grok_models(cwd: &Path) -> Result<Vec<String>, String> {
    let output = run("grok", &["models"], cwd)?;
    non_empty("grok models", parse_grok_models(&text(&output)))
}

fn discover_agy_models(cwd: &Path) -> Result<Vec<String>, String> {
    let output = run("agy", &["models"], cwd)?;
    non_empty("agy models", parse_agy_models(&text(&output)))
}

fn discover_claude_models(cwd: &Path) -> Vec<String> {
    let paths = claude_settings_paths(cwd);
    let custom = std::env::var("ANTHROPIC_CUSTOM_MODEL_OPTION").ok();
    claude_models_from_settings(&paths, custom.as_deref())
}

fn claude_models_from_settings(paths: &[PathBuf], custom: Option<&str>) -> Vec<String> {
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
    models
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

fn non_empty(command: &str, models: Vec<String>) -> Result<Vec<String>, String> {
    if models.is_empty() {
        Err(format!("`{command}` returned no available models"))
    } else {
        Ok(models)
    }
}

fn parse_grok_models(output: &str) -> Vec<String> {
    let mut models = Vec::new();
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
            extend_unique(&mut models, [model.to_string()]);
        }
    }
    models
}

fn parse_agy_models(output: &str) -> Vec<String> {
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

    #[test]
    fn parses_and_deduplicates_grok_models_output() {
        let models = parse_grok_models(
            "Default model: grok-build\nAvailable models:\n  * grok-build (default)\n  - grok-4.5\n",
        );
        assert_eq!(models, vec!["grok-build", "grok-4.5"]);
    }

    #[test]
    fn parses_agy_display_names_verbatim() {
        let models = parse_agy_models("Gemini 3.5 Flash (Medium)\nClaude Sonnet 4.6 (Thinking)\n");
        assert_eq!(
            models,
            vec!["Gemini 3.5 Flash (Medium)", "Claude Sonnet 4.6 (Thinking)"]
        );
    }

    #[test]
    fn codex_catalog_keeps_only_listed_slugs() {
        let models = parse_codex_models(
            br#"{"models":[{"slug":"gpt-a","visibility":"list"},{"slug":"hidden","visibility":"hide"}]}"#,
        )
        .unwrap();
        assert_eq!(models, vec!["gpt-a"]);
    }

    #[test]
    fn claude_uses_documented_aliases_without_restrictions() {
        let models = claude_models_from_settings(&[], None);
        assert!(models.contains(&"sonnet".to_string()));
        assert!(models.contains(&"opusplan".to_string()));
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
            models,
            vec!["company-sonnet", "company-opus", "company-custom"]
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
