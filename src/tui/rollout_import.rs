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
use crate::tui::import_io;

const MAX_ROLLOUT_FILES: usize = 10_000;
const MAX_ROLLOUT_SCAN_ENTRIES: usize = 50_000;

/// A snapshot taken before launching the interactive session, used to import
/// only the messages added while attached.
pub struct RolloutSnapshot {
    /// The Codex session id being attached, if Asterline knows it.
    session_id: Option<String>,
    /// Workspace cwd for fresh Codex sessions where no session id exists yet.
    cwd: Option<String>,
    /// The rollout file identified for this session (if found up front).
    path: Option<PathBuf>,
    /// Number of `message` items already present in `path` before the attach.
    before: usize,
    /// When the attach started, to spot a forked rollout file.
    started: SystemTime,
}

/// Snapshot the codex rollout for `session_id` (if any) before attaching.
pub fn snapshot(session_id: Option<&str>, cwd: &str) -> RolloutSnapshot {
    let path = session_id.and_then(find_rollout);
    let before = path.as_deref().map(count_messages).unwrap_or(0);
    RolloutSnapshot {
        session_id: session_id.map(str::to_string),
        cwd: (!cwd.trim().is_empty()).then(|| cwd.to_string()),
        path,
        before,
        started: SystemTime::now(),
    }
}

/// After the attach exits, return the messages added during it (codex only).
pub fn imported_since(snapshot: RolloutSnapshot) -> Vec<ImportedMessage> {
    import_from_rollouts(snapshot, all_rollouts())
}

fn import_from_rollouts(snapshot: RolloutSnapshot, rollouts: Vec<PathBuf>) -> Vec<ImportedMessage> {
    // When resuming a known Codex session, only consider rollout files whose
    // names contain that session id. Otherwise a concurrent Codex session can
    // become the newest rollout and be imported into the wrong Asterline member.
    let target = match snapshot.session_id.as_deref() {
        Some(session_id) => {
            newest_rollout_for_session_since(&rollouts, session_id, snapshot.started)
                .or(snapshot.path)
        }
        None => snapshot
            .cwd
            .as_deref()
            .and_then(|cwd| newest_rollout_for_cwd_since(&rollouts, cwd, snapshot.started))
            .or(snapshot.path),
    };
    let Some(path) = target else {
        return Vec::new();
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

fn find_rollout(session_id: &str) -> Option<PathBuf> {
    newest_rollout_for_session(&all_rollouts(), session_id)
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

fn newest_rollout_for_cwd_since(
    rollouts: &[PathBuf],
    cwd: &str,
    since: SystemTime,
) -> Option<PathBuf> {
    rollouts
        .iter()
        .filter(|p| {
            rollout_cwd(p)
                .is_some_and(|actual| config::paths_equivalent(Path::new(&actual), Path::new(cwd)))
        })
        .filter_map(|p| modified(p).map(|m| (m, p.clone())))
        .filter(|(m, _)| *m >= since)
        .max_by_key(|(m, _)| *m)
        .map(|(_, p)| p)
}

fn rollout_cwd(path: &Path) -> Option<String> {
    let mut found = None;
    import_io::for_each_json_value(path, |value| {
        let event_type = value.get("type").and_then(Value::as_str);
        if event_type != Some("session_meta") && event_type != Some("turn_context") {
            return true;
        }
        if let Some(cwd) = value
            .get("payload")
            .and_then(|payload| payload.get("cwd"))
            .and_then(Value::as_str)
        {
            found = Some(cwd.to_string());
            return false;
        }
        true
    });
    found
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

/// Join the text of a message's content parts, dropping codex's injected
/// context blocks (environment, AGENTS.md, user-instructions wrappers).
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
        // session_meta, an injected user context, a real user msg, an assistant
        // reply, a developer message (skipped), and a reasoning item (skipped).
        let lines = [
            r#"{"type":"session_meta","payload":{"id":"abc"}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\n cwd </environment_context>"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hi there"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello back"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"sys"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"reasoning","summary":[]}}"#,
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();

        // 4 message items total (3 user/assistant/developer + 1 injected user).
        assert_eq!(count_messages(&path), 4);

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
                cwd: Some("/tmp/attached".to_string()),
                path: Some(attached.clone()),
                before: 1,
                started: SystemTime::UNIX_EPOCH,
            },
            vec![unrelated, attached],
        );

        assert_eq!(
            imported,
            vec![ImportedMessage {
                from_user: true,
                text: "typed while attached".to_string()
            }]
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
    fn fresh_attach_imports_only_rollout_from_matching_cwd() {
        let dir = std::env::temp_dir().join(format!("ast-rollout-cwd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let attached = dir.join("rollout-2026-session-new.jsonl");
        let unrelated = dir.join("rollout-2026-session-other-cwd.jsonl");

        std::fs::write(
            &attached,
            [
                session_meta_line("session-new", "/tmp/attached"),
                message_line("user", "fresh attach message"),
            ]
            .join("\n"),
        )
        .unwrap();
        std::fs::write(
            &unrelated,
            [
                session_meta_line("session-other", "/tmp/other"),
                message_line("user", "wrong cwd"),
            ]
            .join("\n"),
        )
        .unwrap();

        let imported = import_from_rollouts(
            RolloutSnapshot {
                session_id: None,
                cwd: Some("/tmp/attached".to_string()),
                path: None,
                before: 0,
                started: SystemTime::UNIX_EPOCH,
            },
            vec![unrelated, attached],
        );

        assert_eq!(
            imported,
            vec![ImportedMessage {
                from_user: true,
                text: "fresh attach message".to_string()
            }]
        );

        std::fs::remove_dir_all(&dir).ok();
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
                cwd: Some("/tmp/attached".to_string()),
                path: Some(path.clone()),
                before: 0,
                started: SystemTime::UNIX_EPOCH,
            },
            vec![path],
        );

        assert_eq!(imported.len(), import_io::MAX_IMPORTED_ITEMS);
        assert_eq!(imported.last().unwrap().text, "message-999");
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(windows)]
    #[test]
    fn fresh_attach_matches_windows_cwd_case_and_separators() {
        let dir =
            std::env::temp_dir().join(format!("ast-rollout-windows-cwd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let attached = dir.join("rollout-2026-session-new.jsonl");
        std::fs::write(
            &attached,
            [
                session_meta_line("session-new", "c:/work/asterline"),
                message_line("user", "windows attach message"),
            ]
            .join("\n"),
        )
        .unwrap();

        let imported = import_from_rollouts(
            RolloutSnapshot {
                session_id: None,
                cwd: Some(r"C:\Work\Asterline".to_string()),
                path: None,
                before: 0,
                started: SystemTime::UNIX_EPOCH,
            },
            vec![attached],
        );

        assert_eq!(imported[0].text, "windows attach message");
        std::fs::remove_dir_all(dir).ok();
    }

    fn message_line(role: &str, text: &str) -> String {
        format!(
            r#"{{"type":"response_item","payload":{{"type":"message","role":"{role}","content":[{{"type":"input_text","text":"{text}"}}]}}}}"#
        )
    }

    fn session_meta_line(session_id: &str, cwd: &str) -> String {
        format!(
            r#"{{"type":"session_meta","payload":{{"session_id":"{session_id}","cwd":"{cwd}"}}}}"#
        )
    }
}
