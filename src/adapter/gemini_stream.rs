//! Gemini CLI adapter.
//!
//! Drives `gemini -p <prompt> -o text` non-interactively and treats stdout as
//! the agent's reply. v1 is stateless (no session resume) and uses text output
//! rather than `stream-json`, whose exact schema is not yet pinned down here —
//! once verified against a real run, this can switch to structured streaming.

use std::path::{Path, PathBuf};

use crate::adapter::process::{AdapterCommand, LineParser, StreamAdapter};
use crate::domain::event::{AgentEvent, AgentSessionId};
use crate::domain::team::{BackendKind, Effort, PermissionMode, TeamMember};

#[derive(Clone, Debug)]
pub struct GeminiStreamAdapter {
    binary: String,
    cwd: PathBuf,
    model: Option<String>,
    permission_mode: Option<PermissionMode>,
}

impl GeminiStreamAdapter {
    pub fn from_member(member: &TeamMember, workspace: &Path) -> Self {
        Self {
            binary: "gemini".to_string(),
            cwd: member.resolved_cwd(workspace),
            model: member.model.clone(),
            permission_mode: member.permission_mode,
        }
    }

    pub fn with_binary(mut self, binary: impl Into<String>) -> Self {
        self.binary = binary.into();
        self
    }

    /// Map the shared permission mode onto Gemini's `--approval-mode`.
    fn approval_mode(&self) -> &'static str {
        match self.permission_mode {
            Some(PermissionMode::Plan) => "plan",
            Some(PermissionMode::AcceptEdits) => "auto_edit",
            Some(PermissionMode::BypassPermissions) => "yolo",
            _ => "default",
        }
    }
}

impl StreamAdapter for GeminiStreamAdapter {
    fn backend(&self) -> BackendKind {
        BackendKind::Gemini
    }

    fn build_command(
        &self,
        prompt: &str,
        _session: Option<&AgentSessionId>,
        _effort: Option<Effort>,
    ) -> AdapterCommand {
        let mut args = vec![
            "-p".to_string(),
            prompt.to_string(),
            "-o".to_string(),
            "text".to_string(),
            "--approval-mode".to_string(),
            self.approval_mode().to_string(),
        ];
        if let Some(model) = &self.model {
            args.push("-m".to_string());
            args.push(model.clone());
        }
        AdapterCommand {
            program: self.binary.clone(),
            args,
            cwd: self.cwd.clone(),
            stdin: None,
        }
    }

    fn parser(&self) -> Box<dyn LineParser> {
        Box::new(GeminiLineParser::default())
    }
}

/// Accumulates Gemini's plain-text stdout into a single completed message.
#[derive(Default)]
pub struct GeminiLineParser {
    acc: String,
    started: bool,
}

impl LineParser for GeminiLineParser {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_uses_text_output_and_approval_mode() {
        let mut member = TeamMember::new("g", "Gemini", BackendKind::Gemini, "research");
        member.permission_mode = Some(PermissionMode::Plan);
        let adapter = GeminiStreamAdapter::from_member(&member, Path::new("/tmp/ws"));
        let command = adapter.build_command("hi there", None, None);

        assert_eq!(command.program, "gemini");
        assert!(command.args.windows(2).any(|w| w == ["-p", "hi there"]));
        assert!(command.args.windows(2).any(|w| w == ["-o", "text"]));
        assert!(
            command
                .args
                .windows(2)
                .any(|w| w == ["--approval-mode", "plan"])
        );
    }

    #[test]
    fn parser_accumulates_text_into_a_completed_message() {
        let mut parser = GeminiLineParser::default();
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
}
