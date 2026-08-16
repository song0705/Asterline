use super::super::*;
use super::*;

#[test]
fn streaming_message_builds_agent_cell() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    let builder = MemberId::new("builder");
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(1),
        turn: TurnId(1),
        member: builder.clone(),
    });
    state.apply(RuntimeEvent::MessageDelta {
        msg: MessageId(1),
        text: "Hel".to_string(),
    });
    state.apply(RuntimeEvent::MessageDelta {
        msg: MessageId(1),
        text: "lo".to_string(),
    });
    state.apply(RuntimeEvent::MessageCompleted {
        msg: MessageId(1),
        text: "Hello".to_string(),
    });

    assert!(matches!(
        state.chat().last(),
        Some(ChatItem::Agent { text, .. }) if text == "Hello"
    ));
}

#[test]
fn empty_message_completion_keeps_streamed_text() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    let builder = MemberId::new("builder");

    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(1),
        turn: TurnId(1),
        member: builder.clone(),
    });
    state.apply(RuntimeEvent::MessageDelta {
        msg: MessageId(1),
        text: "streamed reply".to_string(),
    });
    state.apply(RuntimeEvent::MessageCompleted {
        msg: MessageId(1),
        text: String::new(),
    });
    assert!(matches!(
        state.chat().last(),
        Some(ChatItem::Agent { text, .. }) if text == "streamed reply"
    ));
}

#[test]
fn reasoning_is_a_live_status_and_never_enters_history() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    let builder = MemberId::new("builder");
    state.apply(RuntimeEvent::MemberStatus {
        member: builder.clone(),
        status: MemberStatus::Running,
    });
    state.apply(RuntimeEvent::Reasoning {
        member: builder.clone(),
        text: "**First look**".to_string(),
    });
    assert_eq!(
        state.active_reasoning().get(&builder).map(String::as_str),
        Some("First look")
    );
    state.apply(RuntimeEvent::ToolStarted {
        member: builder.clone(),
        tool_id: "t1".to_string(),
        name: "read".to_string(),
        summary: "roster.md".to_string(),
    });
    assert!(!state.active_reasoning().contains_key(&builder));
    state.apply(RuntimeEvent::Reasoning {
        member: builder.clone(),
        text: "**Second look**".to_string(),
    });
    state.apply(RuntimeEvent::MemberStatus {
        member: builder.clone(),
        status: MemberStatus::Idle,
    });

    assert!(
        !state
            .chat()
            .iter()
            .any(|item| matches!(item, ChatItem::Thinking { .. }))
    );
    assert!(!state.active_reasoning().contains_key(&builder));
}

#[test]
fn message_completion_does_not_finish_the_member_process() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    let builder = MemberId::new("builder");
    state.apply(RuntimeEvent::MemberStatus {
        member: builder.clone(),
        status: MemberStatus::Running,
    });
    state.apply(RuntimeEvent::Reasoning {
        member: builder.clone(),
        text: "thinking".to_string(),
    });
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(10),
        turn: TurnId(1),
        member: builder.clone(),
    });

    state.apply(RuntimeEvent::MessageCompleted {
        msg: MessageId(10),
        text: "answer".to_string(),
    });

    assert_eq!(state.members()[0].status, MemberStatus::Running);
    assert_eq!(state.running_count(), 1);
    assert!(!state.active_reasoning().contains_key(&builder));
}

#[test]
fn reasoning_status_extracts_the_first_bold_heading() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    let builder = MemberId::new("builder");
    state.apply(RuntimeEvent::Reasoning {
        member: builder.clone(),
        text: "**Checking ".to_string(),
    });
    state.apply(RuntimeEvent::Reasoning {
        member: builder.clone(),
        text: "invariants**\nThe cache must be invalidated.".to_string(),
    });
    state.apply(RuntimeEvent::TurnStarted { turn: TurnId(2) });
    state.apply(RuntimeEvent::TurnFinished { turn: TurnId(2) });

    assert_eq!(
        state.active_reasoning().get(&builder).map(String::as_str),
        Some("Checking invariants")
    );
}

