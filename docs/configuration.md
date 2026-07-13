# Asterline configuration and operations

This page covers team files, runtime data, permissions, CLI options, agent
coordination, and troubleshooting. For the product overview, return to the
[main README](../README.md). For interactive controls, see the
[command reference](commands.md).

## How Asterline resolves a team

At startup, Asterline chooses a roster in this order:

1. `--team <PATH>` loads that JSON file.
2. `<workspace>/.asterline/team.json` is reused unless `--pick-team` is set.
3. Supported backend executables are detected on `PATH` and the Team builder
   opens.
4. If no saved team and no supported executable exists, startup stops with a
   setup message.

Use `/team` to modify the live roster. Press `s` to apply the changes, replace
member runners, and save the updated team.

## Team file

```json
{
  "name": "product-team",
  "workspace": "/path/to/project",
  "default_target": { "member": "builder" },
  "max_auto_relays": 6,
  "members": [
    {
      "display_name": "Builder",
      "backend": "codex",
      "role": "implementation",
      "sandbox": "workspace-write",
      "effort": "high"
    },
    {
      "display_name": "Reviewer",
      "backend": "claude",
      "role": "review and risk analysis",
      "permission_mode": "plan",
      "effort": "medium"
    },
    {
      "display_name": "Grok",
      "backend": "grok",
      "role": "implementation",
      "sandbox": "workspace-write",
      "permission_mode": "auto"
    }
  ],
  "modes": {
    "review": {
      "builder": "builder",
      "reviewer": "reviewer",
      "max_iterations": 3
    },
    "lead": {
      "leader": "builder",
      "reviewer": "reviewer",
      "max_iterations": 3,
      "auto_verify": true
    },
    "roundtable": {
      "participants": ["builder", "reviewer", "grok"],
      "moderator": "reviewer",
      "rounds": 2
    }
  },
  "approvals": {
    "gate": ["git", "shell", "file"],
    "apply_to": ["user", "relay", "mode"]
  }
}
```

`id` is optional. Asterline derives a stable handle from `display_name`, so
`QA Lead` becomes `qa-lead`. Set `id` only when a custom `@handle` is required.

### Team fields

| Field             | Required | Meaning                                         |
| ----------------- | -------- | ----------------------------------------------- |
| `name`            | Yes      | Team name shown in Asterline                    |
| `workspace`       | Yes      | Default working directory                       |
| `members`         | Yes      | Non-empty member list                           |
| `default_target`  | No       | `{"member":"id"}`, `"all"`, or the first member |
| `max_auto_relays` | No       | Automatic teammate handoff limit; default `6`   |
| `modes`           | No       | Role bindings and budgets for collab modes      |
| `approvals`       | No       | Approval-gate categories and surfaces           |

### Collaboration modes (`modes`)

Optional bindings for `/review`, `/plan` (`/lead`), and `/roundtable`. When a
field is omitted, Asterline derives it from member roles and `default_target`
(builder ≈ default target or first non-reviewer; reviewer ≈ role contains
"review"; leader/moderator ≈ role contains "plan" or "lead"; participants =
full roster). Defaults for budgets: `max_iterations = 3`, `rounds = 2`,
`auto_verify = true`.

| Field            | Modes              | Meaning                                            |
| ---------------- | ------------------ | -------------------------------------------------- |
| `builder`        | review, lead       | Member who implements changes                      |
| `reviewer`       | review, lead       | Member who emits `@@review` verdicts               |
| `leader`         | lead               | Member who writes the owned checklist              |
| `moderator`      | roundtable         | Optional synthesizer after discussion rounds       |
| `participants`   | roundtable         | Roster for discussion turns                        |
| `max_iterations` | review, lead       | Builder↔reviewer loop budget before blocking       |
| `rounds`         | roundtable         | Number of full discussion rounds                   |
| `auto_verify`    | lead (and similar) | Run suggested verification after approve when true |

Inline `/review builder=@x max_iterations=5 …` overrides beat `team.json`, which
beats role derivation.

### Approvals (`approvals`)

Policy for the approval gate. With no `approvals` section, all built-in
categories and all surfaces are enabled.

| Field      | Meaning                                                                 |
| ---------- | ----------------------------------------------------------------------- |
| `gate`     | Built-in categories to keep: `git`, `shell`, `file`. Omit for all three |
| `keywords` | Custom categories: name → keyword list (case-insensitive match)         |
| `apply_to` | Surfaces: `user`, `relay`, `mode`. Omit for all surfaces                |

`user` is ordinary user messages; `relay` is agent-to-agent routes; `mode` is
engine dispatches for collaboration modes. Set `ASTERLINE_NO_BELL=1` to disable
terminal BEL/OSC 9 notifications on approval, paused route, blocked run, and
member error events.

