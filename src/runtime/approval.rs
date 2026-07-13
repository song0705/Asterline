//! Heuristic gate for risky-looking requests.
//!
//! Backends enforce their own sandbox/permission policy; this is an additional
//! coarse gate that pauses a prompt mentioning shell/file/git (or custom
//! keyword) actions until the user approves it, so destructive intent is never
//! auto-relayed. Categories and surfaces are configured via
//! [`ApprovalPolicy`](crate::domain::team::ApprovalPolicy).

use crate::domain::team::{ApprovalPolicy, ApprovalSurface};

/// Built-in category order used when classifying a prompt body.
const BUILTIN_ORDER: &[&str] = &["git", "shell", "file"];

/// Classifies prompt bodies against an [`ApprovalPolicy`] and reports which
/// surfaces the gate covers.
pub struct ApprovalMatcher {
    /// Enabled built-in category names, in check order.
    enabled_builtins: Vec<&'static str>,
    /// Custom categories sorted alphabetically: (name, lowercased keywords).
    custom: Vec<(String, Vec<String>)>,
    /// Surfaces the gate applies to. `None` means all surfaces.
    apply_to: Option<Vec<ApprovalSurface>>,
}

impl ApprovalMatcher {
    pub fn from_policy(policy: &ApprovalPolicy) -> Self {
        let enabled_builtins = match &policy.gate {
            None => BUILTIN_ORDER.to_vec(),
            Some(list) => BUILTIN_ORDER
                .iter()
                .copied()
                .filter(|name| list.iter().any(|entry| entry == name))
                .collect(),
        };

        let mut custom: Vec<(String, Vec<String>)> = policy
            .keywords
            .iter()
            .map(|(name, keywords)| {
                let kws = keywords
                    .iter()
                    .map(|k| k.to_ascii_lowercase())
                    .collect::<Vec<_>>();
                (name.clone(), kws)
            })
            .collect();
        custom.sort_by(|a, b| a.0.cmp(&b.0));

        Self {
            enabled_builtins,
            custom,
            apply_to: policy.apply_to.clone(),
        }
    }

    /// Classify a prompt body as a risky category, if any. Built-in categories
    /// first (only those enabled by `gate`), then custom `keywords` categories
    /// in alphabetical order (deterministic).
    pub fn classify(&self, body: &str) -> Option<String> {
        if let Some(kind) = classify_builtins(body, &self.enabled_builtins) {
            return Some(kind.to_string());
        }
        classify_custom(body, &self.custom)
    }

    /// Whether the gate covers a surface (`None` in policy -> true for all).
    pub fn applies_to(&self, surface: ApprovalSurface) -> bool {
        match &self.apply_to {
            None => true,
            Some(list) => list.contains(&surface),
        }
    }
}

fn classify_builtins(body: &str, enabled: &[&str]) -> Option<&'static str> {
    let lower = body.to_ascii_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();

    for kind in enabled {
        match *kind {
            "git" if matches_git(&lower, &words) => return Some("git"),
            "shell" if matches_shell(&lower, &words) => return Some("shell"),
            "file" if matches_file(&lower, &words) => return Some("file"),
            _ => {}
        }
    }
    None
}

fn matches_git(lower: &str, words: &[&str]) -> bool {
    words.contains(&"git") || lower.starts_with("git ") || lower.contains(" git ")
}

fn matches_shell(lower: &str, words: &[&str]) -> bool {
    words
        .iter()
        .any(|word| matches!(*word, "shell" | "bash" | "sh" | "zsh" | "command"))
        || lower.contains("run `")
}

fn matches_file(lower: &str, words: &[&str]) -> bool {
    lower.contains("edit file")
        || lower.contains("modify file")
        || lower.contains("write file")
        || lower.contains("delete file")
        || lower.contains("remove file")
        || words
            .iter()
            .any(|word| matches!(*word, "rm" | "mv" | "chmod" | "chown"))
}

