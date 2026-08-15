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
const GATEWAY_MODEL_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);
const GROK_MODELS_CACHE_MAX_BYTES: u64 = 1024 * 1024;
const LOCAL_SETTINGS_MAX_BYTES: u64 = 1024 * 1024;

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

/// The native defaults discovered alongside a backend's model catalog.
///
/// These are intentionally display-only: a member with no Asterline override
/// must keep using its CLI's own configuration instead of having that
/// configuration copied into (and then pinned by) `team.json`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct DiscoveredCatalog {
    pub(crate) models: Vec<DiscoveredModel>,
    pub(crate) native_permission: Option<String>,
}

pub fn discover_models(backend: BackendKind, cwd: &Path) -> Result<Vec<DiscoveredModel>, String> {
    discover_catalog(backend, cwd).map(|catalog| catalog.models)
}

pub(crate) fn discover_catalog(
    backend: BackendKind,
    cwd: &Path,
) -> Result<DiscoveredCatalog, String> {
    match backend {
        BackendKind::Codex => Ok(DiscoveredCatalog {
            models: discover_codex_models(cwd)?,
            native_permission: codex_permission_from_config(),
        }),
        BackendKind::Claude => Ok(discover_claude_catalog(cwd)),
        BackendKind::Grok => Ok(DiscoveredCatalog {
            models: discover_grok_models(cwd)?,
            native_permission: grok_permission_from_config(),
        }),
        BackendKind::Agy => Ok(DiscoveredCatalog {
            models: discover_agy_models(cwd)?,
            native_permission: agy_permission_from_settings(),
        }),
    }
}

fn discover_codex_models(cwd: &Path) -> Result<Vec<DiscoveredModel>, String> {
    match crate::adapter::codex_app_server::discover_models(cwd) {
        Ok(models) => Ok(models),
        // Keep the legacy inspector as a compatibility fallback for an older
        // locally-installed Codex that has not acquired App Server yet. The
        // product runner itself never silently changes transport.
        Err(app_server_error) => {
            let output = run("codex", &["debug", "models"], cwd).map_err(|debug_error| {
                format!(
                    "Codex App Server model/list failed: {app_server_error}; \
                     legacy `codex debug models` also failed: {debug_error}"
                )
            })?;
            parse_codex_models(&output.stdout)
        }
    }
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
    let models = parse_grok_models(&text(&output));
    let models = grok_models_cache()
        .as_ref()
        .map_or(models.clone(), |cache| enrich_grok_models(models, cache));
    non_empty("grok models", models)
}

fn discover_agy_models(cwd: &Path) -> Result<Vec<DiscoveredModel>, String> {
    let output = run("agy", &["models"], cwd)?;
    let mut models = parse_agy_models(&text(&output));
    mark_agy_configured_default(&mut models, agy_model_from_settings().as_deref());
    non_empty("agy models", models)
}

fn discover_claude_catalog(cwd: &Path) -> DiscoveredCatalog {
    let paths = claude_settings_paths(cwd);
    let environment = claude_environment(&paths);
    let mut catalog = claude_catalog_from_sources(&paths, &environment);

    // Claude Code deliberately makes gateway discovery opt-in: a shared key
    // could otherwise expose every model reachable by that key. Mirror that
    // gate exactly. A failure remains non-fatal and uses the cache Claude Code
    // itself maintains from a previous successful discovery.
    if let Some(gateway) = environment.gateway.as_ref() {
        let models = discover_gateway_models(gateway)
            .or_else(|_| cached_gateway_models())
            .unwrap_or_default();
        let models = filter_gateway_models(models, claude_available_models(&paths));
        extend_models(&mut catalog.models, models);
    }
    catalog
}

#[cfg(test)]
fn claude_models_from_settings(paths: &[PathBuf], custom: Option<&str>) -> Vec<DiscoveredModel> {
    let environment = ClaudeEnvironment {
        custom_model: custom.map(str::to_string),
        ..ClaudeEnvironment::default()
    };
    claude_catalog_from_sources(paths, &environment).models
}

#[derive(Clone, Default)]
struct ClaudeEnvironment {
    selected_model: Option<String>,
    alias_targets: Vec<(String, String)>,
    custom_model: Option<String>,
    gateway: Option<ClaudeGateway>,
}

