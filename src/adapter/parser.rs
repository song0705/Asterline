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
}
