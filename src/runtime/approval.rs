//! Heuristic gate for risky-looking requests.
//!
//! Backends enforce their own sandbox/permission policy; this is an additional
//! coarse gate that pauses a prompt mentioning shell/file/git actions until the
//! user approves it, so destructive intent is never auto-relayed.

/// Classify a request body as a risky action kind, if any.
pub fn risky_action_kind(body: &str) -> Option<&'static str> {
    let lower = body.to_ascii_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();

    if words.iter().any(|word| *word == "git")
        || lower.starts_with("git ")
        || lower.contains(" git ")
    {
        return Some("git");
    }

    if words
        .iter()
        .any(|word| matches!(*word, "shell" | "bash" | "sh" | "zsh" | "command"))
        || lower.contains("run `")
    {
        return Some("shell");
    }

    if lower.contains("edit file")
        || lower.contains("modify file")
        || lower.contains("write file")
        || lower.contains("delete file")
        || lower.contains("remove file")
        || words
            .iter()
            .any(|word| matches!(*word, "rm" | "mv" | "chmod" | "chown"))
    {
        return Some("file");
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_git_shell_and_file_actions() {
        assert_eq!(risky_action_kind("run git status first"), Some("git"));
        assert_eq!(risky_action_kind("please run `cargo test`"), Some("shell"));
        assert_eq!(risky_action_kind("delete file foo.rs"), Some("file"));
        assert_eq!(risky_action_kind("rm the temp dir"), Some("file"));
    }

    #[test]
    fn plain_requests_are_not_flagged() {
        assert_eq!(risky_action_kind("summarize the architecture"), None);
        assert_eq!(risky_action_kind("explain this function"), None);
    }
}