#[derive(Clone)]
struct ClaudeGateway {
    models_url: String,
    auth_token: Option<String>,
    api_key: Option<String>,
    custom_headers: Vec<(String, String)>,
}

fn claude_environment(paths: &[PathBuf]) -> ClaudeEnvironment {
    let settings_env = claude_settings_environment(paths);
    let selected_model = claude_env_value(&settings_env, "ANTHROPIC_MODEL");
    let mut alias_targets = Vec::new();
    for (alias, variable) in [
        ("opus", "ANTHROPIC_DEFAULT_OPUS_MODEL"),
        ("sonnet", "ANTHROPIC_DEFAULT_SONNET_MODEL"),
        ("haiku", "ANTHROPIC_DEFAULT_HAIKU_MODEL"),
    ] {
        if let Some(model) = claude_env_value(&settings_env, variable) {
            set_model_target(&mut alias_targets, alias.to_string(), model);
        }
    }
    let custom_model = claude_env_value(&settings_env, "ANTHROPIC_CUSTOM_MODEL_OPTION");
    let gateway = claude_env_value(&settings_env, "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY")
        .is_some_and(|value| value == "1")
        .then(|| {
            claude_env_value(&settings_env, "ANTHROPIC_BASE_URL")
                .and_then(|base_url| gateway_models_url(&base_url))
                .map(|models_url| ClaudeGateway {
                    models_url,
                    auth_token: claude_env_value(&settings_env, "ANTHROPIC_AUTH_TOKEN"),
                    api_key: claude_env_value(&settings_env, "ANTHROPIC_API_KEY"),
                    custom_headers: claude_env_value(&settings_env, "ANTHROPIC_CUSTOM_HEADERS")
                        .map(|headers| parse_custom_headers(&headers))
                        .unwrap_or_default(),
                })
        })
        .flatten();
    ClaudeEnvironment {
        selected_model,
        alias_targets,
        custom_model,
        gateway,
    }
}

fn claude_catalog_from_sources(
    paths: &[PathBuf],
    environment: &ClaudeEnvironment,
) -> DiscoveredCatalog {
    let mut configured = Vec::new();
    let mut selected_model = None;
    let mut native_permission = None;
    let mut model_targets = Vec::new();
    for settings_path in paths {
        let Some(value) = json_settings(settings_path) else {
            continue;
        };
        if let Some(models) = value.get("availableModels").and_then(Value::as_array) {
            extend_unique(
                &mut configured,
                models.iter().filter_map(Value::as_str).map(str::to_string),
            );
        }
        if let Some(model) = value.get("model").and_then(Value::as_str) {
            let model = model.trim();
            if !model.is_empty() && !model.eq_ignore_ascii_case("default") {
                selected_model = Some(model.to_string());
            }
        }
        if let Some(mode) = value
            .get("permissions")
            .and_then(|permissions| permissions.get("defaultMode"))
            .and_then(Value::as_str)
        {
            let mode = mode.trim();
            if !mode.is_empty() {
                native_permission = Some(mode.to_string());
            }
        }
        if let Some(overrides) = value.get("modelOverrides").and_then(Value::as_object) {
            for (claude_model, provider_model) in overrides {
                let provider_model = provider_model.as_str().map(str::trim).unwrap_or_default();
                if claude_model.trim().is_empty() || provider_model.is_empty() {
                    continue;
                }
                extend_unique(&mut configured, [claude_model.clone()]);
                set_model_target(
                    &mut model_targets,
                    claude_model.clone(),
                    provider_model.to_string(),
                );
            }
        }
    }

    for (alias, target) in &environment.alias_targets {
        extend_unique(&mut configured, [alias.clone()]);
        set_model_target(&mut model_targets, alias.clone(), target.clone());
    }
    if let Some(model) = environment.selected_model.as_ref() {
        selected_model = Some(model.clone());
    }

    let custom = environment
        .custom_model
        .as_deref()
        .map(str::trim)
        .filter(|custom| !custom.is_empty());
    let mut models = configured;
    if let Some(model) = selected_model.as_ref() {
        // The configured selection is meaningful even if an incorrect local
        // allowlist would otherwise hide it; showing it makes the mismatch
        // visible rather than silently replacing it with a generic alias.
        extend_unique(&mut models, [model.clone()]);
    }
    if let Some(custom) = custom {
        extend_unique(&mut models, [custom.to_string()]);
    }
    let models = models
        .into_iter()
        .map(|id| {
            let target = model_target(&model_targets, &id);
            let mut model = claude_model(id, target);
            model.is_default = selected_model.as_deref() == Some(&model.id);
            model
        })
        .collect();
    DiscoveredCatalog {
        models,
        native_permission,
    }
}

