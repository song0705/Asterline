//! Composer completion: compute `/command` and `@member` suggestions from the
//! text before the cursor. Pure logic so it is fully unit-tested; the popup is
//! rendered and navigated by the TUI.

use std::collections::HashMap;

use crate::domain::team::BackendKind;

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

/// A backend skill that can be invoked after an explicit `@member` target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentSkill {
    pub name: String,
    pub description: String,
    /// The native CLI that can invoke this skill.
    pub backend: BackendKind,
    /// Exact text accepted by that backend, including any plugin namespace.
    pub invocation: String,
}

/// (name, hint, takes_argument). Also feeds the `/help` palette drawer.
pub(crate) const COMMANDS: &[(&str, &str, bool)] = &[
    ("ask", "send to one member", true),
    ("all", "send to everyone", true),
    ("attach", "open a member's native CLI session", true),
    ("abort", "cancel running members", false),
    ("approve", "approve first pending", false),
    ("block", "mark a run blocked", true),
    ("continue", "resume latest or selected run", true),
    ("diff", "show working-tree git diff", false),
    ("exit", "exit Asterline", false),
    (
        "effort",
        "set reasoning effort (low…ultra; model-dependent)",
        true,
    ),
    ("find", "search the transcript", true),
    ("focus", "view a member's logs", true),
    ("help", "show commands", false),
    ("logs", "raw logs · stderr · warnings", false),
    ("mode", "set the mode for subsequent messages", true),
    ("model", "choose or set a member's model", true),
    (
        "new",
        "start a fresh chat (new session, cleared transcript)",
        false,
    ),
    ("note", "record a run checkpoint", true),
    ("reject", "reject first pending", false),
    ("resume", "choose and restore a saved chat", false),
    ("retry", "re-send the latest user request", false),
    ("runs", "run status · next action", false),
    ("skills", "choose a skill for the next prompt", false),
    ("step", "manage run checklist", true),
    ("team", "edit roster · sessions · approvals", false),
    ("verify", "verify latest or selected run", true),
];

/// (name, hint) entries shown after `/mode `.
const MODES: &[(&str, &str)] = &[
    ("normal", "keep using direct messages until changed"),
    ("review", "keep using builder/reviewer runs until changed"),
    ("plan", "keep using leader/checklist runs until changed"),
    (
        "brainstorm",
        "keep using multi-wave idea generation until changed",
    ),
    (
        "team",
        "keep using coordinator-driven team runs until changed",
    ),
];

/// Compute the completion for `head` (composer text up to the cursor).
pub fn compute(head: &str, members: &[String]) -> Option<Completion> {
    compute_with_agent_skills(head, members, &[], &HashMap::new())
}