#[test]
fn streamed_message_and_tool_detail_have_total_memory_bounds() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    let builder = MemberId::new("builder");
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(9),
        turn: TurnId(1),
        member: builder.clone(),
    });
    state.apply(RuntimeEvent::MessageDelta {
        msg: MessageId(9),
        text: "你".repeat(MAX_CHAT_ITEM_BYTES),
    });
    state.apply(RuntimeEvent::ToolStarted {
        member: builder.clone(),
        tool_id: "large-tool".to_string(),
        name: "shell".to_string(),
        summary: "large output".to_string(),
    });
    state.apply(RuntimeEvent::ToolProgress {
        member: builder,
        tool_id: "large-tool".to_string(),
        delta: "x".repeat(MAX_CHAT_ITEM_BYTES + 1),
    });

    let message = state
        .chat()
        .iter()
        .find_map(|item| match item {
            ChatItem::Agent { text, .. } => Some(text),
            _ => None,
        })
        .unwrap();
    assert!(message.len() <= MAX_CHAT_ITEM_BYTES);
    assert!(message.contains("output truncated"));
    assert!(message.is_char_boundary(message.len()));

    let detail = state
        .chat()
        .iter()
        .find_map(|item| match item {
            ChatItem::Tool { detail, .. } => Some(detail),
            _ => None,
        })
        .unwrap();
    assert!(detail.len() <= MAX_CHAT_ITEM_BYTES);
    assert!(detail.contains("output truncated"));
}

#[test]
fn current_conversation_keeps_its_first_message() {
    let replayed = (0..MAX_CHAT_ITEMS + 25)
        .map(|index| ChatItem::Notice {
            text: format!("history-{index}"),
        })
        .collect::<Vec<_>>();
    let mut state = AppState::new(replayed);
    assert_eq!(state.chat().len(), MAX_CHAT_ITEMS + 25);
    assert!(matches!(
        state.chat().first(),
        Some(ChatItem::Notice { text }) if text == "history-0"
    ));

    for index in 0..40 {
        state.apply(RuntimeEvent::Notice(format!(
            "large-{index}-{}",
            "x".repeat(512 * 1024)
        )));
    }
    assert!(matches!(
        state.chat().first(),
        Some(ChatItem::Notice { text }) if text == "history-0"
    ));
    assert!(
        state
            .chat()
            .iter()
            .all(|item| chat_item_bytes(item) <= MAX_CHAT_ITEM_BYTES)
    );
}

#[test]
fn oversized_replayed_and_live_items_are_visibly_truncated() {
    let mut state = AppState::new(vec![ChatItem::Notice {
        text: "r".repeat(2 * 1024 * 1024),
    }]);
    let replayed = state.chat().first().unwrap();
    assert!(chat_item_bytes(replayed) <= MAX_CHAT_ITEM_BYTES);
    assert!(matches!(replayed, ChatItem::Notice { text } if text.contains("output truncated")));

    state.apply(RuntimeEvent::UserMessage {
        turn: TurnId(1),
        targets: Vec::new(),
        body: "你".repeat(1024 * 1024),
    });
    let live = state.chat().last().unwrap();
    assert!(chat_item_bytes(live) <= MAX_CHAT_ITEM_BYTES);
    assert!(matches!(live, ChatItem::User { body, .. } if body.contains("output truncated")));
    assert!(
        state
            .chat()
            .iter()
            .all(|item| chat_item_bytes(item) <= MAX_CHAT_ITEM_BYTES)
    );
}