fn set_model_target(targets: &mut Vec<(String, String)>, key: String, value: String) {
    if let Some((_, existing)) = targets.iter_mut().find(|(existing, _)| existing == &key) {
        *existing = value;
    } else {
        targets.push((key, value));
    }
}

fn model_target<'a>(targets: &'a [(String, String)], id: &str) -> Option<&'a str> {
    targets
        .iter()
        .find(|(key, _)| key == id)
        .map(|(_, value)| value.as_str())
}

fn claude_model(id: String, target: Option<&str>) -> DiscoveredModel {
    let name = match target {
        Some(target) if target != id => format!("{target} (via {id})"),
        _ => id.clone(),
    };
    DiscoveredModel {
        id,
        name,
        description: None,
        // Claude Code has no machine-readable effort capability catalog. Do
        // not infer one from a model name: this is particularly wrong for
        // provider-specific gateway IDs.
        default_effort: None,
        supported_efforts: Vec::new(),
        is_default: false,
    }
}

/// Resolve the environment Claude Code will receive from its layered settings.
/// Later settings scopes win, just as they do in Claude Code itself. An actual
/// environment variable remains the final override because it is also inherited
/// by the child process Asterline launches.
fn claude_settings_environment(paths: &[PathBuf]) -> Vec<(String, String)> {
    let mut environment = Vec::new();
    for path in paths {
        let Some(values) = json_settings(path)
            .and_then(|settings| settings.get("env").cloned())
            .and_then(|value| value.as_object().cloned())
        else {
            continue;
        };
        for (name, value) in values {
            let Some(value) = value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            set_model_target(&mut environment, name, value.to_string());
        }
    }
    environment
}

fn claude_env_value(settings: &[(String, String)], name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| model_target(settings, name).map(str::to_string))
}

fn gateway_models_url(base_url: &str) -> Option<String> {
    let base_url = base_url.trim().trim_end_matches('/');
    if !(base_url.starts_with("https://") || base_url.starts_with("http://"))
        || base_url.contains(['?', '#'])
    {
        return None;
    }
    let authority = base_url
        .split_once("://")?
        .1
        .split('/')
        .next()
        .unwrap_or_default();
    let host = authority.rsplit('@').next()?.split(':').next()?.trim();
    if host.eq_ignore_ascii_case("api.anthropic.com") {
        return None;
    }
    Some(if base_url.ends_with("/v1") {
        format!("{base_url}/models")
    } else {
        format!("{base_url}/v1/models")
    })
}

fn parse_custom_headers(headers: &str) -> Vec<(String, String)> {
    headers
        .lines()
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            let name = name.trim();
            let value = value.trim();
            (!name.is_empty()
                && !value.is_empty()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'))
            .then(|| (name.to_string(), value.to_string()))
        })
        .collect()
}

fn discover_gateway_models(gateway: &ClaudeGateway) -> Result<Vec<DiscoveredModel>, String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(GATEWAY_MODEL_DISCOVERY_TIMEOUT))
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::Rustls)
                .build(),
        )
        .build()
        .new_agent();
    let mut request = agent
        .get(&gateway.models_url)
        .header(
            "User-Agent",
            concat!("Asterline/", env!("CARGO_PKG_VERSION")),
        )
        .header("Accept", "application/json");
    if let Some(token) = gateway.auth_token.as_deref() {
        request = request.header("Authorization", &format!("Bearer {token}"));
    } else if let Some(api_key) = gateway.api_key.as_deref() {
        request = request.header("x-api-key", api_key);
    }
    for (name, value) in &gateway.custom_headers {
        request = request.header(name, value);
    }
    let mut response = request
        .call()
        .map_err(|_| "gateway model discovery request failed".to_string())?;
    let body = response
        .body_mut()
        .with_config()
        .limit(LOCAL_SETTINGS_MAX_BYTES)
        .read_to_string()
        .map_err(|_| "gateway model discovery response could not be read".to_string())?;
    parse_gateway_models(&body)
}

