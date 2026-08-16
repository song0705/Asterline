//! Backend adapters.
//!
//! The product path runs each member through a [`MemberRunner`] that streams
//! [`AgentEvent`]s. Codex uses a persistent App Server connection. Claude and
//! Agy use [`ProcessRunner`] over a [`StreamAdapter`],
//! while Grok uses its bidirectional ACP stdio protocol. Tests and offline mode
//! use [`fake::FakeRunner`]. `cli_pty` is retained as a raw-terminal/debug
//! capability and is not part of the product path.

pub mod agy_stream;
pub mod claude_stream;
pub mod cli_pty;
pub mod codex_app_server;
pub mod fake;
pub mod grok_stream;
pub mod models;
pub mod parser;
pub mod process;
pub mod prompt_images;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::SyncSender;

use crate::domain::event::{AgentEvent, AgentSessionId, ApprovalDecision};
use crate::domain::team::{BackendKind, Effort, TeamMember};

pub use agy_stream::AgyStreamAdapter;
pub use claude_stream::ClaudeStreamAdapter;
pub use codex_app_server::CodexAppServerRunner;
pub use fake::FakeRunner;
pub use grok_stream::GrokAcpRunner;
pub(crate) use models::DiscoveredCatalog;
pub use models::{DiscoveredModel, discover_models};
pub use process::{AdapterCommand, LineParser, ProcessRunner, StreamAdapter, run_streaming};

/// Inputs for one member turn.
pub struct RunRequest {
    pub prompt: String,
    /// Resumable backend session id, if one exists for this member.
    pub session: Option<AgentSessionId>,
    /// Set to request cancellation of the run.
    pub cancel: Arc<AtomicBool>,
    /// Reasoning effort for this run, if set.
    pub effort: Option<Effort>,
}

/// Runs one member turn, streaming [`AgentEvent`]s to `events` until the run
/// finishes. Implementations block and should end with [`AgentEvent::Exited`];
/// the transport synthesizes a failed exit if an implementation returns
/// without one.
pub trait MemberRunner: Send + Sync {
    fn backend(&self) -> BackendKind;
    fn run(&self, req: RunRequest, events: SyncSender<AgentEvent>);

    /// Resolve a live, backend-originated approval request. Most transports
    /// have no such control plane; they deliberately report `false` instead
    /// of pretending that an approval reached the native backend.
    fn resolve_native_approval(&self, _request_id: u64, _decision: ApprovalDecision) -> bool {
        false
    }
}

/// Build a real CLI runner for a member, based on its backend.
pub fn runner_for(member: &TeamMember, workspace: &Path) -> Box<dyn MemberRunner> {
    match member.backend {
        BackendKind::Claude => Box::new(ProcessRunner::new(ClaudeStreamAdapter::from_member(
            member, workspace,
        ))),
        BackendKind::Codex => Box::new(CodexAppServerRunner::from_member(member, workspace)),
        BackendKind::Grok => Box::new(GrokAcpRunner::from_member(member, workspace)),
        BackendKind::Agy => Box::new(ProcessRunner::new(AgyStreamAdapter::from_member(
            member, workspace,
        ))),
    }
}
