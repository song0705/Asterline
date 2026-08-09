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

Use `/team` to modify the live roster. Opening it refreshes installed Agent
CLIs and preloads their model and reasoning-effort catalogs. Press `s` to
apply the changes, replace member runners, and save the updated team.

### Platform paths and backend history

On Unix, Asterline uses `HOME` for user-level configuration; on Windows it
prefers `USERPROFILE`. Each platform accepts the other variable as a fallback.
`CODEX_HOME` overrides the default `.codex` directory for both session picking,
post-attach transcript import, and global Codex skill/plugin discovery.

Default history roots are `<Codex home>/sessions`, `<user home>/.claude/projects`,
and `<user home>/.grok/sessions`. Windows project matching treats drive-letter
case and `/` versus `\` separators as equivalent. Backend discovery uses the
platform `PATH`; on Windows it also honors `PATHEXT` and launches the resolved
`.exe`, `.cmd`, or `.bat` path. When leaving an attached CLI, use `Ctrl+D` on
Unix or `Ctrl+Z` followed by `Enter` on Windows (or type `/exit`).

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
    "plan": {
      "leader": "builder",
      "reviewer": "reviewer",
      "max_iterations": 3,
      "auto_verify": true
    },
    "brainstorm": {
      "participants": ["builder", "reviewer", "grok"],
      "generation_rounds": 3,
      "ideas_per_round": 4
    },
    "team": {
      "coordinator": "builder"
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
Explicit IDs may contain only ASCII letters, digits, `-`, and `_`. IDs and
display names must be unique for routing, and `all` is reserved for broadcast
targets (case-insensitive).

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

Optional bindings for `/mode review`, `/mode plan`, `/mode brainstorm`, and `/mode team`. When a
field is omitted, Asterline derives it from member roles and `default_target`
(builder ≈ default target or first non-reviewer; reviewer ≈ role contains
"review"; leader ≈ role contains "plan" or "lead", else first
participant; participants = full roster). Defaults for budgets:
`max_iterations = 3`, `generation_rounds = 3`, `ideas_per_round = 4`,
`auto_verify = true`.
Brainstorm requires at least two distinct resolved participants; repeating an
ID or referring to the same member once by ID and once by display name is
rejected.

Brainstorm separates divergence from convergence. The first wave collects independent
seeds; later waves expose each participant to a rotating anonymous peer sample
for building, combining, mutating, and stretching ideas. Earlier contributions
remain in the persisted idea set. After the final generation wave, every
participant privately ranks labeled ideas. Asterline aggregates ballots with a
deterministic Borda tally and dispatches one neutral ranked synthesis.
Each generated idea is extracted from an `@@brainstorm_card` envelope and
assigned a canonical candidate ID, so ballots do not depend on model-specific
Markdown numbering.

| Field               | Mode             | Meaning                                                     |
| ------------------- | ---------------- | ----------------------------------------------------------- |
| `builder`           | review           | Member who implements changes                               |
| `reviewer`          | review/plan      | Member who emits `@@review` verdicts                        |
| `leader`            | plan             | Member who writes the owned checklist                       |
| `participants`      | brainstorm       | Roster for all generation waves                             |
| `generation_rounds` | brainstorm       | Seed/build/stretch wave budget (default 3, minimum 2)       |
| `ideas_per_round`   | brainstorm       | Requested idea cards per member/wave (default 4, minimum 3) |
| `coordinator`       | team             | Member who coordinates the whole-team run                   |
| `max_iterations`    | review/plan/team | Loop budget before blocking or failing verify (def 3)       |
| `auto_verify`       | review/plan/team | Run verification after approval/finish (default true)       |
| `verify_command`    | review/plan/team | Explicit auto-verify shell command (else heuristic)         |

Each mode has its own configuration shape; unrelated fields are rejected by
Serde instead of being silently accepted and ignored.

### Approvals (`approvals`)

Policy for the approval gate. With no `approvals` section, all built-in
categories and all surfaces are enabled.

| Field      | Meaning                                                                 |
| ---------- | ----------------------------------------------------------------------- |
| `gate`     | Built-in categories to keep: `git`, `shell`, `file`. Omit for all three |
| `keywords` | Custom categories: name → keyword list (case-insensitive match)         |
| `apply_to` | Surfaces: `user`, `relay`, `mode`. Omit for all surfaces                |

`user` is ordinary user messages; `relay` is agent-to-agent routes and
agent-requested roster additions; `mode` is engine dispatches for collaboration
modes. Roster additions are always held when the `relay` surface is enabled,
independent of keyword categories. Set `ASTERLINE_NO_BELL=1` to disable terminal
BEL/OSC 9 notifications on approval, paused route, blocked run, and member error
events.

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
| `model`           | No                          | Omitted delegates to the backend CLI                    |
| `effort`          | No                          | Chosen with the model; `default` delegates to the CLI   |
| `system_prompt`   | No                          | Additional member instructions                          |
| `sandbox`         | No                          | `read-only`, `workspace-write`, or `danger-full-access` |
| `permission_mode` | No                          | Backend-native permission mode                          |
| `allowed_tools`   | No                          | Backend-specific tool allowlist                         |
| `session_policy`  | No                          | `resume` (default) or `fresh`                           |
| `session_id`      | No                          | Native CLI session/conversation ID to resume            |

Both policies pin and reuse the backend session ID after the first call.
`resume` keeps an existing persisted ID when available. Switching a member to
`fresh` discards its old ID once, so the next call creates a new native CLI
conversation; that newly discovered ID is then reused for subsequent calls.
`fresh` does not create a separate conversation for every turn.

Set `session_id` to bind a team member to a specific conversation from its
native CLI history. Asterline passes it through the backend's native resume
mechanism (`codex exec resume`, `claude --resume`, `grok --resume`, or Agy
`--conversation`). In the Team editor, use `default` to clear the explicit ID.

Permission modes, sandbox mappings, and allowed-tool behavior depend on the
backend. Do not assume a field has the same effect across all four CLIs.

## Backend setting support

This table describes what the current Asterline adapters actually pass to each
CLI. It is intentionally narrower than the union of fields accepted by the
Team editor.

| Setting                | Codex                                                              | Claude                                      | Grok ACP                                                        | Agy                                                                          |
| ---------------------- | ------------------------------------------------------------------ | ------------------------------------------- | --------------------------------------------------------------- | ---------------------------------------------------------------------------- |
| `cwd`                  | Process cwd and `-C` on a fresh session                            | Process cwd                                 | ACP session `cwd`                                               | Process cwd plus `--add-dir`; prompt identifies the project workspace        |
| `model`                | `-m`                                                               | `--model`                                   | Agent `--model`                                                 | `--model`                                                                    |
| `effort`               | `model_reasoning_effort`; picker follows model metadata            | `--effort` (through `max`)                  | Agent `--reasoning-effort`                                      | `--effort` (`low`, `medium`, or `high`)                                      |
| `sandbox`              | `-s` on fresh sessions; resumed sessions restore their own sandbox | Not passed                                  | Top-level `--sandbox` with an Asterline profile mapping         | `--sandbox` unless configured as `danger-full-access`                        |
| `permission_mode`      | Not passed                                                         | `--permission-mode` (omitted for `default`) | Top-level mode plus ACP permission responses                    | `acceptEdits` → `--mode accept-edits`; `plan` → `--mode plan`; bypass → flag |
| `allowed_tools`        | Not passed                                                         | `--allowed-tools`                           | Added to ACP session rules; not a hard protocol-level allowlist | Not passed                                                                   |
| custom `system_prompt` | `-c developer_instructions=…`                                      | `--append-system-prompt`                    | ACP session `rules`                                             | Prepended to the print prompt                                                |
| `session_policy`       | Resume or fresh                                                    | Resume or fresh                             | ACP `session/load` or `session/new`                             | Resume or fresh conversation                                                 |
| `session_id`           | `codex exec resume <id>`                                           | `claude --resume <id>`                      | ACP `session/load`                                              | `agy --conversation <id>`                                                    |

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
| Grok    | `grok --no-auto-update models`                                  |
| Agy     | `agy models`                                                    |

The Team builder and `/team` editor start background discovery as soon as they
open. The Agent field lists all four supported CLIs, includes installation
status and discovered model/effort summaries, and disables missing CLIs. Open
the member's `model` field to browse the already-loading catalog. Type to
filter by display name, model ID, or description, use `↑`/`↓` for the model,
and use `←`/`→` for that model's effort. `Enter` applies both values. The
picker selects the CLI-marked default when discovery returns models or,
if none is marked, the first discovered model. It shows only actual model
entries in that case. `default` is available only when discovery returns no
models. Press `e` on the field to enter a model ID manually.

Reasoning effort is model-aware when discovery returns capability metadata.
Unsupported levels are omitted and the model's reported default effort is
shown directly when available. Agy exposes the three levels its CLI accepts:
`low`, `medium`, and `high`.

## Streaming and resource limits

Asterline applies explicit limits at process, adapter, runtime, import, and UI
boundaries so a malformed or excessively verbose backend cannot grow memory
without bound:

- JSON protocol records are limited to 8 MiB; stderr records are limited to
  1 MiB.
- A visible assistant message is limited to 4 MiB and one tool detail to
  1 MiB. PTY output retains at most 4 MiB of unread data.
- Verification output retains at most 1 MiB, preserving useful data from both
  the beginning and end of the stream.
- The product runtime-to-TUI queue holds 2,048 events and applies backpressure.
  Abort and shutdown use a separate control channel so they remain responsive
  while stream traffic is saturated.
- Imported JSONL records are limited to 8 MiB and an imported message to
  1 MiB.

When content is shortened, Asterline inserts an explicit truncation marker
instead of presenting the shortened value as complete.

## Runtime data

The default workspace state is:

```text
<workspace>/.asterline/
├── team.json
└── asterline.sqlite3
```

SQLite stores conversations, tool events, teammate routes, raw backend events,
logs, approvals, session identifiers, runs, checklists, timelines, and
verification outcomes.

Protect this directory like any other development transcript. Most repositories
should ignore it:

```gitignore
.asterline/
```

`/new` creates a clean conversation in normal mode and new backend sessions
while retaining older database records. It is rejected while members, runs, or
verification are active; use `/abort` and wait for cancellation first.
`--no-restore` skips startup replay without deleting data. `--db <PATH>` moves
the database outside the workspace.

`/resume` opens the saved-chat picker. Restoring a chat also restores the
roster, full member configuration, and each member's native backend session ID
that belonged to that chat.

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

It also creates `.agents/skills/asterline-brainstorm/SKILL.md` when missing.
Brainstorm mode loads that file for every generation, vote, and synthesis
dispatch. Existing copies are never upgraded or overwritten, so deployments
can customize the method while retaining the `@@brainstorm_card` and
`@@brainstorm_vote` schemas.

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

### Run checklist updates

During an active run, an agent can add, update, assign, rename, or
remove checklist steps:

```text
@@run_step {"action":"add","owner":"builder","title":"Write tests"}
@@run_step {"action":"doing","step":1,"note":"Implementing edge cases"}
@@run_step {"action":"done","step":1,"note":"Tests pass"}
@@run_step {"action":"block","step":2,"note":"Waiting for credentials"}
@@run_step {"action":"assign","step":2,"owner":"reviewer"}
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

Wait for the automatic catalog-loading notice in the Team editor to finish,
then reopen the `model` field. If discovery failed, verify the selected CLI is
authenticated and can list models in the member's working directory. Press
`e` to enter a model name manually.

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
