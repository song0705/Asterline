//! Compact, chat-friendly tool labels. Stored summaries may still be JSON;
//! this module turns them into a short path/command/query before paint.

use serde_json::Value;

use crate::adapter::parser::tool_brief;
use crate::domain::event::FileChangeItem;

/// Short verb for the subordinate tool row (`Read`, `Shell`, `Search`).
pub(crate) fn tool_kind(name: &str) -> String {
    let raw = name.trim();
    if raw.is_empty() {
        return "tool".to_string();
    }
    let tail = raw.rsplit([':', '/', '.']).next().unwrap_or(raw).trim();
    let key = tail.to_ascii_lowercase().replace('-', "_");
    let key = key.strip_prefix("mcp_").unwrap_or(&key);
    let kind = match key {
        "read" | "read_file" | "readfile" | "view" | "view_file" | "cat" | "open" => "read",
        "write" | "write_file" | "writefile" | "create" | "create_file" => "write",
        "edit" | "edit_file" | "str_replace" | "search_replace" | "replace" | "apply_patch"
        | "applypatch" => "edit",
        "delete" | "delete_file" | "remove" | "rm" => "delete",
        "bash"
        | "shell"
        | "run_terminal_command"
        | "run_command"
        | "execute"
        | "terminal"
        | "powershell"
        | "cmd" => "shell",
        "grep" | "rg" | "search" | "codebase_search" | "glob" | "glob_file_search" | "find"
        | "list_dir" | "ls" | "search_files" => "search",
        "webfetch" | "web_fetch" | "websearch" | "web_search" | "fetch" | "open_page" => "fetch",
        "todowrite" | "todo_write" | "todoread" | "todo_read" => "todo",
        other => {
            let cleaned = other
                .trim_end_matches("_file")
                .trim_end_matches("_tool")
                .trim_start_matches("tool_");
            if cleaned.is_empty() {
                return "Tool".to_string();
            }
            return title_case_words(&cleaned.replace('_', " "));
        }
    };
    title_case_kind(kind)
}

fn title_case_kind(kind: &str) -> String {
    let mut chars = kind.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn title_case_words(text: &str) -> String {
    text.split_whitespace()
        .map(title_case_kind)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Path / command / query after the kind has been split off the title.
pub(crate) fn tool_target(name: &str, summary: &str, detail: &str) -> String {
    let headline = tool_headline(name, summary, detail);
    let kind = tool_kind(name);
    strip_leading_label(&headline, name)
        .or_else(|| strip_leading_label(&headline, &kind))
        .unwrap_or(headline)
}

/// Claude-style edit tools do not emit a separate file-change event. Recover
/// the edit from their input so the chat has one consistent file-change card
/// rather than a duplicate `Edit` row in the tools group.
pub(crate) fn file_change_from_edit_tool(
    name: &str,
    summary: &str,
    detail: &str,
) -> Option<FileChangeItem> {
    if tool_kind(name) != "Edit" {
        return None;
    }
    let input = split_json_payload(detail)
        .or_else(|| split_json_payload(summary))
        .map(|(_, value, _)| value);
    let old_text = input
        .as_ref()
        .and_then(|value| json_field(value, &["old_string", "oldText", "before"]));
    let new_text = input
        .as_ref()
        .and_then(|value| json_field(value, &["new_string", "newText", "after"]));
    let path = input
        .as_ref()
        .and_then(|value| {
            json_field(
                value,
                &["file_path", "target_file", "path", "file", "filename"],
            )
        })
        .filter(|path| !path.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            let target = tool_target(name, summary, detail);
            (!target.is_empty())
                .then_some(target)
                .unwrap_or_else(|| "edited file".to_string())
        });
    let kind = match (old_text, new_text) {
        (None, Some(_)) => "add",
        (Some(_), None) => "delete",
        _ => "update",
    };
    Some(FileChangeItem::new(path, kind).with_texts(old_text, new_text))
}

fn json_field<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
}

