use super::super::*;
use super::*;
use crate::domain::event::FileChangeItem;

#[test]
fn pure_conversation_does_not_show_work_separator() {
    let state = AppState::new(vec![
        ChatItem::User {
            body: "explain this function".to_string(),
            targets: vec![MemberId::new("builder")],
            interrupted: Vec::new(),
        },
        ChatItem::Agent {
            member: MemberId::new("builder"),
            display_name: "Builder".to_string(),
            backend: BackendKind::Codex,
            text: "It parses the request.".to_string(),
        },
    ]);
    let mut lines = Vec::new();

    render_chat_history(&state, 40, 0, &mut lines);

    let text = plain_text(&lines);
    assert!(!text.iter().any(|line| is_separator_text(line)));
    assert!(text.iter().any(|line| line.contains("◆ You")));
    assert!(
        text.iter()
            .any(|line| line.contains("explain this function"))
    );
}

#[test]
fn consecutive_agent_messages_keep_a_title_on_each_reply() {
    let state = AppState::new(vec![
        ChatItem::Agent {
            member: MemberId::new("builder"),
            display_name: "Builder".to_string(),
            backend: BackendKind::Codex,
            text: "first reply".to_string(),
        },
        ChatItem::Agent {
            member: MemberId::new("builder"),
            display_name: "Builder".to_string(),
            backend: BackendKind::Codex,
            text: "second reply".to_string(),
        },
        ChatItem::Agent {
            member: MemberId::new("reviewer"),
            display_name: "Reviewer".to_string(),
            backend: BackendKind::Claude,
            text: "review reply".to_string(),
        },
    ]);
    let mut lines = Vec::new();

    render_chat_history(&state, 60, 0, &mut lines);

    let text = plain_text(&lines);
    let builder_headers = text
        .iter()
        .filter(|line| line.contains("Builder") && line.contains("codex"))
        .count();
    let reviewer_headers = text
        .iter()
        .filter(|line| line.contains("Reviewer") && line.contains("claude"))
        .count();
    assert_eq!(builder_headers, 2);
    assert_eq!(reviewer_headers, 1);
    assert!(text.iter().any(|line| line.contains("first reply")));
    assert!(text.iter().any(|line| line.contains("second reply")));
    let first = text
        .iter()
        .position(|line| line.contains("first reply"))
        .unwrap();
    let second = text
        .iter()
        .position(|line| line.contains("second reply"))
        .unwrap();
    assert!(first < second);
    assert!(
        text[first + 1..second]
            .iter()
            .any(|line| line.trim().is_empty()),
        "consecutive replies must not glue together: {text:?}"
    );
}

#[test]
fn same_member_reply_after_work_keeps_its_header_and_has_a_rail_gap() {
    let member = MemberId::new("builder");
    let state = AppState::new(vec![
        ChatItem::Tool {
            member: member.clone(),
            name: "shell".to_string(),
            summary: "cargo test".to_string(),
            detail: "ok".to_string(),
            ok: Some(true),
        },
        ChatItem::Agent {
            member,
            display_name: "Builder".to_string(),
            backend: BackendKind::Codex,
            text: "Tests passed.".to_string(),
        },
    ]);
    let mut lines = Vec::new();

    render_chat_history(&state, 80, 0, &mut lines);

    let text = plain_text(&lines);
    let headers = text
        .iter()
        .filter(|line| line.contains("Builder") && line.contains("codex"))
        .count();
    assert_eq!(
        headers, 1,
        "work and its reply must share one header: {text:?}"
    );
    let tool = text
        .iter()
        .position(|line| line.contains("cargo test"))
        .expect("tool row");
    let reply = text
        .iter()
        .enumerate()
        .skip(tool + 1)
        .find_map(|(index, line)| line.contains("Tests passed.").then_some(index))
        .expect("reply");
    assert_eq!(reply, tool + 2, "the reply needs one rail gap: {text:?}");
    assert!(text[tool + 1].trim().is_empty());
    assert_eq!(
        lines[tool + 1].spans.first().and_then(|span| span.style.bg),
        Some(member_rail_color(&state, &MemberId::new("builder"))),
        "the gap must preserve the member rail",
    );
}

#[test]
fn concurrent_members_keep_each_members_work_together() {
    use crate::domain::event::{MessageId, TurnId};

    let mut state = AppState::new(Vec::new());
    state.apply(RuntimeEvent::Ready {
        modes: Default::default(),
        mode_overrides: Default::default(),
        suggested_verify: None,
        team: "t".to_string(),
        workspace: String::new(),
        default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
        runs: Vec::new(),
        members: vec![
            member_summary(
                "builder",
                "Builder",
                BackendKind::Codex,
                "impl",
                MemberStatus::Idle,
            ),
            member_summary(
                "planer",
                "Planer",
                BackendKind::Claude,
                "plan",
                MemberStatus::Idle,
            ),
        ],
    });
    state.apply(RuntimeEvent::UserMessage {
        turn: TurnId(1),
        targets: vec![MemberId::new("builder")],
        body: "fix the parser".to_string(),
    });
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(1),
        turn: TurnId(1),
        member: MemberId::new("builder"),
    });
    state.apply(RuntimeEvent::MessageCompleted {
        msg: MessageId(1),
        text: "builder started".to_string(),
    });
    state.apply(RuntimeEvent::MemberStatus {
        member: MemberId::new("builder"),
        status: MemberStatus::Running,
    });
    state.apply(RuntimeEvent::UserMessage {
        turn: TurnId(2),
        targets: vec![MemberId::new("planer")],
        body: "draft the plan".to_string(),
    });
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(2),
        turn: TurnId(2),
        member: MemberId::new("planer"),
    });
    state.apply(RuntimeEvent::MessageDelta {
        msg: MessageId(2),
        text: "planer started".to_string(),
    });
    state.apply(RuntimeEvent::ToolStarted {
        member: MemberId::new("builder"),
        tool_id: "b1".to_string(),
        name: "shell".to_string(),
        summary: "cargo test".to_string(),
    });
    state.apply(RuntimeEvent::ToolCompleted {
        member: MemberId::new("builder"),
        tool_id: "b1".to_string(),
        ok: true,
        output: "ok".to_string(),
    });
    state.apply(RuntimeEvent::MessageCompleted {
        msg: MessageId(1),
        text: "builder started\nbuilder done".to_string(),
    });
    state.apply(RuntimeEvent::ToolStarted {
        member: MemberId::new("planer"),
        tool_id: "p1".to_string(),
        name: "read_file".to_string(),
        summary: "docs".to_string(),
    });
    state.apply(RuntimeEvent::MessageCompleted {
        msg: MessageId(2),
        text: "planer started\nplaner done".to_string(),
    });

    let mut lines = Vec::new();
    render_chat_history(&state, 80, 0, &mut lines);
    let text = plain_text(&lines);
    let joined = text.join("\n");
    let builder_start = joined.find("builder started").expect(&joined);
    let builder_tool = joined.find("cargo test").expect(&joined);
    let plan_prompt = joined.find("draft the plan").expect(&joined);
    let planer_start = joined.find("planer started").expect(&joined);
    let planer_tool = joined.find("docs").expect(&joined);
    assert!(
        joined.contains("You → Builder") && joined.contains("You → Planer"),
        "{joined}"
    );
    assert!(
        builder_start < builder_tool
            && builder_tool < plan_prompt
            && plan_prompt < planer_start
            && planer_start < planer_tool,
        "{joined}"
    );
}

