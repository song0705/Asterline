//! Small shared helpers for the JSON line parsers.

use serde_json::Value;

use crate::domain::event::FileChangeItem;

/// Maximum retained text for one assistant message. Stream transports may
/// produce arbitrarily many individually-valid chunks, so the per-line limit
/// alone is not a total-memory bound.
pub const MAX_MESSAGE_TEXT_BYTES: usize = 4 * 1024 * 1024;
/// Maximum retained detail for one tool invocation.
pub const MAX_TOOL_DETAIL_BYTES: usize = 1024 * 1024;
const OUTPUT_TRUNCATION_MARKER: &str = "\n[asterline: output truncated]\n";

/// Claude/Agy print this when a command is refused. It is a tool result, not a
/// crashed member run — Plan/Team must not treat it as a terminal failure.
pub fn is_permission_denied_tool(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("user denied permission") || lower.contains("denied permission to run command")
}

/// The refused command, when the backend included it after `to run command:`.
pub fn permission_denied_command(text: &str) -> Option<&str> {
    let lower = text.to_ascii_lowercase();
    let marker = "to run command:";
    let index = lower.find(marker)?;
    let rest = text[index + marker.len()..].trim();
    let line = rest.lines().next().unwrap_or(rest).trim();
    (!line.is_empty()).then_some(line)
}

pub fn format_permission_denial(text: &str) -> String {
    permission_denied_command(text)
        .map(|command| format!("`{command}`"))
        .unwrap_or_else(|| text.lines().next().unwrap_or(text).trim().to_string())
}

/// Classify Write/Edit/Delete tools, including Agy `write_to_file`.
pub fn file_change_tool_class(name: &str) -> Option<&'static str> {
    match file_tool_kind(name)? {
        FileToolKind::Write => Some("write"),
        FileToolKind::Edit => Some("edit"),
        FileToolKind::Delete => Some("delete"),
    }
}

/// Append a UTF-8 chunk without letting the retained string exceed `max`
/// bytes. The first overflow appends a visible marker; subsequent chunks are
/// ignored. The returned delta is exactly what was appended.
pub fn append_bounded_text(target: &mut String, chunk: &str, max: usize) -> Option<String> {
    let content_limit = max.saturating_sub(OUTPUT_TRUNCATION_MARKER.len());
    // Under this helper's invariant, only a truncated value can be longer
    // than content_limit because normal content never crosses that boundary.
    if target.len() > content_limit || chunk.is_empty() {
        return None;
    }
    let remaining = content_limit.saturating_sub(target.len());
    if chunk.len() <= remaining {
        target.push_str(chunk);
        return Some(chunk.to_string());
    }

    let mut end = remaining.min(chunk.len());
    while end > 0 && !chunk.is_char_boundary(end) {
        end -= 1;
    }
    let prefix = &chunk[..end];
    let mut delta = String::with_capacity(prefix.len() + OUTPUT_TRUNCATION_MARKER.len());
    delta.push_str(prefix);
    delta.push_str(OUTPUT_TRUNCATION_MARKER);
    target.push_str(&delta);
    Some(delta)
}

/// Return one bounded, visibly-truncated UTF-8 value.
pub fn bounded_text(text: &str, max: usize) -> String {
    let mut bounded = String::with_capacity(text.len().min(max));
    let _ = append_bounded_text(&mut bounded, text, max);
    bounded
}

