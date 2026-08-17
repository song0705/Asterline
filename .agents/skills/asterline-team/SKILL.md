---
name: asterline-team
description: Use when acting as an Asterline team member who actually needs to message teammates, coordinate explicitly collaborative work, update run steps, or request that Asterline add a teammate to the live roster.
metadata:
  version: 20
---
<!-- managed-by: asterline (auto-upgraded; local edits will be overwritten) -->

# Asterline Team Protocol

Asterline reads special control lines from your final output. Put each control line on its own line with valid single-line JSON. Parsed control lines are removed from the visible chat.

## Roster And Messaging Policy

Read `.asterline/roster.md` for the current default target and every member's id, role, backend, and status. Asterline rewrites that file when the team or a status changes.

Do not send `@@team_message` merely because teammates are listed, another member has a relevant role, or the task involves search, research, review, or planning. Message only when the user asks for collaboration, the active run requires a handoff, or you are blocked on a specific teammate. If you can finish the request yourself, do not emit `@@team_message`.

## Every Received Message Must Be Answered

When you receive an Asterline relay from another member, remember the sender. Before ending your turn, you MUST emit exactly one `@@team_message` back to that sender. This rule applies in normal chat and every collaboration mode.

```text
@@team_message {"to":"original-sender","kind":"reply","body":"The full deliverable, decision, question, or blocker"}
```

Visible response text, checklist updates, and tool output do not count as delivering to the sender. Writing the plan, review, or patch "for the user" is not delivery. The teammate who asked cannot see your user-facing prose; they only see this `@@team_message` body.

If the sender asked you to plan, design, review, implement, or investigate, the `body` MUST contain the actual artifact: the full plan, the field list, the verdict, the findings. A pointer such as "方案已写给用户" or "see the chat" is not a reply.

Put the deliverable in `body` even when it is long. Escape newlines as `\n` so the control line stays one line of JSON.

For delegated checklist work, update every owned run step to `done` or `block`, then send the reply with results, changed files, checks run, and blockers.

To stop acknowledgement loops, a message with `"kind":"reply"` does not require another reply unless its body contains a new question, request, correction, or blocker requiring action.

## Message Teammates

When the messaging policy above permits it, send necessary work or questions to one or more teammates:

```text
@@team_message {"to":"reviewer","body":"Please review the parser changes."}
@@team_message {"to":["builder","reviewer"],"body":"Let's agree on the data model."}
@@team_message {"to":"all","body":"Status update?"}
```

`to` accepts a member id, display name, array of ids/names, or `all`.

## Add A Teammate

Only emit this when the current prompt says the roster may grow. In
`/mode team` the default is the current roster; do not emit `@@team_member`
when the prompt says the roster is locked.

When adding is allowed and the roster lacks a needed specialty, request a new teammate:

```text
@@team_member {"display_name":"QA","backend":"codex","role":"tests"}
```

Required fields: `display_name`, `backend`, `role`.
Optional fields: `id`, `model`, `effort`, `cwd`, `sandbox`, `permission_mode`, `allowed_tools`, `session_policy`, `session_id`, `system_prompt`.

Omitted `sandbox` and `permission_mode` use Asterline's write defaults for
that backend (Agy: `accept-edits` with sandbox off). Set them only to
override. Do not send `plan` unless the user asked for a plan-first seat.

Rules:
- `backend` must be `codex`, `claude`, `grok`, or `agy`.
- `effort` may be `low`, `medium`, `high`, `xhigh`, or `max`.
- Only adding is supported; do not request deletes or overwrites.
- Asterline derives a stable lowercase id from `display_name`; set `id` only when you need a custom handle.
- Avoid ids or display names already in the roster.

## Review Verdicts

When asked to review work, you MUST end your reply with exactly one control line that reports your verdict:

```text
@@review {"verdict":"approve","summary":"Parser covers the edge cases and tests pass."}
@@review {"verdict":"request_changes","summary":"Needs fixes before merge","items":["Add a regression test for empty input","Rename helper to match module style"]}
```

- `verdict` is required and must be `approve` or `request_changes`.
- `summary` is optional free-text explaining the decision.
- `items` is optional; use it for a short bullet list of concrete changes when requesting work.

## Brainstorm Generation and Voting

For `/mode brainstorm`, follow the deployed `$asterline-brainstorm` skill and
the current phase prompt. That separate skill defines the structured
`@@brainstorm_card` schema, judgment-free generation waves, private
`@@brainstorm_vote` ballot, and final synthesis. Do not substitute a free-form
card format.

## Update Run Steps

During `/mode plan` or `/continue` work, keep the run checklist current:

```text
@@run_step {"action":"add","owner":"builder","title":"Write parser tests"}
@@run_step {"action":"doing","step":1,"note":"Implementing lexer edge cases"}
@@run_step {"action":"done","step":1,"note":"Covered lexer edge cases"}
@@run_step {"action":"block","step":2,"note":"Waiting for API credentials"}
@@run_step {"action":"assign","step":2,"owner":"reviewer"}
@@run_step {"action":"unassign","step":2}
@@run_step {"action":"rename","step":2,"title":"Document API credential setup"}
@@run_step {"action":"remove","step":3}
```

Use `add` for new checklist items. Use `todo`, `doing`, `done`, or `block` with
the 1-based step number shown in `/runs` to update an existing item. Use
`assign` with a member handle to set ownership, `unassign` to clear it,
`rename` to fix a step title, and `remove` only for duplicate or obsolete steps.

## Waiting For User Approval

By default Asterline auto-approves Codex tool asks. If the user enabled
manual approvals, a live tool (especially a Codex command, file change, or
permission escalation) pauses until they agree in the card above the
composer. A Plan run with manual execute confirmation waits the same way
before the Builder is sent the checklist.

When you are waiting:

- Do not retry the same tool, command, or handoff.
- Do not assume the action succeeded or failed.
- Do not send another `@@team_message` or `@@team_member` just to nudge.
- Stop and wait for the next prompt. After approval, the tool result or next
  dispatch arrives normally. After rejection, treat the action as denied and
  continue another way.

Everything else you write is shown to the user.
