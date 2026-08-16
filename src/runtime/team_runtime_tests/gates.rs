use super::super::*;
use super::*;

fn two_member_team() -> TeamConfig {
    let mut config = TeamConfig::new("ab", "/tmp/ws")
        .with_member(TeamMember::new("a", "A", BackendKind::Codex, "impl"))
        .with_member(TeamMember::new("b", "B", BackendKind::Claude, "review"));
    config.default_target = Some(DefaultTarget::Member(MemberId::new("a")));
    config
}

/// Drive a non-risky user message to member A, then have A emit a team_message.
fn relay_after_user(rt: &mut TeamRuntime, body: &str) -> RuntimeStep {
    let a = MemberId::new("a");
    rt.on_ui_command(UiCommand::UserMessage {
        target: MessageTarget::Member(a.clone()),
        body: "please coordinate with b".to_string(),
    });
    rt.on_agent_event(
        &a,
        AgentEvent::MessageCompleted(format!(r#"@@team_message {{"to":"b","body":"{body}"}}"#)),
    )
}

#[test]
fn relay_with_risky_body_requires_approval() {
    let mut rt = TeamRuntime::new(two_member_team(), SqliteStore::in_memory().unwrap());
    let step = relay_after_user(&mut rt, "please run git status");

    assert!(
        step.events.iter().any(|e| matches!(
            e,
            RuntimeEvent::ApprovalRequested {
                member: Some(m),
                action,
                body,
                ..
            } if m.as_str() == "a" && action == "git" && body == "please run git status"
        )),
        "risky relay must request approval from the sender: {step_events:?}",
        step_events = step.events
    );
    assert!(
        !step.actions.iter().any(|a| a.member == MemberId::new("b")),
        "no RunAction for b while approval is held"
    );
}

#[test]
fn approved_relay_dispatches_wrapped_prompt() {
    let mut rt = TeamRuntime::new(two_member_team(), SqliteStore::in_memory().unwrap());
    let step = relay_after_user(&mut rt, "please run git status");
    let id = step
        .events
        .iter()
        .find_map(|e| match e {
            RuntimeEvent::ApprovalRequested { id, .. } => Some(*id),
            _ => None,
        })
        .expect("approval requested");

    let step = rt.on_ui_command(UiCommand::Approve {
        id,
        decision: ApprovalDecision::Approve,
    });
    let action = step
        .actions
        .iter()
        .find(|a| a.member == MemberId::new("b"))
        .expect("approved relay must dispatch to b");
    assert!(
        action.prompt.starts_with("[relay from"),
        "prompt should be relay-wrapped: {}",
        action.prompt
    );
    assert!(
        action.prompt.contains("please run git status"),
        "prompt should contain original body: {}",
        action.prompt
    );
    assert!(
        action
            .prompt
            .contains(r#"@@team_message {"to":"a","kind":"reply""#),
        "ordinary relays must require a reply to their sender: {}",
        action.prompt
    );
}

#[test]
fn reply_relay_does_not_require_an_acknowledgement_loop() {
    let mut rt = TeamRuntime::new(two_member_team(), SqliteStore::in_memory().unwrap())
        .with_approvals(false);
    let a = MemberId::new("a");
    let b = MemberId::new("b");
    rt.on_ui_command(UiCommand::UserMessage {
        target: MessageTarget::Member(a.clone()),
        body: "coordinate with b".to_string(),
    });

    let step = rt.on_agent_event(
        &a,
        AgentEvent::MessageCompleted(
            r#"@@team_message {"to":"b","kind":"reply","body":"done"}"#.to_string(),
        ),
    );
    let action = step
        .actions
        .iter()
        .find(|action| action.member == b)
        .expect("reply should still be delivered");

    assert!(action.prompt.contains("marked as a reply"));
    assert!(!action.prompt.contains("MUST answer the sender"));
}

#[test]
fn rejected_relay_finishes_turn() {
    let mut rt = TeamRuntime::new(two_member_team(), SqliteStore::in_memory().unwrap());
    let a = MemberId::new("a");
    let step = relay_after_user(&mut rt, "please run git status");
    let id = step
        .events
        .iter()
        .find_map(|e| match e {
            RuntimeEvent::ApprovalRequested { id, .. } => Some(*id),
            _ => None,
        })
        .expect("approval requested");

    // A finishes so the held approval is the only thing keeping the turn alive.
    rt.on_agent_event(
        &a,
        AgentEvent::Exited {
            code: Some(0),
            ok: true,
        },
    );

    let step = rt.on_ui_command(UiCommand::Approve {
        id,
        decision: ApprovalDecision::Reject,
    });
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::Notice(text) if text.contains("request rejected")
    )));
    assert!(
        step.events
            .iter()
            .any(|e| matches!(e, RuntimeEvent::TurnFinished { .. })),
        "rejecting the only pending work should finish the turn: {:?}",
        step.events
    );
    assert!(
        !step.actions.iter().any(|a| a.member == MemberId::new("b")),
        "reject must not dispatch to b"
    );
}

#[test]
fn relay_gate_respects_apply_to() {
    let mut config = two_member_team();
    config.approvals.apply_to = Some(vec![ApprovalSurface::User]);
    let mut rt = TeamRuntime::new(config, SqliteStore::in_memory().unwrap());

    let step = relay_after_user(&mut rt, "please run git status");
    assert!(
        !step
            .events
            .iter()
            .any(|e| matches!(e, RuntimeEvent::ApprovalRequested { .. })),
        "relay surface not in apply_to must skip the gate"
    );
    assert!(
        step.actions.iter().any(|a| a.member == MemberId::new("b")),
        "risky relay must dispatch immediately when only User is gated"
    );
}

#[test]
fn custom_keyword_category_gates_user_message() {
    let mut config = two_member_team();
    config
        .approvals
        .keywords
        .insert("deploy".to_string(), vec!["kubectl".to_string()]);
    let mut rt = TeamRuntime::new(config, SqliteStore::in_memory().unwrap());

    let step = rt.on_ui_command(UiCommand::UserMessage {
        target: MessageTarget::Member(MemberId::new("a")),
        body: "kubectl apply now".to_string(),
    });
    assert!(step.events.iter().any(|e| matches!(
        e,
        RuntimeEvent::ApprovalRequested { action, body, .. }
            if action == "deploy" && body == "kubectl apply now"
    )));
    assert!(step.actions.is_empty(), "custom keyword must gate the run");
}

#[test]
fn debug_mode_disables_all_gates() {
    let mut rt = TeamRuntime::new(two_member_team(), SqliteStore::in_memory().unwrap())
        .with_approvals(false);

    let step = rt.on_ui_command(UiCommand::UserMessage {
        target: MessageTarget::Member(MemberId::new("a")),
        body: "run git push origin main".to_string(),
    });
    assert!(
        !step
            .events
            .iter()
            .any(|e| matches!(e, RuntimeEvent::ApprovalRequested { .. }))
    );
    assert!(
        step.actions.iter().any(|a| a.member == MemberId::new("a")),
        "risky user message must dispatch when approvals are disabled"
    );

    let step = rt.on_agent_event(
        &MemberId::new("a"),
        AgentEvent::MessageCompleted(
            r#"@@team_message {"to":"b","body":"please run git status"}"#.to_string(),
        ),
    );
    assert!(
        !step
            .events
            .iter()
            .any(|e| matches!(e, RuntimeEvent::ApprovalRequested { .. }))
    );
    assert!(
        step.actions.iter().any(|a| a.member == MemberId::new("b")),
        "risky relay must dispatch when approvals are disabled"
    );
}
