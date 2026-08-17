use super::super::*;
use super::*;

#[test]
fn fmt_elapsed_compact_scales_units() {
    assert_eq!(status_indicator::fmt_elapsed_compact(8), "8s");
    assert_eq!(status_indicator::fmt_elapsed_compact(64), "1m 04s");
    assert_eq!(status_indicator::fmt_elapsed_compact(3723), "1h 02m 03s");
}

#[test]
fn renders_empty_state_quick_start() {
    let mut state = AppState::new(Vec::new());
    state.apply(RuntimeEvent::Ready {
        modes: Default::default(),
        mode_overrides: Default::default(),
        suggested_verify: None,
        team: "default-mixed".to_string(),
        workspace: "/Users/me/proj".to_string(),
        default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
        runs: Vec::new(),
        members: vec![member_summary(
            "builder",
            "Builder",
            BackendKind::Codex,
            "implementation",
            MemberStatus::Idle,
        )],
    });

    let mut terminal = Terminal::new(TestBackend::new(96, 16)).unwrap();
    terminal
        .draw(|frame| {
            let _ = render(frame, &state);
        })
        .unwrap();
    let view = format!("{}", terminal.backend());
    eprintln!("\n{view}");

    assert!(view.contains("Members:"));
    assert!(view.contains("builder (codex, implementation)"));
    assert!(view.contains("@builder <message>"));
    assert!(view.contains("/mode"));
    assert!(!view.contains("/mode plan"));
    assert!(view.contains("/help"));
}

#[test]
fn ready_replaces_a_cached_team_loading_empty_state() {
    let mut state = AppState::new(Vec::new());
    let mut terminal = Terminal::new(TestBackend::new(96, 16)).unwrap();

    terminal
        .draw(|frame| {
            let _ = render(frame, &state);
        })
        .unwrap();
    assert!(format!("{}", terminal.backend()).contains("Team is loading..."));

    state.apply(RuntimeEvent::Ready {
        modes: Default::default(),
        mode_overrides: Default::default(),
        suggested_verify: None,
        team: "default-mixed".to_string(),
        workspace: "/Users/me/proj".to_string(),
        default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
        runs: Vec::new(),
        members: vec![member_summary(
            "builder",
            "Builder",
            BackendKind::Codex,
            "implementation",
            MemberStatus::Idle,
        )],
    });
    terminal
        .draw(|frame| {
            let _ = render(frame, &state);
        })
        .unwrap();

    let view = format!("{}", terminal.backend());
    assert!(
        !view.contains("Team is loading..."),
        "stale loading view: {view}"
    );
    assert!(view.contains("builder (codex, implementation)"));
}

#[test]
fn session_reset_discards_the_painted_scrollback_cache() {
    let mut state = AppState::new(vec![ChatItem::Notice {
        text: "history that /new must clear".to_string(),
    }]);
    let mut terminal = Terminal::new(TestBackend::new(80, 14)).unwrap();

    terminal
        .draw(|frame| {
            let _ = render(frame, &state);
        })
        .unwrap();
    assert!(format!("{}", terminal.backend()).contains("history that /new must clear"));

    state.apply(RuntimeEvent::SessionReset);
    terminal
        .draw(|frame| {
            let _ = render(frame, &state);
        })
        .unwrap();

    let view = format!("{}", terminal.backend());
    assert!(state.chat().is_empty());
    assert!(
        !view.contains("history that /new must clear"),
        "old scrollback survived SessionReset: {view}"
    );
}

#[test]
fn cached_working_indicator_moves_to_a_new_targeted_prompt() {
    use crate::domain::event::TurnId;

    let builder = MemberId::new("builder");
    let mut state = AppState::new(vec![ChatItem::Agent {
        member: builder.clone(),
        display_name: "Builder".to_string(),
        backend: BackendKind::Codex,
        text: "completed reply".to_string(),
    }]);
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
    let mut terminal = Terminal::new(TestBackend::new(100, 36)).unwrap();

    terminal
        .draw(|frame| {
            let _ = render(frame, &state);
        })
        .unwrap();
    // Establish a cached prefix containing the original member region.
    state.apply(RuntimeEvent::Notice("cache boundary".to_string()));
    terminal
        .draw(|frame| {
            let _ = render(frame, &state);
        })
        .unwrap();

    state.apply(RuntimeEvent::UserMessage {
        turn: TurnId(1),
        targets: vec![builder],
        body: "new request".to_string(),
    });
    terminal
        .draw(|frame| {
            let _ = render(frame, &state);
        })
        .unwrap();

    let view = format!("{}", terminal.backend());
    assert_eq!(view.matches("Working (").count(), 1, "{view}");
    assert!(
        view.find("new request") < view.find("Working ("),
        "live status must belong to the current prompt: {view}"
    );
}