#[test]
fn completed_and_streaming_updates_cannot_bypass_chat_budget() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    let builder = MemberId::new("builder");
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(71),
        turn: TurnId(1),
        member: builder.clone(),
    });
    state.apply(RuntimeEvent::MessageDelta {
        msg: MessageId(71),
        text: "m".repeat(2 * 1024 * 1024),
    });
    assert_chat_budget(&state);
    state.apply(RuntimeEvent::MessageCompleted {
        msg: MessageId(71),
        text: "c".repeat(2 * 1024 * 1024),
    });
    assert_chat_budget(&state);

    state.apply(RuntimeEvent::ToolStarted {
        member: builder.clone(),
        tool_id: "bounded-tool".to_string(),
        name: "shell".to_string(),
        summary: "large".to_string(),
    });
    state.apply(RuntimeEvent::ToolProgress {
        member: builder.clone(),
        tool_id: "bounded-tool".to_string(),
        delta: "p".repeat(2 * 1024 * 1024),
    });
    assert_chat_budget(&state);
    state.apply(RuntimeEvent::ToolCompleted {
        member: builder.clone(),
        tool_id: "bounded-tool".to_string(),
        ok: true,
        output: "o".repeat(2 * 1024 * 1024),
    });
    assert_chat_budget(&state);
    state.apply(RuntimeEvent::ToolCompleted {
        member: builder,
        tool_id: "missing-tool".to_string(),
        ok: false,
        output: "f".repeat(2 * 1024 * 1024),
    });
    assert_chat_budget(&state);
    assert!(
        state
            .chat()
            .iter()
            .all(|item| chat_item_bytes(item) <= MAX_CHAT_ITEM_BYTES)
    );
}

#[test]
fn active_streams_are_preferred_but_do_not_bypass_hard_budget() {
    let mut state = AppState::new(Vec::new());
    let member = MemberId::new("builder");
    for id in 0..5 {
        state.apply(RuntimeEvent::MessageStarted {
            msg: MessageId(id),
            turn: TurnId(1),
            member: member.clone(),
        });
        state.apply(RuntimeEvent::MessageDelta {
            msg: MessageId(id),
            text: id.to_string().repeat(MAX_CHAT_ITEM_BYTES),
        });
        assert_chat_budget(&state);
    }
    assert_eq!(state.chat().len(), 5);
    state.apply(RuntimeEvent::MessageCompleted {
        msg: MessageId(0),
        text: format!(
            "final answer survives eviction {}",
            "z".repeat(MAX_CHAT_ITEM_BYTES)
        ),
    });
    assert_chat_budget(&state);
    assert!(state.chat().iter().any(|item| matches!(
        item,
        ChatItem::Agent { member: cell_member, text, .. }
            if cell_member == &member && text.contains("final answer survives eviction")
    )));
}

#[test]
fn evicted_active_tool_completion_keeps_identity_and_omission() {
    let mut state = AppState::new(Vec::new());
    let member = MemberId::new("builder");
    for id in 0..5 {
        state.apply(RuntimeEvent::ToolStarted {
            member: member.clone(),
            tool_id: format!("tool-{id}"),
            name: format!("shell-{id}"),
            summary: format!("command-{id}"),
        });
        state.apply(RuntimeEvent::ToolProgress {
            member: member.clone(),
            tool_id: format!("tool-{id}"),
            delta: id.to_string().repeat(MAX_CHAT_ITEM_BYTES),
        });
    }
    assert_chat_budget(&state);
    assert!(state.tool_index.values().all(|cell| !cell.omitted));

    state.apply(RuntimeEvent::ToolCompleted {
        member,
        tool_id: "tool-0".to_string(),
        ok: true,
        output: "completed output".to_string(),
    });

    assert_chat_budget(&state);
    assert!(state.chat().iter().any(|item| matches!(
        item,
        ChatItem::Tool {
            name,
            summary,
            detail,
            ok: Some(true),
            ..
        } if name == "shell-0"
            && summary == "command-0"
            && detail.contains("completed output")
    )));
}