#[test]
fn incoming_relay_starts_a_fresh_target_work_block() {
    use crate::domain::event::{MessageId, TurnId};

    let planner = MemberId::new("planer");
    let builder = MemberId::new("builder");
    let mut state = AppState::new(Vec::new());
    state.apply(RuntimeEvent::Ready {
        modes: Default::default(),
        mode_overrides: Default::default(),
        suggested_verify: None,
        team: "t".to_string(),
        workspace: String::new(),
        default_target: Some(DefaultTarget::Member(builder.clone())),
        runs: Vec::new(),
        members: vec![
            member_summary(
                "builder",
                "Builder",
                BackendKind::Codex,
                "implementation",
                MemberStatus::Idle,
            ),
            member_summary(
                "planer",
                "Planer",
                BackendKind::Claude,
                "planning",
                MemberStatus::Idle,
            ),
        ],
    });
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(1),
        turn: TurnId(1),
        member: planner.clone(),
    });
    state.apply(RuntimeEvent::MessageCompleted {
        msg: MessageId(1),
        text: "planner review".to_string(),
    });
    state.apply(RuntimeEvent::Route {
        turn: TurnId(1),
        from: planner.clone(),
        to: vec![builder.to_string()],
        body: "review delivered".to_string(),
    });
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(2),
        turn: TurnId(1),
        member: builder.clone(),
    });
    state.apply(RuntimeEvent::MessageCompleted {
        msg: MessageId(2),
        text: "builder received review".to_string(),
    });
    state.apply(RuntimeEvent::Route {
        turn: TurnId(1),
        from: builder,
        to: vec![planner.to_string()],
        body: "implement the follow-up".to_string(),
    });
    state.apply(RuntimeEvent::ToolStarted {
        member: planner,
        tool_id: "p1".to_string(),
        name: "shell".to_string(),
        summary: "planner implementation work".to_string(),
    });

    let mut lines = Vec::new();
    render_chat_history(&state, 80, 0, &mut lines);
    let text = plain_text(&lines);
    let joined = text.join("\n");
    let review = joined.find("planner review").expect(&joined);
    let planner_to_builder = joined.find("planer → builder").expect(&joined);
    let builder_reply = joined.find("builder received review").expect(&joined);
    let builder_to_planner = joined.find("builder → planer").expect(&joined);
    let planner_work = joined.find("planner implementation work").expect(&joined);

    assert!(
        review < planner_to_builder
            && planner_to_builder < builder_reply
            && builder_reply < builder_to_planner
            && builder_to_planner < planner_work,
        "incoming relays must preserve the real handoff order:\n{joined}"
    );
    assert_eq!(
        text.iter()
            .filter(|line| line.contains("Planer  · claude"))
            .count(),
        2,
        "the planner's post-relay work needs a new titled rail:\n{joined}"
    );
}

#[test]
fn broadcast_keeps_each_members_search_in_its_own_block() {
    use crate::domain::event::{MessageId, TurnId};

    let mut state = AppState::new(Vec::new());
    let builder = MemberId::new("builder");
    let planer = MemberId::new("planer");
    let reviewer = MemberId::new("reviewer");
    state.apply(RuntimeEvent::Ready {
        modes: Default::default(),
        mode_overrides: Default::default(),
        suggested_verify: None,
        team: "t".to_string(),
        workspace: String::new(),
        default_target: Some(DefaultTarget::Member(builder.clone())),
        runs: Vec::new(),
        members: vec![
            member_summary(
                "builder",
                "Builder",
                BackendKind::Codex,
                "impl",
                MemberStatus::Idle,
            ),
            member_summary(
                "planer",
                "Planer",
                BackendKind::Claude,
                "plan",
                MemberStatus::Idle,
            ),
            member_summary(
                "reviewer",
                "Reviewer",
                BackendKind::Grok,
                "review",
                MemberStatus::Idle,
            ),
        ],
    });
    state.apply(RuntimeEvent::UserMessage {
        turn: TurnId(1),
        targets: vec![builder.clone(), planer.clone(), reviewer.clone()],
        body: "search the repo".to_string(),
    });
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(1),
        turn: TurnId(1),
        member: builder.clone(),
    });
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(2),
        turn: TurnId(1),
        member: planer.clone(),
    });
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(3),
        turn: TurnId(1),
        member: reviewer.clone(),
    });
    state.apply(RuntimeEvent::ToolStarted {
        member: builder.clone(),
        tool_id: "b1".to_string(),
        name: "grep".to_string(),
        summary: "parser".to_string(),
    });
    state.apply(RuntimeEvent::ToolStarted {
        member: reviewer.clone(),
        tool_id: "r1".to_string(),
        name: "find".to_string(),
        summary: "review notes".to_string(),
    });
    state.apply(RuntimeEvent::ToolStarted {
        member: planer.clone(),
        tool_id: "p1".to_string(),
        name: "read_file".to_string(),
        summary: "plan.md".to_string(),
    });
    state.apply(RuntimeEvent::MessageCompleted {
        msg: MessageId(1),
        text: "builder found it".to_string(),
    });
    state.apply(RuntimeEvent::MessageCompleted {
        msg: MessageId(3),
        text: "reviewer found it".to_string(),
    });
    state.apply(RuntimeEvent::MessageCompleted {
        msg: MessageId(2),
        text: "planer found it".to_string(),
    });

    let mut lines = Vec::new();
    render_chat_history(&state, 80, 0, &mut lines);
    let joined = plain_text(&lines).join("\n");
    assert!(joined.contains("You → all"), "{joined}");
    let mut labeled = [
        (joined.find("builder found it").expect(&joined), 'b'),
        (joined.find("parser").expect(&joined), 'b'),
        (joined.find("planer found it").expect(&joined), 'p'),
        (joined.find("plan.md").expect(&joined), 'p'),
        (joined.find("reviewer found it").expect(&joined), 'r'),
        (joined.find("review notes").expect(&joined), 'r'),
    ];
    labeled.sort_by_key(|(pos, _)| *pos);
    let owners: String = labeled.iter().map(|(_, owner)| *owner).collect();
    assert!(
        owners == "bbpprr"
            || owners == "bbrrpp"
            || owners == "ppbbrr"
            || owners == "pprrbb"
            || owners == "rrbbpp"
            || owners == "rrppbb",
        "members should stay in contiguous blocks, got {owners}\n{joined}"
    );
}