#[test]
fn cached_working_indicator_does_not_stick_to_a_prior_same_member_turn() {
    use crate::domain::event::{MessageId, TurnId};

    let builder = MemberId::new("builder");
    let planer = MemberId::new("planer");
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
                BackendKind::Agy,
                "implementation",
                MemberStatus::Running,
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
    state.apply(RuntimeEvent::UserMessage {
        turn: TurnId(1),
        targets: vec![builder.clone()],
        body: "make the snake game; let planner design first".to_string(),
    });
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(1),
        turn: TurnId(1),
        member: builder.clone(),
    });
    state.apply(RuntimeEvent::MessageCompleted {
        msg: MessageId(1),
        text: "dispatched the plan to Planer".to_string(),
    });
    state.apply(RuntimeEvent::Route {
        turn: TurnId(1),
        from: builder.clone(),
        to: vec![planer.to_string()],
        body: "please draft the architecture".to_string(),
    });

    let mut terminal = Terminal::new(TestBackend::new(100, 48)).unwrap();
    terminal
        .draw(|frame| {
            let _ = render(frame, &state);
        })
        .unwrap();
    // Freeze the first Builder region in the painted prefix.
    state.apply(RuntimeEvent::Notice("cache boundary".to_string()));
    terminal
        .draw(|frame| {
            let _ = render(frame, &state);
        })
        .unwrap();

    state.apply(RuntimeEvent::MemberStatus {
        member: builder.clone(),
        status: MemberStatus::Idle,
    });
    state.apply(RuntimeEvent::MemberStatus {
        member: planer.clone(),
        status: MemberStatus::Running,
    });
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(2),
        turn: TurnId(1),
        member: planer.clone(),
    });
    state.apply(RuntimeEvent::MessageCompleted {
        msg: MessageId(2),
        text: "here is the architecture".to_string(),
    });
    state.apply(RuntimeEvent::Route {
        turn: TurnId(1),
        from: planer.clone(),
        to: vec![builder.to_string()],
        body: "architecture ready, please implement".to_string(),
    });
    state.apply(RuntimeEvent::MemberStatus {
        member: planer,
        status: MemberStatus::Idle,
    });
    state.apply(RuntimeEvent::MemberStatus {
        member: builder.clone(),
        status: MemberStatus::Running,
    });
    state.apply(RuntimeEvent::ToolStarted {
        member: builder,
        tool_id: "w1".to_string(),
        name: "write".to_string(),
        summary: "snake game/index.html".to_string(),
    });
    terminal
        .draw(|frame| {
            let _ = render(frame, &state);
        })
        .unwrap();

    let view = format!("{}", terminal.backend());
    assert_eq!(view.matches("Working (").count(), 1, "{view}");
    let first = view.find("dispatched the plan to Planer").expect(&view);
    let implement = view.find("snake game/index.html").expect(&view);
    let working = view.find("Working (").expect(&view);
    assert!(
        first < implement && implement < working,
        "Working must follow the new turn, not the finished Builder reply: {view}"
    );
}

#[test]
fn cached_working_indicator_stays_on_prefix_when_only_others_continue() {
    use crate::domain::event::{MessageId, TurnId};

    let builder = MemberId::new("builder");
    let planer = MemberId::new("planer");
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
                BackendKind::Agy,
                "implementation",
                MemberStatus::Running,
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
    state.apply(RuntimeEvent::UserMessage {
        turn: TurnId(1),
        targets: vec![builder.clone()],
        body: "ask Planer to design it".to_string(),
    });
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(1),
        turn: TurnId(1),
        member: builder.clone(),
    });
    state.apply(RuntimeEvent::MessageCompleted {
        msg: MessageId(1),
        text: "waiting on the plan".to_string(),
    });

    let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
    terminal
        .draw(|frame| {
            let _ = render(frame, &state);
        })
        .unwrap();
    state.apply(RuntimeEvent::Notice("cache boundary".to_string()));
    terminal
        .draw(|frame| {
            let _ = render(frame, &state);
        })
        .unwrap();

    state.apply(RuntimeEvent::MemberStatus {
        member: planer.clone(),
        status: MemberStatus::Running,
    });
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(2),
        turn: TurnId(1),
        member: planer,
    });
    state.apply(RuntimeEvent::MessageCompleted {
        msg: MessageId(2),
        text: "planer still drafting".to_string(),
    });
    terminal
        .draw(|frame| {
            let _ = render(frame, &state);
        })
        .unwrap();

    let view = format!("{}", terminal.backend());
    assert_eq!(view.matches("Working (").count(), 2, "{view}");
    let waiting = view.find("waiting on the plan").expect(&view);
    let drafting = view.find("planer still drafting").expect(&view);
    let mut workings = view.match_indices("Working (").map(|(idx, _)| idx);
    let builder_working = workings.next().expect(&view);
    let planer_working = workings.next().expect(&view);
    assert!(
        waiting < builder_working && builder_working < drafting && drafting < planer_working,
        "Builder's Working must stay on the cached reply while Planer works: {view}"
    );
}

#[test]
fn renders_a_clean_layout_snapshot() {
    let mut state = AppState::new(Vec::new());
    state.apply(RuntimeEvent::Ready {
        modes: Default::default(),
        mode_overrides: Default::default(),
        suggested_verify: None,
        team: "default-mixed".to_string(),
        workspace: "/Users/me/proj".to_string(),
        default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
        runs: Vec::new(),
        members: vec![
            member_summary(
                "builder",
                "Builder",
                BackendKind::Codex,
                "implementation",
                MemberStatus::Running,
            ),
            member_summary(
                "reviewer",
                "Reviewer",
                BackendKind::Claude,
                "review",
                MemberStatus::Idle,
            ),
        ],
    });
    state.apply(RuntimeEvent::Notice("welcome to Asterline".to_string()));
    state.apply(RuntimeEvent::Route {
        turn: crate::domain::event::TurnId(1),
        from: MemberId::new("builder"),
        to: vec!["reviewer".to_string()],
        body: "please review the parser".to_string(),
    });

    let mut terminal = Terminal::new(TestBackend::new(90, 16)).unwrap();
    terminal
        .draw(|frame| {
            let _ = render(frame, &state);
        })
        .unwrap();
    let view = format!("{}", terminal.backend());
    eprintln!("\n{view}");

    assert!(view.contains("Asterline"));
    assert!(view.contains(env!("CARGO_PKG_VERSION")));
    assert!(view.contains("Builder · codex"));
    assert!(view.contains("builder → reviewer"));
    // The running member surfaces a working indicator + interrupt hint.
    assert!(view.contains("Working"));
    assert!(view.contains("interrupt"));
    // The composer is open (top/bottom rules only) — no enclosing box or
    // rounded corners around the conversation or input.
    assert!(!view.contains('╭'));
}

