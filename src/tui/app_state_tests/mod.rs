#![allow(unused_imports)]

pub(super) use super::*;
pub(super) use crate::domain::event::{
    AgentSessionId, ApprovalDecision, ConversationSummary, FileChangeItem, MemberSummary, RunId,
    RunStatus, RunStepSummary, RunSummary, RunVerification, TurnId,
};

pub(super) fn ready() -> RuntimeEvent {
    RuntimeEvent::Ready {
        modes: Default::default(),
        mode_overrides: Default::default(),
        suggested_verify: None,
        team: "mixed".to_string(),
        workspace: "/tmp/ws".to_string(),
        default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
        runs: Vec::new(),
        members: vec![MemberSummary {
            id: MemberId::new("builder"),
            display_name: "Builder".to_string(),
            backend: BackendKind::Codex,
            role: "impl".to_string(),
            status: MemberStatus::Idle,
            session: None,
            cwd: String::new(),
            model: None,
            effort: None,
            sandbox: SandboxPolicy::ReadOnly,
            permission_mode: Some(PermissionMode::Default),
            session_policy: SessionPolicy::Resume,
        }],
    }
}

mod apply;
mod chat;
mod mode_panel;
mod ui;