#[test]
fn sequential_turns_stay_chronological_when_nobody_else_is_working() {
    use crate::domain::event::{MessageId, TurnId};

    let mut state = AppState::new(Vec::new());
    state.apply(RuntimeEvent::Ready {
        modes: Default::default(),
        mode_overrides: Default::default(),
        suggested_verify: None,
        team: "t".to_string(),
        workspace: String::new(),
        default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
        runs: Vec::new(),
        members: vec![
            member_summary(
                "builder",
                "Builder",
                BackendKind::Codex,
                "impl",
                MemberStatus::Idle,
            ),
            member_summary(
                "planer",
                "Planer",
                BackendKind::Claude,
                "plan",
                MemberStatus::Idle,
            ),
        ],
    });
    state.apply(RuntimeEvent::UserMessage {
        turn: TurnId(1),
        targets: vec![MemberId::new("builder")],
        body: "first job".to_string(),
    });
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(1),
        turn: TurnId(1),
        member: MemberId::new("builder"),
    });
    state.apply(RuntimeEvent::MessageCompleted {
        msg: MessageId(1),
        text: "builder finished first".to_string(),
    });
    state.apply(RuntimeEvent::MemberStatus {
        member: MemberId::new("builder"),
        status: MemberStatus::Idle,
    });
    state.apply(RuntimeEvent::UserMessage {
        turn: TurnId(2),
        targets: vec![MemberId::new("planer")],
        body: "second job".to_string(),
    });
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(2),
        turn: TurnId(2),
        member: MemberId::new("planer"),
    });
    state.apply(RuntimeEvent::MessageCompleted {
        msg: MessageId(2),
        text: "planer reply".to_string(),
    });
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(3),
        turn: TurnId(1),
        member: MemberId::new("builder"),
    });
    state.apply(RuntimeEvent::MessageCompleted {
        msg: MessageId(3),
        text: "late builder note".to_string(),
    });

    let mut lines = Vec::new();
    render_chat_history(&state, 80, 0, &mut lines);
    let joined = plain_text(&lines).join("\n");
    let first = joined.find("builder finished first").expect(&joined);
    let second_prompt = joined.find("second job").expect(&joined);
    let planer = joined.find("planer reply").expect(&joined);
    let late = joined.find("late builder note").expect(&joined);
    assert!(
        first < second_prompt && second_prompt < planer && planer < late,
        "{joined}"
    );
}

#[test]
fn tool_block_introduces_its_member_before_the_tool_and_final_reply() {
    let mut state = AppState::new(Vec::new());
    state.apply(RuntimeEvent::Ready {
        modes: Default::default(),
        mode_overrides: Default::default(),
        suggested_verify: None,
        team: "t".to_string(),
        workspace: String::new(),
        default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
        runs: Vec::new(),
        members: vec![member_summary(
            "builder",
            "Builder",
            BackendKind::Grok,
            "implementation",
            MemberStatus::Idle,
        )],
    });
    state.apply(RuntimeEvent::ToolStarted {
        member: MemberId::new("builder"),
        tool_id: "read-1".to_string(),
        name: "read_file".to_string(),
        summary: "src/lib.rs".to_string(),
    });
    state.apply(RuntimeEvent::ToolCompleted {
        member: MemberId::new("builder"),
        tool_id: "read-1".to_string(),
        ok: true,
        output: "ok".to_string(),
    });
    state.apply(RuntimeEvent::MessageStarted {
        msg: crate::domain::event::MessageId(1),
        turn: crate::domain::event::TurnId(1),
        member: MemberId::new("builder"),
    });
    state.apply(RuntimeEvent::MessageCompleted {
        msg: crate::domain::event::MessageId(1),
        text: "final reply".to_string(),
    });

    let mut lines = Vec::new();
    render_chat_history(&state, 70, 0, &mut lines);
    let text = plain_text(&lines);
    let header = text
        .iter()
        .position(|line| line.contains("Builder") && line.contains("grok"))
        .expect("member header");
    let tool = text
        .iter()
        .position(|line| line.contains("src/lib.rs") || line.contains("read"))
        .expect("tool line");
    let reply = text
        .iter()
        .position(|line| line.contains("final reply"))
        .expect("final reply");
    assert!(header < tool && tool < reply, "{text:?}");
    assert!(
        text.iter()
            .filter(|line| line.contains("Builder") && line.contains("grok"))
            .count()
            >= 1
    );
}

#[test]
fn split_turn_keeps_tools_between_preamble_and_final_reply() {
    use crate::domain::event::{MessageId, TurnId};

    let mut state = AppState::new(Vec::new());
    state.apply(RuntimeEvent::Ready {
        modes: Default::default(),
        mode_overrides: Default::default(),
        suggested_verify: None,
        team: "t".to_string(),
        workspace: String::new(),
        default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
        runs: Vec::new(),
        members: vec![member_summary(
            "builder",
            "Builder",
            BackendKind::Grok,
            "implementation",
            MemberStatus::Idle,
        )],
    });
    state.apply(RuntimeEvent::UserMessage {
        turn: TurnId(1),
        targets: vec![MemberId::new("builder")],
        body: "what is asterline".to_string(),
    });
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(1),
        turn: TurnId(1),
        member: MemberId::new("builder"),
    });
    state.apply(RuntimeEvent::MessageCompleted {
        msg: MessageId(1),
        text: "I'll look it up.".to_string(),
    });
    state.apply(RuntimeEvent::ToolStarted {
        member: MemberId::new("builder"),
        tool_id: "search-1".to_string(),
        name: "search".to_string(),
        summary: "asterline".to_string(),
    });
    state.apply(RuntimeEvent::ToolCompleted {
        member: MemberId::new("builder"),
        tool_id: "search-1".to_string(),
        ok: true,
        output: "hits".to_string(),
    });
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(2),
        turn: TurnId(1),
        member: MemberId::new("builder"),
    });
    state.apply(RuntimeEvent::MessageCompleted {
        msg: MessageId(2),
        text: "Asterline is a team TUI.".to_string(),
    });

    let mut lines = Vec::new();
    render_chat_history(&state, 80, 0, &mut lines);
    let joined = plain_text(&lines).join("\n");
    let preamble = joined.find("I'll look it up.").expect(&joined);
    let tool = joined.find("Search").expect(&joined);
    let reply = joined.find("Asterline is a team TUI.").expect(&joined);
    assert!(preamble < tool && tool < reply, "{joined}");
}

#[test]
fn empty_closed_agent_does_not_hide_the_following_tool_header() {
    use crate::domain::event::{MessageId, TurnId};

    let mut state = AppState::new(Vec::new());
    state.apply(RuntimeEvent::Ready {
        modes: Default::default(),
        mode_overrides: Default::default(),
        suggested_verify: None,
        team: "t".to_string(),
        workspace: String::new(),
        default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
        runs: Vec::new(),
        members: vec![member_summary(
            "builder",
            "Builder",
            BackendKind::Grok,
            "implementation",
            MemberStatus::Idle,
        )],
    });
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(1),
        turn: TurnId(1),
        member: MemberId::new("builder"),
    });
    state.apply(RuntimeEvent::MessageCompleted {
        msg: MessageId(1),
        text: String::new(),
    });
    state.apply(RuntimeEvent::ToolStarted {
        member: MemberId::new("builder"),
        tool_id: "search-1".to_string(),
        name: "search".to_string(),
        summary: "asterline".to_string(),
    });
    state.apply(RuntimeEvent::ToolCompleted {
        member: MemberId::new("builder"),
        tool_id: "search-1".to_string(),
        ok: true,
        output: String::new(),
    });
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(2),
        turn: TurnId(1),
        member: MemberId::new("builder"),
    });
    state.apply(RuntimeEvent::MessageCompleted {
        msg: MessageId(2),
        text: "Asterline is a team TUI.".to_string(),
    });

    let mut lines = Vec::new();
    render_chat_history(&state, 80, 0, &mut lines);
    let text = plain_text(&lines);
    let header = text
        .iter()
        .position(|line| line.contains("Builder") && line.contains("grok"))
        .expect("member header");
    let tool = text
        .iter()
        .position(|line| line.contains("Search"))
        .expect("tool");
    let reply = text
        .iter()
        .position(|line| line.contains("Asterline is a team TUI."))
        .expect("reply");
    assert!(header < tool && tool < reply, "{text:?}");
}