#[test]
fn inactive_member_retires_unfinished_message_and_tool_cells() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    let member = MemberId::new("builder");
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(81),
        turn: TurnId(1),
        member: member.clone(),
    });
    state.apply(RuntimeEvent::MessageDelta {
        msg: MessageId(81),
        text: "partial answer".to_string(),
    });
    state.apply(RuntimeEvent::ToolStarted {
        member: member.clone(),
        tool_id: "interrupted-tool".to_string(),
        name: "shell".to_string(),
        summary: "cargo test".to_string(),
    });
    state.apply(RuntimeEvent::ToolProgress {
        member: member.clone(),
        tool_id: "interrupted-tool".to_string(),
        delta: "partial output".to_string(),
    });

    state.apply(RuntimeEvent::MemberStatus {
        member: member.clone(),
        status: MemberStatus::Idle,
    });

    assert!(!state.has_active_message(&member));
    assert_eq!(state.omitted_active_output_count(), 0);
    assert!(state.chat().iter().any(|item| matches!(
        item,
        ChatItem::Agent { text, .. }
            if text.contains("ended before a completion event")
                && text.contains("partial answer")
    )));
    assert!(state.chat().iter().any(|item| matches!(
        item,
        ChatItem::Tool {
            name,
            summary,
            detail,
            ok: Some(false),
            ..
        } if name == "shell"
            && summary == "cargo test"
            && detail.contains("ended before a completion event")
            && detail.contains("partial output")
    )));
}

#[test]
fn inactive_member_retires_evicted_live_tombstones() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    let member = MemberId::new("builder");
    for id in 0..5 {
        state.apply(RuntimeEvent::MessageStarted {
            msg: MessageId(90 + id),
            turn: TurnId(1),
            member: member.clone(),
        });
        state.apply(RuntimeEvent::MessageDelta {
            msg: MessageId(90 + id),
            text: id.to_string().repeat(MAX_CHAT_ITEM_BYTES),
        });
    }
    assert!(state.message_index.values().all(|cell| !cell.omitted));

    state.apply(RuntimeEvent::MemberStatus {
        member: member.clone(),
        status: MemberStatus::Idle,
    });

    assert!(!state.has_active_message(&member));
    assert_eq!(state.omitted_active_output_count(), 0);
    assert_chat_budget(&state);
    assert!(state.chat().iter().any(|item| matches!(
        item,
        ChatItem::Agent { text, .. }
            if text.contains("ended before a completion event")
    )));
}

#[test]
fn runtime_disconnect_retires_live_work_and_unavailable_controls() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    let member = MemberId::new("builder");
    state.apply(RuntimeEvent::MemberStatus {
        member: member.clone(),
        status: MemberStatus::Running,
    });
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(89),
        turn: TurnId(1),
        member: member.clone(),
    });
    state.apply(RuntimeEvent::RoutePaused {
        turn: TurnId(1),
        from: member,
        to: vec!["reviewer".to_string()],
        reason: "relay paused".to_string(),
        queued: 1,
    });
    state.apply(RuntimeEvent::ApprovalRequested {
        id: ApprovalId(88),
        member: None,
        action: "shell".to_string(),
        body: "publish".to_string(),
    });

    state.mark_runtime_unavailable();

    assert!(!state.runtime_available());
    assert_eq!(state.running_count(), 0);
    assert_eq!(state.members()[0].status, MemberStatus::Failed);
    assert!(state.running_since.is_empty());
    assert_eq!(state.paused_routes(), 0);
    assert!(state.pending_approvals().is_empty());
    assert!(state.message_index.is_empty());
}

#[test]
fn duplicate_tool_start_retires_the_previous_cell_and_bounds_metadata() {
    let mut state = AppState::new(Vec::new());
    let member = MemberId::new("builder");
    let id = "same-tool".repeat(MAX_ACTIVE_TOOL_ID_BYTES);
    state.apply(RuntimeEvent::ToolStarted {
        member: member.clone(),
        tool_id: id.clone(),
        name: "first".to_string(),
        summary: "old command".to_string(),
    });
    state.apply(RuntimeEvent::ToolStarted {
        member: member.clone(),
        tool_id: id.clone(),
        name: "n".repeat(MAX_CHAT_ITEM_BYTES),
        summary: "s".repeat(MAX_CHAT_ITEM_BYTES),
    });

    assert_eq!(state.tool_index.len(), 1);
    assert!(state.chat().iter().any(|item| matches!(
        item,
        ChatItem::Tool {
            name,
            detail,
            ok: Some(false),
            ..
        } if name == "first" && detail.contains("ended before a completion event")
    )));
    let active = state.tool_index.values().next().unwrap();
    assert!(active.name.len() <= MAX_ACTIVE_TOOL_NAME_BYTES);
    assert!(active.summary.len() <= MAX_ACTIVE_TOOL_SUMMARY_BYTES);
    assert!(state.tool_index.keys().next().unwrap().1.len() <= MAX_ACTIVE_TOOL_ID_BYTES);

    state.apply(RuntimeEvent::ToolCompleted {
        member,
        tool_id: id,
        ok: true,
        output: "done".to_string(),
    });
    assert!(state.tool_index.is_empty());
    assert_chat_budget(&state);
}