#[test]
fn composer_shows_mode_at_bottom_right_and_header_clips_workspace() {
    let mut state = AppState::new(Vec::new());
    state.apply(RuntimeEvent::Ready {
        modes: Default::default(),
        mode_overrides: Default::default(),
        suggested_verify: None,
        team: "t".to_string(),
        workspace: "/Users/我/很长的项目路径名称超级超级长/子目录".to_string(),
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

    // Narrow terminal: the CJK path must clip by display width, not chars.
    let mut terminal = Terminal::new(TestBackend::new(52, 10)).unwrap();
    terminal
        .draw(|frame| {
            let _ = render(frame, &state);
        })
        .unwrap();
    let view = format!("{}", terminal.backend());
    eprintln!("\n{view}");

    assert!(view.contains("Asterline"));
    assert!(view.contains(env!("CARGO_PKG_VERSION")));
    let rows = view.lines().collect::<Vec<_>>();
    assert!(
        !rows.first().is_some_and(|row| row.contains("mode:normal")),
        "mode should not occupy the header: {view}"
    );
    let mode_row = rows
        .iter()
        .position(|row| row.contains("mode:normal"))
        .expect("mode label in composer");
    assert!(
        mode_row >= rows.len().saturating_sub(3),
        "mode should render on the composer bottom border: {view}"
    );
    assert!(view.contains('…'));
}

#[test]
fn renders_completion_popup() {
    let mut state = AppState::new(Vec::new());
    state.apply(RuntimeEvent::Ready {
        modes: Default::default(),
        mode_overrides: Default::default(),
        suggested_verify: None,
        team: "t".to_string(),
        workspace: ".".to_string(),
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
    for ch in "/a".chars() {
        state.insert_char(ch);
    }

    let mut terminal = Terminal::new(TestBackend::new(70, 14)).unwrap();
    let mut layout = None;
    terminal
        .draw(|frame| {
            layout = render(frame, &state);
        })
        .unwrap();
    let view = format!("{}", terminal.backend());
    eprintln!("\n{view}");

    assert!(view.contains("/ask"));
    assert!(view.contains("/all"));
    assert!(view.contains("/attach"));
    assert!(!view.contains("╭"));
    assert!(!view.contains("@member to send"));
    assert!(view.contains("› /ask      send to one member"));
    assert_eq!(
        layout.and_then(|layout| layout.completion_area),
        Some(Rect::new(0, 10, 70, 4))
    );
}

#[test]
fn completion_popup_uses_text_only_selection() {
    let completion = Completion {
        title: "commands",
        token_start: 0,
        items: vec![
            crate::tui::completion::CompletionItem {
                label: "/ask — send to one member".to_string(),
                insert: "/ask ".to_string(),
            },
            crate::tui::completion::CompletionItem {
                label: "/all — send to everyone".to_string(),
                insert: "/all ".to_string(),
            },
        ],
    };
    let mut terminal = Terminal::new(TestBackend::new(40, 2)).unwrap();
    terminal
        .draw(|frame| render_popup(frame, frame.area(), &completion, 0))
        .unwrap();

    let buffer = terminal.backend().buffer();
    let selected_name = buffer.cell((2, 0)).unwrap();
    let selected_hint = buffer.cell((8, 0)).unwrap();
    let unselected_name = buffer.cell((2, 1)).unwrap();

    assert_eq!(selected_name.fg, theme::accent_color());
    assert_eq!(selected_name.bg, Color::Reset);
    assert_eq!(selected_hint.fg, theme::accent_color());
    assert_eq!(selected_hint.bg, Color::Reset);
    assert_eq!(unselected_name.fg, theme::emphasis_color());
    assert_eq!(unselected_name.bg, Color::Reset);
}

#[test]
fn running_status_shows_model_and_effort() {
    let mut builder = member_summary(
        "builder",
        "Builder",
        BackendKind::Codex,
        "impl",
        MemberStatus::Running,
    );
    builder.model = Some("gpt-5-codex".to_string());
    builder.effort = Some(Effort::High);
    let mut state = AppState::new(Vec::new());
    state.apply(RuntimeEvent::Ready {
        modes: Default::default(),
        mode_overrides: Default::default(),
        suggested_verify: None,
        team: "t".to_string(),
        workspace: String::new(),
        default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
        runs: Vec::new(),
        members: vec![builder],
    });

    let mut terminal = Terminal::new(TestBackend::new(120, 12)).unwrap();
    terminal
        .draw(|frame| {
            let _ = render(frame, &state);
        })
        .unwrap();
    let view = format!("{}", terminal.backend());
    eprintln!("\n{view}");

    // The activity line spells the profile out; the header stays compact.
    assert!(view.contains("model: gpt-5-codex · high"));
}

#[test]
fn queued_waiting_and_approval_are_active_in_header_and_footer() {
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
                MemberStatus::Queued,
            ),
            member_summary(
                "reviewer",
                "Reviewer",
                BackendKind::Claude,
                "review",
                MemberStatus::Waiting,
            ),
            member_summary(
                "qa",
                "QA",
                BackendKind::Codex,
                "verify",
                MemberStatus::NeedsApproval,
            ),
        ],
    });

    let mut terminal = Terminal::new(TestBackend::new(150, 18)).unwrap();
    terminal
        .draw(|frame| {
            let _ = render(frame, &state);
        })
        .unwrap();
    let view = format!("{}", terminal.backend());

    assert!(view.contains("Active 3 members"));
    assert!(view.contains("Builder queued"));
    assert!(view.contains("Reviewer waiting"));
    assert!(view.contains("QA approval"));
    assert!(!view.contains("○ Reviewer"));
    assert!(!view.contains("@member first"));
}

