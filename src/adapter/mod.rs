//! Backend adapters.
//!
//! The product path runs each member through a [`MemberRunner`] that streams
//! [`AgentEvent`]s. Real members use [`ProcessRunner`] over a [`StreamAdapter`]
//! (`claude_stream` / `codex_stream`); tests and offline mode use
//! [`fake::FakeRunner`]. `cli_pty` is retained as a raw-terminal/debug
//! capability and is not part of the product path.

pub mod claude_stream;
pub mod cli_pty;
pub mod codex_stream;
pub mod fake;
pub mod parser;
pub mod process;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender;

use crate::domain::event::{AgentEvent, AgentSessionId};
use crate::domain::team::{BackendKind, TeamMember};

pub use claude_stream::ClaudeStreamAdapter;
pub use codex_stream::CodexStreamAdapter;
pub use fake::FakeRunner;
pub use process::{AdapterCommand, LineParser, ProcessRunner, StreamAdapter, run_streaming};

/// Inputs for one member turn.
pub struct RunRequest {
    pub prompt: String,
    /// Resumable backend session id, if one exists for this member.
    pub session: Option<AgentSessionId>,
    /// Set to request cancellation of the run.
    pub cancel: Arc<AtomicBool>,
}

/// Runs one member turn, streaming [`AgentEvent`]s to `events` until the run
/// finishes. Implementations block; the runtime calls `run` on a worker thread.
pub trait MemberRunner: Send + Sync {
    fn backend(&self) -> BackendKind;
    fn run(&self, req: RunRequest, events: Sender<AgentEvent>);
}

/// Build a real CLI runner for a member, based on its backend.
pub fn runner_for(member: &TeamMember, workspace: &Path) -> Box<dyn MemberRunner> {
    match member.backend {
        BackendKind::Claude => Box::new(ProcessRunner::new(ClaudeStreamAdapter::from_member(
            member, workspace,
        ))),
        BackendKind::Codex => Box::new(ProcessRunner::new(CodexStreamAdapter::from_member(
            member, workspace,
        ))),
    }
}
