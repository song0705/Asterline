//! Import messages from a member's native backend session transcript after an
//! interactive attach.
//!
//! Codex records every session as a JSONL "rollout" at
//! `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<session_id>.jsonl`, one event per
//! line. When the user attaches to a member's `codex resume <session_id>`,
//! chats, and exits, the new turns are appended there. We diff the rollout
//! around the attach (count messages before, re-read after) and import the
//! delta so it shows up — and persists — in the Asterline transcript.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

use crate::domain::{config, event::ImportedMessage};
use crate::tui::attach::AttachOutcome;
use crate::tui::import_io;

const MAX_ROLLOUT_FILES: usize = 10_000;
const MAX_ROLLOUT_SCAN_ENTRIES: usize = 50_000;

/// A snapshot taken before launching the interactive session, used to import
/// only the messages added while attached.
pub struct RolloutSnapshot {
    /// The Codex session id being attached, if Asterline knows it.
    session_id: Option<String>,
    /// The rollout file identified for this session (if found up front).
    path: Option<PathBuf>,
    /// Number of `message` items already present in `path` before the attach.
    before: usize,
    /// When the attach started, to spot a forked rollout file.
    started: SystemTime,
}

/// Snapshot the codex rollout for `session_id` (if any) before attaching.
pub fn snapshot(session_id: Option<&str>, _cwd: &str) -> RolloutSnapshot {
    // A fresh attach is fail-closed and does not inspect nearby rollouts, so
    // avoid walking the entire Codex history until there is a bound id to diff.
    let path = session_id.and_then(|id| newest_rollout_for_session(&all_rollouts(), id));
    let before = path.as_deref().map(count_messages).unwrap_or(0);
    RolloutSnapshot {
        session_id: session_id.map(str::to_string),
        path,
        before,
        started: SystemTime::now(),
    }
}

/// After the attach exits, return the messages added during it (codex only).
pub fn imported_since(snapshot: RolloutSnapshot) -> Vec<ImportedMessage> {
    imported_attach_since(snapshot).items
}

/// Return messages from the already-bound native session. A fresh native
/// session is deliberately not guessed from nearby rollout files: another
/// concurrent CLI in the same workspace cannot be distinguished reliably.
pub(crate) fn imported_attach_since(snapshot: RolloutSnapshot) -> AttachOutcome {
    import_from_rollouts(snapshot, all_rollouts())
}

fn import_from_rollouts(snapshot: RolloutSnapshot, rollouts: Vec<PathBuf>) -> AttachOutcome {
    // When resuming a known Codex session, only consider rollout files whose
    // names contain that session id. Otherwise a concurrent Codex session can
    // become the newest rollout and be imported into the wrong Asterline member.
    let Some(session_id) = snapshot.session_id.as_deref() else {
        return AttachOutcome {
            notice: Some(
                "the fresh Codex session is not yet bound to this member, so its transcript was not imported; select its session ID in /team before the next attach"
                    .to_string(),
            ),
            ..AttachOutcome::default()
        };
    };
    let target =
        newest_rollout_for_session_since(&rollouts, session_id, snapshot.started).or(snapshot.path);
    let fallback_session = import_io::safe_session_id(session_id);
    let Some(path) = target else {
        return AttachOutcome::default();
    };
    let mut imported = Vec::new();
    let mut retained_bytes = 0_usize;
    let mut message_index = 0_usize;
    import_io::for_each_json_value(&path, |value| {
        let Some(message) = parse_rollout_message(&value) else {
            return true;
        };
        let index = message_index;
        message_index = message_index.saturating_add(1);
        if index < snapshot.before {
            return true;
        }
        let Some(message) = to_imported(message) else {
            return true;
        };
        import_io::push_imported_bounded(&mut imported, &mut retained_bytes, message)
    });
    AttachOutcome {
        items: imported,
        // The session was selected before attach, so preserve that established
        // identity instead of trusting uncorrelated transcript metadata.
        session: fallback_session,
        notice: None,
    }
}

/// Load every importable message from the bound Codex session's rollout.
pub(crate) fn messages_for_session(session_id: &str) -> Vec<ImportedMessage> {
    let Some(path) = newest_rollout_for_session(&all_rollouts(), session_id) else {
        return Vec::new();
    };
    let mut imported = Vec::new();
    let mut retained_bytes = 0_usize;
    import_io::for_each_json_value(&path, |value| {
        let Some(message) = parse_rollout_message(&value).and_then(to_imported) else {
            return true;
        };
        import_io::push_imported_bounded(&mut imported, &mut retained_bytes, message)
    });
    imported
}

/// `$CODEX_HOME/sessions`, or the platform user profile's `.codex/sessions`.
fn sessions_dir() -> Option<PathBuf> {
    sessions_dir_from_codex_home(config::codex_home_dir())
}

fn sessions_dir_from_codex_home(codex_home: Option<PathBuf>) -> Option<PathBuf> {
    let dir = codex_home?.join("sessions");
    dir.is_dir().then_some(dir)
}

