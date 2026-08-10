//! Backend model catalog discovery.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::adapter::process::{
    ChildProcessTree, MAX_PROTOCOL_LINE_BYTES, MAX_STDERR_LINE_BYTES, configure_process_tree,
};
use crate::domain::config::{resolve_binary_on_path, user_home_dir};
use crate::domain::team::{BackendKind, Effort};

const MODEL_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);

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
    let output = run("grok", &["--no-auto-update", "models"], cwd)?;
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
        "fable",
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
    if let Some(home) = user_home_dir() {
        paths.push(home.join(".claude/settings.json"));
    }
    let project_root = git_project_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    paths.push(project_root.join(".claude/settings.json"));
    paths.push(project_root.join(".claude/settings.local.json"));
    // Claude still reads local settings left in the launch directory by older
    // releases, while current releases resolve them from the repository root.
    if project_root != cwd {
        paths.push(cwd.join(".claude/settings.local.json"));
    }
    paths
}

fn git_project_root(cwd: &Path) -> Option<PathBuf> {
    if let Ok(output) = run("git", &["rev-parse", "--show-toplevel"], cwd)
        && output.status.success()
        && let Ok(root) = std::str::from_utf8(&output.stdout)
    {
        let root = root.trim();
        if !root.is_empty() {
            return Some(PathBuf::from(root));
        }
    }

    // Git is not guaranteed to be installed in every packaged environment
    // (notably a clean Windows PATH). The repository marker is sufficient for
    // locating Claude's project settings even when `git rev-parse` cannot run.
    cwd.ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
        .and_then(|root| std::fs::canonicalize(root).ok())
}

fn run(program: &str, args: &[&str], cwd: &Path) -> Result<Output, String> {
    run_with_timeout(program, args, cwd, MODEL_DISCOVERY_TIMEOUT)
}

fn run_with_timeout(
    program: &str,
    args: &[&str],
    cwd: &Path,
    timeout: Duration,
) -> Result<Output, String> {
    let label = command_label(program, args);
    let resolved_program =
        resolve_binary_on_path(program).unwrap_or_else(|| PathBuf::from(program));
    let mut builder = Command::new(resolved_program);
    builder
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_process_tree(&mut builder);
    let mut child = builder
        .spawn()
        .map_err(|err| format!("could not run `{label}`: {err}"))?;
    let process_tree = match ChildProcessTree::attach(&mut child) {
        Ok(tree) => Arc::new(tree),
        Err(err) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("could not isolate `{label}` process tree: {err}"));
        }
    };
    let stdout = child
        .stdout
        .take()
        .expect("stdout was configured as a pipe");
    let stderr = child
        .stderr
        .take()
        .expect("stderr was configured as a pipe");
    let stdout_thread = read_bounded_pipe(
        stdout,
        MAX_PROTOCOL_LINE_BYTES,
        "stdout",
        Arc::clone(&process_tree),
    );
    let stderr_thread = read_bounded_pipe(
        stderr,
        MAX_STDERR_LINE_BYTES,
        "stderr",
        Arc::clone(&process_tree),
    );

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < timeout => {
                thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                let _ = process_tree.terminate_with_fallback(&mut child);
                let _ = child.wait();
                let _ = stdout_thread.join();
                let _ = stderr_thread.join();
                return Err(format!(
                    "`{label}` timed out after {}ms",
                    timeout.as_millis()
                ));
            }
            Err(err) => {
                let _ = process_tree.terminate_with_fallback(&mut child);
                let _ = child.wait();
                let _ = stdout_thread.join();
                let _ = stderr_thread.join();
                return Err(format!("could not wait for `{label}`: {err}"));
            }
        }
    };
    while !stdout_thread.is_finished() || !stderr_thread.is_finished() {
        if started.elapsed() >= timeout {
            let _ = process_tree.terminate();
            let _ = stdout_thread.join();
            let _ = stderr_thread.join();
            return Err(format!(
                "`{label}` timed out after {}ms while draining output",
                timeout.as_millis()
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }
    let stdout = stdout_thread
        .join()
        .map_err(|_| format!("failed to collect `{label}` stdout"))?
        .map_err(|err| format!("failed to read `{label}` stdout: {err}"))?;
    let stderr = stderr_thread
        .join()
        .map_err(|_| format!("failed to collect `{label}` stderr"))?
        .map_err(|err| format!("failed to read `{label}` stderr: {err}"))?;
    let output = Output {
        status,
        stdout,
        stderr,
    };
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

fn read_bounded_pipe(
    reader: impl Read + Send + 'static,
    max_bytes: usize,
    stream: &'static str,
    process_tree: Arc<ChildProcessTree>,
) -> thread::JoinHandle<std::io::Result<Vec<u8>>> {
    thread::spawn(move || {
        let mut bytes = Vec::with_capacity(max_bytes.min(8192));
        let result = reader
            .take(max_bytes.saturating_add(1) as u64)
            .read_to_end(&mut bytes)
            .and_then(|_| {
                if bytes.len() > max_bytes {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("{stream} exceeded {max_bytes} bytes"),
                    ))
                } else {
                    Ok(())
                }
            });
        if let Err(err) = result {
            let _ = process_tree.terminate();
            return Err(err);
        }
        Ok(bytes)
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
        assert!(ids(&models).contains(&"fable"));
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

    #[test]
    fn claude_discovers_repository_root_settings_from_nested_cwd() {
        let root = std::env::temp_dir().join(format!(
            "asterline-claude-git-root-settings-{}",
            std::process::id()
        ));
        let nested = root.join("crates/member");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir(root.join(".git")).unwrap();
        std::fs::write(
            root.join(".claude/settings.json"),
            r#"{"availableModels":["root-managed-model"]}"#,
        )
        .unwrap();

        let paths = claude_settings_paths(&nested);
        let models = claude_models_from_settings(&paths, None);
        let canonical_root = std::fs::canonicalize(&root).unwrap();

        assert!(paths.contains(&canonical_root.join(".claude/settings.json")));
        assert!(paths.contains(&canonical_root.join(".claude/settings.local.json")));
        assert!(ids(&models).contains(&"root-managed-model"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn model_discovery_times_out_and_kills_the_child_tree() {
        let started = Instant::now();
        let error = run_with_timeout(
            "/bin/sh",
            &["-c", "sleep 30"],
            Path::new("/tmp"),
            Duration::from_millis(100),
        )
        .unwrap_err();

        assert!(error.contains("timed out after 100ms"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[test]
    fn model_discovery_timeout_covers_descendants_holding_output_pipes() {
        let error = run_with_timeout(
            "/bin/sh",
            &["-c", "sleep 30 & exit 0"],
            Path::new("/tmp"),
            Duration::from_millis(100),
        )
        .unwrap_err();

        assert!(error.contains("timed out after 100ms"), "{error}");
        assert!(error.contains("draining output"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn model_discovery_rejects_oversized_output_without_hanging() {
        let started = Instant::now();
        let error = run_with_timeout(
            "/bin/sh",
            &["-c", "yes x | tr -d '\\n'"],
            Path::new("/tmp"),
            Duration::from_secs(3),
        )
        .unwrap_err();

        assert!(error.contains("stdout exceeded"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
