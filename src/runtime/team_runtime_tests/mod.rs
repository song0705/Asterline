#![allow(unused_imports)]

pub(super) use super::*;
pub(super) use crate::domain::event::{ChatItem, FileChangeItem};
pub(super) use crate::domain::mode::{
    BrainstormModeConfig, CollabMode, PlanModeConfig, ReviewModeConfig, TeamModeConfig,
    resolve_mode_roles,
};
pub(super) use crate::domain::team::{
    ApprovalSurface, BackendKind, DefaultTarget, Effort, SessionPolicy, TeamMember,
};
pub(super) use crate::runtime::mode_prompts::{
    BRAINSTORM_BUILD_HINT, BRAINSTORM_PROPOSE_HINT, BRAINSTORM_STRETCH_HINT,
    BRAINSTORM_SYNTHESIS_HINT, BRAINSTORM_VOTE_HINT, PLAN_MODE_HINT, REVIEW_PROTOCOL_HINT,
};
pub(super) use rusqlite::Connection;

pub(super) fn team() -> TeamConfig {
    let mut config = TeamConfig::new("mixed", "/tmp/ws")
        .with_member(TeamMember::new(
            "builder",
            "Builder",
            BackendKind::Codex,
            "impl",
        ))
        .with_member(TeamMember::new(
            "reviewer",
            "Reviewer",
            BackendKind::Claude,
            "review",
        ));
    config.default_target = Some(DefaultTarget::Member(MemberId::new("builder")));
    config
}

pub(super) fn runtime() -> TeamRuntime {
    TeamRuntime::new(team(), SqliteStore::in_memory().unwrap()).with_approvals(false)
}

pub(super) fn runtime_in_workspace(workspace: impl Into<PathBuf>) -> TeamRuntime {
    let mut config = TeamConfig::new("mixed", workspace)
        .with_member(TeamMember::new(
            "builder",
            "Builder",
            BackendKind::Codex,
            "impl",
        ))
        .with_member(TeamMember::new(
            "reviewer",
            "Reviewer",
            BackendKind::Claude,
            "review",
        ));
    config.default_target = Some(DefaultTarget::Member(MemberId::new("builder")));
    TeamRuntime::new(config, SqliteStore::in_memory().unwrap()).with_approvals(false)
}

pub(super) fn remove_sqlite_test_files(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

pub(super) fn user(body: &str) -> UiCommand {
    UiCommand::UserMessage {
        target: MessageTarget::Default,
        body: body.to_string(),
    }
}

pub(super) fn start_team(rt: &mut TeamRuntime, goal: &str) -> RuntimeStep {
    rt.on_ui_command(UiCommand::SetMode {
        mode: TerminalMode::Team,
    });
    rt.on_ui_command(user(goal))
}

pub(super) fn run_mode(task: &str) -> UiCommand {
    UiCommand::RunMode {
        mode: CollabMode::Review,
        task: task.to_string(),
    }
}

pub(super) fn complete_ok(rt: &mut TeamRuntime, member: &MemberId, text: &str) -> RuntimeStep {
    let mut step = rt.on_agent_event(member, AgentEvent::MessageCompleted(text.to_string()));
    let exit = rt.on_agent_event(
        member,
        AgentEvent::Exited {
            code: Some(0),
            ok: true,
        },
    );
    // Merge so callers can assert on envelopes recorded at MessageCompleted
    // and transitions that fire on Exited (TurnFinished / mode dispatch).
    step.events.extend(exit.events);
    step.actions.extend(exit.actions);
    step.verify_actions.extend(exit.verify_actions);
    step.runner_changes.extend(exit.runner_changes);
    if exit.persist_team.is_some() {
        step.persist_team = exit.persist_team;
    }
    step
}

pub(super) fn latest_run(rt: &TeamRuntime) -> RunSummary {
    rt.store.latest_run().unwrap().expect("run exists")
}

pub(super) fn find_run_id(step: &RuntimeStep) -> RunId {
    step.events
        .iter()
        .find_map(|e| match e {
            RuntimeEvent::RunUpdated { run } => Some(run.id),
            _ => None,
        })
        .expect("run id")
}

pub(super) fn plan_team() -> TeamConfig {
    let mut config = TeamConfig::new("plan-team", "/tmp/ws")
        .with_member(TeamMember::new(
            "planner",
            "Planner",
            BackendKind::Codex,
            "planning lead",
        ))
        .with_member(TeamMember::new(
            "builder",
            "Builder",
            BackendKind::Claude,
            "impl",
        ))
        .with_member(TeamMember::new(
            "reviewer",
            "Reviewer",
            BackendKind::Grok,
            "review",
        ));
    config.default_target = Some(DefaultTarget::Member(MemberId::new("builder")));
    config
}

pub(super) fn plan_runtime() -> TeamRuntime {
    let mut config = plan_team();
    config.modes.plan = Some(PlanModeConfig {
        builder: Some(MemberId::new("builder")),
        reviewer: Some(MemberId::new("reviewer")),
        ..PlanModeConfig::default()
    });
    TeamRuntime::new(config, SqliteStore::in_memory().unwrap()).with_approvals(false)
}

pub(super) fn run_plan(task: &str) -> UiCommand {
    UiCommand::RunMode {
        mode: CollabMode::Plan,
        task: task.to_string(),
    }
}

pub(super) fn run_brainstorm(task: &str) -> UiCommand {
    UiCommand::RunMode {
        mode: CollabMode::Brainstorm,
        task: task.to_string(),
    }
}

pub(super) fn complete_all(rt: &mut TeamRuntime, members: &[(MemberId, &str)]) -> RuntimeStep {
    let mut merged = RuntimeStep::default();
    for (member, text) in members {
        let step = complete_ok(rt, member, text);
        merged.events.extend(step.events);
        merged.actions.extend(step.actions);
        merged.verify_actions.extend(step.verify_actions);
        merged.runner_changes.extend(step.runner_changes);
        if step.persist_team.is_some() {
            merged.persist_team = step.persist_team;
        }
    }
    merged
}

mod gates;
mod mode_overrides;
mod plan;
mod review;
mod roster;
mod turns;