/// Recursively collect every `*.jsonl` rollout under the sessions directory.
fn all_rollouts() -> Vec<PathBuf> {
    sessions_dir()
        .map(|dir| collect_rollouts(&dir))
        .unwrap_or_default()
}

fn collect_rollouts(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut entries_remaining = MAX_ROLLOUT_SCAN_ENTRIES;
    collect_jsonl(dir, &mut out, &mut entries_remaining, 0);
    out
}

fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>, entries_remaining: &mut usize, depth: usize) {
    if depth > 6 || out.len() >= MAX_ROLLOUT_FILES || *entries_remaining == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if out.len() >= MAX_ROLLOUT_FILES || *entries_remaining == 0 {
            break;
        }
        *entries_remaining -= 1;
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_jsonl(&path, out, entries_remaining, depth + 1);
        } else if file_type.is_file()
            && path
                .extension()
                .is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case("jsonl"))
            && path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("rollout-"))
        {
            out.push(path);
        }
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

fn newest_rollout_for_session(rollouts: &[PathBuf], session_id: &str) -> Option<PathBuf> {
    rollouts
        .iter()
        .filter(|p| rollout_matches_session(p, session_id))
        .filter_map(|p| modified(p).map(|m| (m, p.clone())))
        .max_by_key(|(m, _)| *m)
        .map(|(_, p)| p)
}

fn newest_rollout_for_session_since(
    rollouts: &[PathBuf],
    session_id: &str,
    since: SystemTime,
) -> Option<PathBuf> {
    rollouts
        .iter()
        .filter(|p| rollout_matches_session(p, session_id))
        .filter_map(|p| modified(p).map(|m| (m, p.clone())))
        .filter(|(m, _)| *m >= since)
        .max_by_key(|(m, _)| *m)
        .map(|(_, p)| p)
}

fn rollout_matches_session(path: &Path, session_id: &str) -> bool {
    if session_id.is_empty() || session_id.contains(['/', '\\']) {
        return false;
    }
    let suffix = format!("-{session_id}.jsonl");
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().ends_with(&suffix))
}

/// One parsed `message` response item from the rollout.
struct RolloutMessage {
    role: String,
    text: String,
}

fn count_messages(path: &Path) -> usize {
    let mut count = 0_usize;
    import_io::for_each_json_value(path, |value| {
        if parse_rollout_message(&value).is_some() {
            count = count.saturating_add(1);
        }
        true
    });
    count
}

#[cfg(test)]
fn parse_messages(path: &Path) -> Vec<RolloutMessage> {
    let mut out = Vec::new();
    import_io::for_each_json_value(path, |value| {
        if let Some(message) = parse_rollout_message(&value) {
            out.push(message);
        }
        true
    });
    out
}

fn parse_rollout_message(value: &Value) -> Option<RolloutMessage> {
    if value.get("type").and_then(Value::as_str) != Some("response_item") {
        return None;
    }
    let payload = value.get("payload")?;
    if payload.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    let role = payload
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let text = payload
        .get("content")
        .and_then(Value::as_array)
        .map(|items| join_text(items))
        .unwrap_or_default();
    Some(RolloutMessage { role, text })
}

/// Join the text of a message's content parts, dropping Codex's injected
/// context blocks (environment, plugin inventory, AGENTS.md, and
/// user-instructions wrappers). These are encoded as `user` messages in a
/// rollout but are not user-authored chat content.
fn join_text(items: &[Value]) -> String {
    let mut parts = Vec::new();
    for item in items {
        let Some(text) = item.get("text").and_then(Value::as_str) else {
            continue;
        };
        if is_injected_context(text) {
            continue;
        }
        parts.push(text.trim_end().to_string());
    }
    parts.join("\n").trim().to_string()
}

fn is_injected_context(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("<environment_context>")
        || t.starts_with("<recommended_plugins>")
        || t.starts_with("<user_instructions>")
        || t.starts_with("# AGENTS.md")
        || t.starts_with("<INSTRUCTIONS>")
}