See [approvals and tool-level control](approvals.md) for how this gate relates
to backend-native sandbox and permission enforcement.

### Member fields

| Field             | Required                    | Meaning                                                 |
| ----------------- | --------------------------- | ------------------------------------------------------- |
| `display_name`    | Yes unless `id` supplies it | Visible member name                                     |
| `backend`         | Yes                         | `codex`, `claude`, `grok`, or `agy`                     |
| `role`            | Yes                         | Free-form team responsibility                           |
| `id`              | No                          | Stable handle used by `@member` and routing             |
| `cwd`             | No                          | Member-specific working directory                       |
| `model`           | No                          | Backend model; omitted means backend default            |
| `effort`          | No                          | `low`, `medium`, `high`, `xhigh`, or `max`              |
| `system_prompt`   | No                          | Additional member instructions                          |
| `sandbox`         | No                          | `read-only`, `workspace-write`, or `danger-full-access` |
| `permission_mode` | No                          | Backend-native permission mode                          |
| `allowed_tools`   | No                          | Backend-specific tool allowlist                         |
| `session_policy`  | No                          | `resume` (default) or `fresh`                           |

Permission modes, sandbox mappings, and allowed-tool behavior depend on the
backend. Do not assume a field has the same effect across all four CLIs.

Legacy saved entries using `backend: "gemini"` are migrated to `agy` when the
workspace team file is loaded.

## Backend setting support

This table describes what the current Asterline adapters actually pass to each
CLI. It is intentionally narrower than the union of fields accepted by the
Team editor.

| Setting                | Codex                                                                          | Claude                                      | Grok                                          | Agy                                                               |
| ---------------------- | ------------------------------------------------------------------------------ | ------------------------------------------- | --------------------------------------------- | ----------------------------------------------------------------- |
| `cwd`                  | Process cwd and `-C` on a fresh session                                        | Process cwd                                 | Process cwd                                   | Process cwd                                                       |
| `model`                | `-m`                                                                           | `--model`                                   | `--model`                                     | `--model`                                                         |
| `effort`               | `model_reasoning_effort`; values above `high` clamp to `high`                  | `--effort`                                  | `--reasoning-effort`                          | Not passed                                                        |
| `sandbox`              | `-s` on a fresh session; resumed session restores its own sandbox              | Not passed                                  | `--sandbox` with an Asterline profile mapping | `--sandbox` unless configured as `danger-full-access`             |
| `permission_mode`      | Not passed                                                                     | `--permission-mode` (omitted for `default`) | `--permission-mode`                           | Only `bypassPermissions` maps to `--dangerously-skip-permissions` |
| `allowed_tools`        | Not passed                                                                     | `--allowed-tools`                           | `--tools`                                     | Not passed                                                        |
| custom `system_prompt` | Not passed as a backend system prompt; Asterline prepends current team context | `--append-system-prompt`                    | `--rules`                                     | Prepended to stdin text                                           |
| `session_policy`       | Resume or fresh                                                                | Resume or fresh                             | Resume or fresh                               | Resume or fresh conversation                                      |

For Claude and Grok, choose only permission modes accepted by the installed CLI
version. Asterline serializes the configured value but does not negotiate
vendor-version compatibility before launch. Recent Claude CLIs no longer list
`default` as a `--permission-mode` choice; Asterline omits the flag when the
configured mode is `default` so the CLI default applies.

## Model discovery

Model choices are resolved in each member's effective working directory:

| Backend | Source                                                          |
| ------- | --------------------------------------------------------------- |
| Codex   | `codex debug models`                                            |
| Claude  | documented aliases plus project/user `availableModels` settings |
| Grok    | `grok models`                                                   |
| Agy     | `agy models`                                                    |

Open the member's `model` field and press `Enter`. The first press may start a
background query; press `Enter` again after the loading notice. The picker
always includes `default` and preserves a currently configured custom value.
Press `e` on the field to enter a model manually.

## Runtime data

The default workspace state is:

```text
<workspace>/.asterline/
├── team.json
└── asterline.sqlite3
```

SQLite stores conversations, tool events, teammate routes, raw backend events,
logs, approvals, session identifiers, workflow runs, checklists, timelines, and
verification outcomes.

Protect this directory like any other development transcript. Most repositories
should ignore it:

```gitignore
.asterline/
```

`/new` creates a clean conversation and new backend sessions while retaining
older database records. `--no-restore` skips startup replay without deleting
data. `--db <PATH>` moves the database outside the workspace.

## Terminal color theme