#[test]
fn member_activity_uses_one_full_height_unbroken_rail() {
    let member = MemberId::new("builder");
    let state = AppState::new(vec![
        ChatItem::Agent {
            member: member.clone(),
            display_name: "Builder".to_string(),
            backend: BackendKind::Codex,
            text: "checking now".to_string(),
        },
        ChatItem::Tool {
            member: member.clone(),
            name: "shell".to_string(),
            summary: "cargo test".to_string(),
            detail: "test result: ok".to_string(),
            ok: Some(true),
        },
        ChatItem::Diff {
            member: member.clone(),
            files: vec![FileChangeItem::new("src/lib.rs", "modify")],
            ok: true,
        },
        ChatItem::Error {
            member: Some(member),
            message: "follow-up failed".to_string(),
        },
    ]);
    let mut lines = Vec::new();

    render_chat_history(&state, 70, 0, &mut lines);

    let text = plain_text(&lines);
    let start = text
        .iter()
        .position(|line| line.contains("checking now"))
        .unwrap();
    let end = text
        .iter()
        .position(|line| line.contains("follow-up failed"))
        .unwrap();
    assert!(text[start..=end].iter().all(|line| !line.trim().is_empty()));
    assert!(lines[start..=end].iter().all(|line| {
        line.spans.first().is_some_and(|span| {
            span.content.as_ref() == " "
                && span.style.bg == Some(theme::backend_color(BackendKind::Codex))
        })
    }));

    let rail_lines = lines[start..=end].to_vec();
    let height = u16::try_from(rail_lines.len()).unwrap();
    let mut terminal = Terminal::new(TestBackend::new(70, height)).unwrap();
    terminal
        .draw(|frame| {
            frame.render_widget(Paragraph::new(rail_lines.clone()), frame.area());
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    for y in 0..height {
        assert_eq!(
            buffer.cell((0, y)).unwrap().bg,
            theme::backend_color(BackendKind::Codex),
            "rail cell at row {y} must have a full-cell background"
        );
    }
}

#[test]
fn reasoning_status_stays_on_the_current_turn_then_becomes_history() {
    use crate::domain::event::{MessageId, TurnId};

    let mut state = AppState::new(Vec::new());
    let planer = MemberId::new("planer");
    state.apply(RuntimeEvent::Ready {
        modes: Default::default(),
        mode_overrides: Default::default(),
        suggested_verify: None,
        team: "t".to_string(),
        workspace: String::new(),
        default_target: Some(DefaultTarget::Member(planer.clone())),
        runs: Vec::new(),
        members: vec![member_summary(
            "planer",
            "Planer",
            BackendKind::Codex,
            "plan",
            MemberStatus::Idle,
        )],
    });
    state.apply(RuntimeEvent::UserMessage {
        turn: TurnId(1),
        targets: vec![planer.clone()],
        body: "你好".to_string(),
    });
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(1),
        turn: TurnId(1),
        member: planer.clone(),
    });
    state.apply(RuntimeEvent::MessageCompleted {
        msg: MessageId(1),
        text: "你好！我已就绪。".to_string(),
    });
    state.apply(RuntimeEvent::MemberStatus {
        member: planer.clone(),
        status: MemberStatus::Idle,
    });
    state.apply(RuntimeEvent::UserMessage {
        turn: TurnId(2),
        targets: vec![planer.clone()],
        body: "你有看到附加提示词吗".to_string(),
    });
    state.apply(RuntimeEvent::MemberStatus {
        member: planer.clone(),
        status: MemberStatus::Running,
    });
    state.apply(RuntimeEvent::Reasoning {
        member: planer.clone(),
        text: "**Investigating Asterline prompt injection methods**".to_string(),
    });

    let mut lines = Vec::new();
    render_chat_history(&state, 80, 0, &mut lines);
    let joined = plain_text(&lines).join("\n");
    let greeting = joined.find("你好！我已就绪。").expect(&joined);
    let follow_up = joined.find("你有看到附加提示词吗").expect(&joined);
    let thinking = joined.find("prompt injection").expect(&joined);
    assert!(
        greeting < follow_up && follow_up < thinking,
        "live reasoning status must follow the new prompt, not the earlier reply\n{joined}"
    );
    assert_eq!(joined.matches("prompt injection").count(), 1, "{joined}");

    state.apply(RuntimeEvent::ToolStarted {
        member: planer.clone(),
        tool_id: "read-1".to_string(),
        name: "read_file".to_string(),
        summary: "SKILL.md".to_string(),
    });
    let mut lines = Vec::new();
    render_chat_history(&state, 80, 0, &mut lines);
    let joined = plain_text(&lines).join("\n");
    let greeting = joined.find("你好！我已就绪。").expect(&joined);
    let follow_up = joined.find("你有看到附加提示词吗").expect(&joined);
    let tool = joined.find("SKILL.md").expect(&joined);
    assert!(greeting < follow_up && follow_up < tool, "{joined}");
    assert!(!joined.contains("prompt injection"), "{joined}");
    assert!(
        !state
            .chat()
            .iter()
            .any(|item| matches!(item, ChatItem::Thinking { .. }))
    );
}

#[test]
fn reasoning_status_stays_with_its_member_then_disappears() {
    use crate::domain::event::{MessageId, TurnId};

    let mut state = AppState::new(Vec::new());
    let builder = MemberId::new("builder");
    let planer = MemberId::new("planer");
    state.apply(RuntimeEvent::Ready {
        modes: Default::default(),
        mode_overrides: Default::default(),
        suggested_verify: None,
        team: "t".to_string(),
        workspace: String::new(),
        default_target: Some(DefaultTarget::Member(builder.clone())),
        runs: Vec::new(),
        members: vec![
            member_summary(
                "builder",
                "Builder",
                BackendKind::Codex,
                "impl",
                MemberStatus::Idle,
            ),
            member_summary(
                "planer",
                "Planer",
                BackendKind::Claude,
                "plan",
                MemberStatus::Idle,
            ),
        ],
    });
    state.apply(RuntimeEvent::UserMessage {
        turn: TurnId(1),
        targets: vec![builder.clone()],
        body: "fix the parser".to_string(),
    });
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(1),
        turn: TurnId(1),
        member: builder.clone(),
    });
    state.apply(RuntimeEvent::MessageCompleted {
        msg: MessageId(1),
        text: "builder started".to_string(),
    });
    state.apply(RuntimeEvent::MemberStatus {
        member: builder.clone(),
        status: MemberStatus::Running,
    });
    state.apply(RuntimeEvent::Reasoning {
        member: builder.clone(),
        text: "**Checking parser invariants**".to_string(),
    });
    state.apply(RuntimeEvent::UserMessage {
        turn: TurnId(2),
        targets: vec![planer.clone()],
        body: "draft the plan".to_string(),
    });
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(2),
        turn: TurnId(2),
        member: planer.clone(),
    });
    state.apply(RuntimeEvent::MessageDelta {
        msg: MessageId(2),
        text: "planer started".to_string(),
    });
    let mut lines = Vec::new();
    render_chat_history(&state, 80, 0, &mut lines);
    let joined = plain_text(&lines).join("\n");
    let builder_start = joined.find("builder started").expect(&joined);
    let builder_think = joined.find("parser invariants").expect(&joined);
    let plan_prompt = joined.find("draft the plan").expect(&joined);
    let planer_start = joined.find("planer started").expect(&joined);
    assert!(
        builder_start < builder_think && builder_think < plan_prompt && plan_prompt < planer_start,
        "busy Builder reasoning status must stay in Builder's region\n{joined}"
    );
    assert_eq!(joined.matches("parser invariants").count(), 1, "{joined}");

    state.apply(RuntimeEvent::ToolStarted {
        member: builder.clone(),
        tool_id: "b1".to_string(),
        name: "shell".to_string(),
        summary: "cargo test".to_string(),
    });
    let mut lines = Vec::new();
    render_chat_history(&state, 80, 0, &mut lines);
    let joined = plain_text(&lines).join("\n");
    assert!(joined.contains("cargo test"), "{joined}");
    assert!(!joined.contains("parser invariants"), "{joined}");
    assert!(
        !state
            .chat()
            .iter()
            .any(|item| matches!(item, ChatItem::Thinking { .. }))
    );
}

