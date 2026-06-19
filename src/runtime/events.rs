use crate::types::{AgentId, AgentStatus};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeEvent {
    AgentStatusChanged { agent: AgentId, status: AgentStatus },
    AgentOutput { agent: AgentId, body: String },
    AgentError { agent: AgentId, message: String },
}
