use tokio::sync::mpsc;

use crate::adapter::{
    AgentAdapter,
    claude_print::{ClaudePrintAdapter, ClaudePrintError, ClaudePrintRunner},
    codex_exec::{CodexExecAdapter, CodexExecError, CodexExecRunner},
    fake::FakeAgent,
};
use crate::runtime::events::RuntimeEvent;
use crate::types::{AgentId, AgentStatus};

pub enum CodexBackend {
    Fake(FakeAgent),
    Exec(Box<dyn CodexExecRunner>),
}

pub enum ClaudeBackend {
    Fake(FakeAgent),
    Print(Box<dyn ClaudePrintRunner>),
}

impl ClaudeBackend {
    pub fn fake() -> Self {
        Self::Fake(FakeAgent::claude())
    }

    pub fn print(adapter: ClaudePrintAdapter) -> Self {
        Self::Print(Box::new(adapter))
    }
}

impl CodexBackend {
    pub fn fake() -> Self {
        Self::Fake(FakeAgent::codex())
    }

    pub fn exec(adapter: CodexExecAdapter) -> Self {
        Self::Exec(Box::new(adapter))
    }
}

pub struct Supervisor {
    codex: CodexBackend,
    claude: ClaudeBackend,
    events: mpsc::Sender<RuntimeEvent>,
}

impl Supervisor {
    pub fn new(codex: CodexBackend, events: mpsc::Sender<RuntimeEvent>) -> Self {
        Self::with_backends(codex, ClaudeBackend::fake(), events)
    }

    pub fn with_backends(
        codex: CodexBackend,
        claude: ClaudeBackend,
        events: mpsc::Sender<RuntimeEvent>,
    ) -> Self {
        Self {
            codex,
            claude,
            events,
        }
    }

    pub fn fake(events: mpsc::Sender<RuntimeEvent>) -> Self {
        Self::new(CodexBackend::fake(), events)
    }

    pub async fn send_to_agent(
        &self,
        agent: AgentId,
        body: &str,
    ) -> Result<(), mpsc::error::SendError<RuntimeEvent>> {
        self.events
            .send(RuntimeEvent::AgentStatusChanged {
                agent,
                status: AgentStatus::Running,
            })
            .await?;

        match self.agent_output(agent, body) {
            Ok(output) => {
                self.events
                    .send(RuntimeEvent::AgentOutput {
                        agent,
                        body: output,
                    })
                    .await?;
                self.events
                    .send(RuntimeEvent::AgentStatusChanged {
                        agent,
                        status: AgentStatus::Idle,
                    })
                    .await
            }
            Err(message) => {
                self.events
                    .send(RuntimeEvent::AgentError { agent, message })
                    .await?;
                self.events
                    .send(RuntimeEvent::AgentStatusChanged {
                        agent,
                        status: AgentStatus::Failed,
                    })
                    .await
            }
        }
    }

    fn agent_output(&self, agent: AgentId, body: &str) -> Result<String, String> {
        match agent {
            AgentId::Codex => match &self.codex {
                CodexBackend::Fake(adapter) => Ok(adapter.handle_user_message(body)),
                CodexBackend::Exec(adapter) => adapter
                    .run_prompt(body)
                    .map_err(format_codex_exec_error)
                    .map(|run| run.final_message.unwrap_or_default()),
            },
            AgentId::Claude => match &self.claude {
                ClaudeBackend::Fake(adapter) => Ok(adapter.handle_user_message(body)),
                ClaudeBackend::Print(adapter) => adapter
                    .run_prompt(body)
                    .map_err(format_claude_print_error)
                    .map(|run| run.result),
            },
        }
    }
}

pub type FakeSupervisor = Supervisor;

fn format_codex_exec_error(error: CodexExecError) -> String {
    match error {
        CodexExecError::InvalidJsonLine { message, .. } => {
            format!("Codex emitted invalid JSONL: {message}")
        }
        CodexExecError::MissingEventType(_) => "Codex emitted an event without a type".to_string(),
        CodexExecError::ProcessFailed { status, stderr, .. } => {
            format!("Codex process failed with status {status:?}: {stderr}")
        }
        CodexExecError::Io(message) => format!("Codex process could not start: {message}"),
    }
}