#[test]
fn live_working_status_follows_that_members_last_message() {
    use crate::domain::event::TurnId;

    let mut state = AppState::new(Vec::new());
    let builder = MemberId::new("builder");
    let planer = MemberId::new("planer");
    state.apply(RuntimeEvent::Ready {
        modes: Default::default(),
        mode_overrides: Default::default(),
        suggested_verify: None,
        team: "t".to_string(),
        workspace: String::new(),
        default_target: Some(DefaultTarget::Member(builder.clone())),
        runs: Vec::new(),
        members: vec![
            member_summary(
                "builder",
                "Builder",
                BackendKind::Codex,
                "impl",
                MemberStatus::Idle,
            ),
            member_summary(
                "planer",
                "Planer",
                BackendKind::Claude,
                "plan",
                MemberStatus::Idle,
            ),
        ],
    });
    state.apply(RuntimeEvent::UserMessage {
        turn: TurnId(1),
        targets: vec![planer.clone()],
        body: "draft the plan".to_string(),
    });
    state.apply(RuntimeEvent::MessageStarted {
        msg: crate::domain::event::MessageId(1),
        turn: TurnId(1),
        member: planer.clone(),
    });
    state.apply(RuntimeEvent::MessageCompleted {
        msg: crate::domain::event::MessageId(1),
        text: "here is the plan outline".to_string(),
    });
    state.apply(RuntimeEvent::MemberStatus {
        member: planer.clone(),
        status: MemberStatus::Running,
    });
    state.apply(RuntimeEvent::UserMessage {
        turn: TurnId(2),
        targets: vec![builder.clone()],
        body: "implement step 1".to_string(),
    });
    state.apply(RuntimeEvent::MessageStarted {
        msg: crate::domain::event::MessageId(2),
        turn: TurnId(2),
        member: builder.clone(),
    });
    state.apply(RuntimeEvent::MessageCompleted {
        msg: crate::domain::event::MessageId(2),
        text: "builder started the change".to_string(),
    });

    let mut lines = Vec::new();
    render_chat_history(&state, 80, 0, &mut lines);
    let text = plain_text(&lines);
    let joined = text.join("\n");
    let plan = text
        .iter()
        .position(|line| line.contains("here is the plan outline"))
        .expect(&joined);
    let working = text
        .iter()
        .position(|line| line.contains("Working"))
        .expect(&joined);
    let builder_reply = text
        .iter()
        .position(|line| line.contains("builder started the change"))
        .expect(&joined);
    assert!(
        plan < working && working < builder_reply,
        "Working must sit on Planer's last message, not under Builder\n{joined}"
    );
    assert_eq!(
        text.iter().filter(|line| line.contains("Working")).count(),
        1,
        "{joined}"
    );
}

#[test]
fn legacy_thinking_is_hidden_without_hiding_tool_output() {
    let state = AppState::new(vec![
        ChatItem::Thinking {
            member: MemberId::new("builder"),
            display_name: "Builder".to_string(),
            backend: BackendKind::Codex,
            text: "secret scratch work".to_string(),
            elapsed_secs: Some(8),
        },
        ChatItem::Tool {
            member: MemberId::new("builder"),
            name: "shell".to_string(),
            summary: "cargo test".to_string(),
            detail: "this is huge tool output that must stay hidden".to_string(),
            ok: Some(true),
        },
    ]);
    let mut lines = Vec::new();
    render_chat_history(&state, 80, 0, &mut lines);
    let text = plain_text(&lines).join("\n");
    assert!(!text.contains("secret scratch work"), "{text}");
    assert!(!text.contains("thinking for 8s"), "{text}");
    assert!(text.contains("cargo test"), "{text}");
}

#[test]
fn reasoning_status_shows_a_spinner_then_disappears() {
    let builder = MemberId::new("builder");
    let mut state = AppState::new(Vec::new());
    state.apply(RuntimeEvent::Ready {
        modes: Default::default(),
        mode_overrides: Default::default(),
        suggested_verify: None,
        team: "t".to_string(),
        workspace: String::new(),
        default_target: Some(DefaultTarget::Member(builder.clone())),
        runs: Vec::new(),
        members: vec![member_summary(
            "builder",
            "Builder",
            BackendKind::Codex,
            "implementation",
            MemberStatus::Running,
        )],
    });
    state.apply(RuntimeEvent::Reasoning {
        member: builder.clone(),
        text: "**Looking at the parser**".to_string(),
    });
    let mut live = Vec::new();
    render_chat_history(&state, 80, 0, &mut live);
    let live = plain_text(&live).join("\n");
    assert!(live.contains("Thinking"), "{live}");
    assert!(live.contains("Looking at the parser"), "{live}");
    assert!(
        live.chars().any(|ch| "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏".contains(ch)),
        "live thinking needs a spinner: {live}"
    );
    assert!(!live.contains("Ctrl+T"), "{live}");

    state.apply(RuntimeEvent::ToolStarted {
        member: builder,
        tool_id: "t1".to_string(),
        name: "read_file".to_string(),
        summary: "src/lib.rs".to_string(),
    });
    let mut done = Vec::new();
    render_chat_history(&state, 80, 0, &mut done);
    let done = plain_text(&done).join("\n");
    assert!(done.contains("src/lib.rs"), "{done}");
    assert!(!done.contains("Looking at the parser"), "{done}");
    assert!(!done.contains("thinking for"), "{done}");
    assert!(!done.contains("Ctrl+T expand"), "{done}");
}

#[test]
fn active_tool_spinner_invalidates_the_cached_tail() {
    let state = AppState::new(vec![ChatItem::Tool {
        member: MemberId::new("builder"),
        name: "shell".to_string(),
        summary: "cargo test".to_string(),
        detail: String::new(),
        ok: None,
    }]);
    let tool = &state.chat()[0];

    assert_ne!(
        paint_item_revision(&state, tool, "⠋"),
        paint_item_revision(&state, tool, "⠙")
    );
}