/// Read a string field from a JSON object.
pub fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// Collapse whitespace and truncate to `max` characters for a one-line summary.
pub fn summarize(text: &str, max: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max {
        collapsed
    } else {
        let mut out: String = collapsed.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Preserve tool output formatting while bounding what is retained in chat and
/// SQLite. Unlike [`summarize`], this keeps newlines and indentation intact.
pub fn tool_detail(text: &str, max: usize) -> String {
    let text = text.trim_end();
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// One-line, chat-friendly label for a tool argument object.
///
/// Prefers paths, commands, and queries over pretty-printed JSON so the TUI
/// title row stays readable.
pub fn tool_brief(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => shorten_tool_path(text),
        Value::Number(number) => number.to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Array(items) => {
            let parts = items
                .iter()
                .map(tool_brief)
                .filter(|text| !text.is_empty())
                .take(3)
                .collect::<Vec<_>>();
            let extra = items.len().saturating_sub(parts.len());
            if extra == 0 {
                parts.join(", ")
            } else {
                format!("{} +{extra}", parts.join(", "))
            }
        }
        Value::Object(map) => brief_object(map),
    }
}

const COMMAND_BRIEF_KEYS: &[&str] = &[
    "command",
    "cmd",
    "command_line",
    "query",
    "pattern",
    "url",
    "uri",
];
const PATH_BRIEF_KEYS: &[&str] = &[
    "target_file",
    "file_path",
    "path",
    "file",
    "filename",
    "directory_path",
];

fn brief_object(map: &serde_json::Map<String, Value>) -> String {
    for key in [
        "command",
        "cmd",
        "command_line",
        "query",
        "pattern",
        "url",
        "uri",
        "target_file",
        "file_path",
        "path",
        "file",
        "filename",
        "directory_path",
        "title",
        "name",
    ] {
        let Some(primary) = map_string_by_names(map, &[key]) else {
            continue;
        };
        let primary = primary.trim();
        if primary.is_empty() {
            continue;
        }
        let mut out = if COMMAND_BRIEF_KEYS.contains(&key) {
            primary.split_whitespace().collect::<Vec<_>>().join(" ")
        } else {
            shorten_tool_path(primary)
        };
        if let Some(offset) = int_field(map, "offset").or_else(|| int_field(map, "start_line")) {
            out.push(':');
            out.push_str(&offset.to_string());
            if let Some(limit) = int_field(map, "limit") {
                out.push('+');
                out.push_str(&limit.to_string());
            }
        }
        if !PATH_BRIEF_KEYS.contains(&key)
            && let Some(path) = map_string_by_names(map, &["path", "directory_path"])
                .map(str::trim)
                .filter(|path| !path.is_empty())
        {
            out.push_str(" in ");
            out.push_str(&shorten_tool_path(path));
        }
        if let Some(glob) = map_string_by_names(map, &["glob"])
            .map(str::trim)
            .filter(|glob| !glob.is_empty())
        {
            out.push_str(" (");
            out.push_str(glob);
            out.push(')');
        }
        return out;
    }

    let mut parts = Vec::new();
    for (key, value) in map {
        if is_skipped_brief_key(key) {
            continue;
        }
        let piece = match value {
            Value::String(text) if text.chars().count() <= 80 => {
                format!("{key} {}", shorten_tool_path(text))
            }
            Value::String(text) => format!("{key} · {} chars", text.chars().count()),
            Value::Number(number) => format!("{key} {number}"),
            Value::Bool(flag) => format!("{key} {flag}"),
            _ => continue,
        };
        parts.push(piece);
        if parts.len() == 3 {
            break;
        }
    }
    parts.join(" · ")
}

const SKIP_BRIEF_KEYS: &[&str] = &[
    "type",
    "old_string",
    "new_string",
    "contents",
    "content",
    "prompt",
    "body",
    "diff",
    "patch",
    "text",
    "stdout",
    "stderr",
];

fn int_field(map: &serde_json::Map<String, Value>, key: &str) -> Option<u64> {
    map_value_by_names(map, &[key]).and_then(Value::as_u64)
}

fn is_skipped_brief_key(key: &str) -> bool {
    let want = normalize_json_key(key);
    SKIP_BRIEF_KEYS
        .iter()
        .any(|skip| normalize_json_key(skip) == want)
}

/// Look up a JSON object field, ignoring case and `_`/`-` so Agy `TargetFile`
/// matches `target_file`.
pub fn json_string_field<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    map_string_by_names(value.as_object()?, keys)
}

fn map_string_by_names<'a>(
    map: &'a serde_json::Map<String, Value>,
    names: &[&str],
) -> Option<&'a str> {
    map_value_by_names(map, names).and_then(Value::as_str)
}

fn map_value_by_names<'a>(
    map: &'a serde_json::Map<String, Value>,
    names: &[&str],
) -> Option<&'a Value> {
    for name in names {
        if let Some(value) = map.get(*name) {
            return Some(value);
        }
    }
    for name in names {
        let want = normalize_json_key(name);
        if let Some((_, value)) = map.iter().find(|(key, _)| normalize_json_key(key) == want) {
            return Some(value);
        }
    }
    None
}

fn normalize_json_key(key: &str) -> String {
    key.chars()
        .filter(|ch| *ch != '_' && *ch != '-')
        .flat_map(char::to_lowercase)
        .collect()
}

