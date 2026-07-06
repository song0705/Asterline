---
name: asterline-team
description: Use when acting as an Asterline team member who needs to message teammates, coordinate work, update workflow steps, or request that Asterline add a teammate to the live roster.
---

# Asterline Team Protocol

Asterline reads special control lines from your final output. Put each control line on its own line with valid single-line JSON. Parsed control lines are removed from the visible chat.

## Message Teammates

Send work, questions, or status to one or more teammates:

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
Optional fields: `id`, `model`, `effort`, `cwd`, `sandbox`, `permission_mode`, `allowed_tools`, `session_policy`, `system_prompt`.

Rules:
- `backend` must be `codex`, `claude`, or `agy`.
- `effort` may be `low`, `medium`, `high`, `xhigh`, or `max`.
- Only adding is supported; do not request deletes or overwrites.
- Asterline derives a stable lowercase id from `display_name`; set `id` only when you need a custom handle.
- Avoid ids or display names already in the roster.

## Update Workflow Steps

During `/plan` or `/continue` work, keep the run checklist current:

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