fn cached_gateway_models() -> Result<Vec<DiscoveredModel>, String> {
    let cache_path = claude_config_dir()
        .ok_or_else(|| "could not resolve Claude configuration directory".to_string())?
        .join("cache/gateway-models.json");
    let value =
        json_settings(&cache_path).ok_or_else(|| "no cached Claude gateway models".to_string())?;
    gateway_models_from_value(&value)
}

fn parse_gateway_models(body: &str) -> Result<Vec<DiscoveredModel>, String> {
    let value: Value = serde_json::from_str(body)
        .map_err(|_| "gateway model discovery returned invalid JSON".to_string())?;
    gateway_models_from_value(&value)
}

fn gateway_models_from_value(value: &Value) -> Result<Vec<DiscoveredModel>, String> {
    let entries = value
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| value.get("models").and_then(Value::as_array))
        .or_else(|| value.as_array())
        .ok_or_else(|| "gateway model discovery returned no model list".to_string())?;
    let models = entries
        .iter()
        .filter_map(|entry| {
            let id = entry.get("id").and_then(Value::as_str)?.trim();
            let normalized = id.to_ascii_lowercase();
            (normalized.starts_with("claude") || normalized.starts_with("anthropic")).then(|| {
                DiscoveredModel {
                    id: id.to_string(),
                    name: entry
                        .get("display_name")
                        .or_else(|| entry.get("name"))
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .unwrap_or(id)
                        .to_string(),
                    description: entry
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    default_effort: None,
                    supported_efforts: Vec::new(),
                    is_default: false,
                }
            })
        })
        .collect::<Vec<_>>();
    non_empty("Claude gateway /v1/models", models)
}

fn claude_available_models(paths: &[PathBuf]) -> Option<Vec<String>> {
    let mut present = false;
    let mut available = Vec::new();
    for path in paths {
        let Some(models) = json_settings(path)
            .and_then(|settings| settings.get("availableModels").cloned())
            .and_then(|value| value.as_array().cloned())
        else {
            continue;
        };
        present = true;
        extend_unique(
            &mut available,
            models
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|model| !model.is_empty())
                .map(str::to_string),
        );
    }
    present.then_some(available)
}

fn filter_gateway_models(
    models: Vec<DiscoveredModel>,
    available_models: Option<Vec<String>>,
) -> Vec<DiscoveredModel> {
    let Some(available_models) = available_models else {
        return models;
    };
    models
        .into_iter()
        .filter(|model| {
            available_models
                .iter()
                .any(|available| gateway_model_is_allowed(&model.id, available))
        })
        .collect()
}

fn gateway_model_is_allowed(id: &str, available: &str) -> bool {
    if id.eq_ignore_ascii_case(available) {
        return true;
    }
    let id = id.to_ascii_lowercase();
    match available.to_ascii_lowercase().as_str() {
        "opus" | "sonnet" | "haiku" => id.contains(&format!("-{}-", available)),
        _ => false,
    }
}

fn extend_models(models: &mut Vec<DiscoveredModel>, additional: Vec<DiscoveredModel>) {
    for model in additional {
        if let Some(existing) = models.iter_mut().find(|existing| existing.id == model.id) {
            // A plain configured ID is still the same model the gateway has
            // just described. Prefer the gateway's current human label, while
            // leaving an explicit `modelOverrides` display intact.
            if existing.name == existing.id {
                existing.name = model.name;
                existing.description = model.description;
            }
        } else {
            models.push(model);
        }
    }
}

fn json_settings(path: &Path) -> Option<Value> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > LOCAL_SETTINGS_MAX_BYTES {
        return None;
    }
    let mut contents = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(LOCAL_SETTINGS_MAX_BYTES + 1)
        .read_to_string(&mut contents)
        .ok()?;
    if contents.len() as u64 > LOCAL_SETTINGS_MAX_BYTES {
        return None;
    }
    serde_json::from_str(&contents).ok()
}

fn codex_permission_from_config() -> Option<String> {
    toml_root_string(
        user_home_dir()?.join(".codex/config.toml"),
        "approval_policy",
    )
}

fn grok_permission_from_config() -> Option<String> {
    toml_root_string(
        user_home_dir()?.join(".grok/config.toml"),
        "permission_mode",
    )
}

fn agy_permission_from_settings() -> Option<String> {
    let settings = json_settings(&user_home_dir()?.join(".gemini/antigravity-cli/settings.json"))?;
    agy_execution_mode(&settings)
}