#[test]
fn queued_follow_up_is_previewed_above_the_composer() {
    let mut state = AppState::new(vec![ChatItem::Notice {
        text: "existing chat".to_string(),
    }]);
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
            MemberStatus::Running,
        )],
    });
    state.apply(RuntimeEvent::QueueUpdated {
        member: MemberId::new("builder"),
        prompts: vec!["fix the queued follow-up".to_string()],
    });

    let chat_len = state.chat().len();
    let mut terminal = Terminal::new(TestBackend::new(100, 16)).unwrap();
    terminal
        .draw(|frame| {
            let _ = render(frame, &state);
        })
        .unwrap();
    let view = format!("{}", terminal.backend());

    assert_eq!(state.chat().len(), chat_len);
    assert_eq!(state.members()[0].status, MemberStatus::Running);
    assert!(view.contains("Queued follow-up inputs"));
    assert!(view.contains("@Builder fix the queued follow-up"));
    assert!(view.contains("Shift+← edit last queued message"));
    assert!(view.contains("Working"));
}
#[test]
fn renders_markdown_agent_message() {
    let chat = vec![ChatItem::Agent {
            member: MemberId::new("reviewer"),
            display_name: "Reviewer".to_string(),
            backend: BackendKind::Claude,
            text: "## Findings\n\nThe parser drops a **trailing newline**. Use `trim_end`.\n\n- check the lexer\n- add a test\n\n```rust\nlet x = 1;\n```"
                .to_string(),
        }];
    let state = AppState::new(chat);

    let mut terminal = Terminal::new(TestBackend::new(72, 18)).unwrap();
    terminal
        .draw(|frame| {
            let _ = render(frame, &state);
        })
        .unwrap();
    let view = format!("{}", terminal.backend());
    eprintln!("\n{view}");

    assert!(view.contains("Findings")); // heading, '##' stripped
    assert!(view.contains("• check the lexer")); // bullet marker
    assert!(view.contains("let x = 1;")); // code block body
    assert!(!view.contains("```")); // fences stripped
    assert!(!view.contains("**")); // bold markers consumed
}

#[test]
fn renders_user_band_and_compact_tool() {
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
        body: "run the tests".to_string(),
    });
    let long = "/bin/zsh -lc \"rg -n 'Codex is OpenAIs coding agent' /var/folders/ym/abc/openai-docs-cache/codex-manual.md and a lot more text that used to wrap\"";
    state.apply(RuntimeEvent::ToolStarted {
        member: MemberId::new("builder"),
        tool_id: "t1".to_string(),
        name: "shell".to_string(),
        summary: long.to_string(),
    });
    state.apply(RuntimeEvent::ToolCompleted {
        member: MemberId::new("builder"),
        tool_id: "t1".to_string(),
        ok: true,
        output: "matches found".to_string(),
    });
    let mut terminal = Terminal::new(TestBackend::new(72, 18)).unwrap();
    terminal
        .draw(|frame| {
            let _ = render(frame, &state);
        })
        .unwrap();
    let view = format!("{}", terminal.backend());
    eprintln!("\n{view}");

    assert!(view.contains("◆ You"));
    assert!(view.contains("run the tests"));
    // The long command is truncated to a single line (ellipsis), not wrapped.
    assert!(view.contains('…'));
    assert!(view.contains("⚒ tools") || view.contains("tools"));
    assert!(view.contains("Shell"));
    assert!(
        !view.contains("matches found"),
        "successful tool output stays collapsed: {view}"
    );
}

#[test]
fn collapsed_tool_shows_input_summary_instead_of_bare_label() {
    let state = AppState::new(vec![ChatItem::Tool {
        member: MemberId::new("builder"),
        name: "Bash".to_string(),
        summary: "Bash".to_string(),
        detail: "input:\n{\"command\":\"cargo test\",\"timeout\":120}\n".to_string(),
        ok: None,
    }]);
    let mut lines = Vec::new();

    render_chat_history(&state, 70, 0, &mut lines);

    let text = plain_text(&lines);
    assert!(
        text.iter().any(|line| {
            (line.contains("Shell") || line.contains("Bash")) && line.contains("cargo test")
        }),
        "collapsed tool should show the command, not JSON: {text:?}"
    );
    assert!(
        !text
            .iter()
            .any(|line| line.contains('{') || line.contains("input:")),
        "collapsed tool must not dump JSON: {text:?}"
    );
}

#[test]
fn consecutive_tools_share_one_tools_icon() {
    let state = AppState::new(vec![
        ChatItem::Tool {
            member: MemberId::new("builder"),
            name: "read_file".to_string(),
            summary: r#"{"target_file":"src/lib.rs"}"#.to_string(),
            detail: String::new(),
            ok: Some(true),
        },
        ChatItem::Tool {
            member: MemberId::new("builder"),
            name: "Bash".to_string(),
            summary: "Bash".to_string(),
            detail: "input:\n{\"command\":\"cargo test\"}\n".to_string(),
            ok: Some(true),
        },
    ]);
    let mut lines = Vec::new();
    render_chat_history(&state, 70, 0, &mut lines);
    let text = plain_text(&lines);
    let headers = text.iter().filter(|line| line.contains("tools")).count();
    assert_eq!(headers, 1, "one tools icon for the group: {text:?}");
    assert!(
        text.iter()
            .any(|line| line.contains("Read") && line.contains("src/lib.rs")),
        "read kind is subordinate: {text:?}"
    );
    assert!(
        text.iter()
            .any(|line| line.contains("Shell") && line.contains("cargo test")),
        "shell kind is subordinate: {text:?}"
    );
    assert!(
        !text.iter().any(|line| line.contains("read_file")),
        "raw tool name stays off the title: {text:?}"
    );
}

