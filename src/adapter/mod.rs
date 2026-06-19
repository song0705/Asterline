pub mod claude_print;
pub mod cli_pty;
pub mod codex_exec;
pub mod fake;

use crate::types::AgentId;

pub trait AgentAdapter {
    fn id(&self) -> AgentId;
    fn handle_user_message(&self, body: &str) -> String;
}
