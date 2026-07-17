//! Small shared helpers for the JSON line parsers.

use serde_json::Value;

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
    fn tool_value_pretty_prints_structured_results() {
        let value = serde_json::json!({"stdout": "one\ntwo", "exit_code": 0});
        let rendered = tool_value(&value, 1000);
        assert!(rendered.contains("\n"));
        assert!(rendered.contains("one\\ntwo"));
        assert!(rendered.contains("exit_code"));
    }
}
