//! Collaboration-mode prompt templates.
//!
//! These templates are shared with the fake team runner so unit tests and
//! `--fake` stay in sync with the real engine. Keep them pure (no I/O).

use crate::domain::event::RunId;
use crate::domain::team::MemberId;

/// Marker + instruction block embedded in every reviewer prompt. The fake team
/// runner keys on this constant, so never inline the text elsewhere.
pub const REVIEW_PROTOCOL_HINT: &str = "End your reply with exactly one control line reporting your verdict:\n\
     @@review {\"verdict\":\"approve\",\"summary\":\"why\"}\n\
     or\n\
     @@review {\"verdict\":\"request_changes\",\"summary\":\"what to fix\",\"items\":[\"...\"]}";

/// Marker for the leader planning prompt; the fake team runner keys on it.
pub const PLAN_MODE_HINT: &str = "Plan the work as a checklist now: emit one \
`@@run_step {\"action\":\"add\",\"title\":\"...\"}` line per step. \
Do not do the work yourself. After the reviewer approves, a configured Builder receives the \
whole checklist automatically; with no Builder, this is a reviewed plan only.";

/// Markers for brainstorm generation waves; the fake runner keys on them.
pub const BRAINSTORM_PROPOSE_HINT: &str = "BRAINSTORM_WAVE: SEED";
pub const BRAINSTORM_BUILD_HINT: &str = "BRAINSTORM_WAVE: BUILD";
pub const BRAINSTORM_STRETCH_HINT: &str = "BRAINSTORM_WAVE: STRETCH";
pub const BRAINSTORM_VOTE_HINT: &str = "BRAINSTORM_PHASE: PRIVATE_VOTE";
pub const BRAINSTORM_SYNTHESIS_HINT: &str = "BRAINSTORM_PHASE: SYNTHESIZE_RANKING";

const BRAINSTORM_GENERATION_RULES: &str = "Generation rules:\n\
- Suspend judgment: do not critique, rank, reject, vote, or choose a winner.\n\
- Emit exactly the requested number of cards. Do not emit extra cards.\n\
- Spend the budget on variety, not volume: each card must be a distinct idea.\n\
- Welcome bold, surprising, and temporarily impractical ideas.\n\
- Build on and combine ideas without turning the response into an evaluation.";

/// Prompt sent to the builder on the first iteration of a review run.
pub fn review_task_prompt(task: &str) -> String {
    format!(
        "You are the builder in review mode.\n\n\
         Task:\n{task}\n\n\
         Implement the task in the working tree and report what you changed. \
         Be concrete about files and decisions so a reviewer can assess the work."
    )
}

/// Prompt sent to the reviewer after a builder turn completes.
pub fn review_prompt(
    task: &str,
    builder_display: &str,
    builder_output: &str,
    reviewer_hint: Option<&str>,
) -> String {
    let report = if builder_output.trim().is_empty() {
        "(no report text)"
    } else {
        builder_output
    };
    let hint = match reviewer_hint.map(str::trim).filter(|s| !s.is_empty()) {
        Some(text) => format!("Reviewer note from the user:\n{text}\n\n"),
        None => String::new(),
    };
    format!(
        "You are the reviewer in review mode.\n\n\
         Task:\n{task}\n\n\
         {builder_display} reported:\n{report}\n\n\
         Inspect the actual changes (`git status` / `git diff`) rather than trusting the report text.\n\n\
         {hint}\
         Judge the work on substance — do not be swayed by how confident or polished the report sounds. \
         Decide whether the work is ready or needs changes.\n\n\
         {REVIEW_PROTOCOL_HINT}"
    )
}

/// Prompt sent to the builder when auto-verify fails in review mode.
pub fn verify_failure_prompt(
    task: &str,
    command: &str,
    summary: &str,
    iteration: u32,
    max_iterations: u32,
) -> String {
    format!(
        "You are the builder in review mode (iteration {iteration}/{max_iterations}).\n\n\
         Task:\n{task}\n\n\
         Automatic verification failed.\n\
         Command: {command}\n\
         Output:\n{summary}\n\n\
         Fix the failures in the working tree and report what you changed so the reviewer can reassess."
    )
}

/// Prompt sent to the builder when the reviewer requests changes.
pub fn review_iteration_prompt(
    task: &str,
    reviewer_display: &str,
    feedback: &str,
    iteration: u32,
    max_iterations: u32,
) -> String {
    format!(
        "You are the builder in review mode (iteration {iteration}/{max_iterations}).\n\n\
         Task:\n{task}\n\n\
         {reviewer_display} requested changes:\n{feedback}\n\n\
         Address the feedback in the working tree and report what you fixed."
    )
}

