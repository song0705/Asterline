use crate::adapter::AgentAdapter;
use crate::types::AgentId;
use serde_json::json;

#[derive(Clone, Copy, Debug)]
pub struct FakeAgent {
    id: AgentId,
}

impl FakeAgent {
    pub fn codex() -> Self {
        Self { id: AgentId::Codex }
    }

    pub fn claude() -> Self {
        Self {
            id: AgentId::Claude,
        }
    }
}

impl AgentAdapter for FakeAgent {
    fn id(&self) -> AgentId {
        self.id
    }

    fn handle_user_message(&self, body: &str) -> String {
        let payload = match self.id {
            AgentId::Codex => json!({
                "to": "claude",
                "kind": "question",
                "body": format!("Please review Codex next step for: {body}"),
            }),
            AgentId::Claude => json!({
                "to": "codex",
                "kind": "task",
                "body": format!("Implement the plan for: {body}"),
            }),
        };

        format!("@@team_message {payload}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_codex_requests_claude_review() {
        let response = FakeAgent::codex().handle_user_message("add parser tests");

        assert!(response.starts_with("@@team_message "));
        assert!(response.contains(r#""to":"claude""#));
        assert!(response.contains("add parser tests"));
    }

    #[test]
    fn fake_claude_assigns_codex_task() {
        let response = FakeAgent::claude().handle_user_message("draft plan");

        assert!(response.starts_with("@@team_message "));
        assert!(response.contains(r#""to":"codex""#));
        assert!(response.contains("draft plan"));
    }

    #[test]
    fn fake_agent_escapes_json_body() {
        let response = FakeAgent::codex().handle_user_message(r#"quote " test"#);

        assert!(response.contains(r#"quote \" test"#));
    }
}