/// Recover a file-change card from Write/Edit/Delete tool arguments.
pub fn file_change_from_params(name: &str, params: &Value) -> Option<FileChangeItem> {
    let kind = file_tool_kind(name)?;
    let path = json_string_field(
        params,
        &["target_file", "file_path", "path", "file", "filename"],
    )
    .map(str::trim)
    .filter(|path| !path.is_empty())?;
    let old_text = json_string_field(
        params,
        &[
            "old_string",
            "old_text",
            "before",
            "old_contents",
            "target_content",
        ],
    );
    let new_text = json_string_field(
        params,
        &[
            "code_content",
            "replacement_content",
            "replacement",
            "new_string",
            "new_text",
            "after",
            "contents",
            "content",
            "text",
        ],
    );
    let change_kind = match kind {
        FileToolKind::Delete => "delete",
        FileToolKind::Write => {
            if old_text.is_some() && new_text.is_some() {
                "update"
            } else if old_text.is_some() {
                "delete"
            } else {
                "add"
            }
        }
        FileToolKind::Edit => match (old_text, new_text) {
            (None, Some(_)) => "add",
            (Some(_), None) => "delete",
            _ => "update",
        },
    };
    Some(FileChangeItem::new(path, change_kind).with_texts(old_text, new_text))
}

#[derive(Clone, Copy)]
enum FileToolKind {
    Write,
    Edit,
    Delete,
}

fn file_tool_kind(name: &str) -> Option<FileToolKind> {
    let tail = name.rsplit([':', '/', '.']).next().unwrap_or(name).trim();
    let key = tail.to_ascii_lowercase().replace('-', "_");
    let key = key.strip_prefix("mcp_").unwrap_or(&key);
    match key {
        "write" | "write_file" | "writefile" | "write_to_file" | "writetofile" | "create"
        | "create_file" => Some(FileToolKind::Write),
        "edit"
        | "edit_file"
        | "str_replace"
        | "search_replace"
        | "replace"
        | "replace_file_content"
        | "replacefilecontent"
        | "multi_replace_file_content"
        | "multireplacefilecontent"
        | "apply_patch"
        | "applypatch" => Some(FileToolKind::Edit),
        "delete" | "delete_file" | "remove" | "rm" => Some(FileToolKind::Delete),
        _ => None,
    }
}

fn shorten_tool_path(text: &str) -> String {
    let text = text.trim();
    if text.starts_with("http://") || text.starts_with("https://") {
        return text.to_string();
    }
    let slash = if text.contains('/') {
        '/'
    } else if text.contains('\\') {
        '\\'
    } else {
        return text.split_whitespace().collect::<Vec<_>>().join(" ");
    };
    if text.contains(' ') && !text.starts_with('/') && !text.starts_with('~') {
        return text.split_whitespace().collect::<Vec<_>>().join(" ");
    }
    let parts = text
        .split(slash)
        .filter(|part| !part.is_empty() && *part != "~")
        .collect::<Vec<_>>();
    match parts.as_slice() {
        [] => text.to_string(),
        [name] => (*name).to_string(),
        [.., parent, name] => format!("{parent}/{name}"),
    }
}

