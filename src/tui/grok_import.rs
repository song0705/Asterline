//! Import messages from a Grok Build session transcript.
//!
//! Grok stores sessions at `~/.grok/sessions/<urlencoded-cwd>/<session-id>/`
//! with `chat_history.jsonl` (one JSON object per line). Users often continue
//! that same session in the Grok CLI while Asterline is closed; startup sync
//! reads the file and imports only rows Asterline has not seen yet.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::domain::{config, event::ImportedMessage};
use crate::tui::import_io;

const MAX_SESSION_SCAN_DEPTH: usize = 8;
const MAX_SESSION_SCAN_ENTRIES: usize = 50_000;

/// Load importable user/assistant rows from a bound Grok session.
pub(crate) fn messages_for_session(session_id: &str) -> Vec<ImportedMessage> {
    let Some(path) = find_chat_history(session_id) else {
        return Vec::new();
    };
    messages_from_path(&path)
}

fn find_chat_history(session_id: &str) -> Option<PathBuf> {
    if import_io::safe_session_id(session_id).is_none() {
        return None;
    }
    let root = config::user_home_dir()?.join(".grok").join("sessions");
    find_chat_history_under(&root, session_id)
}

fn find_chat_history_under(root: &Path, session_id: &str) -> Option<PathBuf> {
    let mut remaining = MAX_SESSION_SCAN_ENTRIES;
    find_named_history(root, session_id, 0, &mut remaining)
}

fn find_named_history(
    dir: &Path,
    session_id: &str,
    depth: usize,
    remaining: &mut usize,
) -> Option<PathBuf> {
    if depth > MAX_SESSION_SCAN_DEPTH || *remaining == 0 {
        return None;
    }
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        if *remaining == 0 {
            return None;
        }
        *remaining -= 1;
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if path.file_name().is_some_and(|name| name == session_id) {
                let history = path.join("chat_history.jsonl");
                if history.is_file() {
                    return Some(history);
                }
            }
            if let Some(found) = find_named_history(&path, session_id, depth + 1, remaining) {
                return Some(found);
            }
        }
    }
    None
}

fn messages_from_path(path: &Path) -> Vec<ImportedMessage> {
    let mut out = Vec::new();
    let mut retained_bytes = 0_usize;
    import_io::for_each_json_value(path, |value| {
        let Some(message) = classify_row(&value) else {
            return true;
        };
        import_io::push_imported_bounded(&mut out, &mut retained_bytes, message)
    });
    out
}

fn classify_row(value: &Value) -> Option<ImportedMessage> {
    match value.get("type").and_then(Value::as_str)? {
        "user" => {
            if value.get("synthetic_reason").is_some() {
                return None;
            }
            let text = import_io::imported_text(user_text(value.get("content")?)?)?;
            if text.is_empty() || is_injected_user_meta(&text) {
                return None;
            }
            Some(ImportedMessage {
                from_user: true,
                text,
            })
        }
        "assistant" => {
            let text = import_io::imported_text(assistant_text(value.get("content")?)?)?;
            if text.is_empty() {
                return None;
            }
            Some(ImportedMessage {
                from_user: false,
                text,
            })
        }
        _ => None,
    }
}

fn user_text(content: &Value) -> Option<String> {
    let raw = concat_text(content)?;
    if let Some(query) = extract_tag(&raw, "user_query") {
        return Some(query);
    }
    Some(raw)
}

fn assistant_text(content: &Value) -> Option<String> {
    concat_text(content)
}

fn concat_text(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.trim().to_string()),
        Value::Array(blocks) => {
            let mut parts = Vec::new();
            for block in blocks {
                if block.get("type").and_then(Value::as_str) != Some("text") {
                    continue;
                }
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    parts.push(text.to_string());
                }
            }
            Some(parts.join("\n").trim().to_string())
        }
        _ => None,
    }
}

fn extract_tag(text: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    let inner = text[start..end].trim();
    (!inner.is_empty()).then(|| inner.to_string())
}

fn is_injected_user_meta(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with("<user_info>")
        || trimmed.starts_with("<system-reminder>")
        || trimmed.starts_with("<environment_context>")
        || trimmed.starts_with("This session is being continued")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_user_query_and_skips_meta_rows() {
        let dir = std::env::temp_dir().join(format!(
            "asterline-grok-import-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let session = dir.join("proj").join("sess-abc");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(
            session.join("chat_history.jsonl"),
            r#"{"type":"system","content":"ignore"}
{"type":"user","content":[{"type":"text","text":"<user_info>\nOS\n</user_info>"}]}
{"type":"user","synthetic_reason":"compact","content":[{"type":"text","text":"summary"}]}
{"type":"user","content":[{"type":"text","text":"<user_query>\nhello from cli\n</user_query>"}]}
{"type":"reasoning","summary":"thinking"}
{"type":"assistant","content":"hi back"}
{"type":"tool_result","content":"noise"}
"#,
        )
        .unwrap();

        let messages = messages_from_path(&session.join("chat_history.jsonl"));
        assert_eq!(messages.len(), 2);
        assert!(messages[0].from_user);
        assert_eq!(messages[0].text, "hello from cli");
        assert!(!messages[1].from_user);
        assert_eq!(messages[1].text, "hi back");

        assert_eq!(
            find_chat_history_under(&dir, "sess-abc"),
            Some(session.join("chat_history.jsonl"))
        );
        std::fs::remove_dir_all(dir).ok();
    }
}