/// Compute completion, including backend-native skills after `@member /`.
pub fn compute_with_agent_skills(
    head: &str,
    members: &[String],
    skills: &[AgentSkill],
    member_backends: &HashMap<String, BackendKind>,
) -> Option<Completion> {
    let chars: Vec<char> = head.chars().collect();

    if let Some(completion) = targeted_skill_completion(&chars, skills, member_backends) {
        return Some(completion);
    }

    if head.starts_with('/') {
        return match chars.iter().position(|c| c.is_whitespace()) {
            // Still typing the command name.
            None => {
                let prefix: String = chars[1..].iter().collect();
                let lower = prefix.to_lowercase();
                let items: Vec<CompletionItem> = COMMANDS
                    .iter()
                    .filter(|(name, _, _)| slash_command_matches(name, &lower))
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
                if cmd == "mode" {
                    let arg: Vec<char> = chars[space + 1..].to_vec();
                    if arg.iter().any(|c| c.is_whitespace()) {
                        return None;
                    }
                    let prefix: String = arg.iter().collect();
                    let lower = prefix.to_lowercase();
                    let items = MODES
                        .iter()
                        .filter(|(name, _)| name.starts_with(&lower))
                        .map(|(name, hint)| CompletionItem {
                            label: format!("{name} — {hint}"),
                            insert: format!("{name} "),
                        })
                        .collect();
                    return non_empty("modes", space + 1, items);
                }
                if cmd != "ask"
                    && cmd != "attach"
                    && cmd != "effort"
                    && cmd != "focus"
                    && cmd != "model"
                {
                    return None;
                }
                let arg: Vec<char> = chars[space + 1..].to_vec();
                // Only while still typing the member token (no further space).
                if arg.iter().any(|c| c.is_whitespace()) {
                    return None;
                }
                let prefix: String = arg.iter().collect();
                let mut candidates: Vec<String> = (cmd != "model" && cmd != "attach")
                    .then(|| "all".to_string())
                    .into_iter()
                    .collect();
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

fn slash_command_matches(name: &str, query: &str) -> bool {
    name.starts_with(query) || (name == "new" && "clear".starts_with(query))
}

fn targeted_skill_completion(
    chars: &[char],
    skills: &[AgentSkill],
    member_backends: &HashMap<String, BackendKind>,
) -> Option<Completion> {
    if chars.first() != Some(&'@') {
        return None;
    }
    let member_end = chars.iter().position(|c| c.is_whitespace())?;
    let member: String = chars[1..member_end].iter().collect();
    // Composer routing accepts both a stable member ID and the member's
    // display name, case-insensitively. Completion must mirror that path so
    // `@Claude /` exposes the same actions that `@claude-primary /` does.
    let target_backend = member_backends.iter().find_map(|(candidate, backend)| {
        candidate.eq_ignore_ascii_case(&member).then_some(*backend)
    })?;
    let command_start = chars[member_end..]
        .iter()
        .position(|c| !c.is_whitespace())?
        + member_end;
    if chars.get(command_start) != Some(&'/')
        || chars[command_start + 1..].iter().any(|c| c.is_whitespace())
    {
        return None;
    }

    let prefix: String = chars[command_start + 1..].iter().collect();
    let lower = prefix.to_lowercase();
    let mut items = Vec::new();
    if "attach".starts_with(&lower) {
        items.push(CompletionItem {
            label: "/attach — open this member's native CLI session".to_string(),
            insert: "/attach".to_string(),
        });
    }
    if "model".starts_with(&lower) {
        items.push(CompletionItem {
            label: "/model — choose this member's model and reasoning effort".to_string(),
            insert: "/model".to_string(),
        });
    }
    items.extend(
        skills
            .iter()
            .filter(|skill| skill.backend == target_backend)
            .filter(|skill| {
                skill.name.to_lowercase().starts_with(&lower)
                    || skill
                        .invocation
                        .trim_start_matches(['/', '$'])
                        .to_lowercase()
                        .starts_with(&lower)
            })
            .map(|skill| CompletionItem {
                label: format!("{} — {}", skill.invocation, skill.description),
                insert: format!("{} ", skill.invocation),
            }),
    );
    non_empty("member actions & skills", command_start, items)
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
        assert!(c.items.iter().any(|i| i.insert == "/mode "));
        assert!(c.items.iter().any(|i| i.insert == "/exit"));
        assert!(c.items.iter().any(|i| i.insert == "/attach "));
        assert!(!c.items.iter().any(|i| i.insert == "/plan "));
        assert!(!c.items.iter().any(|i| i.insert == "/review "));
        assert!(c.items.iter().any(|i| i.insert == "/continue "));
        assert!(c.items.iter().any(|i| i.insert == "/note "));
        assert!(c.items.iter().any(|i| i.insert == "/block "));
        assert!(c.items.iter().any(|i| i.insert == "/step "));
        assert!(c.items.iter().any(|i| i.insert == "/skills"));
    }

    #[test]
    fn command_references_cover_every_palette_command() {
        let english = include_str!("../../docs/commands.md");
        let chinese = include_str!("../../docs/commands.zh-CN.md");
        for (name, _, _) in COMMANDS {
            let heading = format!("### `/{name}`");
            assert!(
                english.contains(&heading),
                "English command reference is missing {heading}"
            );
            assert!(
                chinese.contains(&heading),
                "Chinese command reference is missing {heading}"
            );
        }
    }

    #[test]
    fn slash_prefix_filters() {
        assert_eq!(inserts("/as"), vec!["/ask ".to_string()]);
        assert_eq!(
            inserts("/mo"),
            vec!["/mode ".to_string(), "/model ".to_string()]
        );
        assert!(inserts("/pl").is_empty());
        assert_eq!(inserts("/con"), vec!["/continue ".to_string()]);
        assert_eq!(inserts("/no"), vec!["/note ".to_string()]);
        assert_eq!(inserts("/blo"), vec!["/block ".to_string()]);
        assert_eq!(inserts("/ste"), vec!["/step ".to_string()]);
        let a = inserts("/a");
        assert!(a.contains(&"/ask ".to_string()) && a.contains(&"/all ".to_string()));
        let re = inserts("/re");
        assert_eq!(
            re,
            vec![
                "/reject".to_string(),
                "/resume".to_string(),
                "/retry".to_string()
            ]
        );
        assert!(inserts("/find").contains(&"/find ".to_string()));
        assert_eq!(inserts("/cl"), vec!["/new".to_string()]);
        assert_eq!(inserts("/clear"), vec!["/new".to_string()]);
        assert!(inserts("/lead").is_empty());
        assert!(inserts("/round").is_empty());
    }

    #[test]
    fn mode_command_opens_second_level_mode_completion() {
        let modes = compute("/mode ", &members()).unwrap();
        assert_eq!(modes.title, "modes");
        assert_eq!(modes.token_start, 6);
        assert_eq!(
            modes
                .items
                .iter()
                .map(|item| item.insert.as_str())
                .collect::<Vec<_>>(),
            vec!["normal ", "review ", "plan ", "brainstorm ", "team "]
        );
        assert_eq!(inserts("/mode br"), vec!["brainstorm ".to_string()]);
        assert!(compute("/mode review fix it", &members()).is_none());
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

        let attach = compute("/attach rev", &members()).expect("attach member completion");
        assert_eq!(
            attach
                .items
                .iter()
                .map(|item| item.insert.as_str())
                .collect::<Vec<_>>(),
            vec!["reviewer "]
        );
        assert!(compute("/attach all", &members()).is_none());
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

    #[test]
    fn targeted_slash_completes_agent_skills() {
        let skills = vec![
            AgentSkill {
                name: "review-patch".to_string(),
                description: "Review a patch carefully".to_string(),
                backend: BackendKind::Codex,
                invocation: "$review-patch".to_string(),
            },
            AgentSkill {
                name: "write-tests".to_string(),
                description: "Add regression tests".to_string(),
                backend: BackendKind::Codex,
                invocation: "$write-tests".to_string(),
            },
        ];

        let completion = compute_with_agent_skills(
            "@builder /rev",
            &members(),
            &skills,
            &HashMap::from([("builder".to_string(), BackendKind::Codex)]),
        )
        .unwrap();
        assert_eq!(completion.title, "member actions & skills");
        assert_eq!(completion.token_start, 9);
        assert_eq!(
            completion
                .items
                .iter()
                .map(|item| item.insert.as_str())
                .collect::<Vec<_>>(),
            vec!["$review-patch "]
        );
    }

    #[test]
    fn targeted_slash_offers_the_local_model_control_without_skills() {
        let completion = compute_with_agent_skills(
            "@builder /mo",
            &members(),
            &[],
            &HashMap::from([("builder".to_string(), BackendKind::Codex)]),
        )
        .expect("model control");

        assert_eq!(completion.items[0].insert, "/model");
        assert_eq!(
            completion.items[0].label,
            "/model — choose this member's model and reasoning effort"
        );
    }

    #[test]
    fn targeted_completion_offers_native_attach_and_local_model_controls() {
        let completion = compute_with_agent_skills(
            "@builder /",
            &members(),
            &[],
            &HashMap::from([("builder".to_string(), BackendKind::Codex)]),
        )
        .expect("member controls");

        assert_eq!(completion.title, "member actions & skills");
        assert!(
            completion.items.iter().any(|item| {
                item.insert == "/attach" && item.label.contains("native CLI session")
            })
        );
        assert!(completion.items.iter().any(|item| item.insert == "/model"));
        for command in ["/fast", "/permissions", "/compact", "/status", "/review"] {
            assert!(
                !completion.items.iter().any(|item| item.insert == command),
                "native control {command} must not be presented as a noninteractive action"
            );
        }
    }

    #[test]
    fn agent_skill_completion_requires_an_explicit_known_target() {
        let skills = vec![AgentSkill {
            name: "review-patch".to_string(),
            description: "Review a patch carefully".to_string(),
            backend: BackendKind::Codex,
            invocation: "$review-patch".to_string(),
        }];

        assert!(compute_with_agent_skills("/rev", &members(), &skills, &HashMap::new()).is_none());
        assert!(
            compute_with_agent_skills("@unknown /rev", &members(), &skills, &HashMap::new())
                .is_none()
        );
        assert!(
            compute_with_agent_skills("@builder /rev now", &members(), &skills, &HashMap::new())
                .is_none()
        );
    }

    #[test]
    fn targeted_skill_completion_only_shows_the_target_backend() {
        let members = vec![
            "claude".to_string(),
            "codex".to_string(),
            "grok".to_string(),
            "agy".to_string(),
        ];
        let backends = HashMap::from([
            ("claude".to_string(), BackendKind::Claude),
            ("codex".to_string(), BackendKind::Codex),
            ("grok".to_string(), BackendKind::Grok),
            ("agy".to_string(), BackendKind::Agy),
        ]);
        let skills = vec![
            AgentSkill {
                name: "wake".to_string(),
                description: "Claude only".to_string(),
                backend: BackendKind::Claude,
                invocation: "/wake".to_string(),
            },
            AgentSkill {
                name: "review".to_string(),
                description: "Codex only".to_string(),
                backend: BackendKind::Codex,
                invocation: "$review".to_string(),
            },
            AgentSkill {
                name: "inspect".to_string(),
                description: "Grok only".to_string(),
                backend: BackendKind::Grok,
                invocation: "/inspect".to_string(),
            },
            AgentSkill {
                name: "plan".to_string(),
                description: "Agy only".to_string(),
                backend: BackendKind::Agy,
                invocation: "/plan".to_string(),
            },
        ];

        let claude = compute_with_agent_skills("@claude /", &members, &skills, &backends)
            .expect("Claude skills");
        assert_eq!(
            claude
                .items
                .iter()
                .map(|item| item.insert.as_str())
                .collect::<Vec<_>>(),
            vec!["/attach", "/model", "/wake "]
        );

        let codex = compute_with_agent_skills("@codex /", &members, &skills, &backends)
            .expect("Codex skills");
        let codex_inserts = codex
            .items
            .iter()
            .map(|item| item.insert.as_str())
            .collect::<Vec<_>>();
        assert!(codex_inserts.contains(&"/attach"));
        assert!(codex_inserts.contains(&"/model"));
        assert!(codex_inserts.contains(&"$review "));
        assert!(!codex_inserts.contains(&"/fast"));
        assert!(!codex_inserts.contains(&"/review"));
        assert!(!codex_inserts.contains(&"/wake "));
        assert!(!codex_inserts.contains(&"/inspect "));
        assert!(!codex_inserts.contains(&"/plan "));

        for (member, expected) in [("grok", "/inspect "), ("agy", "/plan ")] {
            let completion =
                compute_with_agent_skills(&format!("@{member} /"), &members, &skills, &backends)
                    .unwrap();
            assert_eq!(
                completion
                    .items
                    .iter()
                    .map(|item| item.insert.as_str())
                    .collect::<Vec<_>>(),
                vec!["/attach", "/model", expected]
            );
        }
    }
}