fn strip_leading_label(headline: &str, label: &str) -> Option<String> {
    let headline = headline.trim();
    if headline.eq_ignore_ascii_case(label) {
        return Some(String::new());
    }
    let label_len = label.len();
    if headline.len() > label_len
        && headline[..label_len].eq_ignore_ascii_case(label)
        && headline
            .as_bytes()
            .get(label_len)
            .is_some_and(u8::is_ascii_whitespace)
    {
        return Some(headline[label_len..].trim().to_string());
    }
    None
}

/// Title row: `read  tui/chat_view.rs:800` instead of a JSON blob.
pub(crate) fn tool_headline(name: &str, summary: &str, detail: &str) -> String {
    let name = name.trim();
    let from_summary = friendly_tool_text(summary);
    let from_detail = friendly_tool_text(detail);
    let hint = if is_useful_hint(name, &from_summary) {
        from_summary
    } else {
        from_detail
    };
    if hint.is_empty() || hint.eq_ignore_ascii_case(name) {
        name.to_string()
    } else if hint
        .to_ascii_lowercase()
        .starts_with(&name.to_ascii_lowercase())
    {
        hint
    } else {
        format!("{name}  {hint}")
    }
}

/// Expanded (or failed) tool body: key/value lines for JSON, otherwise text.
pub(crate) fn tool_body(detail: &str) -> String {
    let trimmed = detail.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if let Some((prefix, value, rest)) = split_json_payload(trimmed) {
        if is_generic_type_content_envelope(&value) {
            return rest.trim().to_string();
        }
        if let Some(text) = json_result_text(&value) {
            return join_nonempty([prefix_if_useful(prefix), text, rest.to_string()]);
        }
        let mut out = String::new();
        if !prefix.is_empty() && !is_input_label(prefix) {
            out.push_str(prefix);
            out.push('\n');
        }
        out.push_str(&tool_brief_lines(&value));
        let rest = rest.trim();
        if !rest.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(rest);
        }
        return out;
    }
    trimmed.to_string()
}

/// Failed-tool body: the error/output text only. Invocation JSON is omitted
/// because the kind + target row already says what ran. Long strings are kept
/// as the actual message — never collapsed to "N chars".
pub(crate) fn tool_error_body(detail: &str) -> String {
    let trimmed = detail.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let after_input = strip_leading_input_json(trimmed);
    if !after_input.is_empty() {
        if let Some((_, value, rest)) = split_json_payload(after_input) {
            if let Some(text) = json_error_text(&value) {
                return join_nonempty([text, rest.to_string()]);
            }
            if looks_like_tool_input(&value) {
                return rest.trim().to_string();
            }
        }
        return after_input.to_string();
    }
    if let Some((_, value, rest)) = split_json_payload(trimmed) {
        if let Some(text) = json_error_text(&value) {
            return join_nonempty([text, rest.to_string()]);
        }
        if looks_like_tool_input(&value) {
            return rest.trim().to_string();
        }
    }
    trimmed.to_string()
}

/// The default failure view keeps the actionable signals without dumping a
/// long test snapshot. `Ctrl+O` still exposes the complete tool output.
pub(crate) fn compact_tool_error(body: &str) -> (String, bool) {
    const MAX_LINES: usize = 3;
    const MAX_CHARS_PER_LINE: usize = 120;

    let lines = body
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let already_compact = lines.len() <= MAX_LINES
        && lines
            .iter()
            .all(|line| line.chars().count() <= MAX_CHARS_PER_LINE);
    if already_compact {
        return (body.to_string(), false);
    }

    let mut preview = lines
        .iter()
        .copied()
        .filter(|line| is_error_summary_line(line))
        .take(MAX_LINES)
        .map(|line| clip_error_line(line, MAX_CHARS_PER_LINE))
        .collect::<Vec<_>>();
    if preview.is_empty() {
        preview = lines
            .iter()
            .rev()
            .take(MAX_LINES)
            .copied()
            .map(|line| clip_error_line(line, MAX_CHARS_PER_LINE))
            .collect::<Vec<_>>();
        preview.reverse();
    }
    (preview.join("\n"), true)
}