#[test]
fn long_tool_ids_with_the_same_prefix_remain_distinct() {
    let mut state = AppState::new(Vec::new());
    let member = MemberId::new("builder");
    let prefix = "p".repeat(MAX_ACTIVE_TOOL_ID_BYTES + 1);
    let first = format!("{prefix}-first");
    let second = format!("{prefix}-second");
    for (id, name) in [(&first, "first"), (&second, "second")] {
        state.apply(RuntimeEvent::ToolStarted {
            member: member.clone(),
            tool_id: id.to_string(),
            name: name.to_string(),
            summary: format!("{name} command"),
        });
    }
    assert_eq!(state.tool_index.len(), 2);

    state.apply(RuntimeEvent::ToolCompleted {
        member: member.clone(),
        tool_id: first,
        ok: true,
        output: "first done".to_string(),
    });
    assert_eq!(state.tool_index.len(), 1);
    state.apply(RuntimeEvent::ToolCompleted {
        member,
        tool_id: second,
        ok: true,
        output: "second done".to_string(),
    });
    assert!(state.tool_index.is_empty());
    assert!(state.chat().iter().any(|item| matches!(
        item,
        ChatItem::Tool { name, detail, .. } if name == "first" && detail == "first done"
    )));
    assert!(state.chat().iter().any(|item| matches!(
        item,
        ChatItem::Tool { name, detail, .. } if name == "second" && detail == "second done"
    )));
}

#[test]
fn prompt_history_seed_and_submissions_obey_hard_budget() {
    let replayed = (0..MAX_PROMPT_HISTORY_ITEMS + 20)
        .map(|index| ChatItem::User {
            body: format!("seed-{index}"),
            targets: Vec::new(),
            interrupted: Vec::new(),
        })
        .collect();
    let mut state = AppState::new(replayed);
    assert!(state.prompt_history.len() <= MAX_PROMPT_HISTORY_ITEMS);
    assert!(
        state.prompt_history.iter().map(String::len).sum::<usize>() <= MAX_PROMPT_HISTORY_BYTES
    );

    for index in 0..8 {
        state.record_submission(&format!(
            "large-{index}-{}",
            "x".repeat(MAX_CHAT_ITEM_BYTES)
        ));
    }
    assert!(state.prompt_history.len() <= MAX_PROMPT_HISTORY_ITEMS);
    assert!(
        state
            .prompt_history
            .iter()
            .all(|entry| entry.len() <= MAX_CHAT_ITEM_BYTES)
    );
    assert!(
        state.prompt_history.iter().map(String::len).sum::<usize>() <= MAX_PROMPT_HISTORY_BYTES
    );
    assert!(
        state
            .prompt_history
            .last()
            .is_some_and(|entry| entry.contains("output truncated"))
    );
}

#[test]
fn oversized_paste_is_utf8_bounded_and_warns_once() {
    let mut state = AppState::new(Vec::new());
    let paste = "你\r\n".repeat(MAX_COMPOSER_BYTES);

    state.insert_text(&paste);
    state.insert_text(&paste);

    let text = state.composer().text();
    assert!(text.len() <= MAX_COMPOSER_BYTES);
    assert!(text.is_char_boundary(text.len()));
    assert_eq!(
        state
            .chat()
            .iter()
            .filter(
                |item| matches!(item, ChatItem::Notice { text } if text == COMPOSER_INPUT_TRUNCATED)
            )
            .count(),
        1
    );
}

fn assert_chat_budget(state: &AppState) {
    assert!(
        state
            .chat()
            .iter()
            .all(|item| chat_item_bytes(item) <= MAX_CHAT_ITEM_BYTES)
    );
}