/// Nudge sent when the reviewer finishes a turn without an `@@review` line.
pub fn verdict_nudge_prompt() -> String {
    format!(
        "Your previous reply did not include a structured review verdict. \
         Reply with ONLY the control line — no other text.\n\n\
         {REVIEW_PROTOCOL_HINT}"
    )
}

/// Leader planning prompt for Plan mode. `teammates` are `(id, role)` pairs.
pub fn plan_plan_prompt(task: &str, teammates: &[(String, String)]) -> String {
    let ids: Vec<&str> = teammates.iter().map(|(id, _)| id.as_str()).collect();
    let roles: Vec<String> = teammates
        .iter()
        .map(|(id, role)| format!("{id} ({role})"))
        .collect();
    format!(
        "You are the planning lead.\n\n\
         Task:\n{task}\n\n\
         Teammates: {}\n\
         Roles: {}\n\n\
         {PLAN_MODE_HINT}",
        ids.join(", "),
        roles.join(", ")
    )
}

/// Re-ask the leader for an owned checklist after an empty plan turn.
pub fn plan_nudge_prompt() -> String {
    format!(
        "Your previous reply did not produce an actionable checklist. \
         Emit @@run_step add lines now — every step needs a concrete title.\n\n\
         {PLAN_MODE_HINT}"
    )
}

/// Per-owner dispatch covering all of their owned todo steps.
pub fn step_dispatch_prompt(run_id: RunId, leader: &MemberId, steps: &[(u32, String)]) -> String {
    let list: Vec<String> = steps
        .iter()
        .map(|(n, title)| format!("  - step #{n}: {title}"))
        .collect();
    format!(
        "You own these steps of {run_id}:\n{}\n\n\
         Work through them in the working tree and mark each done with \
         @@run_step {{\"action\":\"done\",\"step\":N}} as you finish. \
         Before ending your turn, send exactly one completion handoff to the planning lead with \
         @@team_message {{\"to\":\"{leader}\",\"kind\":\"reply\",\"body\":\"summary of results, checks, and blockers\"}}. \
         Updating the checklist does not replace this handoff.",
        list.join("\n")
    )
}