fn is_error_summary_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("error:")
        || lower.contains("failed")
        || lower.contains("panicked at")
        || lower.contains("assertion")
}

fn clip_error_line(line: &str, max_chars: usize) -> String {
    let mut chars = line.chars();
    let clipped = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{clipped}…")
    } else {
        clipped
    }
}

fn json_error_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.trim().is_empty() => Some(text.clone()),
        Value::Array(items) => {
            let parts = items.iter().filter_map(json_error_text).collect::<Vec<_>>();
            (!parts.is_empty()).then_some(parts.join("\n"))
        }
        Value::Object(map) => {
            let mut parts = Vec::new();
            for key in [
                "stderr", "error", "message", "reason", "details", "detail", "msg",
            ] {
                if let Some(text) = map.get(key).and_then(json_error_text) {
                    push_unique(&mut parts, text);
                }
            }
            for (key, nested) in map {
                if !is_error_type_key(key) {
                    continue;
                }
                let Some(text) = json_error_text(nested) else {
                    continue;
                };
                let line = if text.eq_ignore_ascii_case(key) {
                    key.clone()
                } else {
                    format!("{key}: {text}")
                };
                push_unique(&mut parts, line);
            }
            for key in ["stdout", "output", "result"] {
                if let Some(text) = map.get(key).and_then(json_error_text) {
                    push_unique(&mut parts, text);
                }
            }
            (!parts.is_empty()).then_some(parts.join("\n"))
        }
        _ => None,
    }
}

fn push_unique(parts: &mut Vec<String>, text: String) {
    let text = text.trim();
    if text.is_empty() || parts.iter().any(|part| part == text || part.contains(text)) {
        return;
    }
    parts.push(text.to_string());
}

fn is_error_type_key(key: &str) -> bool {
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_uppercase()
        && key.chars().any(|ch| ch.is_ascii_lowercase())
        && key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        && !matches!(
            key,
            "Input" | "Arguments" | "Content" | "Stdout" | "Stderr" | "Output" | "Result"
        )
}

fn prefix_if_useful(prefix: &str) -> String {
    if prefix.is_empty() || is_input_label(prefix) {
        String::new()
    } else {
        prefix.to_string()
    }
}

fn join_nonempty(parts: impl IntoIterator<Item = String>) -> String {
    parts
        .into_iter()
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn strip_leading_input_json(text: &str) -> &str {
    let text = strip_input_label(text).trim();
    let Some((prefix, value, rest)) = split_json_payload(text) else {
        return text;
    };
    if (prefix.is_empty() || is_input_label(prefix)) && looks_like_tool_input(&value) {
        return rest.trim();
    }
    text
}

fn looks_like_tool_input(value: &Value) -> bool {
    let Some(map) = value.as_object() else {
        return false;
    };
    if json_result_text(value).is_some() {
        return false;
    }
    const INPUT_KEYS: &[&str] = &[
        "command",
        "cmd",
        "target_file",
        "file_path",
        "path",
        "file",
        "filename",
        "old_string",
        "new_string",
        "contents",
        "content",
        "offset",
        "limit",
        "start_line",
        "pattern",
        "query",
        "glob",
        "url",
        "uri",
        "timeout",
    ];
    map.keys().any(|key| INPUT_KEYS.contains(&key.as_str()))
}

fn json_result_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.trim().is_empty() => Some(text.clone()),
        Value::Object(map) => {
            let mut parts = Vec::new();
            for key in ["stderr", "error", "message", "reason", "details"] {
                if let Some(text) = map.get(key).and_then(json_stringish) {
                    let text = text.trim();
                    if !text.is_empty() {
                        parts.push(text.to_string());
                    }
                }
            }
            for key in ["stdout", "output", "result"] {
                if let Some(text) = map.get(key).and_then(json_stringish) {
                    let text = text.trim();
                    if !text.is_empty() && !parts.iter().any(|part| part == text) {
                        parts.push(text.to_string());
                    }
                }
            }
            (!parts.is_empty()).then_some(parts.join("\n"))
        }
        _ => None,
    }
}