#[test]
fn completed_reasoning_leaves_no_history_item_for_codex() {
    let builder = MemberId::new("builder");
    let mut state = AppState::new(Vec::new());
    state.apply(RuntimeEvent::Ready {
        modes: Default::default(),
        mode_overrides: Default::default(),
        suggested_verify: None,
        team: "t".to_string(),
        workspace: String::new(),
        default_target: Some(DefaultTarget::Member(builder.clone())),
        runs: Vec::new(),
        members: vec![member_summary(
            "builder",
            "Builder",
            BackendKind::Codex,
            "implementation",
            MemberStatus::Running,
        )],
    });
    state.apply(RuntimeEvent::Reasoning {
        member: builder.clone(),
        text: "**Checking the parser**\nsecret scratch work about the parser".to_string(),
    });
    state.apply(RuntimeEvent::ReasoningCompleted {
        member: builder.clone(),
    });

    let mut lines = Vec::new();
    render_chat_history(&state, 80, 0, &mut lines);
    let rendered = plain_text(&lines).join("\n");
    assert!(!rendered.contains("Checking the parser"), "{rendered}");
    assert!(!rendered.contains("secret scratch work"), "{rendered}");
    assert!(
        !state
            .chat()
            .iter()
            .any(|item| matches!(item, ChatItem::Thinking { .. }))
    );
}

#[test]
fn non_codex_reasoning_retains_collapsible_thinking_item() {
    let builder = MemberId::new("builder");
    let mut state = AppState::new(Vec::new());
    state.apply(RuntimeEvent::Ready {
        modes: Default::default(),
        mode_overrides: Default::default(),
        suggested_verify: None,
        team: "t".to_string(),
        workspace: String::new(),
        default_target: Some(DefaultTarget::Member(builder.clone())),
        runs: Vec::new(),
        members: vec![member_summary(
            "builder",
            "Builder",
            BackendKind::Claude,
            "implementation",
            MemberStatus::Running,
        )],
    });
    state.apply(RuntimeEvent::Reasoning {
        member: builder.clone(),
        text: "Thinking about refactoring the module".to_string(),
    });
    state.apply(RuntimeEvent::ReasoningCompleted {
        member: builder.clone(),
    });

    let mut lines = Vec::new();
    render_chat_history(&state, 80, 0, &mut lines);
    let rendered = plain_text(&lines).join("\n");
    assert!(rendered.contains("thinking"), "{rendered}");
    assert!(rendered.contains("Ctrl+T expand"), "{rendered}");
    assert!(
        state
            .chat()
            .iter()
            .any(|item| matches!(item, ChatItem::Thinking { .. }))
    );
}

#[test]
fn same_backend_members_use_different_rail_shades() {
    use crate::domain::event::TurnId;

    let mut state = AppState::new(Vec::new());
    let builder = MemberId::new("builder");
    let extra = MemberId::new("coder");
    state.apply(RuntimeEvent::Ready {
        modes: Default::default(),
        mode_overrides: Default::default(),
        suggested_verify: None,
        team: "t".to_string(),
        workspace: String::new(),
        default_target: Some(DefaultTarget::Member(builder.clone())),
        runs: Vec::new(),
        members: vec![
            member_summary(
                "builder",
                "Builder",
                BackendKind::Codex,
                "impl",
                MemberStatus::Idle,
            ),
            member_summary(
                "coder",
                "Coder",
                BackendKind::Codex,
                "impl",
                MemberStatus::Idle,
            ),
        ],
    });
    state.apply(RuntimeEvent::UserMessage {
        turn: TurnId(1),
        targets: vec![builder.clone(), extra.clone()],
        body: "go".to_string(),
    });
    state.apply(RuntimeEvent::ToolStarted {
        member: builder.clone(),
        tool_id: "b1".to_string(),
        name: "grep".to_string(),
        summary: "builder-tool".to_string(),
    });
    state.apply(RuntimeEvent::ToolStarted {
        member: extra,
        tool_id: "c1".to_string(),
        name: "grep".to_string(),
        summary: "coder-tool".to_string(),
    });

    let mut lines = Vec::new();
    render_chat_history(&state, 80, 0, &mut lines);
    let builder_rail = lines
        .iter()
        .find(|line| {
            plain_text(std::slice::from_ref(*line))
                .join("")
                .contains("builder-tool")
        })
        .and_then(|line| line.spans.first())
        .and_then(|span| span.style.bg);
    let coder_rail = lines
        .iter()
        .find(|line| {
            plain_text(std::slice::from_ref(*line))
                .join("")
                .contains("coder-tool")
        })
        .and_then(|line| line.spans.first())
        .and_then(|span| span.style.bg);
    assert_eq!(builder_rail, Some(theme::backend_color(BackendKind::Codex)));
    assert_eq!(
        coder_rail,
        Some(theme::backend_color_shaded(BackendKind::Codex, 1))
    );
    assert_ne!(builder_rail, coder_rail);
}

#[test]
fn unrostered_agent_uses_its_event_backend_color() {
    let builder = MemberId::new("builder");
    let state = AppState::new(vec![
        ChatItem::User {
            body: "check this".to_string(),
            targets: vec![builder.clone()],
            interrupted: Vec::new(),
        },
        ChatItem::Agent {
            member: builder.clone(),
            display_name: "Builder".to_string(),
            backend: BackendKind::Grok,
            text: "done".to_string(),
        },
    ]);
    let mut lines = Vec::new();
    render_chat_history(&state, 80, 0, &mut lines);

    assert_eq!(
        state.member_color(&builder),
        theme::backend_color(BackendKind::Grok)
    );
    let text = plain_text(&lines).join("\n");
    assert!(text.contains("You → Builder"), "{text}");
    let header = lines
        .iter()
        .find(|line| {
            plain_text(std::slice::from_ref(*line))
                .join("")
                .contains("Builder  · grok")
        })
        .expect("agent header");
    assert_eq!(
        header.spans.first().and_then(|span| span.style.bg),
        Some(theme::backend_color(BackendKind::Grok))
    );
}

#[test]
fn file_changes_expand_by_default_and_toggle_with_ctrl_g() {
    let mut state = AppState::new(vec![ChatItem::Diff {
        member: MemberId::new("builder"),
        files: vec![
            FileChangeItem::new("src/lib.rs", "update").with_texts(Some("old\n"), Some("new\n")),
        ],
        ok: true,
    }]);
    let mut expanded = Vec::new();
    render_chat_history(&state, 70, 0, &mut expanded);
    let expanded_text = plain_text(&expanded).join("\n");
    assert!(expanded_text.contains("Ctrl+G collapse"), "{expanded_text}");
    assert!(expanded_text.contains("1 -old"), "{expanded_text}");
    assert!(expanded_text.contains("1 +new"), "{expanded_text}");
    let path_line = expanded
        .iter()
        .find(|line| {
            plain_text(std::slice::from_ref(*line))
                .join("")
                .contains("src/lib.rs")
        })
        .expect("expanded file path");
    assert_eq!(
        path_line.spans.last().and_then(|span| span.style.fg),
        theme::warning().fg,
        "expanded file path must use the orange warning color"
    );
    assert_eq!(
        path_line.spans[1].style.fg,
        theme::text().fg,
        "the modified-file marker must use the white text color"
    );
    assert!(
        path_line.spans[1].content.contains("Edit"),
        "modified-file marker must say Edit: {path_line:?}"
    );

    for (content, expected) in [("-old", "1 -old"), ("+new", "1 +new")] {
        let line = expanded
            .iter()
            .find(|line| {
                plain_text(std::slice::from_ref(*line))
                    .join("")
                    .contains(content)
            })
            .unwrap_or_else(|| panic!("missing {content} in {expanded_text}"));
        assert!(
            line.spans[2].content.as_ref().starts_with(content),
            "the highlighted content must start at the change marker: {expected}"
        );
        assert!(
            line.spans[1].style.bg.is_none(),
            "line-number gutter is unfilled"
        );
        assert!(
            line.spans[2].style.bg.is_some(),
            "changed text is highlighted"
        );
        assert_eq!(
            theme::display_width(line.spans[2].content.as_ref()),
            70 - theme::display_width(line.spans[0].content.as_ref())
                - theme::display_width(line.spans[1].content.as_ref()),
            "the changed-content background must reach the right edge"
        );
    }

    state.toggle_diffs_expansion();
    let mut collapsed_lines = Vec::new();
    render_chat_history(&state, 70, 0, &mut collapsed_lines);
    let collapsed = plain_text(&collapsed_lines).join("\n");
    assert!(collapsed.contains("Ctrl+G expand"), "{collapsed}");
    assert!(
        !collapsed.contains("-old") && !collapsed.contains("+new"),
        "{collapsed}"
    );
    assert!(
        collapsed_lines
            .iter()
            .filter(|line| {
                plain_text(std::slice::from_ref(*line))
                    .join("")
                    .contains("file changes")
            })
            .all(|line| {
                line.spans
                    .iter()
                    .skip(1)
                    .all(|span| span.style.bg.is_none())
            }),
        "the file-changes summary is not a hunk and must not fill the row"
    );
}

