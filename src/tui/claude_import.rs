//! Import messages from a Claude Code session transcript after an interactive
//! attach.
//!
//! Claude Code stores sessions at
//! `~/.claude/projects/<munged-cwd>/<session-id>.jsonl`, one JSON object per
//! line. When the user attaches (`claude --resume <id>`), chats, and exits,
//! new turns land in that file — or, on resume, Claude may *fork* into a new
//! session id whose file replays the prior history plus the new turns. A pure
//! count-based diff on the old file would miss the fork, so import is primarily
//! timestamp-based (with a count fallback for rows that lack timestamps).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::domain::event::ImportedMessage;

/// Clock-skew grace: import rows whose timestamp is up to this much earlier
/// than the attach start.
const CLOCK_SKEW: Duration = Duration::from_secs(2);

/// A snapshot taken before launching the interactive session, used to import
/// only the messages added while attached.
pub struct ClaudeSnapshot {
    /// The Claude session id being attached, if Asterline knows it.
    /// Retained for diagnostics / future fork matching; import uses `path` + mtime.
    #[allow(dead_code)]
    session_id: Option<String>,
    /// Workspace cwd used to locate `~/.claude/projects/<munged-cwd>/`.
    cwd: String,
    /// Session file located up front (when the session id is known).
    path: Option<PathBuf>,
    /// Message rows already present in `path` before the attach.
    before: usize,
    /// When the attach started, to filter by timestamp and spot new session files.
    started: SystemTime,
}

/// Snapshot the Claude session for `session_id` (if any) before attaching.
pub fn snapshot(session_id: Option<&str>, cwd: &str) -> ClaudeSnapshot {
    snapshot_with_root(&default_projects_root(), session_id, cwd)
}

/// After the attach exits, return the messages added during it.
pub fn imported_since(snapshot: ClaudeSnapshot) -> Vec<ImportedMessage> {
    imported_since_with_root(snapshot, &default_projects_root())
}

/// `~/.claude/projects` (may not exist).
fn default_projects_root() -> PathBuf {
    let home = std::env::var_os("HOME").unwrap_or_default();
    Path::new(&home).join(".claude").join("projects")
}

/// Project directory for `cwd` under a Claude projects root.
///
/// `root` is typically `$HOME/.claude/projects`; tests pass a fixture root.
fn projects_dir_for(root: &Path, cwd: &str) -> PathBuf {
    root.join(munge_cwd(cwd))
}

/// Replace every character that is not `[A-Za-z0-9]` with `-`.
fn munge_cwd(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn snapshot_with_root(root: &Path, session_id: Option<&str>, cwd: &str) -> ClaudeSnapshot {
    let project = projects_dir_for(root, cwd);
    let path = session_id.map(|id| project.join(format!("{id}.jsonl")));
    let before = path.as_deref().map(count_messages).unwrap_or(0);
    ClaudeSnapshot {
        session_id: session_id.map(str::to_string),
        cwd: cwd.to_string(),
        path,
        before,
        started: SystemTime::now(),
    }
}

fn imported_since_with_root(snapshot: ClaudeSnapshot, root: &Path) -> Vec<ImportedMessage> {
    let project = projects_dir_for(root, &snapshot.cwd);
    let candidates = candidate_files(&snapshot, &project);
    let threshold = snapshot
        .started
        .checked_sub(CLOCK_SKEW)
        .unwrap_or(SystemTime::UNIX_EPOCH);

    let mut out: Vec<ImportedMessage> = Vec::new();
    let mut last_text: Option<String> = None;

    for path in candidates {
        let is_original = snapshot.path.as_ref().is_some_and(|p| p == &path);
        let rows = parse_classified(&path);
        for (index, row) in rows.into_iter().enumerate() {
            let qualifies = match row.timestamp {
                Some(ts) => ts >= threshold,
                None => is_original && index >= snapshot.before,
            };
            if !qualifies {
                continue;
            }
            if last_text.as_ref() == Some(&row.msg.text) {
                continue;
            }
            last_text = Some(row.msg.text.clone());
            out.push(row.msg);
        }
    }
    out
}

/// Candidate session files: the snapshot path (if present) plus every `.jsonl`
/// in the project dir whose mtime is at least `started`. Original path first,
/// then others sorted by mtime ascending. Deduplicated.
fn candidate_files(snapshot: &ClaudeSnapshot, project_dir: &Path) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut candidates = Vec::new();

    if let Some(ref path) = snapshot.path
        && path.is_file()
        && seen.insert(path.clone())
    {
        candidates.push(path.clone());
    }

    let mut others: Vec<(SystemTime, PathBuf)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(project_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "jsonl") {
                continue;
            }
            let Some(mtime) = modified(&path) else {
                continue;
            };
            if mtime < snapshot.started {
                continue;
            }
            if seen.insert(path.clone()) {
                others.push((mtime, path));
            }
        }
    }
    others.sort_by_key(|(m, _)| *m);
    candidates.extend(others.into_iter().map(|(_, p)| p));
    candidates
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