fn is_generic_type_content_envelope(value: &Value) -> bool {
    let Some(map) = value.as_object() else {
        return false;
    };
    map.contains_key("type")
        && map
            .keys()
            .all(|key| matches!(key.as_str(), "type" | "content"))
}

fn json_stringish(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

pub(crate) fn friendly_tool_text(raw: &str) -> String {
    let trimmed = strip_input_label(raw.trim());
    if trimmed.is_empty() {
        return String::new();
    }
    if let Some((_, value, _)) = split_json_payload(trimmed) {
        return tool_brief(&value);
    }
    shorten_loose_paths(trimmed)
}

fn is_useful_hint(name: &str, hint: &str) -> bool {
    !hint.is_empty() && !hint.eq_ignore_ascii_case(name) && hint != "{" && hint != "["
}

fn is_input_label(text: &str) -> bool {
    matches!(
        text.trim()
            .trim_end_matches(':')
            .to_ascii_lowercase()
            .as_str(),
        "input" | "arguments" | "rawinput" | "params" | "parameters"
    )
}

fn strip_input_label(text: &str) -> &str {
    for prefix in [
        "input:",
        "Input:",
        "arguments:",
        "Arguments:",
        "rawInput:",
        "params:",
        "parameters:",
    ] {
        if let Some(rest) = text.strip_prefix(prefix) {
            return rest.trim();
        }
    }
    text
}

fn split_json_payload(text: &str) -> Option<(&str, Value, &str)> {
    let start = text.find(['{', '['])?;
    let prefix = text[..start].trim();
    // Tool inputs are either raw JSON or labelled `input:` / `arguments:`.
    // Do not parse an arbitrary JSON-looking fragment embedded in command
    // output (for example a source line printed by `git diff`).
    if !prefix.is_empty() && !is_input_label(prefix) {
        return None;
    }
    let bytes = text.as_bytes();
    let open = bytes[start];
    let close = if open == b'{' { b'}' } else { b']' };
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    for (offset, &byte) in bytes[start..].iter().enumerate() {
        if in_string {
            if escape {
                escape = false;
            } else if byte == b'\\' {
                escape = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b if b == open => depth += 1,
            b if b == close => {
                depth -= 1;
                if depth == 0 {
                    let end = start + offset + 1;
                    if let Ok(value) = serde_json::from_str(&text[start..end]) {
                        return Some((text[..start].trim(), value, text[end..].trim()));
                    }
                    break;
                }
            }
            _ => {}
        }
    }
    let value = extract_partial_object(&text[start..])?;
    Some((text[..start].trim(), value, ""))
}

fn extract_partial_object(text: &str) -> Option<Value> {
    let mut map = serde_json::Map::new();
    for key in [
        "command",
        "cmd",
        "query",
        "pattern",
        "url",
        "uri",
        "target_file",
        "file_path",
        "path",
        "file",
        "filename",
        "title",
        "name",
        "glob",
    ] {
        if let Some(value) = string_field(text, key) {
            map.insert(key.to_string(), Value::String(value));
        }
    }
    for key in ["offset", "limit", "start_line"] {
        if let Some(value) = number_field(text, key) {
            map.insert(key.to_string(), Value::from(value));
        }
    }
    (!map.is_empty()).then_some(Value::Object(map))
}

fn string_field(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let after = text.split_once(&needle)?.1.trim_start();
    let after = after.strip_prefix(':')?.trim_start();
    let after = after.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = after.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
            continue;
        }
        if ch == '"' {
            return Some(out);
        }
        out.push(ch);
    }
    (!out.is_empty()).then_some(out)
}