#[test]
fn expanded_file_changes_leave_a_rail_gap_before_each_later_edit() {
    let state = AppState::new(vec![ChatItem::Diff {
        member: MemberId::new("builder"),
        files: vec![
            FileChangeItem::new("src/first.rs", "update").with_texts(Some("old\n"), Some("new\n")),
            FileChangeItem::new("src/second.rs", "update")
                .with_texts(Some("before\n"), Some("after\n")),
        ],
        ok: true,
    }]);
    let mut lines = Vec::new();
    render_chat_history(&state, 70, 0, &mut lines);
    let text = plain_text(&lines);
    let first_hunk = text
        .iter()
        .position(|line| line.contains("+new"))
        .expect("first file hunk");
    let second_path = text
        .iter()
        .position(|line| line.contains("src/second.rs"))
        .expect("second file path");

    assert_eq!(second_path, first_hunk + 2, "{text:?}");
    assert!(
        lines[first_hunk + 1]
            .spans
            .iter()
            .skip(1)
            .all(|span| span.content.trim().is_empty()),
        "the gap keeps only the member rail: {text:?}"
    );
}

#[test]
fn file_change_hunks_keep_source_indent() {
    let state = AppState::new(vec![ChatItem::Diff {
        member: MemberId::new("builder"),
        files: vec![FileChangeItem::new("src/lib.rs", "update").with_texts(
            Some("fn f() {\n    let x = 1;\n}\n"),
            Some("fn f() {\n    let x = 2;\n}\n"),
        )],
        ok: true,
    }]);
    let mut lines = Vec::new();
    render_chat_history(&state, 70, 0, &mut lines);
    let text = plain_text(&lines).join("\n");
    assert!(
        text.contains("-    let x = 1;"),
        "deleted line must keep its indent: {text}"
    );
    assert!(
        text.contains("+    let x = 2;"),
        "added line must keep its indent: {text}"
    );
}

#[test]
fn failed_file_change_has_a_failure_marker() {
    let state = AppState::new(vec![ChatItem::Diff {
        member: MemberId::new("builder"),
        files: vec![FileChangeItem::new("src/lib.rs", "update")],
        ok: false,
    }]);
    let mut lines = Vec::new();

    render_chat_history(&state, 70, 0, &mut lines);

    assert!(
        plain_text(&lines)
            .iter()
            .any(|line| line.contains("✕ file changes"))
    );
}

#[test]
fn agent_markdown_header_and_table_share_one_continuous_rail() {
    let state = AppState::new(Vec::new());
    let item = ChatItem::Agent {
        member: MemberId::new("builder"),
        display_name: "Builder".to_string(),
        backend: BackendKind::Codex,
        text: "## Plan\n\n| access | codex |\n|---|---|\n| `read-only` | `-s read-only` |"
            .to_string(),
    };
    let mut lines = Vec::new();

    render_item(&item, 80, &state, &mut lines, true);

    assert!(!lines.is_empty());
    assert!(lines.iter().all(|line| {
        line.spans.first().is_some_and(|span| {
            span.content.as_ref() == " "
                && span.style.bg == Some(theme::backend_color(BackendKind::Codex))
        })
    }));
    let text = plain_text(&lines).join("\n");
    assert!(text.contains("read-only"));
    assert!(!text.contains("read-only-s read-only"));
}

#[test]
fn completed_work_turn_gets_separator_before_next_user_message() {
    use crate::domain::event::TurnId;

    let mut state = AppState::new(Vec::new());
    state.apply(RuntimeEvent::Ready {
        modes: Default::default(),
        mode_overrides: Default::default(),
        suggested_verify: None,
        team: "t".to_string(),
        workspace: String::new(),
        default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
        runs: Vec::new(),
        members: vec![member_summary(
            "builder",
            "Builder",
            BackendKind::Codex,
            "impl",
            MemberStatus::Idle,
        )],
    });
    state.apply(RuntimeEvent::UserMessage {
        turn: TurnId(1),
        targets: vec![MemberId::new("builder")],
        body: "run tests".to_string(),
    });
    state.apply(RuntimeEvent::ToolStarted {
        member: MemberId::new("builder"),
        tool_id: "t1".to_string(),
        name: "shell".to_string(),
        summary: "cargo test".to_string(),
    });
    state.apply(RuntimeEvent::ToolCompleted {
        member: MemberId::new("builder"),
        tool_id: "t1".to_string(),
        ok: true,
        output: "test result: ok".to_string(),
    });
    state.apply(RuntimeEvent::UserMessage {
        turn: TurnId(2),
        targets: vec![MemberId::new("builder")],
        body: "now summarize".to_string(),
    });
    let mut lines = Vec::new();

    render_chat_history(&state, 40, 0, &mut lines);

    let text = plain_text(&lines);
    let separators: Vec<_> = text
        .iter()
        .enumerate()
        .filter(|(_, line)| is_separator_text(line))
        .collect();
    assert_eq!(separators.len(), 1);
    let separator_index = separators[0].0;
    assert!(
        text[..separator_index]
            .iter()
            .any(|line| line.contains("Shell"))
    );
    assert!(
        text[separator_index + 1..]
            .iter()
            .any(|line| line.contains("now summarize"))
    );
}

#[test]
fn consecutive_tool_lines_stay_grouped() {
    use crate::domain::event::TurnId;

    let mut state = AppState::new(Vec::new());
    state.apply(RuntimeEvent::UserMessage {
        turn: TurnId(1),
        targets: vec![MemberId::new("builder")],
        body: "go".to_string(),
    });
    for (id, cmd) in [("t1", "cargo build"), ("t2", "cargo test")] {
        state.apply(RuntimeEvent::ToolStarted {
            member: MemberId::new("builder"),
            tool_id: id.to_string(),
            name: "shell".to_string(),
            summary: cmd.to_string(),
        });
        state.apply(RuntimeEvent::ToolCompleted {
            member: MemberId::new("builder"),
            tool_id: id.to_string(),
            ok: true,
            output: "ok".to_string(),
        });
    }
    let mut lines = Vec::new();

    render_chat_history(&state, 60, 0, &mut lines);

    let text = plain_text(&lines);
    let build_idx = text
        .iter()
        .position(|line| line.contains("cargo build"))
        .unwrap();
    let test_idx = text
        .iter()
        .position(|line| line.contains("cargo test"))
        .unwrap();
    // Tool blocks (including their output lines) stay adjacent.
    assert!(test_idx > build_idx);
    assert!(
        text[build_idx + 1..test_idx]
            .iter()
            .all(|line| !line.trim().is_empty())
    );
}

