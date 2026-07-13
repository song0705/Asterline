//! Collaboration-mode prompt templates.
//!
//! These templates are shared with the fake team runner so unit tests and
//! `--fake` stay in sync with the real engine. Keep them pure (no I/O).

use crate::domain::event::WorkflowRunId;

/// Marker + instruction block embedded in every reviewer prompt. The fake team
/// runner keys on this constant, so never inline the text elsewhere.
pub const REVIEW_PROTOCOL_HINT: &str = "End your reply with exactly one control line reporting your verdict:\n\
     @@review {\"verdict\":\"approve\",\"summary\":\"why\"}\n\
     or\n\
     @@review {\"verdict\":\"request_changes\",\"summary\":\"what to fix\",\"items\":[\"...\"]}";

/// Marker for the leader planning prompt; the fake team runner keys on it.
pub const LEAD_PLAN_HINT: &str = "Plan the work as a checklist now: emit one \
`@@workflow_step {\"action\":\"add\",\"owner\":\"<member-id>\",\"title\":\"...\"}` line per step. \
Assign every step an owner from the teammate list. Do not do the work yourself.";

/// Marker for roundtable prompts; the fake team runner keys on it.
pub const ROUNDTABLE_HINT: &str = "Share your own perspective directly in your reply. \
Do not message teammates; the moderator will synthesize.";

/// Marker for the moderator synthesis prompt.
pub const MODERATOR_HINT: &str = "Synthesize the discussion into a single recommendation.";

/// Prompt sent to the builder on the first iteration of a review run.
pub fn review_task_prompt(task: &str) -> String {
    format!(
        "You are the builder in a review workflow.\n\n\
         Task:\n{task}\n\n\
         Implement the task in the working tree and report what you changed. \
         Be concrete about files and decisions so a reviewer can assess the work."
    )
}

/// Prompt sent to the reviewer after a builder turn completes.
pub fn review_prompt(task: &str, builder_display: &str, builder_output: &str) -> String {
    let report = if builder_output.trim().is_empty() {
        "(no report text)"
    } else {
        builder_output
    };
    format!(
        "You are the reviewer in a review workflow.\n\n\
         Task:\n{task}\n\n\
         {builder_display} reported:\n{report}\n\n\
         Inspect the working tree and the report above. Decide whether the work \
         is ready or needs changes.\n\n\
         {REVIEW_PROTOCOL_HINT}"
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
        "You are the builder in a review workflow (iteration {iteration}/{max_iterations}).\n\n\
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

/// Leader planning prompt for Lead mode. `teammates` are `(id, role)` pairs.
pub fn lead_plan_prompt(task: &str, teammates: &[(String, String)]) -> String {
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
         {LEAD_PLAN_HINT}",
        ids.join(", "),
        roles.join(", ")
    )
}

/// Re-ask the leader for an owned checklist after an empty plan turn.
pub fn lead_nudge_prompt() -> String {
    format!(
        "Your previous reply did not produce an actionable owned checklist. \
         Emit owned @@workflow_step add lines now — every step needs an owner.\n\n\
         {LEAD_PLAN_HINT}"
    )
}

/// Per-owner dispatch covering all of their owned todo steps.
pub fn step_dispatch_prompt(run_id: WorkflowRunId, steps: &[(u32, String)]) -> String {
    let list: Vec<String> = steps
        .iter()
        .map(|(n, title)| format!("  - step #{n}: {title}"))
        .collect();
    format!(
        "You own these steps of {run_id}:\n{}\n\n\
         Work through them in the working tree and mark each done with \
         @@workflow_step {{\"action\":\"done\",\"step\":N}} as you finish.",
        list.join("\n")
    )
}

/// Leader prompt after an execution round left unfinished steps.
pub fn lead_progress_prompt(task: &str, unfinished: &[String], iteration: u32, max: u32) -> String {
    let list = if unfinished.is_empty() {
        "(none listed)".to_string()
    } else {
        unfinished.join("\n")
    };
    format!(
        "You are the planning lead (iteration {iteration}/{max}).\n\n\
         Task:\n{task}\n\n\
         Unfinished checklist steps:\n{list}\n\n\
         Re-assess the plan: add, reassign, or clarify steps as needed, then \
         the engine will re-dispatch owners. Do not do the work yourself.\n\n\
         {LEAD_PLAN_HINT}"
    )
}

/// Leader prompt when the reviewer requests changes on a lead run.
pub fn lead_iteration_prompt(
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
         Update the checklist and plan so owners can address the feedback. \
         Do not do the work yourself.\n\n\
         {LEAD_PLAN_HINT}"
    )
}