/// One-time reminder for an owner that ended a work turn without completing
/// the checklist protocol.  The next incomplete completion is re-planned.
pub fn plan_step_nudge_prompt(run_id: RunId, steps: &[(u32, String)]) -> String {
    let list = steps
        .iter()
        .map(|(number, title)| format!("  - step #{number}: {title}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Your previous turn for {run_id} ended while these assigned checklist steps were still doing:\n\
         {list}\n\n\
         If you completed a step, report it now with exactly one \
         @@run_step {{\"action\":\"done\",\"step\":N}} line per completed step. \
         If it is blocked, report the blocker clearly."
    )
}

/// Leader prompt after an execution round left unfinished steps.
pub fn plan_progress_prompt(
    task: &str,
    unfinished: &[String],
    iteration: u32,
    max: u32,
    extra_note: Option<&str>,
) -> String {
    let list = if unfinished.is_empty() {
        "(none listed)".to_string()
    } else {
        unfinished.join("\n")
    };
    let extra = extra_note.map(|s| format!("{s}\n\n")).unwrap_or_default();
    format!(
        "You are the planning lead (iteration {iteration}/{max}).\n\n\
         Task:\n{task}\n\n\
         Unfinished checklist steps:\n{list}\n\n\
         {extra}\
         Re-assess the plan: add, remove, reorder, or clarify steps as needed. Do not do the work \
         yourself. After review approval, the configured Builder receives the whole checklist.\n\n\
         {PLAN_MODE_HINT}"
    )
}

/// Leader prompt when auto-verify fails after plan review approve.
pub fn plan_verify_failure_prompt(
    task: &str,
    command: &str,
    summary: &str,
    iteration: u32,
    max: u32,
) -> String {
    format!(
        "You are the planning lead (iteration {iteration}/{max}).\n\n\
         Task:\n{task}\n\n\
         Automatic verification failed.\n\
         Command: {command}\n\
         Output:\n{summary}\n\n\
         Re-plan the checklist so the Builder can fix the failures. Do not do the work yourself.\n\n\
         {PLAN_MODE_HINT}"
    )
}

/// Leader prompt when the reviewer requests changes on a plan run.
pub fn plan_iteration_prompt(
    task: &str,
    reviewer_display: &str,
    feedback: &str,
    iteration: u32,
    max: u32,
) -> String {
    format!(
        "You are the planning lead (iteration {iteration}/{max}).\n\n\
         Task:\n{task}\n\n\
         {reviewer_display} requested changes:\n{feedback}\n\n\
         Update the checklist and plan so the Builder can address the feedback. \
         Do not do the work yourself.\n\n\
         {PLAN_MODE_HINT}"
    )
}

/// Reviewer prompt after the leader has produced a Plan-mode checklist, before
/// any configured Builder is dispatched.
pub fn plan_review_prompt(task: &str, steps_summary: &str, verify_command: Option<&str>) -> String {
    let summary = if steps_summary.trim().is_empty() {
        "(no steps recorded)"
    } else {
        steps_summary
    };
    let gate = match verify_command.map(str::trim).filter(|s| !s.is_empty()) {
        Some(cmd) => format!(
            "The Builder will be expected to satisfy the project verification gate `{cmd}`. \
             Check that the plan includes work needed to make that realistic.\n\n"
        ),
        None => String::new(),
    };
    format!(
        "You are the reviewer in plan mode. This is a plan review before implementation.\n\n\
         Task:\n{task}\n\n\
         Completed checklist:\n{summary}\n\n\
         Check that the checklist is complete, correctly ordered, testable, and addresses the \
         task's important risks and acceptance criteria. No implementation has been dispatched yet, \
         so judge the plan itself rather than code changes. {gate}\
         Approve only when the plan is ready to hand to a Builder; otherwise request concrete changes.\n\n\
         {REVIEW_PROTOCOL_HINT}"
    )
}

/// Body text for a manual `@owner` step dispatch from the TUI (after `@{owner} `).
pub fn manual_step_dispatch_text(
    run: impl std::fmt::Display,
    instruction: &str,
    number: u32,
    title: &str,
) -> String {
    format!(
        "{instruction} {run} step #{number}: {title}. Update the checklist with @@run_step as you progress."
    )
}

/// First generation wave: each participant creates independent seeds.
pub fn brainstorm_propose_prompt(
    topic: &str,
    n_participants: usize,
    ideas_per_round: u32,
) -> String {
    format!(
        "You are 1 of {n_participants} equal contributors in the first wave of a brainstorm.\n\n\
         Topic:\n{topic}\n\n\
         {BRAINSTORM_GENERATION_RULES}\n\n\
         Emit exactly {ideas_per_round} short, distinct idea cards and no more. \
         Extra cards are discarded. Include one deliberately wild idea among those \
         {ideas_per_round}. Keep each card atomic and emit it using the deployed skill's \
         `@@brainstorm_card` schema. You are not shown other contributors' ideas in this seed wave.\n\n\
         {BRAINSTORM_PROPOSE_HINT}"
    )
}

/// Middle generation waves: use a limited peer sample for cross-pollination.
pub fn brainstorm_build_prompt(
    topic: &str,
    round: u32,
    rounds: u32,
    ideas_per_round: u32,
    context: &str,
) -> String {
    format!(
        "You are in generation wave {round}/{rounds} of a brainstorm. Continue expanding the \
         idea space; this is not a review round.\n\n\
         Topic:\n{topic}\n\n\
         {BRAINSTORM_GENERATION_RULES}\n\n\
         Prior idea batches for inspiration:\n{context}\n\n\
         Emit exactly {ideas_per_round} new atomic idea cards and no more. Extra cards are \
         discarded. Cover these moves across that budget: one independent NEW direction, one \
         BUILD that extends a prior idea, and one COMBINE or MUTATE idea. \
         Emit every card with `@@brainstorm_card`; state the operation and canonical source IDs \
         on each derived card. Do not discuss \
         strengths, risks, trade-offs, or feasibility.\n\n\
         {BRAINSTORM_BUILD_HINT}"
    )
}

/// Final generation wave: force movement away from the most obvious categories.
pub fn brainstorm_stretch_prompt(
    topic: &str,
    round: u32,
    rounds: u32,
    ideas_per_round: u32,
    context: &str,
) -> String {
    format!(
        "You are in the final generation wave {round}/{rounds} of a brainstorm. Stretch the \
         idea space before the idea set closes; this is still not evaluation or decision-making.\n\n\
         Topic:\n{topic}\n\n\
         {BRAINSTORM_GENERATION_RULES}\n\n\
         Prior idea batches for inspiration:\n{context}\n\n\
         Emit exactly {ideas_per_round} new atomic idea cards and no more. Extra cards are \
         discarded. Use different moves from this set: invert an assumption, remove a \
         constraint, borrow an analogy from another domain, bridge two previously separate \
         directions. Emit every card with `@@brainstorm_card`, \
         label each move, and cite canonical source IDs. Preserve strange but relevant \
         possibilities; do not select a preferred option.\n\n\
         {BRAINSTORM_STRETCH_HINT}"
    )
}

/// Independent post-generation ballot. Participants do not see peer votes.
pub fn brainstorm_vote_prompt(topic: &str, idea_set: &str, top_k: usize) -> String {
    format!(
        "The judgment-free generation waves are complete. You are now an independent voter.\n\n\
         Original topic:\n{topic}\n\n\
         Canonical IdeaSet batches:\n{idea_set}\n\n\
         Rank exactly your top {top_k} individual ideas. Identify each idea as \
         <batch>#<item>, for example R2-B#3 means item 3 from batch R2-B. Judge against the \
         original topic using relevance, novelty, feasibility, expected leverage, and how \
         testable the idea is. Do not copy another participant's preferences; you have not \
         been shown their ballot.\n\n\
         Briefly explain your ranking, then end with exactly one control line:\n\
         @@brainstorm_vote {{\"ranked\":[\"R2-B#3\",\"R1-A#1\"],\"summary\":\"short rationale\"}}\n\n\
         {BRAINSTORM_VOTE_HINT}"
    )
}

/// Neutral final report after deterministic ballot aggregation.
pub fn brainstorm_synthesis_prompt(
    topic: &str,
    idea_set: &str,
    tally: &str,
    ballots: &str,
) -> String {
    format!(
        "Act as the neutral facilitator for the completed brainstorm. Generation and private \
         voting are finished.\n\n\
         Original topic:\n{topic}\n\n\
         Canonical IdeaSet batches:\n{idea_set}\n\n\
         Deterministic Borda tally (do not change this order or invent votes):\n{tally}\n\n\
         Private ballot rationales:\n{ballots}\n\n\
         Produce the final decision report in the user's language:\n\
         1. a ranked top-5 table with candidate ID, idea title, score, and why it ranked there;\n\
         2. areas of agreement and disagreement across voters;\n\
         3. one primary recommendation and one backup;\n\
         4. the smallest concrete experiment for the primary recommendation.\n\
         Preserve minority insights and flag ties explicitly.\n\n\
         {BRAINSTORM_SYNTHESIS_HINT}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_task_prompt_contains_task() {
        let prompt = review_task_prompt("add parser tests");
        assert!(prompt.contains("add parser tests"));
        assert!(prompt.contains("builder"));
    }

    #[test]
    fn review_prompt_contains_task_output_and_protocol() {
        let prompt = review_prompt(
            "ship feature",
            "Builder",
            "implemented foo",
            Some("look at the parser tests"),
        );
        assert!(prompt.contains("ship feature"));
        assert!(prompt.contains("Builder"));
        assert!(prompt.contains("implemented foo"));
        assert!(prompt.contains("Reviewer note from the user"));
        assert!(prompt.contains("look at the parser tests"));
        assert!(prompt.contains("git diff"));
        assert!(prompt.contains("substance"));
        assert!(prompt.contains(REVIEW_PROTOCOL_HINT));
        assert!(prompt.contains("@@review"));
        let bare = review_prompt("task", "Builder", "done", None);
        assert!(!bare.contains("Reviewer note from the user"));
    }

    #[test]
    fn verify_failure_prompt_includes_command_and_summary() {
        let prompt = verify_failure_prompt("task", "cargo test", "assertion failed", 2, 3);
        assert!(prompt.contains("cargo test"));
        assert!(prompt.contains("assertion failed"));
        assert!(prompt.contains("2/3"));
    }

    #[test]
    fn review_iteration_prompt_contains_feedback_and_iteration() {
        let prompt = review_iteration_prompt("ship feature", "Reviewer", "fix the edge case", 2, 3);
        assert!(prompt.contains("ship feature"));
        assert!(prompt.contains("Reviewer"));
        assert!(prompt.contains("fix the edge case"));
        assert!(prompt.contains("2/3"));
    }

    #[test]
    fn verdict_nudge_includes_protocol_hint() {
        let prompt = verdict_nudge_prompt();
        assert!(prompt.contains(REVIEW_PROTOCOL_HINT));
        assert!(prompt.contains("ONLY"));
    }

    #[test]
    fn plan_plan_prompt_lists_teammates_and_hint() {
        let teammates = vec![
            ("builder".into(), "impl".into()),
            ("reviewer".into(), "review".into()),
        ];
        let prompt = plan_plan_prompt("ship the release", &teammates);
        assert!(prompt.contains("ship the release"));
        assert!(prompt.contains("Teammates: builder, reviewer"));
        assert!(prompt.contains("builder (impl)"));
        assert!(prompt.contains(PLAN_MODE_HINT));
    }

    #[test]
    fn plan_nudge_includes_plan_hint() {
        assert!(plan_nudge_prompt().contains(PLAN_MODE_HINT));
    }

    #[test]
    fn step_dispatch_prompt_lists_step_numbers() {
        let prompt = step_dispatch_prompt(
            RunId(7),
            &MemberId::new("planner"),
            &[(1, "a".into()), (3, "b".into())],
        );
        assert!(prompt.contains("run-7"));
        assert!(prompt.contains("step #1"));
        assert!(prompt.contains("step #3"));
        assert!(prompt.contains("a"));
        assert!(prompt.contains("b"));
        assert!(prompt.contains(r#"@@team_message {"to":"planner","kind":"reply""#));
        assert!(prompt.contains("checklist does not replace this handoff"));
    }

    #[test]
    fn plan_review_prompt_ends_with_protocol() {
        let prompt = plan_review_prompt("task", "#1 [builder] foo — ok", None);
        assert!(prompt.contains("task"));
        assert!(prompt.contains("#1 [builder] foo"));
        assert!(prompt.contains("before implementation"));
        assert!(prompt.contains("judge the plan itself rather than code changes"));
        assert!(prompt.ends_with(REVIEW_PROTOCOL_HINT) || prompt.contains(REVIEW_PROTOCOL_HINT));
    }

    #[test]
    fn plan_progress_prompt_mentions_builder_handoff() {
        let prompt = plan_progress_prompt(
            "task",
            &["#1 [?] blocked Wire UI — waiting for secret".into()],
            2,
            3,
            Some("a member run failed this round — reassign or adjust the plan"),
        );
        assert!(prompt.contains("waiting for secret"));
        assert!(prompt.contains("configured Builder"));
        assert!(prompt.contains("member run failed"));
        assert!(prompt.contains(PLAN_MODE_HINT));
    }

    #[test]
    fn plan_verify_failure_includes_plan_hint() {
        let prompt = plan_verify_failure_prompt("task", "just check", "clippy boom", 2, 3);
        assert!(prompt.contains("just check"));
        assert!(prompt.contains("clippy boom"));
        assert!(prompt.contains(PLAN_MODE_HINT));
    }

    #[test]
    fn brainstorm_prompts_include_hints() {
        let seed = brainstorm_propose_prompt("topic", 3, 3);
        assert!(seed.contains(BRAINSTORM_PROPOSE_HINT));
        assert!(seed.contains("exactly 3"));
        assert!(!seed.contains("at least 3"));
        assert!(brainstorm_propose_prompt("topic", 3, 4).contains(BRAINSTORM_PROPOSE_HINT));
        assert!(
            brainstorm_build_prompt("topic", 2, 3, 4, "R1-A: seed").contains(BRAINSTORM_BUILD_HINT)
        );
        assert!(
            brainstorm_stretch_prompt("topic", 3, 3, 4, "R2-B: build")
                .contains(BRAINSTORM_STRETCH_HINT)
        );
        assert!(
            brainstorm_vote_prompt("topic", "[R1-A]\n1. seed", 5).contains(BRAINSTORM_VOTE_HINT)
        );
        assert!(
            brainstorm_synthesis_prompt("topic", "ideas", "tally", "ballots")
                .contains(BRAINSTORM_SYNTHESIS_HINT)
        );
    }

    #[test]
    fn manual_step_dispatch_text_matches_expected_wording() {
        let text = manual_step_dispatch_text(RunId(3), "Start", 2, "wire tests");
        assert!(text.contains("Start"));
        assert!(text.contains("step #2"));
        assert!(text.contains("wire tests"));
        assert!(text.contains("@@run_step"));
    }
}