/// Render a structured tool input/result without losing nested fields.
pub fn tool_value(value: &Value, max: usize) -> String {
    let text = match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .map(|block| {
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| serde_json::to_string_pretty(block).unwrap_or_default())
            })
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => serde_json::to_string_pretty(value).unwrap_or_default(),
    };
    tool_detail(&text, max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn permission_denied_tool_matches_agy_and_claude_wording() {
        assert!(is_permission_denied_tool(
            "User denied permission to run command:\n  node -c game.js"
        ));
        assert!(is_permission_denied_tool(
            "denied permission to run command: python3 -m pytest"
        ));
        assert!(!is_permission_denied_tool("rate limited"));
        assert!(!is_permission_denied_tool(
            "permission denied opening /etc/shadow"
        ));
        assert_eq!(
            permission_denied_command(
                "User denied permission to run command:\n  node -c snake-game/game.js"
            ),
            Some("node -c snake-game/game.js")
        );
        assert_eq!(
            format_permission_denial("User denied permission to run command: python3 -m pytest"),
            "`python3 -m pytest`"
        );
    }

    #[test]
    fn str_field_reads_string() {
        let v = json!({"a": "x", "b": 1});
        assert_eq!(str_field(&v, "a"), Some("x"));
        assert_eq!(str_field(&v, "b"), None);
        assert_eq!(str_field(&v, "missing"), None);
    }

    #[test]
    fn summarize_collapses_and_truncates() {
        assert_eq!(
            summarize("hello   world\n  again", 100),
            "hello world again"
        );
        assert_eq!(summarize("abcdefghij", 5), "abcd…");
    }

    #[test]
    fn tool_detail_preserves_lines_and_indentation() {
        assert_eq!(tool_detail("one\n  two\n", 100), "one\n  two");
        assert_eq!(tool_detail("abcdefghij", 5), "abcd…");
    }

    #[test]
    fn tool_brief_prefers_paths_and_commands() {
        assert_eq!(
            tool_brief(
                &json!({"target_file": "/Users/me/src/tui/chat_view.rs", "offset": 800, "limit": 80})
            ),
            "tui/chat_view.rs:800+80"
        );
        assert_eq!(
            tool_brief(&json!({"command": "cargo test", "timeout": 120})),
            "cargo test"
        );
        assert_eq!(
            tool_brief(&json!({"pattern": "tool_brief", "path": "src/tui", "glob": "*.rs"})),
            "tool_brief in src/tui (*.rs)"
        );
        assert_eq!(
            tool_brief(&json!({"file_path": "src/a.rs", "old_string": "aaa", "new_string": "bbb"})),
            "src/a.rs"
        );
        assert_eq!(
            tool_brief(
                &json!({"TargetFile": "snake game/index.html", "Contents": "<html></html>"})
            ),
            "snake game/index.html"
        );
        assert_eq!(
            tool_brief(&json!({"DirectoryPath": "engine/Asterline"})),
            "engine/Asterline"
        );
        assert_eq!(tool_brief(&json!({"CommandLine": "pwd"})), "pwd");
    }

    #[test]
    fn write_params_become_an_add_file_change() {
        let file = file_change_from_params(
            "Write",
            &json!({"TargetFile": "css/style.css", "Contents": "body { margin: 0; }\n"}),
        )
        .expect("write file change");
        assert_eq!(file.path, "css/style.css");
        assert_eq!(file.kind, "add");
        assert_eq!(file.new_text.as_deref(), Some("body { margin: 0; }\n"));
        assert!(file.old_text.is_none());
    }

    #[test]
    fn agy_write_to_file_is_a_file_change() {
        let file = file_change_from_params(
            "write_to_file",
            &json!({
                "TargetFile": "snake-game/index.html",
                "CodeContent": "<html></html>\n"
            }),
        )
        .expect("agy write_to_file");
        assert_eq!(file.path, "snake-game/index.html");
        assert_eq!(file.kind, "add");
        assert_eq!(file.new_text.as_deref(), Some("<html></html>\n"));
        assert_eq!(file_change_tool_class("write_to_file"), Some("write"));
        assert_eq!(file_change_tool_class("replace_file_content"), Some("edit"));
    }

    #[test]
    fn agy_replace_file_content_keeps_before_and_after() {
        let file = file_change_from_params(
            "replace_file_content",
            &json!({
                "TargetFile": "snake-game/game.js",
                "TargetContent": "let score = 0;\n",
                "ReplacementContent": "let score = 1;\n"
            }),
        )
        .expect("agy replace");
        assert_eq!(file.path, "snake-game/game.js");
        assert_eq!(file.kind, "update");
        assert_eq!(file.old_text.as_deref(), Some("let score = 0;\n"));
        assert_eq!(file.new_text.as_deref(), Some("let score = 1;\n"));
    }

    #[test]
    fn tool_value_pretty_prints_structured_results() {
        let value = serde_json::json!({"stdout": "one\ntwo", "exit_code": 0});
        let rendered = tool_value(&value, 1000);
        assert!(rendered.contains("\n"));
        assert!(rendered.contains("one\\ntwo"));
        assert!(rendered.contains("exit_code"));
    }

    #[test]
    fn bounded_text_stops_total_chunk_growth_on_a_utf8_boundary() {
        let max = OUTPUT_TRUNCATION_MARKER.len() + 5;
        let mut text = String::new();
        assert_eq!(
            append_bounded_text(&mut text, "ab", max).as_deref(),
            Some("ab")
        );
        let overflow = append_bounded_text(&mut text, "你cd", max).unwrap();

        assert!(overflow.starts_with('你'));
        assert!(text.ends_with(OUTPUT_TRUNCATION_MARKER));
        assert!(text.len() <= max);
        assert!(append_bounded_text(&mut text, "ignored", max).is_none());
    }
}