fn format_claude_print_error(error: ClaudePrintError) -> String {
    match error {
        ClaudePrintError::InvalidJson { message } => {
            format!("Claude emitted invalid JSON: {message}")
        }
        ClaudePrintError::MissingResult => {
            "Claude JSON output did not include a result".to_string()
        }
        ClaudePrintError::ProcessFailed { status, stderr, .. } => {
            format!("Claude process failed with status {status:?}: {stderr}")
        }
        ClaudePrintError::Io(message) => format!("Claude process could not start: {message}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fake_supervisor_emits_status_and_output_events() {
        let (tx, mut rx) = mpsc::channel(8);
        let supervisor = Supervisor::fake(tx);

        supervisor
            .send_to_agent(AgentId::Codex, "build parser")
            .await
            .expect("events should be accepted");

        assert_eq!(
            rx.recv().await,
            Some(RuntimeEvent::AgentStatusChanged {
                agent: AgentId::Codex,
                status: AgentStatus::Running,
            })
        );
        assert!(matches!(
            rx.recv().await,
            Some(RuntimeEvent::AgentOutput {
                agent: AgentId::Codex,
                body
            }) if body.contains("@@team_message")
        ));
        assert_eq!(
            rx.recv().await,
            Some(RuntimeEvent::AgentStatusChanged {
                agent: AgentId::Codex,
                status: AgentStatus::Idle,
            })
        );
    }

    struct StubCodexRunner {
        result: Result<crate::adapter::codex_exec::CodexExecRun, CodexExecError>,
    }

    struct StubClaudeRunner {
        result: Result<crate::adapter::claude_print::ClaudePrintRun, ClaudePrintError>,
    }

    impl CodexExecRunner for StubCodexRunner {
        fn run_prompt(
            &self,
            _prompt: &str,
        ) -> Result<crate::adapter::codex_exec::CodexExecRun, CodexExecError> {
            self.result.clone()
        }
    }

    impl ClaudePrintRunner for StubClaudeRunner {
        fn run_prompt(
            &self,
            _prompt: &str,
        ) -> Result<crate::adapter::claude_print::ClaudePrintRun, ClaudePrintError> {
            self.result.clone()
        }
    }

    #[tokio::test]
    async fn supervisor_can_emit_real_codex_exec_output() {
        let (tx, mut rx) = mpsc::channel(8);
        let supervisor = Supervisor::new(
            CodexBackend::Exec(Box::new(StubCodexRunner {
                result: Ok(crate::adapter::codex_exec::CodexExecRun {
                    events: Vec::new(),
                    final_message: Some("real codex reply".to_string()),
                    raw_stdout: String::new(),
                    raw_stderr: String::new(),
                }),
            })),
            tx,
        );

        supervisor
            .send_to_agent(AgentId::Codex, "summarize")
            .await
            .expect("events should be accepted");

        assert_eq!(
            rx.recv().await,
            Some(RuntimeEvent::AgentStatusChanged {
                agent: AgentId::Codex,
                status: AgentStatus::Running,
            })
        );
        assert_eq!(
            rx.recv().await,
            Some(RuntimeEvent::AgentOutput {
                agent: AgentId::Codex,
                body: "real codex reply".to_string(),
            })
        );
        assert_eq!(
            rx.recv().await,
            Some(RuntimeEvent::AgentStatusChanged {
                agent: AgentId::Codex,
                status: AgentStatus::Idle,
            })
        );
    }

    #[tokio::test]
    async fn supervisor_marks_codex_exec_failures_failed() {
        let (tx, mut rx) = mpsc::channel(8);
        let supervisor = Supervisor::new(
            CodexBackend::Exec(Box::new(StubCodexRunner {
                result: Err(CodexExecError::Io("missing binary".to_string())),
            })),
            tx,
        );

        supervisor
            .send_to_agent(AgentId::Codex, "summarize")
            .await
            .expect("events should be accepted");

        assert_eq!(
            rx.recv().await,
            Some(RuntimeEvent::AgentStatusChanged {
                agent: AgentId::Codex,
                status: AgentStatus::Running,
            })
        );
        assert_eq!(
            rx.recv().await,
            Some(RuntimeEvent::AgentError {
                agent: AgentId::Codex,
                message: "Codex process could not start: missing binary".to_string(),
            })
        );
        assert_eq!(
            rx.recv().await,
            Some(RuntimeEvent::AgentStatusChanged {
                agent: AgentId::Codex,
                status: AgentStatus::Failed,
            })
        );
    }

    #[tokio::test]
    async fn supervisor_can_emit_real_claude_print_output() {
        let (tx, mut rx) = mpsc::channel(8);
        let supervisor = Supervisor::with_backends(
            CodexBackend::fake(),
            ClaudeBackend::Print(Box::new(StubClaudeRunner {
                result: Ok(crate::adapter::claude_print::ClaudePrintRun {
                    session_id: Some("session-1".to_string()),
                    result: "real claude reply".to_string(),
                    is_error: false,
                    raw_stdout: String::new(),
                    raw_stderr: String::new(),
                }),
            })),
            tx,
        );

        supervisor
            .send_to_agent(AgentId::Claude, "summarize")
            .await
            .expect("events should be accepted");

        assert_eq!(
            rx.recv().await,
            Some(RuntimeEvent::AgentStatusChanged {
                agent: AgentId::Claude,
                status: AgentStatus::Running,
            })
        );
        assert_eq!(
            rx.recv().await,
            Some(RuntimeEvent::AgentOutput {
                agent: AgentId::Claude,
                body: "real claude reply".to_string(),
            })
        );
        assert_eq!(
            rx.recv().await,
            Some(RuntimeEvent::AgentStatusChanged {
                agent: AgentId::Claude,
                status: AgentStatus::Idle,
            })
        );
    }
}