fn agy_model_from_settings() -> Option<String> {
    json_settings(&user_home_dir()?.join(".gemini/antigravity-cli/settings.json"))?
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string)
}

/// `permissions.allow` is a rule list, not AGY's execution mode. Showing its
/// length in the mode field made a normal configuration look like a selected
/// mode. AGY exposes only `accept-edits` and `plan` as persisted mode values;
/// absent one, its CLI default must remain the displayed value.
fn agy_execution_mode(settings: &Value) -> Option<String> {
    settings
        .get("mode")
        .or_else(|| {
            settings
                .get("permissions")
                .and_then(|permissions| permissions.get("mode"))
        })
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|mode| matches!(*mode, "accept-edits" | "plan"))
        .map(str::to_string)
}

/// Read a top-level quoted TOML scalar without pulling a TOML parser into the
/// runtime just for two CLI configuration keys. Values below a `[section]` are
/// deliberately ignored so an unrelated subsection cannot masquerade as the
/// CLI's global default.
fn toml_root_string(path: PathBuf, key: &str) -> Option<String> {
    let metadata = std::fs::metadata(&path).ok()?;
    if !metadata.is_file() || metadata.len() > LOCAL_SETTINGS_MAX_BYTES {
        return None;
    }
    let contents = std::fs::read_to_string(path).ok()?;
    if contents.len() as u64 > LOCAL_SETTINGS_MAX_BYTES {
        return None;
    }
    for line in contents.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            break;
        }
        let Some((candidate, value)) = line.split_once('=') else {
            continue;
        };
        if candidate.trim() != key {
            continue;
        }
        let value = value.trim().split('#').next()?.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .or_else(|| {
                value
                    .strip_prefix('\'')
                    .and_then(|value| value.strip_suffix('\''))
            })?
            .trim();
        return (!value.is_empty()).then(|| value.to_string());
    }
    None
}