fn number_field(text: &str, key: &str) -> Option<u64> {
    let needle = format!("\"{key}\"");
    let after = text.split_once(&needle)?.1.trim_start();
    let after = after.strip_prefix(':')?.trim_start();
    let digits = after
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    digits.parse().ok()
}

fn tool_brief_lines(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let headline = tool_brief(value);
            let mut lines = Vec::new();
            if !headline.is_empty() {
                lines.push(headline);
            }
            for (key, nested) in map {
                if matches!(
                    key.as_str(),
                    "type"
                        | "command"
                        | "cmd"
                        | "query"
                        | "pattern"
                        | "url"
                        | "uri"
                        | "target_file"
                        | "file_path"
                        | "path"
                        | "file"
                        | "filename"
                        | "title"
                        | "name"
                        | "glob"
                        | "offset"
                        | "limit"
                        | "start_line"
                ) {
                    continue;
                }
                if matches!(
                    key.as_str(),
                    "old_string" | "new_string" | "contents" | "content" | "prompt" | "body"
                ) {
                    let chars = match nested {
                        Value::String(text) => text.chars().count(),
                        _ => continue,
                    };
                    if chars > 0 {
                        lines.push(format!("{key} · {chars} chars"));
                    }
                    continue;
                }
                match nested {
                    Value::String(text) if text.chars().count() <= 80 => {
                        lines.push(format!("{key}  {text}"));
                    }
                    Value::Number(number) => lines.push(format!("{key}  {number}")),
                    Value::Bool(flag) => lines.push(format!("{key}  {flag}")),
                    _ => {}
                }
            }
            lines.join("\n")
        }
        _ => tool_brief(value),
    }
}

