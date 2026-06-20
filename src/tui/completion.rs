//! Composer completion: compute `/command` and `@member` suggestions from the
//! text before the cursor. Pure logic so it is fully unit-tested; the popup is
//! rendered and navigated by the TUI.

/// One suggestion: a label shown in the popup and the text to insert in place
/// of the current token.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompletionItem {
    pub label: String,
    pub insert: String,
}

/// An active completion: a titled list of items replacing the token that starts
/// at `token_start` (a char index into the composer head).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Completion {
    pub title: &'static str,
    pub token_start: usize,
    pub items: Vec<CompletionItem>,
}

/// (name, hint, takes_argument)
const COMMANDS: &[(&str, &str, bool)] = &[
    ("ask", "send to one member", true),
    ("all", "send to everyone", true),
    ("effort", "set reasoning effort (low…max)", true),
    ("team", "roster · sessions · approvals", false),
    ("logs", "raw logs · stderr · warnings", false),
    ("sessions", "session ids", false),
    ("status", "team status", false),
    ("retry", "resume paused route / re-run", false),
    ("abort", "cancel running members", false),
    ("approve", "approve first pending", false),
    ("reject", "reject first pending", false),
    ("workflow", "coordinate a goal across the team", true),
    ("focus", "view a member's logs", true),
    ("help", "show commands", false),
];

/// Compute the completion for `head` (composer text up to the cursor).
pub fn compute(head: &str, members: &[String]) -> Option<Completion> {
    let chars: Vec<char> = head.chars().collect();

    if head.starts_with('/') {
        return match chars.iter().position(|c| c.is_whitespace()) {
            // Still typing the command name.
            None => {
                let prefix: String = chars[1..].iter().collect();
                let lower = prefix.to_lowercase();
                let items: Vec<CompletionItem> = COMMANDS
                    .iter()
                    .filter(|(name, _, _)| name.starts_with(&lower))
                    .map(|(name, hint, takes_arg)| CompletionItem {
                        label: format!("/{name} — {hint}"),
                        insert: if *takes_arg {
                            format!("/{name} ")
                        } else {
                            format!("/{name}")
                        },
                    })
                    .collect();
                non_empty("commands", 0, items)
            }
            // Command chosen; only `/ask` completes its first argument (a member).
            Some(space) => {
                let cmd: String = chars[1..space].iter().collect();
                if cmd != "ask" && cmd != "effort" && cmd != "focus" {
                    return None;
                }
                let arg: Vec<char> = chars[space + 1..].to_vec();
                // Only while still typing the member token (no further space).
                if arg.iter().any(|c| c.is_whitespace()) {
                    return None;
                }
                let prefix: String = arg.iter().collect();
                let mut candidates = vec!["all".to_string()];
                candidates.extend(members.iter().cloned());
                member_completion(
                    &prefix,
                    space + 1,
                    &candidates,
                    "ask a member",
                    |m| m.to_string(),
                    |m| format!("{m} "),
                )
            }
        };
    }

    // `@member` mention anywhere: complete the last whitespace-delimited token
    // if it starts with '@'.
    let word_start = chars
        .iter()
        .rposition(|c| c.is_whitespace())
        .map(|i| i + 1)
        .unwrap_or(0);
    if chars.get(word_start) == Some(&'@') {
        let prefix: String = chars[word_start + 1..].iter().collect();
        let mut candidates = vec!["all".to_string()];
        candidates.extend(members.iter().cloned());
        return member_completion(
            &prefix,
            word_start,
            &candidates,
            "mention a member",
            |m| format!("@{m}"),
            |m| format!("@{m} "),
        );
    }

    None
}

fn member_completion(
    prefix: &str,
    token_start: usize,
    members: &[String],
    title: &'static str,
    label: impl Fn(&str) -> String,
    insert: impl Fn(&str) -> String,
) -> Option<Completion> {
    let lower = prefix.to_lowercase();
    let items: Vec<CompletionItem> = members
        .iter()
        .filter(|m| m.to_lowercase().starts_with(&lower))
        .map(|m| CompletionItem {
            label: label(m),
            insert: insert(m),
        })
        .collect();
    non_empty(title, token_start, items)
}

fn non_empty(
    title: &'static str,
    token_start: usize,
    items: Vec<CompletionItem>,
) -> Option<Completion> {
    if items.is_empty() {
        None
    } else {
        Some(Completion {
            title,
            token_start,
            items,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn members() -> Vec<String> {
        vec!["builder".to_string(), "reviewer".to_string()]
    }

    fn inserts(head: &str) -> Vec<String> {
        compute(head, &members())
            .map(|c| c.items.into_iter().map(|i| i.insert).collect())
            .unwrap_or_default()
    }

    #[test]
    fn slash_lists_all_commands() {
        let c = compute("/", &members()).unwrap();
        assert_eq!(c.token_start, 0);
        assert!(c.items.iter().any(|i| i.insert == "/ask "));
        assert!(c.items.iter().any(|i| i.insert == "/team"));
    }

    #[test]
    fn slash_prefix_filters() {
        assert_eq!(inserts("/as"), vec!["/ask ".to_string()]);
        let a = inserts("/a");
        assert!(a.contains(&"/ask ".to_string()) && a.contains(&"/all ".to_string()));
    }

    #[test]
    fn ask_completes_member_argument() {
        let c = compute("/ask rev", &members()).unwrap();
        assert_eq!(c.title, "ask a member");
        assert_eq!(c.token_start, 5);
        assert_eq!(
            c.items,
            vec![CompletionItem {
                label: "reviewer".to_string(),
                insert: "reviewer ".to_string()
            }]
        );
    }

    #[test]
    fn ask_with_no_prefix_lists_all_members() {
        let c = compute("/ask ", &members()).unwrap();
        assert_eq!(c.items.len(), 3);
        assert_eq!(c.items[0].insert, "all ");
    }

    #[test]
    fn ask_after_member_chosen_has_no_popup() {
        assert!(compute("/ask reviewer hello", &members()).is_none());
    }

    #[test]
    fn other_commands_do_not_complete_args() {
        assert!(compute("/all hello", &members()).is_none());
        assert!(compute("/team ", &members()).is_none());
    }

    #[test]
    fn at_mention_completes_member() {
        let c = compute("@rev", &members()).unwrap();
        assert_eq!(c.token_start, 0);
        assert_eq!(
            c.items[0],
            CompletionItem {
                label: "@reviewer".to_string(),
                insert: "@reviewer ".to_string()
            }
        );
    }

    #[test]
    fn at_mention_completes_all() {
        let c = compute("@a", &members()).unwrap();
        assert_eq!(c.token_start, 0);
        assert_eq!(
            c.items[0],
            CompletionItem {
                label: "@all".to_string(),
                insert: "@all ".to_string()
            }
        );
    }

    #[test]
    fn at_mention_mid_text() {
        let c = compute("ping @bu", &members()).unwrap();
        assert_eq!(c.token_start, 5);
        assert_eq!(
            c.items[0],
            CompletionItem {
                label: "@builder".to_string(),
                insert: "@builder ".to_string()
            }
        );
    }

    #[test]
    fn plain_text_has_no_completion() {
        assert!(compute("hello world", &members()).is_none());
        assert!(compute("", &members()).is_none());
    }

    #[test]
    fn unknown_slash_prefix_has_no_items() {
        assert!(compute("/zzz", &members()).is_none());
    }
}