#[test]
fn failed_tool_shows_error_output_without_expanding() {
    let state = AppState::new(vec![ChatItem::Tool {
        member: MemberId::new("builder"),
        name: "shell".to_string(),
        summary: "cargo test".to_string(),
        detail: "error: test parser failed\nexpected true, got false".to_string(),
        ok: Some(false),
    }]);
    let mut lines = Vec::new();

    render_chat_history(&state, 70, 0, &mut lines);

    let text = plain_text(&lines).join("\n");
    assert!(text.contains("✕ tools") || text.contains("✕"));
    assert!(text.contains("Shell"));
    assert!(text.contains("cargo test"));
    assert!(text.contains("error: test parser failed"));
    assert!(text.contains("expected true, got false"));
}

#[test]
fn failed_tool_only_colors_the_error_lines() {
    let state = AppState::new(vec![ChatItem::Tool {
        member: MemberId::new("builder"),
        name: "shell".to_string(),
        summary: "cargo test".to_string(),
        detail: "Compiling asterline\nerror: test parser failed".to_string(),
        ok: Some(false),
    }]);
    let mut lines = Vec::new();
    render_chat_history(&state, 70, 0, &mut lines);
    let compiling = lines
        .iter()
        .find(|line| {
            plain_text(std::slice::from_ref(*line))
                .join("")
                .contains("Compiling")
        })
        .expect("compiling output");
    let error = lines
        .iter()
        .find(|line| {
            plain_text(std::slice::from_ref(*line))
                .join("")
                .contains("error:")
        })
        .expect("error output");

    assert_eq!(compiling.spans[2].style.fg, theme::muted().fg);
    assert_eq!(error.spans[2].style.fg, theme::error().fg);
}

#[test]
fn failed_tool_collapses_a_long_test_snapshot() {
    let state = AppState::new(vec![ChatItem::Tool {
        member: MemberId::new("builder"),
        name: "shell".to_string(),
        summary: "cargo test".to_string(),
        detail: concat!(
            "Compiling asterline\n",
            "test tui::chat_view::tests::render::example ... FAILED\n",
            "failures:\n",
            "thread 'example' panicked at src/tui/chat_view_tests/render.rs:173:5\n",
            "assertion `left == right` failed: a very long rendered terminal snapshot follows\n",
            "large snapshot payload that should stay behind Ctrl+O\n"
        )
        .to_string(),
        ok: Some(false),
    }]);
    let mut lines = Vec::new();
    render_chat_history(&state, 80, 0, &mut lines);
    let text = plain_text(&lines).join("\n");

    assert!(text.contains("... FAILED"), "{text}");
    assert!(text.contains("panicked at"), "{text}");
    assert!(text.contains("assertion"), "{text}");
    assert!(!text.contains("large snapshot payload"), "{text}");
    assert!(text.contains("Ctrl+O expand tool output"), "{text}");
}

#[test]
fn failed_tool_does_not_dump_structured_input_as_the_error() {
    let state = AppState::new(vec![ChatItem::Tool {
        member: MemberId::new("builder"),
        name: "read_file".to_string(),
        summary: r#"{"target_file":"src/missing.rs"}"#.to_string(),
        detail: "input:\n{\"target_file\":\"src/missing.rs\",\"offset\":1}\n".to_string(),
        ok: Some(false),
    }]);
    let mut lines = Vec::new();
    render_chat_history(&state, 70, 0, &mut lines);
    let text = plain_text(&lines).join("\n");
    assert!(
        text.contains("Read") && text.contains("src/missing.rs"),
        "{text}"
    );
    assert!(!text.contains("input:"), "{text}");
    assert!(!text.contains("offset"), "{text}");
    assert!(!text.contains('{'), "{text}");
}

