#![allow(unused_imports)]

pub(super) use super::*;
pub(super) use crate::domain::event::{
    MemberStatus, RunEventSummary, RunId, RunStatus, RunStepStatus, RunStepSummary, RunSummary,
    RunVerification, RuntimeEvent,
};
pub(super) use crate::domain::team::{
    BackendKind, DefaultTarget, Effort, MemberId, PermissionMode, SandboxPolicy, SessionPolicy,
};
pub(super) use crate::tui::drawers::Drawer;
pub(super) use ratatui::Terminal;
pub(super) use ratatui::backend::TestBackend;
pub(super) use ratatui::style::Color;

fn member_summary(
    id: &str,
    display_name: &str,
    backend: BackendKind,
    role: &str,
    status: MemberStatus,
) -> crate::domain::event::MemberSummary {
    crate::domain::event::MemberSummary {
        id: MemberId::new(id),
        display_name: display_name.to_string(),
        backend,
        role: role.to_string(),
        status,
        session: None,
        cwd: String::new(),
        model: None,
        effort: None,
        sandbox: SandboxPolicy::ReadOnly,
        permission_mode: Some(PermissionMode::Default),
        approvals_reviewer: crate::domain::team::CodexApprovalsReviewer::User,
        session_policy: SessionPolicy::Resume,
    }
}

fn plain_text(lines: &[Line<'_>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

fn is_separator_text(text: &str) -> bool {
    let trimmed = text.trim();
    !trimmed.is_empty() && trimmed.chars().all(|ch| ch == '─')
}

mod grouping;
mod render;
