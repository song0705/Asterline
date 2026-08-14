//! Bounded streaming helpers for backend JSONL transcripts.

use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader};
use std::path::Path;

use serde_json::Value;

pub(crate) const MAX_JSONL_LINE_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_IMPORTED_MESSAGE_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_IMPORTED_ITEMS: usize = 1_000;
pub(crate) const MAX_IMPORTED_TOTAL_BYTES: usize = 8 * 1024 * 1024;

/// Accept a native session identifier only when it is safe to persist and
/// later hand back to a CLI or use in a transcript filename lookup.
pub(crate) fn safe_session_id(value: &str) -> Option<crate::domain::event::AgentSessionId> {
    let valid = !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    valid.then(|| crate::domain::event::AgentSessionId(value.to_string()))
}

/// Append one imported message while enforcing the same aggregate budget as
/// the runtime trust boundary. Returning `false` tells streaming parsers to
/// stop scanning once no additional message can be retained.
pub(crate) fn push_imported_bounded(
    out: &mut Vec<crate::domain::event::ImportedMessage>,
    retained_bytes: &mut usize,
    message: crate::domain::event::ImportedMessage,
) -> bool {
    if out.len() >= MAX_IMPORTED_ITEMS
        || retained_bytes.saturating_add(message.text.len()) > MAX_IMPORTED_TOTAL_BYTES
    {
        return false;
    }
    *retained_bytes = retained_bytes.saturating_add(message.text.len());
    out.push(message);
    out.len() < MAX_IMPORTED_ITEMS && *retained_bytes < MAX_IMPORTED_TOTAL_BYTES
}

pub(crate) fn for_each_json_value(path: &Path, mut visit: impl FnMut(Value) -> bool) {
    for_each_json_value_limit(path, usize::MAX, &mut visit);
}

pub(crate) fn for_each_json_value_limit(
    path: &Path,
    max_lines: usize,
    mut visit: impl FnMut(Value) -> bool,
) {
    let Ok(file) = open_regular_file(path) else {
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

/// Open a transcript only when its leaf is a regular file. Transcript paths
/// originate in external CLI state, so following a symlink here could import
/// unrelated local data into the Asterline conversation.
fn open_regular_file(path: &Path) -> io::Result<File> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("transcript is not a regular file: {}", path.display()),
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
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

    #[test]
    fn aggregate_import_budget_stops_before_unbounded_growth() {
        let mut out = Vec::new();
        let mut retained_bytes = 0;
        for index in 0..MAX_IMPORTED_ITEMS + 10 {
            if !push_imported_bounded(
                &mut out,
                &mut retained_bytes,
                crate::domain::event::ImportedMessage {
                    from_user: true,
                    text: format!("message-{index}"),
                },
            ) {
                break;
            }
        }
        assert_eq!(out.len(), MAX_IMPORTED_ITEMS);
        assert!(retained_bytes <= MAX_IMPORTED_TOTAL_BYTES);
    }

    #[test]
    fn safe_session_ids_cannot_be_paths_or_controls() {
        assert_eq!(
            safe_session_id("session-123_abc").unwrap().as_str(),
            "session-123_abc"
        );
        for unsafe_id in ["", "../secret", "a/b", "a\\b", "id\nnext"] {
            assert!(safe_session_id(unsafe_id).is_none(), "{unsafe_id:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_transcript_is_not_read() {
        use std::os::unix::fs::symlink;

        let dir =
            std::env::temp_dir().join(format!("asterline-import-symlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("outside.jsonl");
        let link = dir.join("session.jsonl");
        std::fs::write(&target, r#"{"secret":"must not import"}"#).unwrap();
        symlink(&target, &link).unwrap();

        let mut visited = false;
        for_each_json_value(&link, |_| {
            visited = true;
            true
        });
        assert!(!visited);

        std::fs::remove_dir_all(dir).ok();
    }
}