#[test]
fn failed_edit_shows_the_real_error_not_a_char_count() {
    let message = "The string to replace was found multiple times. Add more context.";
    let state = AppState::new(vec![ChatItem::Tool {
        member: MemberId::new("builder"),
        name: "search_replace".to_string(),
        summary: r#"{"target_file":"tui/app_state.rs"}"#.to_string(),
        detail: format!(r#"{{"MultipleMatchesFound":"{message}","type":"SearchReplace"}}"#),
        ok: Some(false),
    }]);
    let mut lines = Vec::new();
    render_chat_history(&state, 80, 0, &mut lines);
    let text = plain_text(&lines).join("\n");
    assert!(text.contains("✕") && text.contains("Edit"), "{text}");
    assert!(text.contains("app_state.rs"), "{text}");
    assert!(
        text.contains("The string to replace was found multiple times"),
        "{text}"
    );
    assert!(text.contains("Add more context"), "{text}");
    assert!(!text.contains("chars"), "{text}");
    assert!(!text.contains("SearchReplace"), "{text}");
}

#[test]
fn renders_verdict_card_with_title_and_summary() {
    let state = AppState::new(vec![
        ChatItem::Verdict {
            member: MemberId::new("reviewer"),
            approve: true,
            summary: "Looks good; ship it.".to_string(),
        },
        ChatItem::Verdict {
            member: MemberId::new("reviewer"),
            approve: false,
            summary: "Needs a regression test.".to_string(),
        },
    ]);
    let mut lines = Vec::new();
    render_chat_history(&state, 70, 0, &mut lines);
    let text = plain_text(&lines).join("\n");
    assert!(
        text.contains("✓ review approved"),
        "missing approve title: {text}"
    );
    assert!(
        text.contains("Looks good; ship it."),
        "missing approve summary: {text}"
    );
    assert!(
        text.contains("✗ changes requested"),
        "missing reject title: {text}"
    );
    assert!(
        text.contains("Needs a regression test."),
        "missing reject summary: {text}"
    );
}

#[test]
fn drag_selection_only_restyles_the_covered_columns() {
    let line = Line::from(vec![
        Span::styled("hello ", theme::text()),
        Span::styled("world", theme::emphasis()),
    ]);
    let styled = restyle_column_range(&line, 6, 11);
    assert_eq!(styled.spans.len(), 2);
    assert_eq!(styled.spans[0].content, "hello ");
    assert_eq!(styled.spans[0].style, theme::text());
    assert_eq!(styled.spans[1].content, "world");
    assert_eq!(styled.spans[1].style, theme::chat_selection());
}

#[test]
fn selected_text_joins_visible_chat_lines() {
    let layout = ChatLayout {
        area: Rect::new(0, 0, 20, 4),
        first_line: 0,
        total_lines: 2,
        width: 20,
        lines: vec!["hello world".into(), "second line".into()],
        completion_area: None,
        composer_area: None,
        composer_wrap: 0,
        composer_text_origin: 0,
    };
    let text = layout.selected_text(crate::tui::app_state::ChatSelection {
        start: (0, 0),
        end: (1, 5),
    });
    assert_eq!(text, "hello world\nsecond");
}

#[test]
fn renders_scrollable_diff_drawer() {
    let mut state = AppState::new(Vec::new());
    state.set_diff(
        "diff --git a/src/lib.rs b/src/lib.rs\n@@ -1,3 +1,3 @@\n-old line\n+new line\n context"
            .to_string(),
    );
    state.toggle_drawer(Drawer::Diff);

    let mut terminal = Terminal::new(TestBackend::new(80, 16)).unwrap();
    terminal
        .draw(|frame| {
            let _ = render(frame, &state);
        })
        .unwrap();
    let view = format!("{}", terminal.backend());
    eprintln!("\n{view}");

    assert!(view.contains("Working-tree diff"));
    assert!(view.contains("scroll"));
    assert!(view.contains("+new line"));
    assert!(view.contains("-old line"));
}

fn ready_with_run(run: RunSummary) -> AppState {
    let mut state = AppState::new(Vec::new());
    state.apply(RuntimeEvent::Ready {
        modes: Default::default(),
        mode_overrides: Default::default(),
        suggested_verify: None,
        team: "t".to_string(),
        workspace: String::new(),
        default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
        runs: vec![run],
        members: vec![member_summary(
            "builder",
            "Builder",
            BackendKind::Codex,
            "impl",
            MemberStatus::Idle,
        )],
    });
    state
}

#[test]
fn renders_run_footer_next_step() {
    let state = ready_with_run(RunSummary {
        id: RunId(7),
        number: 0,
        goal: "ship parser".to_string(),
        status: RunStatus::Done,
        coordinator: Some(MemberId::new("builder")),
        verification: None,
        created_at: "2026-06-28 10:00:00".to_string(),
        updated_at: "2026-06-28 10:00:00".to_string(),
        attempt: 1,
        events: Vec::new(),
        steps: Vec::new(),
        mode: None,
        legacy_mode: None,
    });

    let mut terminal = Terminal::new(TestBackend::new(100, 16)).unwrap();
    terminal
        .draw(|frame| {
            let _ = render(frame, &state);
        })
        .unwrap();
    let view = format!("{}", terminal.backend());
    eprintln!("\n{view}");

    assert!(view.contains("run-7 done"));
    assert!(view.contains("/runs details"));
}

#[test]
fn renders_run_footer_step_progress() {
    let state = ready_with_run(RunSummary {
        id: RunId(7),
        number: 0,
        goal: "ship parser".to_string(),
        status: RunStatus::Running,
        coordinator: Some(MemberId::new("builder")),
        verification: None,
        created_at: "2026-06-28 10:00:00".to_string(),
        updated_at: "2026-06-28 10:00:00".to_string(),
        attempt: 1,
        events: Vec::new(),
        steps: vec![
            RunStepSummary {
                number: 1,
                status: RunStepStatus::Done,
                owner: None,
                title: "Map parser states".to_string(),
                note: None,
                updated_at: "2026-06-28 10:05:00".to_string(),
            },
            RunStepSummary {
                number: 2,
                status: RunStepStatus::Doing,
                owner: None,
                title: "Wire checklist UI".to_string(),
                note: None,
                updated_at: "2026-06-28 10:10:00".to_string(),
            },
        ],
        mode: None,
        legacy_mode: None,
    });

    let mut terminal = Terminal::new(TestBackend::new(100, 16)).unwrap();
    terminal
        .draw(|frame| {
            let _ = render(frame, &state);
        })
        .unwrap();
    let view = format!("{}", terminal.backend());
    eprintln!("\n{view}");

    assert!(view.contains("run-7 running"));
    assert!(view.contains("1/2 done"));
    assert!(view.contains("1 doing"));
    assert!(view.contains("/runs details"));
}

#[test]
fn renders_runs_drawer() {
    let mut state = ready_with_run(RunSummary {
        id: RunId(1),
        number: 0,
        goal: "ship parser".to_string(),
        status: RunStatus::Done,
        coordinator: Some(MemberId::new("builder")),
        verification: Some(RunVerification {
            command: "cargo test".to_string(),
            ok: true,
            summary: "ok".to_string(),
        }),
        created_at: "2026-06-28 10:00:00".to_string(),
        updated_at: "2026-06-28 10:15:00".to_string(),
        attempt: 1,
        events: vec![
            RunEventSummary {
                kind: "note".to_string(),
                title: "User note".to_string(),
                detail: Some("checkpoint saved".to_string()),
                created_at: "2026-06-28 10:10:00".to_string(),
                attempt: 1,
            },
            RunEventSummary {
                kind: "verification_passed".to_string(),
                title: "Verification passed".to_string(),
                detail: Some("cargo test\nok".to_string()),
                created_at: "2026-06-28 10:15:00".to_string(),
                attempt: 1,
            },
        ],
        steps: vec![
            RunStepSummary {
                number: 1,
                status: RunStepStatus::Done,
                owner: Some(MemberId::new("builder")),
                title: "Map parser states".to_string(),
                note: None,
                updated_at: "2026-06-28 10:05:00".to_string(),
            },
            RunStepSummary {
                number: 2,
                status: RunStepStatus::Blocked,
                owner: None,
                title: "Document edge cases".to_string(),
                note: Some("waiting for reviewer".to_string()),
                updated_at: "2026-06-28 10:12:00".to_string(),
            },
        ],
        mode: None,
        legacy_mode: None,
    });
    state.toggle_drawer(Drawer::Runs);

    let mut terminal = Terminal::new(TestBackend::new(90, 34)).unwrap();
    terminal
        .draw(|frame| {
            let _ = render(frame, &state);
        })
        .unwrap();
    let view = format!("{}", terminal.backend());
    eprintln!("\n{view}");

    assert!(view.contains("Runs"));
    assert!(view.contains("Enter status"));
    assert!(view.contains("Tab dispatch"));
    assert!(view.contains("x details"));
    assert!(view.contains("←→ run"));
    assert!(view.contains("View: compact"));
    assert!(view.contains("Selected: run-1"));
    assert!(view.contains("Goal: ship parser"));
    assert!(view.contains("Progress:"));
    assert!(view.contains("Action: /mode plan"));
    assert!(view.contains("Steps:"));
    // Compact mode hides the deep-dive fields.
    assert!(!view.contains("Owners:"));
    assert!(!view.contains("Next:"));
    assert!(!view.contains("Outcome:"));
    assert!(!view.contains("Stages:"));
    assert!(!view.contains("Timeline:"));
    assert!(!view.contains("checkpoint saved"));

    assert!(state.toggle_runs_detail());
    terminal
        .draw(|frame| {
            let _ = render(frame, &state);
        })
        .unwrap();
    let view = format!("{}", terminal.backend());
    eprintln!("\n{view}");

    assert!(view.contains("x compact"));
    assert!(view.contains("History: 1 run"));
    assert!(view.contains("View: details"));
    assert!(view.contains("1 verified"));
    assert!(view.contains("Selected: run-1"));
    assert!(view.contains("Goal: ship parser"));
    assert!(view.contains("Owner: builder"));
    assert!(view.contains("Attempt: #1"));
    assert!(view.contains("Time: created 06-28 10:00"));
    assert!(view.contains("updated 06-28 10:15"));
    assert!(view.contains("Progress:"));
    assert!(view.contains("1/2 done"));
    assert!(view.contains("1 blocked"));
    assert!(view.contains("Owners:"));
    assert!(view.contains("@builder 0/1 done"));
    assert!(view.contains("unassigned 1/1 1 blocked"));
    assert!(view.contains("Outcome: verified by cargo test"));
    assert!(view.contains("Next: verified"));
    assert!(view.contains("Action: /mode plan"));
    assert!(view.contains("Stages:"));
    assert!(view.contains("Steps:"));
    assert!(view.contains("@builder"));
    assert!(view.contains("Map parser states"));
    assert!(view.contains("Document edge cases"));
    assert!(view.contains("waiting for reviewer"));
    assert!(view.contains("Timeline:"));
    assert!(view.contains("User note"));
    assert!(view.contains("checkpoint saved"));
    assert!(view.contains("Verification passed"));
    assert!(view.contains("plan done"));
    assert!(view.contains("work done"));
    assert!(view.contains("verify done"));
    assert!(view.contains("run-1"));
    assert!(view.contains("Try"));
    assert!(view.contains("Steps"));
    assert!(view.contains("#1"));
    assert!(view.contains("Updated"));
    assert!(view.contains("06-28 10:15"));
    assert!(view.contains("ship parser"));
    assert!(view.contains("cargo test"));
    assert!(view.contains("ok"));
}

#[test]
fn renders_selected_run_step_action() {
    let mut state = ready_with_run(RunSummary {
        id: RunId(1),
        number: 0,
        goal: "ship parser".to_string(),
        status: RunStatus::Running,
        coordinator: Some(MemberId::new("builder")),
        verification: None,
        created_at: "2026-06-28 10:00:00".to_string(),
        updated_at: "2026-06-28 10:15:00".to_string(),
        attempt: 1,
        events: Vec::new(),
        steps: vec![RunStepSummary {
            number: 1,
            status: RunStepStatus::Doing,
            owner: Some(MemberId::new("builder")),
            title: "Wire checklist UI".to_string(),
            note: None,
            updated_at: "2026-06-28 10:05:00".to_string(),
        }],
        mode: None,
        legacy_mode: None,
    });
    state.toggle_drawer(Drawer::Runs);
    state.select_next_run_step();

    let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
    terminal
        .draw(|frame| {
            let _ = render(frame, &state);
        })
        .unwrap();
    let view = format!("{}", terminal.backend());
    eprintln!("\n{view}");

    assert!(view.contains("Action: /step done run-1 1"));
    assert!(view.contains("Dispatch: @builder Continue run-1 step #1"));
    assert!(view.contains("@builder"));
    assert!(view.contains("› 1."));
    assert!(view.contains("Wire checklist UI"));
}

#[test]
fn renders_failed_run_continue_action() {
    let mut state = ready_with_run(RunSummary {
        id: RunId(1),
        number: 0,
        goal: "ship parser".to_string(),
        status: RunStatus::Failed,
        coordinator: Some(MemberId::new("builder")),
        verification: Some(RunVerification {
            command: "cargo test".to_string(),
            ok: false,
            summary: "tests failed".to_string(),
        }),
        created_at: "2026-06-28 10:00:00".to_string(),
        updated_at: "2026-06-28 10:15:00".to_string(),
        attempt: 2,
        events: vec![RunEventSummary {
            kind: "verification_failed".to_string(),
            title: "Verification failed".to_string(),
            detail: Some("cargo test\ntests failed".to_string()),
            created_at: "2026-06-28 10:15:00".to_string(),
            attempt: 2,
        }],
        steps: Vec::new(),
        mode: None,
        legacy_mode: None,
    });
    state.toggle_drawer(Drawer::Runs);

    let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
    terminal
        .draw(|frame| {
            let _ = render(frame, &state);
        })
        .unwrap();
    let view = format!("{}", terminal.backend());
    eprintln!("\n{view}");

    assert!(view.contains("Outcome: verification failed: cargo test"));
    assert!(view.contains("Timeline:"));
    assert!(view.contains("Verification failed"));
    assert!(view.contains("Attempt: #2"));
    assert!(view.contains("Next: run the Action command to continue fixes"));
    assert!(view.contains("Action: /continue run-1 fix failing verification"));
    assert!(view.contains("#2"));
}

#[test]
fn renders_multiline_composer() {
    let mut state = AppState::new(Vec::new());
    for ch in "line one".chars() {
        state.insert_char(ch);
    }
    state.insert_newline();
    for ch in "line two".chars() {
        state.insert_char(ch);
    }

    let mut terminal = Terminal::new(TestBackend::new(60, 16)).unwrap();
    terminal
        .draw(|frame| {
            let _ = render(frame, &state);
        })
        .unwrap();
    let view = format!("{}", terminal.backend());
    eprintln!("\n{view}");

    // Both composer lines are visible (first with the prompt gutter).
    assert!(view.contains("> line one"));
    assert!(view.contains("line two"));
}

#[test]
fn renders_attached_image_inside_the_composer() {
    let dir = std::env::temp_dir().join(format!("asterline-composer-image-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("shot.png");
    std::fs::write(&path, [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']).unwrap();
    let mut state = AppState::new(Vec::new());
    state
        .attach_pending_image(crate::adapter::prompt_images::PromptImage::from_path(&path).unwrap())
        .unwrap();
    state.insert_text("123");

    let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
    terminal
        .draw(|frame| {
            let _ = render(frame, &state);
        })
        .unwrap();
    let view = format!("{}", terminal.backend());

    assert!(view.contains("> [Image #1]123"), "{view}");
    assert!(!view.contains("📎"), "{view}");
    assert!(!view.contains("shot.png"), "{view}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn screen_to_content_round_trip_edges() {
    let layout = ChatLayout {
        area: Rect::new(2, 3, 20, 4),
        first_line: 1,
        total_lines: 5,
        width: 20,
        lines: vec![
            "second line".into(),
            "third".into(),
            "fourth line here".into(),
            "fifth".into(),
        ],
        completion_area: None,
        composer_area: None,
        composer_wrap: 0,
        composer_text_origin: 0,
    };
    // Top-left of area → first_line, col 0.
    assert_eq!(
        layout.screen_to_content(layout.area.x, layout.area.y),
        Some((1, 0))
    );
    // Bottom row of area: first_line 1 + row 3 = line 4, col 3 of "fifth".
    let bottom_y = layout.area.y + layout.area.height - 1;
    assert_eq!(
        layout.screen_to_content(layout.area.x + 3, bottom_y),
        Some((4, 3))
    );
    // Out-of-area clamp: above and left of area.
    assert_eq!(layout.screen_to_content(0, 0), Some((1, 0)));
    // Past right edge of a short line clamps to last cell.
    assert_eq!(
        layout.screen_to_content(layout.area.x + 50, layout.area.y),
        Some((1, theme::display_width("second line") - 1))
    );
}

#[test]
fn large_chat_is_trimmed_before_frame_flattening() {
    let chat = (0..crate::tui::app_state::MAX_CHAT_ITEMS + 200)
        .map(|index| ChatItem::Notice {
            text: format!("notice {index}"),
        })
        .collect();
    let state = AppState::new(chat);
    let mut terminal = Terminal::new(TestBackend::new(60, 16)).unwrap();
    let mut flattened = 0;

    terminal
        .draw(|frame| {
            flattened = render(frame, &state).unwrap().lines.len();
        })
        .unwrap();

    assert_eq!(
        state.chat().len(),
        crate::tui::app_state::MAX_CHAT_ITEMS + 200
    );
    assert!(
        flattened <= 16,
        "the frame must only keep the visible viewport: {flattened}"
    );
}

#[test]
fn approval_card_renders_above_the_composer() {
    let mut state = AppState::new(Vec::new());
    state.apply(RuntimeEvent::ApprovalRequested {
        id: ApprovalId(1),
        member: Some(MemberId::new("builder")),
        action: "command".to_string(),
        body: "git push origin main".to_string(),
    });

    let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
    terminal
        .draw(|frame| {
            let _ = render(frame, &state);
        })
        .unwrap();
    let view = format!("{}", terminal.backend());
    assert!(view.contains("Approval"), "{view}");
    assert!(view.contains("command"), "{view}");
    assert!(view.contains("git push origin main"), "{view}");
    assert!(view.contains("y agree"), "{view}");
    assert!(view.contains("n deny"), "{view}");
}