fn shorten_loose_paths(text: &str) -> String {
    text.split_whitespace()
        .map(|token| {
            if token.starts_with('/') || token.starts_with("~/") {
                crate::adapter::parser::tool_brief(&Value::String(token.to_string()))
            } else {
                token.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_collapses_backend_specific_names() {
        assert_eq!(tool_kind("read_file"), "Read");
        assert_eq!(tool_kind("Bash"), "Shell");
        assert_eq!(tool_kind("functions.Grep"), "Search");
        assert_eq!(tool_kind("mcp_web_fetch"), "Fetch");
        assert_eq!(tool_kind("edit_file"), "Edit");
    }

    #[test]
    fn target_drops_the_kind_prefix() {
        assert_eq!(
            tool_target(
                "read_file",
                r#"{"target_file":"/Users/me/src/tui/chat_view.rs","offset":10}"#,
                ""
            ),
            "tui/chat_view.rs:10"
        );
        assert_eq!(
            tool_target(
                "Bash",
                "Bash",
                "input:\n{\"command\":\"cargo test\",\"timeout\":120}\n"
            ),
            "cargo test"
        );
    }

    #[test]
    fn headline_uses_path_not_json() {
        assert_eq!(
            tool_headline(
                "read_file",
                r#"{"target_file":"/Users/me/src/tui/chat_view.rs","offset":10}"#,
                ""
            ),
            "read_file  tui/chat_view.rs:10"
        );
    }

    #[test]
    fn headline_recovers_command_from_input_prefix() {
        assert_eq!(
            tool_headline(
                "Bash",
                "Bash",
                "input:\n{\"command\":\"cargo test\",\"timeout\":120}\n"
            ),
            "Bash  cargo test"
        );
    }

    #[test]
    fn suppresses_generic_type_content_result_envelope() {
        let detail = r#"{"type":"TaskOutput","content":"unused metadata"}"#;

        assert!(friendly_tool_text(detail).is_empty());
        assert!(tool_body(detail).is_empty());
    }

    #[test]
    fn error_body_skips_structured_input() {
        assert_eq!(
            tool_error_body("input:\n{\"target_file\":\"/tmp/a.rs\",\"offset\":3}\n"),
            ""
        );
        assert_eq!(
            tool_error_body(r#"{"command":"cargo test","timeout":120}"#),
            ""
        );
    }

    #[test]
    fn error_body_keeps_plain_error_text_after_input() {
        assert_eq!(
            tool_error_body(
                "input:\n{\"command\":\"cargo test\"}\nerror: test parser failed\nexpected true"
            ),
            "error: test parser failed\nexpected true"
        );
    }

    #[test]
    fn error_body_extracts_stderr_without_json() {
        let body =
            tool_error_body("{\n  \"stdout\": \"ok\",\n  \"stderr\": \"error: missing file\"\n}");
        assert!(body.contains("error: missing file"), "{body}");
        assert!(body.contains("ok"), "{body}");
        assert!(!body.contains('{'), "{body}");
        assert!(!body.contains("stderr"), "{body}");
    }

    #[test]
    fn error_body_shows_named_error_message_not_char_count() {
        let message = "The string to replace was found multiple times in tui/app_state.rs. Add more surrounding context to make the match unique.";
        let body = tool_error_body(&format!(
            r#"{{"MultipleMatchesFound":"{message}","type":"SearchReplace"}}"#
        ));
        assert!(body.contains(message), "{body}");
        assert!(body.contains("MultipleMatchesFound"), "{body}");
        assert!(!body.contains("chars"), "{body}");
        assert!(!body.contains("SearchReplace"), "{body}");
    }

    #[test]
    fn claude_edit_input_becomes_a_file_change() {
        let file = file_change_from_edit_tool(
            "Edit",
            "team_runtime_tests/review.rs",
            "input:\n{\"file_path\":\"/workspace/team_runtime_tests/review.rs\",\"old_string\":\"old\\n\",\"new_string\":\"new\\n\"}\nupdated successfully",
        )
        .expect("edit file change");

        assert_eq!(file.path, "/workspace/team_runtime_tests/review.rs");
        assert_eq!(file.kind, "update");
        assert_eq!(file.old_text.as_deref(), Some("old\n"));
        assert_eq!(file.new_text.as_deref(), Some("new\n"));
    }

    #[test]
    fn body_lists_extra_fields_without_braces() {
        let body = tool_body(r#"{"target_file":"/tmp/a.rs","offset":3,"limit":8,"timeout":30}"#);
        assert!(body.contains("tmp/a.rs:3+8"), "{body}");
        assert!(body.contains("timeout  30"), "{body}");
        assert!(!body.contains('{'), "{body}");
    }

    #[test]
    fn truncated_json_still_yields_a_path() {
        assert_eq!(
            friendly_tool_text(r#"{"target_file":"/Users/me/project/src/lib.rs","off"#),
            "src/lib.rs"
        );
    }

    #[test]
    fn shell_output_does_not_treat_embedded_source_json_as_tool_input() {
        let output = "590 detail: r#\"{\\\"target_file\\\":\\\"src/missing.rs\\\"}\"#.to_string(),";
        assert_eq!(friendly_tool_text(output), output);
    }

    #[test]
    fn compact_error_keeps_cargo_failure_signals_not_the_snapshot() {
        let body = concat!(
            "Compiling asterline\n",
            "test tui::chat_view::tests::render::example ... FAILED\n",
            "failures:\n",
            "thread 'example' panicked at src/tui/chat_view_tests/render.rs:173:5\n",
            "assertion `left == right` failed: ",
            "this rendered terminal snapshot is intentionally much longer than the default preview\n",
            "a large snapshot line that should not be shown\n"
        );
        let (preview, clipped) = compact_tool_error(body);

        assert!(clipped);
        assert!(preview.contains("FAILED"), "{preview}");
        assert!(preview.contains("panicked at"), "{preview}");
        assert!(preview.contains("assertion"), "{preview}");
        assert!(!preview.contains("large snapshot"), "{preview}");
        assert!(preview.lines().count() <= 3, "{preview}");
    }
}
