//! Small shared helpers for the JSON line parsers.

use serde_json::Value;

/// Maximum retained text for one assistant message. Stream transports may
/// produce arbitrarily many individually-valid chunks, so the per-line limit
/// alone is not a total-memory bound.
pub const MAX_MESSAGE_TEXT_BYTES: usize = 4 * 1024 * 1024;
/// Maximum retained detail for one tool invocation.
pub const MAX_TOOL_DETAIL_BYTES: usize = 1024 * 1024;
const OUTPUT_TRUNCATION_MARKER: &str = "\n[asterline: output truncated]\n";

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
