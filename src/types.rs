use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentId {
    Codex,
    Claude,
}

impl AgentId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
}

impl TryFrom<&str> for AgentId {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "codex" => Ok(Self::Codex),
            "claude" => Ok(Self::Claude),
            _ => Err(format!("unknown agent id: {value}")),
        }
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentStatus {
    Idle,
    Running,
    Waiting,
    NeedsLogin,
    NeedsApproval,
    Failed,
}

impl fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::NeedsLogin => "needs_login",
            Self::NeedsApproval => "needs_approval",
            Self::Failed => "failed",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Participant {
    You,
    Team,
    Agent(AgentId),
}

impl fmt::Display for Participant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::You => f.write_str("You"),
            Self::Team => f.write_str("Team"),
            Self::Agent(agent) => write!(f, "{agent}"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RouteTarget {
    pub from: Participant,
    pub to: Participant,
}

impl RouteTarget {
    pub const fn new(from: Participant, to: Participant) -> Self {
        Self { from, to }
    }
}

impl fmt::Display for RouteTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} -> {}", self.from, self.to)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageTarget {
    Team,
    Agent(AgentId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserMessage {
    pub target: MessageTarget,
    pub body: String,
}
