use super::*;
use crate::domain::event::UiCommand;
use crate::domain::mode::{ModesConfig, ReviewModeConfig, TerminalMode};
use crate::tui::mode_editor::ModeEditorOutcome;
use crossterm::event::{KeyCode, KeyModifiers};

fn ready_two() -> RuntimeEvent {
    RuntimeEvent::Ready {
        modes: Default::default(),
        mode_overrides: Default::default(),
        suggested_verify: Some("cargo test".to_string()),
        team: "mixed".to_string(),
        workspace: "/tmp/ws".to_string(),
        default_target: Some(DefaultTarget::Member(MemberId::new("builder"))),
        runs: Vec::new(),
        members: vec![
            MemberSummary {
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
                approvals_reviewer: crate::domain::team::CodexApprovalsReviewer::User,
                session_policy: SessionPolicy::Resume,
            },
            MemberSummary {
                id: MemberId::new("reviewer"),
                display_name: "Reviewer".to_string(),
                backend: BackendKind::Claude,
                role: "review".to_string(),
                status: MemberStatus::Idle,
                session: None,
                cwd: String::new(),
                model: None,
                effort: None,
                sandbox: SandboxPolicy::ReadOnly,
                permission_mode: Some(PermissionMode::Default),
                approvals_reviewer: crate::domain::team::CodexApprovalsReviewer::User,
                session_policy: SessionPolicy::Resume,
            },
        ],
    }
}

fn open_mode_panel() -> AppState {
    let mut state = AppState::new(Vec::new());
    state.apply(ready_two());
    state.toggle_drawer(Drawer::Mode);
    state
}

#[test]
fn slash_mode_opens_the_mode_drawer() {
    let state = open_mode_panel();
    assert_eq!(state.drawer(), Some(Drawer::Mode));
    assert!(state.mode_editor().is_some());
    assert_eq!(
        state.mode_editor().map(|editor| editor.selected_mode()),
        Some(TerminalMode::Normal)
    );
    assert_eq!(
        state.mode_editor().map(|editor| editor.selected_index()),
        Some(0)
    );
    assert_eq!(
        state.mode_editor().map(|editor| editor.field_index()),
        Some(0)
    );
}

#[test]
fn ready_seeds_mode_bindings_and_suggested_verify() {
    let mut state = AppState::new(Vec::new());
    let mut ready = ready_two();
    if let RuntimeEvent::Ready {
        modes,
        mode_overrides,
        suggested_verify,
        ..
    } = &mut ready
    {
        *modes = ModesConfig {
            review: Some(ReviewModeConfig {
                max_iterations: Some(4),
                ..ReviewModeConfig::default()
            }),
            ..ModesConfig::default()
        };
        *mode_overrides = ModesConfig {
            review: Some(ReviewModeConfig {
                max_iterations: Some(6),
                ..ReviewModeConfig::default()
            }),
            ..ModesConfig::default()
        };
        *suggested_verify = Some("just check".to_string());
    }
    state.apply(ready);
    assert_eq!(
        state
            .modes()
            .review
            .as_ref()
            .and_then(|cfg| cfg.max_iterations),
        Some(4)
    );
    assert_eq!(
        state
            .mode_overrides()
            .review
            .as_ref()
            .and_then(|cfg| cfg.max_iterations),
        Some(6)
    );
    assert_eq!(state.suggested_verify(), Some("just check"));
}

#[test]
fn enter_on_list_opens_fields_without_selecting_mode() {
    let mut state = open_mode_panel();
    state.handle_mode_editor_key(KeyCode::Down, KeyModifiers::NONE);
    let outcome = state.handle_mode_editor_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(outcome, ModeEditorOutcome::Consumed(Vec::new()));
    assert!(
        state
            .mode_editor()
            .is_some_and(|editor| editor.field_mode())
    );
}

#[test]
fn s_submits_pending_overrides_before_set_mode() {
    let mut state = open_mode_panel();
    state.handle_mode_editor_key(KeyCode::Down, KeyModifiers::NONE); // review
    state.handle_mode_editor_key(KeyCode::Enter, KeyModifiers::NONE);
    state.handle_mode_editor_key(KeyCode::Down, KeyModifiers::NONE);
    state.handle_mode_editor_key(KeyCode::Down, KeyModifiers::NONE); // max_iterations
    state.handle_mode_editor_key(KeyCode::Right, KeyModifiers::NONE);
    let outcome = state.handle_mode_editor_key(KeyCode::Char('s'), KeyModifiers::NONE);
    match outcome {
        ModeEditorOutcome::Consumed(commands) => {
            assert!(matches!(
                commands.as_slice(),
                [
                    UiCommand::SetModeOverrides { .. },
                    UiCommand::SetMode {
                        mode: TerminalMode::Review
                    }
                ]
            ));
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn w_emits_save_mode_defaults() {
    let mut state = open_mode_panel();
    state.handle_mode_editor_key(KeyCode::Down, KeyModifiers::NONE);
    state.handle_mode_editor_key(KeyCode::Enter, KeyModifiers::NONE);
    state.handle_mode_editor_key(KeyCode::Down, KeyModifiers::NONE);
    state.handle_mode_editor_key(KeyCode::Down, KeyModifiers::NONE);
    state.handle_mode_editor_key(KeyCode::Right, KeyModifiers::NONE);
    let outcome = state.handle_mode_editor_key(KeyCode::Char('w'), KeyModifiers::NONE);
    match outcome {
        ModeEditorOutcome::Consumed(commands) => {
            assert!(matches!(
                commands.as_slice(),
                [
                    UiCommand::SetModeOverrides { .. },
                    UiCommand::SaveModeDefaults {
                        mode: TerminalMode::Review
                    }
                ]
            ));
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn team_allow_add_members_toggle_applies_to_this_chat() {
    let mut state = open_mode_panel();
    for _ in 0..4 {
        state.handle_mode_editor_key(KeyCode::Down, KeyModifiers::NONE);
    }
    assert_eq!(
        state.mode_editor().map(|editor| editor.selected_mode()),
        Some(TerminalMode::Team)
    );
    state.handle_mode_editor_key(KeyCode::Enter, KeyModifiers::NONE);
    state.handle_mode_editor_key(KeyCode::Down, KeyModifiers::NONE);
    let outcome = state.handle_mode_editor_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(outcome, ModeEditorOutcome::Consumed(Vec::new()));
    let outcome = state.handle_mode_editor_key(KeyCode::Char('s'), KeyModifiers::NONE);
    match outcome {
        ModeEditorOutcome::Consumed(commands) => {
            assert!(matches!(
                commands.as_slice(),
                [
                    UiCommand::SetModeOverrides { overrides },
                    UiCommand::SetMode {
                        mode: TerminalMode::Team
                    }
                ] if overrides.team.as_ref().and_then(|cfg| cfg.allow_add_members) == Some(true)
            ));
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn session_reset_keeps_conversation_overrides() {
    let mut state = AppState::new(Vec::new());
    let overrides = ModesConfig {
        review: Some(ReviewModeConfig {
            max_iterations: Some(9),
            ..ReviewModeConfig::default()
        }),
        ..ModesConfig::default()
    };
    let mut ready = ready_two();
    if let RuntimeEvent::Ready { mode_overrides, .. } = &mut ready {
        *mode_overrides = overrides.clone();
    }
    state.apply(ready);
    state.apply(RuntimeEvent::SessionReset);
    assert_eq!(state.mode_overrides(), &overrides);
    assert_eq!(state.active_mode(), TerminalMode::Normal);
    assert!(state.mode_editor().is_none());
}