/// One classified chat row plus its optional timestamp.
struct ClassifiedRow {
    msg: ImportedMessage,
    timestamp: Option<SystemTime>,
}

fn count_messages(path: &Path) -> usize {
    parse_classified(path).len()
}

fn parse_classified(path: &Path) -> Vec<ClassifiedRow> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in content.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(msg) = classify_row(&value) else {
            continue;
        };
        let timestamp = value
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(rfc3339_to_system_time);
        out.push(ClassifiedRow { msg, timestamp });
    }
    out
}

/// Classify a single JSONL row into an importable chat message, or `None` if
/// it is meta / sidechain / empty / injected.
fn classify_row(value: &Value) -> Option<ImportedMessage> {
    if value.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let row_type = value.get("type").and_then(Value::as_str)?;
    match row_type {
        "user" => {
            let text = user_text(value.get("message")?.get("content")?)?;
            if text.is_empty() || is_injected_user_meta(&text) {
                return None;
            }
            Some(ImportedMessage {
                from_user: true,
                text,
            })
        }
        "assistant" => {
            let text = assistant_text(value.get("message")?.get("content")?)?;
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

fn is_injected_user_meta(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("<command-name>")
        || t.starts_with("<local-command-stdout")
        || t.starts_with("Caveat:")
}

/// User content: string as-is, or array of blocks (skip row if any `tool_result`;
/// otherwise concatenate `text` blocks).
fn user_text(content: &Value) -> Option<String> {
    match content {
        Value::String(s) => Some(s.trim().to_string()),
        Value::Array(blocks) => {
            if blocks
                .iter()
                .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
            {
                return None;
            }
            let text = concat_text_blocks(blocks);
            Some(text)
        }
        _ => None,
    }
}

/// Assistant content: concatenate `text` blocks (ignore `thinking` / `tool_use`).
fn assistant_text(content: &Value) -> Option<String> {
    match content {
        Value::String(s) => Some(s.trim().to_string()),
        Value::Array(blocks) => Some(concat_text_blocks(blocks)),
        _ => None,
    }
}

fn concat_text_blocks(blocks: &[Value]) -> String {
    let mut parts = Vec::new();
    for block in blocks {
        if block.get("type").and_then(Value::as_str) != Some("text") {
            continue;
        }
        if let Some(text) = block.get("text").and_then(Value::as_str) {
            parts.push(text.to_string());
        }
    }
    parts.join("\n").trim().to_string()
}

/// Parse `YYYY-MM-DDTHH:MM:SS(.fff)?Z` (UTC only) into [`SystemTime`].
fn rfc3339_to_system_time(s: &str) -> Option<SystemTime> {
    let s = s.trim();
    if !s.ends_with('Z') {
        return None;
    }
    let body = &s[..s.len() - 1];
    let (date, time) = body.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i32 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) || day == 0 || day > 31 {
        return None;
    }

    let (hms, frac) = match time.split_once('.') {
        Some((hms, frac)) => (hms, Some(frac)),
        None => (time, None),
    };
    let mut time_parts = hms.split(':');
    let hour: u64 = time_parts.next()?.parse().ok()?;
    let minute: u64 = time_parts.next()?.parse().ok()?;
    let second: u64 = time_parts.next()?.parse().ok()?;
    if time_parts.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let days = days_from_civil(year, month, day)?;
    let secs = days
        .checked_mul(86_400)?
        .checked_add(hour as i64 * 3600)?
        .checked_add(minute as i64 * 60)?
        .checked_add(second as i64)?;
    if secs < 0 {
        return None;
    }
    let mut duration = Duration::from_secs(secs as u64);
    if let Some(frac) = frac {
        // Up to 9 digits of fractional seconds → nanoseconds.
        let digits: String = frac
            .chars()
            .take(9)
            .filter(|c| c.is_ascii_digit())
            .collect();
        if digits.len() != frac.chars().take(9).count() {
            return None;
        }
        if digits.is_empty() && !frac.is_empty() {
            return None;
        }
        let padded = format!("{digits:0<9}");
        let nanos: u32 = padded.parse().ok()?;
        duration = duration.checked_add(Duration::from_nanos(nanos as u64))?;
    }
    UNIX_EPOCH.checked_add(duration)
}

/// Days since civil 1970-01-01 (Howard Hinnant's algorithm). Returns `None` for
/// clearly invalid calendar dates (e.g. month out of range already checked).
fn days_from_civil(y: i32, m: u32, d: u32) -> Option<i64> {
    // Reject impossible day-of-month roughly; leap-year Feb 29 is allowed.
    let dim = days_in_month(y, m)?;
    if d == 0 || d > dim {
        return None;
    }
    let mut y = y;
    let m = m as i32;
    y -= i32::from(m <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) as u32 + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era as i64 * 146_097 + doe as i64 - 719_468)
}

fn days_in_month(y: i32, m: u32) -> Option<u32> {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => Some(31),
        4 | 6 | 9 | 11 => Some(30),
        2 => Some(if is_leap(y) { 29 } else { 28 }),
        _ => None,
    }
}

fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ast-claude-import-{}-{}-{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_session(root: &Path, cwd: &str, session_id: &str, lines: &[&str]) -> PathBuf {
        let dir = projects_dir_for(root, cwd);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{session_id}.jsonl"));
        std::fs::write(&path, lines.join("\n")).unwrap();
        path
    }

    fn user_line(text: &str, ts: &str) -> String {
        format!(
            r#"{{"type":"user","isSidechain":false,"timestamp":"{ts}","message":{{"role":"user","content":"{text}"}}}}"#
        )
    }

    fn assistant_line(text: &str, ts: &str) -> String {
        format!(
            r#"{{"type":"assistant","isSidechain":false,"timestamp":"{ts}","message":{{"role":"assistant","content":[{{"type":"text","text":"{text}"}}]}}}}"#
        )
    }

    #[test]
    fn munge_cwd_replaces_non_alnum() {
        assert_eq!(munge_cwd("/Users/x/proj.name"), "-Users-x-proj-name");
        assert_eq!(munge_cwd("/tmp/你好"), "-tmp---");
        assert_eq!(
            munge_cwd("/Users/pys/project/git/engine/Asterline"),
            "-Users-pys-project-git-engine-Asterline"
        );
    }

    #[test]
    fn projects_dir_for_joins_munged_cwd() {
        let root = Path::new("/fake/claude/projects");
        assert_eq!(
            projects_dir_for(root, "/Users/x/proj"),
            PathBuf::from("/fake/claude/projects/-Users-x-proj")
        );
    }

    #[test]
    fn classify_skips_sidechain() {
        let v: Value = serde_json::from_str(
            r#"{"type":"user","isSidechain":true,"message":{"content":"hi"}}"#,
        )
        .unwrap();
        assert!(classify_row(&v).is_none());
    }

    #[test]
    fn classify_user_string_content() {
        let v: Value = serde_json::from_str(
            r#"{"type":"user","isSidechain":false,"message":{"content":"hello world"}}"#,
        )
        .unwrap();
        assert_eq!(
            classify_row(&v),
            Some(ImportedMessage {
                from_user: true,
                text: "hello world".to_string()
            })
        );
    }

    #[test]
    fn classify_user_array_with_tool_result_skipped() {
        let v: Value = serde_json::from_str(
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"},{"type":"text","text":"x"}]}}"#,
        )
        .unwrap();
        assert!(classify_row(&v).is_none());
    }

    #[test]
    fn classify_user_injected_meta_skipped() {
        for text in [
            "<command-name>/help</command-name>",
            "<local-command-stdout>out</local-command-stdout>",
            "Caveat: The messages below were generated",
        ] {
            let v = serde_json::json!({
                "type": "user",
                "message": { "content": text }
            });
            assert!(classify_row(&v).is_none(), "expected skip for {text:?}");
        }
    }

    #[test]
    fn classify_assistant_text_concat() {
        let v: Value = serde_json::from_str(
            r#"{"type":"assistant","message":{"content":[
                {"type":"thinking","thinking":"secret"},
                {"type":"text","text":"Hello "},
                {"type":"tool_use","name":"Bash"},
                {"type":"text","text":"world"}
            ]}}"#,
        )
        .unwrap();
        assert_eq!(
            classify_row(&v),
            Some(ImportedMessage {
                from_user: false,
                text: "Hello \nworld".to_string()
            })
        );
    }

    #[test]
    fn classify_meta_types_skipped() {
        for t in [
            "file-history-snapshot",
            "summary",
            "system",
            "queue-operation",
            "attachment",
        ] {
            let v = serde_json::json!({ "type": t, "message": { "content": "x" } });
            assert!(classify_row(&v).is_none(), "expected skip for type {t}");
        }
    }

    #[test]
    fn rfc3339_parse_with_and_without_fraction() {
        let a = rfc3339_to_system_time("2020-01-01T00:00:00Z").unwrap();
        let b = rfc3339_to_system_time("2020-01-01T00:00:00.500Z").unwrap();
        assert_eq!(
            a.duration_since(UNIX_EPOCH).unwrap().as_secs(),
            1_577_836_800
        );
        assert_eq!(
            b.duration_since(UNIX_EPOCH).unwrap().as_millis(),
            1_577_836_800_500
        );
        assert!(rfc3339_to_system_time("not-a-date").is_none());
        assert!(rfc3339_to_system_time("2020-01-01T00:00:00+00:00").is_none());
        assert!(rfc3339_to_system_time("2020-13-01T00:00:00Z").is_none());
    }

    #[test]
    fn snapshot_import_round_trip_timestamp_filter() {
        let root = fixture_root("roundtrip");
        let cwd = "/tmp/ws-claude";
        let sid = "sess-round";
        let old1 = user_line("old user", "2020-01-01T00:00:00Z");
        let old2 = assistant_line("old assistant", "2020-01-01T00:00:01Z");
        write_session(&root, cwd, sid, &[&old1, &old2]);

        let snap = snapshot_with_root(&root, Some(sid), cwd);
        assert_eq!(snap.before, 2);
        assert!(snap.path.as_ref().is_some_and(|p| p.is_file()));

        let new1 = user_line("new user", "2099-01-01T00:00:00Z");
        let new2 = assistant_line("new assistant", "2099-01-01T00:00:01Z");
        let path = snap.path.clone().unwrap();
        let mut content = std::fs::read_to_string(&path).unwrap();
        content.push('\n');
        content.push_str(&new1);
        content.push('\n');
        content.push_str(&new2);
        std::fs::write(&path, content).unwrap();

        let imported = imported_since_with_root(snap, &root);
        assert_eq!(
            imported,
            vec![
                ImportedMessage {
                    from_user: true,
                    text: "new user".to_string()
                },
                ImportedMessage {
                    from_user: false,
                    text: "new assistant".to_string()
                },
            ]
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn fork_session_imports_only_new_timestamped_rows() {
        let root = fixture_root("fork");
        let cwd = "/tmp/ws-fork";
        let old_sid = "sess-old";
        let old1 = user_line("prior user", "2020-06-01T12:00:00Z");
        let old2 = assistant_line("prior assistant", "2020-06-01T12:00:01Z");
        write_session(&root, cwd, old_sid, &[&old1, &old2]);

        let snap = snapshot_with_root(&root, Some(old_sid), cwd);
        assert_eq!(snap.before, 2);

        // Forked session file: full replay of old history + new turns.
        let new_sid = "sess-forked";
        let new1 = user_line("while attached", "2099-06-01T12:00:00Z");
        let new2 = assistant_line("fork reply", "2099-06-01T12:00:01Z");
        write_session(&root, cwd, new_sid, &[&old1, &old2, &new1, &new2]);

        let imported = imported_since_with_root(snap, &root);
        assert_eq!(
            imported,
            vec![
                ImportedMessage {
                    from_user: true,
                    text: "while attached".to_string()
                },
                ImportedMessage {
                    from_user: false,
                    text: "fork reply".to_string()
                },
            ]
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn count_fallback_imports_untimestamped_rows_after_before() {
        let root = fixture_root("count-fallback");
        let cwd = "/tmp/ws-count";
        let sid = "sess-count";
        let dir = projects_dir_for(&root, cwd);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{sid}.jsonl"));

        let lines = [
            r#"{"type":"user","message":{"content":"already there"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"old reply"}]}}"#,
            r#"{"type":"user","message":{"content":"typed while attached"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"fresh reply"}]}}"#,
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();

        // before = 2 (the two pre-existing classified rows).
        let snap = ClaudeSnapshot {
            session_id: Some(sid.to_string()),
            cwd: cwd.to_string(),
            path: Some(path),
            before: 2,
            started: SystemTime::now(),
        };

        let imported = imported_since_with_root(snap, &root);
        assert_eq!(
            imported,
            vec![
                ImportedMessage {
                    from_user: true,
                    text: "typed while attached".to_string()
                },
                ImportedMessage {
                    from_user: false,
                    text: "fresh reply".to_string()
                },
            ]
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn consecutive_duplicate_texts_deduped_across_candidates() {
        let root = fixture_root("dedup");
        let cwd = "/tmp/ws-dedup";
        let old_sid = "sess-a";
        let new_line = user_line("same text", "2099-01-01T00:00:00Z");
        write_session(&root, cwd, old_sid, &[]);

        let snap = snapshot_with_root(&root, Some(old_sid), cwd);
        // Append to original and also write a forked file with the same new text.
        let path = snap.path.clone().unwrap();
        std::fs::write(&path, &new_line).unwrap();
        write_session(&root, cwd, "sess-b", &[&new_line]);

        let imported = imported_since_with_root(snap, &root);
        assert_eq!(
            imported,
            vec![ImportedMessage {
                from_user: true,
                text: "same text".to_string()
            }]
        );

        std::fs::remove_dir_all(&root).ok();
    }
}
