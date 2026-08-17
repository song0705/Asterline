use super::super::*;
use super::*;

fn two_member_team() -> TeamConfig {
    let mut config = TeamConfig::new("ab", "/tmp/ws")
        .with_member(TeamMember::new("a", "A", BackendKind::Codex, "impl"))
        .with_member(TeamMember::new("b", "B", BackendKind::Claude, "review"));
    config.default_target = Some(DefaultTarget::Member(MemberId::new("a")));
    config
}

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
fn relay_with_tool_looking_body_dispatches_immediately() {
    let mut rt = TeamRuntime::new(two_member_team(), SqliteStore::in_memory().unwrap());
    let step = relay_after_user(&mut rt, "please run git status");

    assert!(
        !step
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::ApprovalRequested { .. })),
        "prompt keywords must not hold a relay: {:?}",
        step.events
    );
    let action = step
        .actions
        .iter()
        .find(|action| action.member == MemberId::new("b"))
        .expect("relay must dispatch to b");
    assert!(
        action.prompt.starts_with("[relay from"),
        "prompt should be relay-wrapped: {}",
        action.prompt
    );
    assert!(action.prompt.contains("please run git status"));
    assert!(
        action
            .prompt
            .contains(r#"@@team_message {"to":"a","kind":"reply""#),
        "ordinary relays must require a reply to their sender: {}",
        action.prompt
    );
    assert!(action.prompt.contains("Asterline team skill"));
    assert!(!action.prompt.contains("$asterline-team"));
}

#[test]
fn reply_relay_does_not_require_an_acknowledgement_loop() {
    let mut rt = TeamRuntime::new(two_member_team(), SqliteStore::in_memory().unwrap());
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
fn prompt_keywords_do_not_hold_user_or_relay_messages() {
    let mut config = two_member_team();
    config
        .approvals
        .keywords
        .insert("deploy".to_string(), vec!["kubectl".to_string()]);
    let mut rt = TeamRuntime::new(config, SqliteStore::in_memory().unwrap());

    let user = rt.on_ui_command(UiCommand::UserMessage {
        target: MessageTarget::Member(MemberId::new("a")),
        body: "kubectl apply now".to_string(),
    });
    assert!(
        !user
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::ApprovalRequested { .. }))
    );
    assert!(
        user.actions
            .iter()
            .any(|action| action.member == MemberId::new("a"))
    );

    let relay = relay_after_user(&mut rt, "please run git status");
    assert!(
        !relay
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::ApprovalRequested { .. }))
    );
    assert!(
        relay
            .actions
            .iter()
            .any(|action| action.member == MemberId::new("b"))
    );
}
