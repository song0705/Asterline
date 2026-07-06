//! Antigravity CLI (`agy`) adapter.
//!
//! Drives `agy --print <prompt>` non-interactively and treats stdout as the
//! agent's reply. Agy prints plain text, so session discovery comes from the
//! per-run log file (`Created conversation <uuid>`) or, as a fallback, Agy's
//! workspace-to-conversation cache.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::adapter::process::{AdapterCommand, LineParser, StreamAdapter};
use crate::domain::event::{AgentEvent, AgentSessionId};
use crate::domain::team::{BackendKind, Effort, PermissionMode, SandboxPolicy, TeamMember};

#[derive(Clone, Debug)]
pub struct AgyStreamAdapter {
    binary: String,
    cwd: PathBuf,
    workspace: PathBuf,
    member_id: String,
    log_dir: PathBuf,
    model: Option<String>,
    system_prompt: Option<String>,
    sandbox: SandboxPolicy,
    permission_mode: Option<PermissionMode>,
    last_log_path: Arc<Mutex<Option<PathBuf>>>,
}

impl AgyStreamAdapter {
    pub fn from_member(member: &TeamMember, workspace: &Path) -> Self {
        let workspace = workspace.to_path_buf();
        Self {
            binary: "agy".to_string(),
            cwd: member.resolved_cwd(&workspace),
            log_dir: workspace.join(".asterline").join("agy"),
            workspace,
            member_id: member.id.as_str().to_string(),
            model: member.model.clone(),
            system_prompt: member.system_prompt.clone(),
            sandbox: member.sandbox,
            permission_mode: member.permission_mode,
            last_log_path: Arc::new(Mutex::new(None)),
        }
    }

    pub fn with_binary(mut self, binary: impl Into<String>) -> Self {
        self.binary = binary.into();
        self
    }

    fn log_path(&self) -> PathBuf {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        self.log_dir
            .join(format!("{}-{millis}.log", self.member_id))
    }

    fn prompt_with_system(&self, prompt: &str) -> String {
        match &self.system_prompt {
            Some(system_prompt) if !system_prompt.trim().is_empty() => {
                format!("System instructions:\n{system_prompt}\n\nUser message:\n{prompt}")
            }
            _ => prompt.to_string(),
        }
    }
}

impl StreamAdapter for AgyStreamAdapter {
    fn backend(&self) -> BackendKind {
        BackendKind::Agy
    }

    fn build_command(
        &self,
        prompt: &str,
        session: Option<&AgentSessionId>,
        _effort: Option<Effort>,
    ) -> AdapterCommand {
        let _ = std::fs::create_dir_all(&self.log_dir);
        let log_path = self.log_path();
        if let Ok(mut slot) = self.last_log_path.lock() {
            *slot = Some(log_path.clone());
        }

        let mut args = vec![
            "--print".to_string(),
            "--print-timeout".to_string(),
            "5m0s".to_string(),
            "--log-file".to_string(),
            log_path.display().to_string(),
        ];
        if let Some(session) = session {
            args.push("--conversation".to_string());
            args.push(session.as_str().to_string());
        }
        if let Some(model) = &self.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        if self.sandbox != SandboxPolicy::DangerFullAccess {
            args.push("--sandbox".to_string());
        }
        if self.permission_mode == Some(PermissionMode::BypassPermissions) {
            args.push("--dangerously-skip-permissions".to_string());
        }
        AdapterCommand {
            program: self.binary.clone(),
            args,
            cwd: self.cwd.clone(),
            stdin: Some(self.prompt_with_system(prompt)),
        }
    }

    fn parser(&self) -> Box<dyn LineParser> {
        let log_path = self.last_log_path.lock().ok().and_then(|slot| slot.clone());
        Box::new(AgyLineParser::new(log_path, self.workspace.clone()))
    }
}

/// Accumulates Agy's plain-text stdout into a single completed message.
pub struct AgyLineParser {
    acc: String,
    started: bool,
    log_path: Option<PathBuf>,
    workspace: PathBuf,
    cache_path: Option<PathBuf>,
}

impl AgyLineParser {
    pub fn new(log_path: Option<PathBuf>, workspace: PathBuf) -> Self {
        Self {
            acc: String::new(),
            started: false,
            log_path,
            workspace,
            cache_path: default_cache_path(),
        }
    }

    pub fn with_cache_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.cache_path = Some(path.into());
        self
    }

    fn discover_session(&self) -> Option<AgentSessionId> {
        self.log_path
            .as_deref()
            .and_then(session_from_log)
            .or_else(|| {
                self.cache_path
                    .as_deref()
                    .and_then(|path| session_from_cache(path, &self.workspace))
            })
            .map(AgentSessionId)
    }
}

impl LineParser for AgyLineParser {
    fn parse_line(&mut self, line: &str) -> Vec<AgentEvent> {
        if line.is_empty() && !self.started {
            return Vec::new();
        }
        self.started = true;
        if !self.acc.is_empty() {
            self.acc.push('\n');
        }
        self.acc.push_str(line);
        vec![AgentEvent::TextDelta(format!("{line}\n"))]
    }

