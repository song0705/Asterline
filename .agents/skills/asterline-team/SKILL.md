---
name: asterline-team
description: Use when acting as an Asterline team member who actually needs to message teammates, coordinate explicitly collaborative work, update workflow steps, or request that Asterline add a teammate to the live roster. A visible roster only lists available members and does not by itself trigger messaging or delegation.
metadata:
  version: 5
---
<!-- managed-by: asterline (auto-upgraded; local edits will be overwritten) -->

# Asterline Team Protocol

Asterline reads special control lines from your final output. Put each control line on its own line with valid single-line JSON. Parsed control lines are removed from the visible chat.

## Roster And Messaging Policy

The roster is an availability directory, not an assignment or an instruction to contact anyone. Work independently by default.

Do not send a teammate message merely because teammates are listed, because another member has a relevant role, or because the task involves search, research, review, or planning. Send a message only when at least one of these conditions holds:

- The user explicitly requests collaboration, delegation, or a teammate's input.
- The active Asterline workflow explicitly requires a handoff or coordinated multi-member work.
- You are blocked on information or action that a specific teammate must provide and you cannot complete the task independently.

If you can complete the request yourself, do not emit `@@team_message`. Do not send unsolicited status updates or FYI messages.

## Message Teammates

When the messaging policy above permits it, send necessary work or questions to one or more teammates:

```text
@@team_message {"to":"reviewer","body":"Please review the parser changes."}
@@team_message {"to":["builder","reviewer"],"body":"Let's agree on the data model."}
@@team_message {"to":"all","body":"Status update?"}
```

`to` accepts a member id, display name, array of ids/names, or `all`.

## Add A Teammate

When the roster lacks a needed specialty, request a new teammate:

```text
@@team_member {"display_name":"QA","backend":"codex","role":"tests"}
```

Required fields: `display_name`, `backend`, `role`.
Optional fields: `id`, `model`, `effort`, `cwd`, `sandbox`, `permission_mode`, `allowed_tools`, `session_policy`, `session_id`, `system_prompt`.

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

## Update Workflow Steps

During `/mode plan` or `/continue` work, keep the run checklist current:

```text
@@workflow_step {"action":"add","owner":"builder","title":"Write parser tests"}
@@workflow_step {"action":"doing","step":1,"note":"Implementing lexer edge cases"}
@@workflow_step {"action":"done","step":1,"note":"Covered lexer edge cases"}
@@workflow_step {"action":"block","step":2,"note":"Waiting for API credentials"}
@@workflow_step {"action":"assign","step":2,"owner":"reviewer"}
@@workflow_step {"action":"unassign","step":2}
@@workflow_step {"action":"rename","step":2,"title":"Document API credential setup"}
@@workflow_step {"action":"remove","step":3}
```

Use `add` for new checklist items. Use `todo`, `doing`, `done`, or `block` with
the 1-based step number shown in `/runs` to update an existing item. Use
`assign` with a member handle to set ownership, `unassign` to clear it,
`rename` to fix a step title, and `remove` only for duplicate or obsolete steps.

Everything else you write is shown to the user.
