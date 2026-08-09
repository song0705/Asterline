//! Bounded streaming helpers for backend JSONL transcripts.

use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;

use serde_json::Value;

pub(crate) const MAX_JSONL_LINE_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_IMPORTED_MESSAGE_BYTES: usize = 1024 * 1024;

pub(crate) fn for_each_json_value(path: &Path, mut visit: impl FnMut(Value) -> bool) {
    for_each_json_value_limit(path, usize::MAX, &mut visit);
}

pub(crate) fn for_each_json_value_limit(
    path: &Path,
    max_lines: usize,
    mut visit: impl FnMut(Value) -> bool,
) {
    let Ok(file) = File::open(path) else {
        return;
    };
    let mut reader = BufReader::new(file);
    for _ in 0..max_lines {
        match read_bounded_line(&mut reader, MAX_JSONL_LINE_BYTES) {
            Ok(BoundedLine::Eof) | Err(_) => break,
            Ok(BoundedLine::Oversized) => continue,
            Ok(BoundedLine::Bytes(line)) => {
                if let Ok(value) = serde_json::from_slice(&line)
                    && !visit(value)
                {
                    break;
                }
            }
        }
    }
}

pub(crate) fn imported_text(text: String) -> Option<String> {
    (text.len() <= MAX_IMPORTED_MESSAGE_BYTES).then_some(text)
}

enum BoundedLine {
    Eof,
    Bytes(Vec<u8>),
    Oversized,
}

/// Read one line without retaining more than `limit` bytes. Oversized lines
/// are fully drained so the next valid JSON object remains readable.
fn read_bounded_line(reader: &mut impl BufRead, limit: usize) -> io::Result<BoundedLine> {
    let mut line = Vec::new();
    let mut saw_data = false;
    let mut oversized = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if !saw_data {
                Ok(BoundedLine::Eof)
            } else if oversized {
                Ok(BoundedLine::Oversized)
            } else {
                Ok(BoundedLine::Bytes(line))
            };
        }
        saw_data = true;
        let newline = available.iter().position(|byte| *byte == b'\n');
        let data_len = newline.unwrap_or(available.len());
        if !oversized {
            if line.len().saturating_add(data_len) <= limit {
                line.extend_from_slice(&available[..data_len]);
            } else {
                oversized = true;
                line.clear();
            }
        }
        let consumed = newline.map_or(data_len, |position| position + 1);
        reader.consume(consumed);
        if newline.is_some() {
            return if oversized {
                Ok(BoundedLine::Oversized)
            } else {
                Ok(BoundedLine::Bytes(line))
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_line_is_drained_before_the_next_line() {
        let mut fixture = vec![b'x'; 9];
        fixture.extend_from_slice(b"\n{\"ok\":true}\n");
        let mut reader = io::Cursor::new(fixture);

        assert!(matches!(
            read_bounded_line(&mut reader, 8).unwrap(),
            BoundedLine::Oversized
        ));
        let BoundedLine::Bytes(next) = read_bounded_line(&mut reader, 8 * 1024).unwrap() else {
            panic!("expected the next bounded line");
        };
        assert_eq!(next, br#"{"ok":true}"#);
    }
}