    fn finish(&mut self) -> Vec<AgentEvent> {
        if !self.started {
            return Vec::new();
        }
        vec![AgentEvent::MessageCompleted(
            self.acc.trim_end().to_string(),
        )]
    }

    fn finish_after_exit(&mut self, ok: bool) -> Vec<AgentEvent> {
        if !ok {
            return Vec::new();
        }
        self.discover_session()
            .map(AgentEvent::SessionDiscovered)
            .into_iter()
            .collect()
    }
}

fn default_cache_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join(".gemini")
            .join("antigravity-cli")
            .join("cache")
            .join("last_conversations.json")
    })
}

fn session_from_log(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    extract_created_conversation(&text)
}

fn extract_created_conversation(text: &str) -> Option<String> {
    text.lines().rev().find_map(|line| {
        let (_, rest) = line.split_once("Created conversation ")?;
        rest.split_whitespace()
            .next()
            .filter(|candidate| is_conversation_id(candidate))
            .map(str::to_string)
    })
}

fn session_from_cache(path: &Path, workspace: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let object = value.as_object()?;
    let workspace_display = workspace.display().to_string();
    if let Some(session) = object
        .get(&workspace_display)
        .and_then(serde_json::Value::as_str)
        .filter(|candidate| is_conversation_id(candidate))
    {
        return Some(session.to_string());
    }

    let canonical = workspace.canonicalize().ok()?;
    let canonical_display = canonical.display().to_string();
    object
        .get(&canonical_display)
        .and_then(serde_json::Value::as_str)
        .filter(|candidate| is_conversation_id(candidate))
        .map(str::to_string)
}

fn is_conversation_id(candidate: &str) -> bool {
    candidate.len() == 36 && candidate.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_uses_print_log_sandbox_and_model() {
        let mut member = TeamMember::new("a", "Agy", BackendKind::Agy, "research");
        member.model = Some("model-x".to_string());
        let adapter = AgyStreamAdapter::from_member(&member, Path::new("/tmp/ws"));
        let command = adapter.build_command("hi there", None, None);

        assert_eq!(command.program, "agy");
        assert!(command.args.contains(&"--print".to_string()));
        assert!(command.args.windows(2).any(|w| w == ["--model", "model-x"]));
        assert!(command.args.contains(&"--sandbox".to_string()));
        assert!(
            command
                .args
                .windows(2)
                .any(|w| w[0] == "--log-file" && w[1].contains(".asterline/agy/a-"))
        );
        assert_eq!(command.stdin.as_deref(), Some("hi there"));
    }

    #[test]
    fn resume_command_uses_conversation_id() {
        let member = TeamMember::new("a", "Agy", BackendKind::Agy, "research");
        let adapter = AgyStreamAdapter::from_member(&member, Path::new("/tmp/ws"));
        let command = adapter.build_command(
            "again",
            Some(&AgentSessionId(
                "1ddde77f-dcaf-47cf-97e8-b3e6a3f4e43d".to_string(),
            )),
            None,
        );

        assert!(
            command
                .args
                .windows(2)
                .any(|w| { w == ["--conversation", "1ddde77f-dcaf-47cf-97e8-b3e6a3f4e43d",] })
        );
    }

    #[test]
    fn bypass_permissions_maps_to_agy_flag() {
        let mut member = TeamMember::new("a", "Agy", BackendKind::Agy, "research");
        member.permission_mode = Some(PermissionMode::BypassPermissions);
        member.sandbox = SandboxPolicy::DangerFullAccess;
        let adapter = AgyStreamAdapter::from_member(&member, Path::new("/tmp/ws"));
        let command = adapter.build_command("hi", None, None);

        assert!(
            command
                .args
                .contains(&"--dangerously-skip-permissions".to_string())
        );
        assert!(!command.args.contains(&"--sandbox".to_string()));
    }

    #[test]
    fn parser_accumulates_text_into_a_completed_message() {
        let mut parser = AgyLineParser::new(None, PathBuf::from("/tmp/ws"));
        let mut events = Vec::new();
        events.extend(parser.parse_line("Hello"));
        events.extend(parser.parse_line("world"));
        events.extend(parser.finish());

        assert!(matches!(events[0], AgentEvent::TextDelta(_)));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::MessageCompleted(text) if text == "Hello\nworld"
        )));
    }

    #[test]
    fn extracts_conversation_from_log() {
        let text = "I0622 Created conversation 1ddde77f-dcaf-47cf-97e8-b3e6a3f4e43d\n";
        assert_eq!(
            extract_created_conversation(text),
            Some("1ddde77f-dcaf-47cf-97e8-b3e6a3f4e43d".to_string())
        );
    }

    #[test]
    fn discovers_session_from_cache_for_workspace() {
        let dir = std::env::temp_dir().join(format!("asterline-agy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cache = dir.join("last_conversations.json");
        std::fs::write(
            &cache,
            r#"{"/tmp/ws":"1ddde77f-dcaf-47cf-97e8-b3e6a3f4e43d"}"#,
        )
        .unwrap();

        let session = session_from_cache(&cache, Path::new("/tmp/ws"));
        assert_eq!(
            session,
            Some("1ddde77f-dcaf-47cf-97e8-b3e6a3f4e43d".to_string())
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
