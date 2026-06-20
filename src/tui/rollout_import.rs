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

use crate::domain::event::ImportedMessage;

/// A snapshot taken before launching the interactive session, used to import
/// only the messages added while attached.
pub struct RolloutSnapshot {
    /// The rollout file identified for this session (if found up front).
    path: Option<PathBuf>,
    /// Number of `message` items already present in `path` before the attach.
    before: usize,
    /// When the attach started, to spot a forked rollout file.
    started: SystemTime,
}

/// Snapshot the codex rollout for `session_id` (if any) before attaching.
pub fn snapshot(session_id: Option<&str>) -> RolloutSnapshot {
    let path = session_id.and_then(find_rollout);
    let before = path.as_deref().map(count_messages).unwrap_or(0);
    RolloutSnapshot {
        path,
        before,
        started: SystemTime::now(),
    }
}

/// After the attach exits, return the messages added during it (codex only).
pub fn imported_since(snapshot: RolloutSnapshot) -> Vec<ImportedMessage> {
    // The file codex wrote during the attach is the most-recently-modified
    // rollout at/after the start (covers both append-in-place and fork).
    let target = newest_rollout_since(snapshot.started).or(snapshot.path);
    let Some(path) = target else {
        return Vec::new();
    };
    let messages = parse_messages(&path);
    messages
        .into_iter()
        .skip(snapshot.before)
        .filter_map(to_imported)
        .collect()
}

/// `~/.codex/sessions`, if `HOME` is set.
fn sessions_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let dir = Path::new(&home).join(".codex").join("sessions");
    dir.is_dir().then_some(dir)
}

/// Recursively collect every `*.jsonl` rollout under the sessions directory.
fn all_rollouts() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(dir) = sessions_dir() {
        collect_jsonl(&dir, &mut out, 0);
    }
    out
}

fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 6 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl(&path, out, depth + 1);
        } else if path.extension().is_some_and(|e| e == "jsonl")
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("rollout-"))
        {
            out.push(path);
        }
    }
}

fn find_rollout(session_id: &str) -> Option<PathBuf> {
    all_rollouts().into_iter().find(|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.contains(session_id))
    })
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

fn newest_rollout_since(since: SystemTime) -> Option<PathBuf> {
    all_rollouts()
        .into_iter()
        .filter_map(|p| modified(&p).map(|m| (m, p)))
        .filter(|(m, _)| *m >= since)
        .max_by_key(|(m, _)| *m)
        .map(|(_, p)| p)
}

/// One parsed `message` response item from the rollout.
struct RolloutMessage {
    role: String,
    text: String,
}

fn count_messages(path: &Path) -> usize {
    parse_messages(path).len()
}

fn parse_messages(path: &Path) -> Vec<RolloutMessage> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in content.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("response_item") {
            continue;
        }
        let payload = match value.get("payload") {
            Some(p) => p,
            None => continue,
        };
        if payload.get("type").and_then(Value::as_str) != Some("message") {
            continue;
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
        out.push(RolloutMessage { role, text });
    }
    out
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
    let text = msg.text.trim().to_string();
    if text.is_empty() {
        return None;
    }
    Some(ImportedMessage { from_user, text })
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