Asterline uses separate backend identity palettes for dark and light terminal
backgrounds. By default it reads the conventional `COLORFGBG` value and falls
back to the dark palette when the terminal does not expose its background.

Set `ASTERLINE_THEME` when automatic detection does not match the terminal:

```bash
ASTERLINE_THEME=dark asterline
ASTERLINE_THEME=light asterline
```

`auto` restores detection. Backend identity is also communicated by member
names, backend labels, and continuous conversation rails, so color is not the
only cue.

## Permissions and safety

Asterline launches backend CLIs locally and inherits their credentials,
environment variables, filesystem access, and network access. It does not
provide a security boundary around a backend process.

Backend-native permission and sandbox settings still apply. Asterline also
places requests it classifies as risky behind its own approval gate. Use
`/approve` or `/reject` to resolve the first pending request.

`--debug` disables the Asterline approval gate. It does not add a sandbox and
should only be used in a controlled development environment.

The `danger-full-access` sandbox and bypass-style permission modes should be
treated as explicit trust decisions. Never assume a team role or model name
limits what the underlying process can access.

## Agent-to-agent coordination

Asterline creates `.agents/skills/asterline-team/SKILL.md` when it is missing
and injects a compact skill hint into each member's system instructions. The
full protocol remains in the workspace instead of being repeated in every
prompt.

### Teammate messages

```text
@@team_message {"to":"reviewer","body":"implementation is ready for review"}
@@team_message {"to":["builder","reviewer"],"body":"align on the API"}
@@team_message {"to":"all","body":"report status"}
```

Asterline removes valid envelopes from the visible response, renders the
handoff, persists it, and delivers the body to the target members. Automatic
handoffs are capped by `max_auto_relays`; `/retry` resumes a paused route.

### Roster requests

An agent may request a missing specialty:

```text
@@team_member {"display_name":"QA","backend":"codex","role":"tests"}
```

Asterline validates duplicate IDs and names, starts the runner, saves the
roster, and broadcasts the updated team. Agent envelopes can add members but
cannot delete them; deletion remains a `/team` action.

### Workflow checklist updates

During an active workflow turn, an agent can add, update, assign, rename, or
remove checklist steps:

```text
@@workflow_step {"action":"add","owner":"builder","title":"Write tests"}
@@workflow_step {"action":"doing","step":1,"note":"Implementing edge cases"}
@@workflow_step {"action":"done","step":1,"note":"Tests pass"}
@@workflow_step {"action":"block","step":2,"note":"Waiting for credentials"}
@@workflow_step {"action":"assign","step":2,"owner":"reviewer"}
```

These updates appear in `/runs` and are recorded in the run timeline.

## CLI options

| Option               | Description                                          |
| -------------------- | ---------------------------------------------------- |
| `--team <PATH>`      | Load a JSON team and skip the builder                |
| `--pick-team`        | Ignore the saved roster and open the builder         |
| `--workspace <PATH>` | Set the workspace; defaults to the current directory |
| `--db <PATH>`        | Set the SQLite database path                         |
| `--no-restore`       | Do not replay persisted chat on startup              |
| `--debug`            | Disable Asterline's approval gate                    |
| `--fake`             | Use offline fake agents instead of backend CLIs      |
| `--banner`           | Print a compact startup banner before the TUI        |
| `-h`, `--help`       | Print command-line help                              |

Examples:

```bash
asterline --workspace ~/code/api
asterline --pick-team
asterline --team ./team.json --db ~/.local/share/asterline/api.sqlite3
asterline --fake --no-restore
```

## Troubleshooting

### No supported backend was found

Confirm that at least one of `codex`, `claude`, `grok`, or `agy` is installed,
authenticated, and on `PATH`. Alternatively, pass a valid file with `--team`.

### The model picker only shows `default`

Press `Enter` on the `model` field to start discovery. If a loading notice
appears, wait briefly and press `Enter` again. Press `e` to enter a model name
manually.

### The wrong roster opens

Run `asterline --pick-team` to rebuild the saved roster, or use `/team` and
press `s` to apply changes.

### Start without the previous transcript

Use `asterline --no-restore`. This skips replay but does not delete SQLite data.
Use `/new` for a clean conversation with new backend sessions.

### Test without invoking backend CLIs

Run `asterline --fake`. Fake mode exercises the runtime and TUI without calling
Codex, Claude, Grok, or Agy.

### Keyboard input is malformed after leaving an attached CLI

Install the current Asterline build first; it restores terminal keyboard state
when suspending, resuming, and exiting, and disables enhanced keyboard reporting
in VS Code and Cursor terminals. If an older build left the terminal protocol
enabled, run this once in the affected shell:

```bash
printf '\033[=0u'
```

Then start the newly installed binary in a fresh terminal session.
