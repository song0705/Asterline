//! Shared live-status text for the chat view. Inspired by Codex's status row:
//! keep elapsed time and the interrupt hint in a stable place while the rest of
//! the UI changes around it.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::domain::event::MemberStatus;

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub(crate) fn spinner() -> &'static str {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    spinner_at_millis(millis)
}

fn spinner_at_millis(millis: u128) -> &'static str {
    let idx = ((millis / 80) % SPINNER.len() as u128) as usize;
    SPINNER[idx]
}

// Mirrors Codex's compact elapsed style: 8s, 1m 04s, 1h 02m 03s.
pub(crate) fn fmt_elapsed_compact(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!(
            "{}h {:02}m {:02}s",
            secs / 3600,
            (secs % 3600) / 60,
            secs % 60
        )
    }
}

pub(crate) fn member_activity_text(
    status: MemberStatus,
    reasoning: Option<&str>,
    elapsed_secs: Option<u64>,
    spin: &str,
    runtime_profile: Option<&str>,
) -> String {
    let runtime_profile = runtime_profile.filter(|text| !text.is_empty());
    if let Some(reasoning) = reasoning.filter(|text| !text.is_empty()) {
        let context = activity_context(elapsed_secs, runtime_profile);
        return format!("{spin} Thinking{context}: {reasoning}");
    }

    match status {
        MemberStatus::Running => {
            let elapsed = elapsed_secs
                .map(fmt_elapsed_compact)
                .unwrap_or_else(|| "0s".to_string());
            let mut parts = vec![elapsed];
            if let Some(profile) = runtime_profile {
                parts.push(profile.to_string());
            }
            parts.push("Ctrl+C to interrupt".to_string());
            format!("{spin} Working ({})", parts.join(" • "))
        }
        MemberStatus::Queued => format!("{spin} Queued"),
        MemberStatus::Waiting => format!("{spin} Waiting"),
        MemberStatus::NeedsApproval => format!("{spin} Waiting for approval"),
        MemberStatus::Failed => format!("{spin} Failed"),
        MemberStatus::Idle => format!("{spin} Idle"),
    }
}

fn activity_context(elapsed_secs: Option<u64>, runtime_profile: Option<&str>) -> String {
    let mut parts = Vec::new();
    if let Some(elapsed) = elapsed_secs.map(fmt_elapsed_compact) {
        parts.push(elapsed);
    }
    if let Some(profile) = runtime_profile {
        parts.push(profile.to_string());
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(" • "))
    }
}

pub(crate) fn running_footer_text(
    running_count: usize,
    elapsed_secs: Option<u64>,
    names: &[String],
    spin: &str,
) -> Option<String> {
    if running_count == 0 {
        return None;
    }

    let noun = if running_count == 1 {
        "member"
    } else {
        "members"
    };
    let elapsed = elapsed_secs
        .map(fmt_elapsed_compact)
        .unwrap_or_else(|| "0s".to_string());
    let mut text =
        format!("{spin} Working {running_count} {noun} ({elapsed} • Ctrl+C to interrupt)");
    if !names.is_empty() {
        text.push_str(" · ");
        text.push_str(&names.join(", "));
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_elapsed_compact_scales_units_like_codex() {
        assert_eq!(fmt_elapsed_compact(8), "8s");
        assert_eq!(fmt_elapsed_compact(64), "1m 04s");
        assert_eq!(fmt_elapsed_compact(3723), "1h 02m 03s");
    }

    #[test]
    fn activity_text_keeps_interrupt_hint_stable() {
        assert_eq!(
            member_activity_text(MemberStatus::Running, None, Some(64), "⠋", None),
            "⠋ Working (1m 04s • Ctrl+C to interrupt)"
        );
        assert_eq!(
            member_activity_text(
                MemberStatus::Running,
                Some("reading files"),
                Some(64),
                "⠋",
                None
            ),
            "⠋ Thinking (1m 04s): reading files"
        );
        assert_eq!(
            member_activity_text(
                MemberStatus::Running,
                None,
                Some(64),
                "⠋",
                Some("model: gpt-5-codex • effort: high")
            ),
            "⠋ Working (1m 04s • model: gpt-5-codex • effort: high • Ctrl+C to interrupt)"
        );
    }

    #[test]
    fn footer_text_names_running_members() {
        assert_eq!(
            running_footer_text(
                2,
                Some(3723),
                &["Builder".to_string(), "QA".to_string()],
                "⠋"
            ),
            Some(
                "⠋ Working 2 members (1h 02m 03s • Ctrl+C to interrupt) · Builder, QA".to_string()
            )
        );
        assert_eq!(running_footer_text(0, None, &[], "⠋"), None);
    }

    #[test]
    fn spinner_advances_every_eighty_millis() {
        assert_eq!(spinner_at_millis(0), "⠋");
        assert_eq!(spinner_at_millis(80), "⠙");
        assert_eq!(spinner_at_millis(800), "⠋");
    }
}