#[test]
fn edits_split_file_change_cards_and_are_hidden_from_tools() {
    let builder = MemberId::new("builder");
    let state = AppState::new(vec![
        ChatItem::Tool {
            member: builder.clone(),
            name: "edit".to_string(),
            summary: "src/a.rs".to_string(),
            detail: String::new(),
            ok: Some(true),
        },
        ChatItem::Diff {
            member: builder.clone(),
            files: vec![FileChangeItem::new("src/a.rs", "modify")],
            ok: true,
        },
        ChatItem::Tool {
            member: builder.clone(),
            name: "edit".to_string(),
            summary: "src/b.rs".to_string(),
            detail: String::new(),
            ok: Some(true),
        },
        ChatItem::Diff {
            member: builder,
            files: vec![FileChangeItem::new("/workspace/src/b.rs", "modify")],
            ok: true,
        },
        ChatItem::Tool {
            member: MemberId::new("builder"),
            name: "shell".to_string(),
            summary: "cargo test".to_string(),
            detail: String::new(),
            ok: Some(true),
        },
    ]);
    let mut lines = Vec::new();
    render_chat_history(&state, 80, 0, &mut lines);
    let text = plain_text(&lines);
    let joined = text.join("\n");
    let tool_headers = text.iter().filter(|line| line.contains("tools")).count();
    let diff_headers = text
        .iter()
        .filter(|line| line.contains("file changes"))
        .count();
    assert_eq!(tool_headers, 1, "{joined}");
    assert_eq!(diff_headers, 2, "{joined}");
    assert!(
        joined.contains("Shell") && joined.contains("cargo test"),
        "{joined}"
    );
    assert!(
        !text.iter().any(|line| line.contains("· Edit")),
        "successful Edit operations belong only in file changes: {joined}"
    );
    assert!(
        joined.contains("src/a.rs") && joined.contains("/workspace/src/b.rs"),
        "{joined}"
    );
    assert!(
        joined.find("src/a.rs") < joined.find("/workspace/src/b.rs"),
        "file changes must follow Edit order: {joined}"
    );
    let first_edit = text
        .iter()
        .position(|line| line.contains("src/a.rs"))
        .expect(&joined);
    let second_edit = text
        .iter()
        .position(|line| line.contains("/workspace/src/b.rs"))
        .expect(&joined);
    assert!(
        text[first_edit + 1..second_edit]
            .iter()
            .any(|line| line.trim().is_empty()),
        "separate Edit file-change cards need a visual gap: {joined}"
    );
}

#[test]
fn write_tools_are_file_change_cards_not_tools() {
    let builder = MemberId::new("builder");
    let state = AppState::new(vec![
        ChatItem::Tool {
            member: builder.clone(),
            name: "Search".to_string(),
            summary: "DirectoryPath engine/Asterline".to_string(),
            detail: String::new(),
            ok: Some(true),
        },
        ChatItem::Tool {
            member: builder.clone(),
            name: "Write".to_string(),
            summary: "TargetFile snake game/index.html".to_string(),
            detail: String::new(),
            ok: None,
        },
        ChatItem::Tool {
            member: builder,
            name: "Write".to_string(),
            summary: "TargetFile css/style.css".to_string(),
            detail: String::new(),
            ok: None,
        },
    ]);
    let mut lines = Vec::new();
    render_chat_history(&state, 80, 0, &mut lines);
    let text = plain_text(&lines);
    let joined = text.join("\n");

    assert!(joined.contains("tools"), "{joined}");
    assert!(joined.contains("Search"), "{joined}");
    assert!(joined.contains("file changes"), "{joined}");
    assert!(
        joined.contains("snake game/index.html") && joined.contains("css/style.css"),
        "{joined}"
    );
    assert!(
        !text.iter().any(|line| line.contains("Write")),
        "Write belongs in file changes, not tools: {joined}"
    );
}

#[test]
fn claude_edit_without_a_native_diff_is_a_file_change_card() {
    let state = AppState::new(vec![ChatItem::Tool {
        member: MemberId::new("planer"),
        name: "Edit".to_string(),
        summary: "team_runtime_tests/review.rs".to_string(),
        detail: "input:\n{\"file_path\":\"team_runtime_tests/review.rs\",\"old_string\":\"before\\n\",\"new_string\":\"after\\n\"}\nupdated successfully".to_string(),
        ok: Some(true),
    }]);
    let mut lines = Vec::new();
    render_chat_history(&state, 80, 0, &mut lines);
    let text = plain_text(&lines).join("\n");

    assert!(text.contains("file changes"), "{text}");
    assert!(text.contains("Edit team_runtime_tests/review.rs"), "{text}");
    assert!(text.contains("-before"), "{text}");
    assert!(text.contains("+after"), "{text}");
    assert!(!text.contains("tools"), "{text}");
}

#[test]
fn different_members_compact_tools_leave_a_gap_between_rails() {
    use crate::domain::event::TurnId;

    let mut state = AppState::new(Vec::new());
    state.apply(RuntimeEvent::Ready {
        modes: Default::default(),
        mode_overrides: Default::default(),
        suggested_verify: None,
        team: "t".to_string(),
        workspace: String::new(),
        default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
        runs: Vec::new(),
        members: vec![
            member_summary(
                "builder",
                "Builder",
                BackendKind::Codex,
                "impl",
                MemberStatus::Idle,
            ),
            member_summary(
                "planer",
                "Planer",
                BackendKind::Claude,
                "plan",
                MemberStatus::Idle,
            ),
        ],
    });
    state.apply(RuntimeEvent::UserMessage {
        turn: TurnId(1),
        targets: vec![MemberId::new("builder"), MemberId::new("planer")],
        body: "search".to_string(),
    });
    state.apply(RuntimeEvent::ToolStarted {
        member: MemberId::new("builder"),
        tool_id: "b1".to_string(),
        name: "shell".to_string(),
        summary: "ls builder".to_string(),
    });
    state.apply(RuntimeEvent::ToolCompleted {
        member: MemberId::new("builder"),
        tool_id: "b1".to_string(),
        ok: true,
        output: "ok".to_string(),
    });
    state.apply(RuntimeEvent::ToolStarted {
        member: MemberId::new("planer"),
        tool_id: "p1".to_string(),
        name: "shell".to_string(),
        summary: "ls planer".to_string(),
    });
    state.apply(RuntimeEvent::ToolCompleted {
        member: MemberId::new("planer"),
        tool_id: "p1".to_string(),
        ok: true,
        output: "ok".to_string(),
    });

    let mut lines = Vec::new();
    render_chat_history(&state, 60, 0, &mut lines);
    let text = plain_text(&lines);
    let builder = text
        .iter()
        .position(|line| line.contains("ls builder"))
        .unwrap();
    let planer = text
        .iter()
        .position(|line| line.contains("ls planer"))
        .unwrap();
    assert!(builder < planer, "{text:?}");
    assert!(
        text[builder..planer]
            .iter()
            .any(|line| line.trim().is_empty()),
        "rails must not touch across members: {text:?}"
    );
}
