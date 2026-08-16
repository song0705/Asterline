use super::*;
use crate::domain::mode::{ModesConfig, ReviewModeConfig};

fn review_iteration_overrides(max_iterations: u32) -> ModesConfig {
    ModesConfig {
        review: Some(ReviewModeConfig {
            max_iterations: Some(max_iterations),
            ..ReviewModeConfig::default()
        }),
        ..ModesConfig::default()
    }
}

#[test]
fn session_overrides_change_the_next_resolve_not_an_active_mode_session() {
    let mut rt = runtime();
    rt.on_ui_command(UiCommand::SetModeOverrides {
        overrides: review_iteration_overrides(5),
    });
    let started = rt.on_ui_command(run_mode("fix parser"));
    let first = started
        .events
        .iter()
        .find_map(|event| match event {
            RuntimeEvent::RunUpdated { run } => run.mode.as_ref().map(|mode| mode.state.clone()),
            _ => None,
        })
        .expect("review run");
    assert_eq!(first.max_iterations, 5);

    rt.on_ui_command(UiCommand::SetModeOverrides {
        overrides: review_iteration_overrides(8),
    });
    let still = rt
        .store
        .latest_run()
        .unwrap()
        .expect("run exists")
        .mode
        .expect("mode state")
        .state;
    assert_eq!(still.max_iterations, 5);
}

#[test]
fn new_session_clears_mode_overrides() {
    let mut rt = runtime();
    rt.on_ui_command(UiCommand::SetModeOverrides {
        overrides: review_iteration_overrides(6),
    });
    match rt.ready_event() {
        RuntimeEvent::Ready { mode_overrides, .. } => {
            assert_eq!(
                mode_overrides.review.and_then(|cfg| cfg.max_iterations),
                Some(6)
            );
        }
        other => panic!("unexpected {other:?}"),
    }

    rt.on_ui_command(UiCommand::NewSession);
    match rt.ready_event() {
        RuntimeEvent::Ready { mode_overrides, .. } => {
            assert_eq!(mode_overrides, ModesConfig::default());
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn mode_overrides_round_trip_through_snapshot() {
    let path = std::env::temp_dir().join(format!(
        "asterline-mode-overrides-{}.sqlite3",
        std::process::id()
    ));
    remove_sqlite_test_files(&path);
    {
        let store = SqliteStore::open(&path).unwrap();
        let mut rt = TeamRuntime::new(team(), store).with_approvals(false);
        rt.on_ui_command(UiCommand::SetModeOverrides {
            overrides: review_iteration_overrides(7),
        });
        rt.on_ui_command(UiCommand::SetMode {
            mode: TerminalMode::Review,
        });
    }

    let restored = TeamRuntime::new(team(), SqliteStore::open(&path).unwrap());
    match restored.ready_event() {
        RuntimeEvent::Ready { mode_overrides, .. } => {
            assert_eq!(
                mode_overrides.review.and_then(|cfg| cfg.max_iterations),
                Some(7)
            );
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(restored.active_mode(), TerminalMode::Review);
    drop(restored);
    remove_sqlite_test_files(&path);
}

#[test]
fn save_mode_defaults_writes_team_json_and_clears_conversation_overrides() {
    let mut rt = runtime();
    rt.on_ui_command(UiCommand::SetModeOverrides {
        overrides: review_iteration_overrides(5),
    });
    let step = rt.on_ui_command(UiCommand::SaveModeDefaults {
        mode: TerminalMode::Review,
    });
    let persisted = step.persist_team.expect("team.json write requested");
    assert_eq!(
        persisted
            .modes
            .review
            .as_ref()
            .and_then(|cfg| cfg.max_iterations),
        Some(5)
    );
    assert_eq!(persisted.members.len(), 2);
    match rt.ready_event() {
        RuntimeEvent::Ready {
            modes,
            mode_overrides,
            ..
        } => {
            assert_eq!(
                modes.review.as_ref().and_then(|cfg| cfg.max_iterations),
                Some(5)
            );
            assert_eq!(mode_overrides, ModesConfig::default());
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn save_mode_defaults_rejects_illegal_bindings_without_writing() {
    let mut rt = runtime();
    let illegal = ModesConfig {
        review: Some(ReviewModeConfig {
            builder: Some(MemberId::new("builder")),
            reviewer: Some(MemberId::new("builder")),
            ..ReviewModeConfig::default()
        }),
        ..ModesConfig::default()
    };
    let rejected = rt.on_ui_command(UiCommand::SetModeOverrides { overrides: illegal });
    assert!(rejected.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text) if text.contains("two distinct")
    )));
    assert!(rejected.persist_team.is_none());
    match rt.ready_event() {
        RuntimeEvent::Ready { mode_overrides, .. } => {
            assert_eq!(mode_overrides, ModesConfig::default());
        }
        other => panic!("unexpected {other:?}"),
    }

    let save = rt.on_ui_command(UiCommand::SaveModeDefaults {
        mode: TerminalMode::Review,
    });
    assert!(save.persist_team.is_none());
    assert!(save.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::Notice(text) if text.contains("no this-chat overrides")
    )));
}

#[test]
fn set_mode_overrides_emits_modes_updated_not_ready() {
    let mut rt = runtime();
    let step = rt.on_ui_command(UiCommand::SetModeOverrides {
        overrides: review_iteration_overrides(4),
    });
    assert!(step.events.iter().any(|event| matches!(
        event,
        RuntimeEvent::ModesUpdated { overrides, .. }
            if overrides.review.as_ref().and_then(|cfg| cfg.max_iterations) == Some(4)
    )));
    assert!(
        !step
            .events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::Ready { .. }))
    );
}