#[test]
fn chat_eviction_rebases_active_message_and_tool_indices() {
    let mut state = AppState::new(
        (0..MAX_CHAT_ITEMS - 2)
            .map(|index| ChatItem::Notice {
                text: format!("old-{index}"),
            })
            .collect(),
    );
    let builder = MemberId::new("builder");
    state.apply(RuntimeEvent::MessageStarted {
        msg: MessageId(99),
        turn: TurnId(1),
        member: builder.clone(),
    });
    state.apply(RuntimeEvent::ToolStarted {
        member: builder.clone(),
        tool_id: "active-tool".to_string(),
        name: "shell".to_string(),
        summary: "work".to_string(),
    });
    state.apply(RuntimeEvent::Notice("forces-eviction".to_string()));
    state.apply(RuntimeEvent::MessageDelta {
        msg: MessageId(99),
        text: "still here".to_string(),
    });
    state.apply(RuntimeEvent::ToolProgress {
        member: builder,
        tool_id: "active-tool".to_string(),
        delta: "tool output".to_string(),
    });

    assert!(state.chat().iter().any(|item| matches!(
        item,
        ChatItem::Agent { text, .. } if text == "still here"
    )));
    assert!(state.chat().iter().any(|item| matches!(
        item,
        ChatItem::Tool { detail, .. } if detail == "tool output"
    )));
    assert!(matches!(
        state.chat().first(),
        Some(ChatItem::Notice { text }) if text == "old-0"
    ));
}

#[test]
fn tool_completion_updates_existing_cell() {
    let mut state = AppState::new(Vec::new());
    state.apply(ready());
    let builder = MemberId::new("builder");
    state.apply(RuntimeEvent::ToolStarted {
        member: builder.clone(),
        tool_id: "t1".to_string(),
        name: "shell".to_string(),
        summary: "cargo test".to_string(),
    });
    state.apply(RuntimeEvent::ToolCompleted {
        member: builder,
        tool_id: "t1".to_string(),
        ok: true,
        output: "all tests passed".to_string(),
    });
    // One tool cell, now marked ok.
    let tools: Vec<_> = state
        .chat()
        .iter()
        .filter(|i| matches!(i, ChatItem::Tool { .. }))
        .collect();
    assert_eq!(tools.len(), 1);
    assert!(matches!(
        tools[0],
        ChatItem::Tool {
            ok: Some(true),
            summary,
            detail,
            ..
        } if summary == "cargo test" && detail == "all tests passed"
    ));
}

#[test]
fn same_backend_tool_id_is_isolated_by_member() {
    let mut state = AppState::new(Vec::new());
    let builder = MemberId::new("builder");
    let reviewer = MemberId::new("reviewer");
    for (member, summary) in [(&builder, "build"), (&reviewer, "review")] {
        state.apply(RuntimeEvent::ToolStarted {
            member: member.clone(),
            tool_id: "agy-step-0".to_string(),
            name: "shell".to_string(),
            summary: summary.to_string(),
        });
    }
    state.apply(RuntimeEvent::ToolProgress {
        member: builder.clone(),
        tool_id: "agy-step-0".to_string(),
        delta: "builder progress".to_string(),
    });
    state.apply(RuntimeEvent::ToolCompleted {
        member: builder.clone(),
        tool_id: "agy-step-0".to_string(),
        ok: true,
        output: "builder output".to_string(),
    });
    state.apply(RuntimeEvent::ToolCompleted {
        member: reviewer.clone(),
        tool_id: "agy-step-0".to_string(),
        ok: false,
        output: "reviewer output".to_string(),
    });

    let tools = state
        .chat()
        .iter()
        .filter_map(|item| match item {
            ChatItem::Tool {
                member,
                summary,
                detail,
                ok,
                ..
            } => Some((member, summary, detail, ok)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(tools.len(), 2);
    assert!(matches!(
        tools[0],
        (member, summary, detail, Some(true))
            if member == &builder && summary == "build" && detail == "builder output"
    ));
    assert!(matches!(
        tools[1],
        (member, summary, detail, Some(false))
            if member == &reviewer && summary == "review" && detail == "reviewer output"
    ));
}