fn classify_custom(body: &str, custom: &[(String, Vec<String>)]) -> Option<String> {
    let lower = body.to_ascii_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();

    for (name, keywords) in custom {
        for keyword in keywords {
            if keyword.chars().any(char::is_whitespace) {
                if lower.contains(keyword.as_str()) {
                    return Some(name.clone());
                }
            } else if words.contains(&keyword.as_str()) {
                return Some(name.clone());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn flags_git_shell_and_file_actions() {
        let matcher = ApprovalMatcher::from_policy(&ApprovalPolicy::default());
        assert_eq!(matcher.classify("run git status first"), Some("git".into()));
        assert_eq!(
            matcher.classify("please run `cargo test`"),
            Some("shell".into())
        );
        assert_eq!(matcher.classify("delete file foo.rs"), Some("file".into()));
        assert_eq!(matcher.classify("rm the temp dir"), Some("file".into()));
    }

    #[test]
    fn plain_requests_are_not_flagged() {
        let matcher = ApprovalMatcher::from_policy(&ApprovalPolicy::default());
        assert_eq!(matcher.classify("summarize the architecture"), None);
        assert_eq!(matcher.classify("explain this function"), None);
    }

    #[test]
    fn gate_enables_only_listed_builtins() {
        let policy = ApprovalPolicy {
            gate: Some(vec!["shell".to_string()]),
            ..Default::default()
        };
        let matcher = ApprovalMatcher::from_policy(&policy);

        assert_eq!(
            matcher.classify("please run `cargo test`"),
            Some("shell".into())
        );
        assert_eq!(matcher.classify("run git status"), None);
        assert_eq!(matcher.classify("delete file foo.rs"), None);
    }

    #[test]
    fn gate_none_enables_all_builtins() {
        let matcher = ApprovalMatcher::from_policy(&ApprovalPolicy::default());
        assert_eq!(matcher.classify("run git status"), Some("git".into()));
        assert_eq!(
            matcher.classify("please run `cargo test`"),
            Some("shell".into())
        );
        assert_eq!(matcher.classify("rm the temp dir"), Some("file".into()));
    }

    #[test]
    fn gate_ignores_unknown_category_names() {
        let policy = ApprovalPolicy {
            gate: Some(vec!["nope".to_string(), "git".to_string()]),
            ..Default::default()
        };
        let matcher = ApprovalMatcher::from_policy(&policy);
        assert_eq!(matcher.classify("run git status"), Some("git".into()));
        assert_eq!(matcher.classify("please run `cargo test`"), None);
    }

    #[test]
    fn custom_category_matches_word_and_phrase() {
        let policy = ApprovalPolicy {
            gate: Some(vec![]), // disable builtins so custom wins cleanly
            keywords: HashMap::from([
                ("deploy".to_string(), vec!["kubectl".to_string()]),
                ("release".to_string(), vec!["ship it".to_string()]),
            ]),
            ..Default::default()
        };
        let matcher = ApprovalMatcher::from_policy(&policy);

        assert_eq!(
            matcher.classify("please kubectl apply now"),
            Some("deploy".into())
        );
        assert_eq!(
            matcher.classify("time to ship it today"),
            Some("release".into())
        );
        // whole-word only for non-whitespace keywords
        assert_eq!(matcher.classify("kubectlize the cluster"), None);
    }

    #[test]
    fn custom_categories_checked_alphabetically() {
        let policy = ApprovalPolicy {
            gate: Some(vec![]),
            keywords: HashMap::from([
                ("zeta".to_string(), vec!["danger".to_string()]),
                ("alpha".to_string(), vec!["danger".to_string()]),
            ]),
            ..Default::default()
        };
        let matcher = ApprovalMatcher::from_policy(&policy);
        assert_eq!(matcher.classify("this is danger"), Some("alpha".into()));
    }

    #[test]
    fn no_match_returns_none() {
        let matcher = ApprovalMatcher::from_policy(&ApprovalPolicy::default());
        assert_eq!(matcher.classify("summarize the architecture"), None);
    }

    #[test]
    fn applies_to_defaults_and_explicit_lists() {
        let matcher = ApprovalMatcher::from_policy(&ApprovalPolicy::default());
        assert!(matcher.applies_to(ApprovalSurface::User));
        assert!(matcher.applies_to(ApprovalSurface::Relay));
        assert!(matcher.applies_to(ApprovalSurface::Mode));

        let policy = ApprovalPolicy {
            apply_to: Some(vec![ApprovalSurface::User]),
            ..Default::default()
        };
        let matcher = ApprovalMatcher::from_policy(&policy);
        assert!(matcher.applies_to(ApprovalSurface::User));
        assert!(!matcher.applies_to(ApprovalSurface::Relay));
        assert!(!matcher.applies_to(ApprovalSurface::Mode));
    }

    #[test]
    fn builtins_take_priority_over_custom() {
        let policy = ApprovalPolicy {
            keywords: HashMap::from([("deploy".to_string(), vec!["git".to_string()])]),
            ..Default::default()
        };
        let matcher = ApprovalMatcher::from_policy(&policy);
        assert_eq!(matcher.classify("run git status"), Some("git".into()));
    }
}