/// Reviewer prompt after all lead-mode steps are Done.
pub fn lead_review_prompt(task: &str, steps_summary: &str) -> String {
    let summary = if steps_summary.trim().is_empty() {
        "(no steps recorded)"
    } else {
        steps_summary
    };
    format!(
        "You are the reviewer in a lead workflow.\n\n\
         Task:\n{task}\n\n\
         Completed checklist:\n{summary}\n\n\
         Inspect the working tree and the completed steps. Decide whether the \
         work is ready or needs changes.\n\n\
         {REVIEW_PROTOCOL_HINT}"
    )
}

/// Body text for a manual `@owner` step dispatch from the TUI (after `@{owner} `).
pub fn manual_step_dispatch_text(
    run_id: WorkflowRunId,
    instruction: &str,
    number: u32,
    title: &str,
) -> String {
    format!(
        "{instruction} {run_id} step #{number}: {title}. Update the checklist with @@workflow_step as you progress."
    )
}

/// Roundtable participant prompt for a discussion round.
pub fn roundtable_prompt(topic: &str, round: u32, rounds: u32) -> String {
    format!(
        "You are a participant in a roundtable discussion (round {round}/{rounds}).\n\n\
         Topic:\n{topic}\n\n\
         {ROUNDTABLE_HINT}"
    )
}

/// Roundtable participant prompt with digests of other participants' prior replies.
pub fn roundtable_digest_prompt(topic: &str, round: u32, rounds: u32, others: &str) -> String {
    let others = if others.trim().is_empty() {
        "(no other replies yet)"
    } else {
        others
    };
    format!(
        "You are a participant in a roundtable discussion (round {round}/{rounds}).\n\n\
         Topic:\n{topic}\n\n\
         Other participants said last round:\n{others}\n\n\
         Respond to the discussion. {ROUNDTABLE_HINT}"
    )
}

/// Moderator synthesis prompt after all discussion rounds.
pub fn moderator_prompt(topic: &str, transcript: &str) -> String {
    let transcript = if transcript.trim().is_empty() {
        "(no discussion recorded)"
    } else {
        transcript
    };
    format!(
        "You are the moderator of a roundtable discussion.\n\n\
         Topic:\n{topic}\n\n\
         Discussion:\n{transcript}\n\n\
         {MODERATOR_HINT}"
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
        let prompt = review_prompt("ship feature", "Builder", "implemented foo");
        assert!(prompt.contains("ship feature"));
        assert!(prompt.contains("Builder"));
        assert!(prompt.contains("implemented foo"));
        assert!(prompt.contains(REVIEW_PROTOCOL_HINT));
        assert!(prompt.contains("@@review"));
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
    fn lead_plan_prompt_lists_teammates_and_hint() {
        let teammates = vec![
            ("builder".into(), "impl".into()),
            ("reviewer".into(), "review".into()),
        ];
        let prompt = lead_plan_prompt("ship the release", &teammates);
        assert!(prompt.contains("ship the release"));
        assert!(prompt.contains("Teammates: builder, reviewer"));
        assert!(prompt.contains("builder (impl)"));
        assert!(prompt.contains(LEAD_PLAN_HINT));
    }

    #[test]
    fn lead_nudge_includes_plan_hint() {
        assert!(lead_nudge_prompt().contains(LEAD_PLAN_HINT));
    }

    #[test]
    fn step_dispatch_prompt_lists_step_numbers() {
        let prompt = step_dispatch_prompt(WorkflowRunId(7), &[(1, "a".into()), (3, "b".into())]);
        assert!(prompt.contains("run-7"));
        assert!(prompt.contains("step #1"));
        assert!(prompt.contains("step #3"));
        assert!(prompt.contains("a"));
        assert!(prompt.contains("b"));
    }

    #[test]
    fn lead_review_prompt_ends_with_protocol() {
        let prompt = lead_review_prompt("task", "#1 [builder] foo — ok");
        assert!(prompt.contains("task"));
        assert!(prompt.contains("#1 [builder] foo"));
        assert!(prompt.ends_with(REVIEW_PROTOCOL_HINT) || prompt.contains(REVIEW_PROTOCOL_HINT));
    }

    #[test]
    fn roundtable_prompts_include_hints() {
        assert!(roundtable_prompt("topic", 1, 2).contains(ROUNDTABLE_HINT));
        assert!(roundtable_digest_prompt("topic", 2, 2, "A: hi").contains(ROUNDTABLE_HINT));
        assert!(moderator_prompt("topic", "A: hi").contains(MODERATOR_HINT));
    }

    #[test]
    fn manual_step_dispatch_text_matches_legacy_wording() {
        let text = manual_step_dispatch_text(WorkflowRunId(3), "Start", 2, "wire tests");
        assert!(text.contains("Start"));
        assert!(text.contains("step #2"));
        assert!(text.contains("wire tests"));
        assert!(text.contains("@@workflow_step"));
    }
}