fn claude_settings_paths(cwd: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(config_dir) = claude_config_dir() {
        paths.push(config_dir.join("settings.json"));
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

fn claude_config_dir() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| user_home_dir().map(|home| home.join(".claude")))
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

pub(crate) fn run_with_timeout(
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

/// `grok models` intentionally prints only the selectable model IDs. The
/// authenticated Grok CLI stores the corresponding per-model capability menu
/// in this cache after it fetches models. Keep the CLI listing authoritative
/// for which IDs we expose and enrich only matching IDs from the cache.
fn grok_models_cache() -> Option<Value> {
    let path = user_home_dir()?.join(".grok/models_cache.json");
    let metadata = std::fs::metadata(&path).ok()?;
    if !metadata.is_file() || metadata.len() > GROK_MODELS_CACHE_MAX_BYTES {
        return None;
    }
    let mut contents = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(GROK_MODELS_CACHE_MAX_BYTES + 1)
        .read_to_string(&mut contents)
        .ok()?;
    if contents.len() as u64 > GROK_MODELS_CACHE_MAX_BYTES {
        return None;
    }
    serde_json::from_str(&contents).ok()
}

fn enrich_grok_models(mut models: Vec<DiscoveredModel>, cache: &Value) -> Vec<DiscoveredModel> {
    for model in &mut models {
        let Some(info) = cache
            .get("models")
            .and_then(|models| models.get(&model.id))
            .and_then(|entry| entry.get("info"))
        else {
            continue;
        };

        if let Some(name) = info.get("name").and_then(Value::as_str)
            && !name.trim().is_empty()
        {
            model.name = name.to_string();
        }
        model.description = info
            .get("description")
            .and_then(Value::as_str)
            .filter(|description| !description.trim().is_empty())
            .map(str::to_string);

        if info
            .get("supports_reasoning_effort")
            .and_then(Value::as_bool)
            != Some(true)
        {
            continue;
        }
        let efforts = info
            .get("reasoning_efforts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                entry
                    .get("value")
                    .or_else(|| entry.get("id"))
                    .and_then(Value::as_str)
                    .and_then(Effort::parse)
            })
            .collect::<Vec<_>>();
        if efforts.is_empty() {
            continue;
        }
        model.default_effort = info
            .get("reasoning_effort")
            .and_then(Value::as_str)
            .and_then(Effort::parse)
            .filter(|effort| efforts.contains(effort));
        if model.default_effort.is_none() {
            model.default_effort = info
                .get("reasoning_efforts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find(|entry| entry.get("default").and_then(Value::as_bool) == Some(true))
                .and_then(|entry| {
                    entry
                        .get("value")
                        .or_else(|| entry.get("id"))
                        .and_then(Value::as_str)
                        .and_then(Effort::parse)
                })
                .filter(|effort| efforts.contains(effort));
        }
        model.supported_efforts = efforts;
    }
    models
}

fn parse_agy_models(output: &str) -> Vec<DiscoveredModel> {
    let mut models = Vec::<(String, String)>::new();
    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let (id, name) = if let Some((id, name)) = line.split_once('\t') {
            (id.trim(), name.trim())
        } else {
            // Older AGY versions emit one ID per line. Ignore status prose
            // such as `Fetching available models...`, which is not a model.
            (line, line)
        };
        if id.is_empty() || (!line.contains('\t') && id.chars().any(char::is_whitespace)) {
            continue;
        }
        if !models.iter().any(|(existing, _)| existing == id) {
            models.push((id.to_string(), name.to_string()));
        }
    }
    models
        .into_iter()
        .map(|(id, name)| {
            let mut model = DiscoveredModel::simple(id);
            model.name = name;
            if let Some(effort) = effort_suffix(&model.id) {
                model.default_effort = Some(effort);
                model.supported_efforts = vec![effort];
            }
            model
        })
        .collect()
}

fn mark_agy_configured_default(models: &mut [DiscoveredModel], configured: Option<&str>) {
    let Some(configured) = configured.map(str::trim).filter(|model| !model.is_empty()) else {
        return;
    };
    if let Some(model) = models.iter_mut().find(|model| {
        model.id.eq_ignore_ascii_case(configured) || model.name.eq_ignore_ascii_case(configured)
    }) {
        model.is_default = true;
    }
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
    fn grok_model_ids_do_not_infer_effort_from_their_names() {
        let models = parse_grok_models("Default model: grok-code-high\n");

        assert_eq!(models[0].default_effort, None);
        assert!(models[0].supported_efforts.is_empty());
    }

    #[test]
    fn grok_cache_enriches_each_listed_model_with_its_own_effort_menu() {
        let models = enrich_grok_models(
            parse_grok_models("Default model: grok-4.6\n  - grok-4.5\n"),
            &serde_json::json!({
                "models": {
                    "grok-4.6": {"info": {
                        "name": "Grok 4.6",
                        "description": "frontier",
                        "supports_reasoning_effort": true,
                        "reasoning_effort": "high",
                        "reasoning_efforts": [
                            {"value": "xhigh"}, {"value": "high"},
                            {"value": "medium"}, {"value": "low"}
                        ]
                    }},
                    "grok-4.5": {"info": {
                        "supports_reasoning_effort": true,
                        "reasoning_effort": "high",
                        "reasoning_efforts": [
                            {"value": "high"}, {"value": "medium"},
                            {"value": "low"}
                        ]
                    }}
                }
            }),
        );

        assert_eq!(models[0].name, "Grok 4.6");
        assert_eq!(models[0].description.as_deref(), Some("frontier"));
        assert_eq!(models[0].default_effort, Some(Effort::High));
        assert_eq!(
            models[0].supported_efforts,
            vec![Effort::Xhigh, Effort::High, Effort::Medium, Effort::Low]
        );
        assert_eq!(
            models[1].supported_efforts,
            vec![Effort::High, Effort::Medium, Effort::Low]
        );
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
    fn agy_uses_the_local_display_name_to_mark_its_configured_default() {
        let mut models = parse_agy_models(
            "Fetching available models...\n\
             gemini-3.7-flash-high\tGemini 3.7 Flash (High)\n\
             gemini-3.5-flash-high\tGemini 3.5 Flash (High)\n",
        );

        mark_agy_configured_default(&mut models, Some("Gemini 3.5 Flash (High)"));

        assert_eq!(
            ids(&models),
            vec!["gemini-3.7-flash-high", "gemini-3.5-flash-high"]
        );
        assert_eq!(models[1].name, "Gemini 3.5 Flash (High)");
        assert!(models[1].is_default);
        assert!(!models[0].is_default);
    }

    #[test]
    fn agy_execution_mode_does_not_confuse_allow_rules_for_a_mode() {
        assert_eq!(
            agy_execution_mode(&serde_json::json!({
                "permissions": {"allow": ["command(cargo test)", "command(git status)"]}
            })),
            None
        );
        assert_eq!(
            agy_execution_mode(&serde_json::json!({"mode": "accept-edits"})),
            Some("accept-edits".to_string())
        );
        assert_eq!(
            agy_execution_mode(&serde_json::json!({"permissions": {"mode": "plan"}})),
            Some("plan".to_string())
        );
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
    fn claude_does_not_invent_a_static_model_catalog_without_local_configuration() {
        let models = claude_models_from_settings(&[], None);
        assert!(models.is_empty());
    }

    #[test]
    fn claude_reads_current_model_and_permission_from_settings() {
        let dir = std::env::temp_dir().join(format!(
            "asterline-claude-native-settings-{}",
            std::process::id()
        ));
        let path = dir.join("settings.json");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &path,
            r#"{"model":"haiku","permissions":{"defaultMode":"bypassPermissions"}}"#,
        )
        .unwrap();

        let catalog = claude_catalog_from_sources(&[path], &ClaudeEnvironment::default());
        let selected = catalog
            .models
            .iter()
            .find(|model| model.id == "haiku")
            .unwrap();
        assert!(selected.is_default);
        assert_eq!(selected.name, "haiku");
        assert_eq!(
            catalog.native_permission.as_deref(),
            Some("bypassPermissions")
        );
        std::fs::remove_dir_all(&dir).ok();
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
    fn claude_gateway_configuration_uses_its_configured_ids_without_static_fallbacks() {
        let dir = std::env::temp_dir().join(format!(
            "asterline-claude-gateway-settings-{}",
            std::process::id()
        ));
        let path = dir.join("settings.json");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &path,
            r#"{"model":"gateway/primary","modelOverrides":{"claude-opus-4-7":"gateway/opus-v7"}}"#,
        )
        .unwrap();

        let catalog = claude_catalog_from_sources(&[path], &ClaudeEnvironment::default());
        assert_eq!(
            ids(&catalog.models),
            vec!["claude-opus-4-7", "gateway/primary"]
        );
        assert!(
            catalog
                .models
                .iter()
                .all(|model| model.id != "claude-sonnet-4-6")
        );
        assert_eq!(
            catalog.models[0].name,
            "gateway/opus-v7 (via claude-opus-4-7)"
        );
        assert!(catalog.models[1].is_default);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn gateway_catalog_uses_the_gateway_display_name_and_filters_non_claude_ids() {
        let models = parse_gateway_models(
            r#"{"data":[
                {"id":"claude-opus-5-20260814","display_name":"Claude Opus 5","description":"Gateway catalog"},
                {"id":"anthropic.sonnet-5","name":"Provider Sonnet"},
                {"id":"gpt-5.6","display_name":"Must not be offered to Claude"}
            ]}"#,
        )
        .unwrap();
        assert_eq!(
            ids(&models),
            vec!["claude-opus-5-20260814", "anthropic.sonnet-5"]
        );
        assert_eq!(models[0].name, "Claude Opus 5");
        assert_eq!(models[0].description.as_deref(), Some("Gateway catalog"));
    }

    #[test]
    fn gateway_description_enriches_a_configured_model_without_losing_its_default() {
        let mut configured = vec![DiscoveredModel {
            id: "claude-opus-5-20260814".to_string(),
            name: "claude-opus-5-20260814".to_string(),
            description: None,
            default_effort: None,
            supported_efforts: Vec::new(),
            is_default: true,
        }];
        let gateway = parse_gateway_models(
            r#"{"data":[{"id":"claude-opus-5-20260814","display_name":"Claude Opus 5"}]}"#,
        )
        .unwrap();

        extend_models(&mut configured, gateway);

        assert_eq!(configured.len(), 1);
        assert_eq!(configured[0].name, "Claude Opus 5");
        assert!(configured[0].is_default);
    }

    #[test]
    fn gateway_url_requires_an_explicit_non_anthropic_http_endpoint() {
        assert_eq!(
            gateway_models_url("https://gateway.example.test"),
            Some("https://gateway.example.test/v1/models".to_string())
        );
        assert_eq!(
            gateway_models_url("https://gateway.example.test/v1/"),
            Some("https://gateway.example.test/v1/models".to_string())
        );
        assert_eq!(gateway_models_url("https://api.anthropic.com"), None);
        assert_eq!(gateway_models_url("file:///tmp/models"), None);
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