fn to_imported(msg: RolloutMessage) -> Option<ImportedMessage> {
    let from_user = match msg.role.as_str() {
        "user" => true,
        "assistant" => false,
        // developer / system / tool messages are not part of the chat.
        _ => return None,
    };
    let text = import_io::imported_text(msg.text.trim().to_string())?;
    if text.is_empty() {
        return None;
    }
    Some(ImportedMessage { from_user, text })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_directory_honors_custom_codex_home_without_environment_mutation() {
        let root =
            std::env::temp_dir().join(format!("ast-rollout-custom-home-{}", std::process::id()));
        let sessions = root.join("sessions");
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&sessions).unwrap();

        assert_eq!(
            sessions_dir_from_codex_home(Some(root.clone())),
            Some(sessions)
        );

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn parses_and_filters_rollout_messages() {
        let dir = std::env::temp_dir().join(format!("ast-rollout-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rollout-x-abc.jsonl");
        // session_meta, injected user context/plugin inventory, a real user
        // message, an assistant reply, a developer message (skipped), and a
        // reasoning item (skipped).
        let lines = [
            r#"{"type":"session_meta","payload":{"id":"abc"}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\n cwd </environment_context>"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<recommended_plugins>\n plugin list </recommended_plugins>"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hi there"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello back"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"sys"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"reasoning","summary":[]}}"#,
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();

        // 5 message items total (4 user/assistant/developer + 2 injected user).
        assert_eq!(count_messages(&path), 5);

        // Import everything: injected context dropped, developer dropped.
        let imported: Vec<ImportedMessage> = parse_messages(&path)
            .into_iter()
            .filter_map(to_imported)
            .collect();
        assert_eq!(
            imported,
            vec![
                ImportedMessage {
                    from_user: true,
                    text: "hi there".to_string()
                },
                ImportedMessage {
                    from_user: false,
                    text: "hello back".to_string()
                },
            ]
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parser_drains_oversized_rows_and_skips_oversized_messages() {
        let dir =
            std::env::temp_dir().join(format!("ast-rollout-large-row-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rollout-large.jsonl");
        let mut fixture = "x".repeat(import_io::MAX_JSONL_LINE_BYTES + 1);
        fixture.push('\n');
        fixture.push_str(&message_line(
            "assistant",
            &"y".repeat(import_io::MAX_IMPORTED_MESSAGE_BYTES + 1),
        ));
        fixture.push('\n');
        fixture.push_str(&message_line("assistant", "still parsed"));
        std::fs::write(&path, fixture).unwrap();

        let parsed = parse_messages(&path)
            .into_iter()
            .filter_map(to_imported)
            .collect::<Vec<_>>();

        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].text, "still parsed");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn import_prefers_matching_session_rollout_over_newer_unrelated_rollout() {
        let dir = std::env::temp_dir().join(format!("ast-rollout-match-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let attached = dir.join("rollout-2026-session-abc.jsonl");
        let unrelated = dir.join("rollout-2026-session-other.jsonl");

        std::fs::write(
            &attached,
            [
                message_line("user", "already imported"),
                message_line("user", "typed while attached"),
            ]
            .join("\n"),
        )
        .unwrap();
        std::fs::write(&unrelated, message_line("user", "wrong session")).unwrap();

        let imported = import_from_rollouts(
            RolloutSnapshot {
                session_id: Some("session-abc".to_string()),
                path: Some(attached.clone()),
                before: 1,
                started: SystemTime::UNIX_EPOCH,
            },
            vec![unrelated, attached],
        );

        assert_eq!(
            imported.items,
            vec![ImportedMessage {
                from_user: true,
                text: "typed while attached".to_string()
            }]
        );
        assert_eq!(
            imported.session.as_ref().map(|session| session.as_str()),
            Some("session-abc")
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rollout_session_matching_is_exact_and_rejects_empty_ids() {
        let path = Path::new("rollout-2026-session-abc.jsonl");
        assert!(rollout_matches_session(path, "session-abc"));
        assert!(!rollout_matches_session(path, "session"));
        assert!(!rollout_matches_session(path, ""));
        assert!(!rollout_matches_session(path, "../session-abc"));
    }

    #[test]
    fn fresh_attach_never_guesses_a_rollout_or_session() {
        let path = PathBuf::from("rollout-2026-session-unrelated.jsonl");
        let imported = import_from_rollouts(
            RolloutSnapshot {
                session_id: None,
                path: None,
                before: 0,
                started: SystemTime::UNIX_EPOCH,
            },
            vec![path],
        );

        assert!(imported.items.is_empty());
        assert!(imported.session.is_none());
        assert!(imported.notice.is_some());
    }

    #[test]
    fn attached_codex_import_stops_at_aggregate_item_budget() {
        let dir =
            std::env::temp_dir().join(format!("ast-rollout-import-budget-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rollout-2026-session-budget.jsonl");
        let lines = (0..import_io::MAX_IMPORTED_ITEMS + 25)
            .map(|index| message_line("user", &format!("message-{index}")))
            .collect::<Vec<_>>();
        std::fs::write(&path, lines.join("\n")).unwrap();

        let imported = import_from_rollouts(
            RolloutSnapshot {
                session_id: Some("session-budget".to_string()),
                path: Some(path.clone()),
                before: 0,
                started: SystemTime::UNIX_EPOCH,
            },
            vec![path],
        );

        assert_eq!(imported.items.len(), import_io::MAX_IMPORTED_ITEMS);
        assert_eq!(imported.items.last().unwrap().text, "message-999");
        std::fs::remove_dir_all(dir).ok();
    }

    fn message_line(role: &str, text: &str) -> String {
        format!(
            r#"{{"type":"response_item","payload":{{"type":"message","role":"{role}","content":[{{"type":"input_text","text":"{text}"}}]}}}}"#
        )
    }
}
