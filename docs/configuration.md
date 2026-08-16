# Asterline configuration and operations

[简体中文](configuration.zh-CN.md)

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

Use `/team` to modify the live roster. Asterline loads installed Agent CLIs
and their model catalogs once when it starts; opening `/team` reuses that
cache. The same lookup reads non-Codex CLIs' local permission defaults for
display, without copying them into `team.json`. Codex instead receives
Asterline's explicit default `approvalPolicy: "never"` at thread start/resume.
Focus a member's **model** field and press `t` to re-fetch its catalog at any
time. Press `s` to apply the changes, replace member runners, and save the
updated team.

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

### Installation-aware updates

Only copies installed by the Windows Setup executable update automatically.
Portable ZIP copies and source builds never rewrite themselves. An installed
copy checks GitHub's latest stable Release at most once every 24 hours. When a
new version exists, Asterline downloads its Setup executable, verifies it
against the same Release's `SHA256SUMS`, and starts Setup in silent mode after
the current Asterline process exits.

Run `ast update` to force a check now. On macOS and Linux, it updates only an
installation that it proves belongs to the official Homebrew Formula, using
`brew update` followed by a targeted Formula upgrade. It never overwrites a
portable archive, source build, direct macOS package, or direct `.deb`/`.rpm`
install; those use their own explicit replacement path. `ast --update` remains
an alias.

Use `ast --no-auto-update` to skip the Windows automatic check for one launch.
Network failures are ignored during background checks and never prevent
Asterline from starting; a forced Windows check reports the error to the
terminal.

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
      "builder": "builder",
      "reviewer": "reviewer",
      "max_iterations": 3,
      "auto_execute": true,
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

Optional bindings for `/mode review`, `/mode plan`, `/mode brainstorm`, and `/mode team`. Most
omitted role fields are derived from member roles and `default_target`
(the Review-mode builder ≈ default target or first non-reviewer; reviewer ≈
role contains "review"; leader ≈ role contains "plan" or "lead", else first
participant; participants = full roster). The Plan-mode `builder` is deliberately
different: it is required and has no derived fallback. The Plan `reviewer` is optional:
when omitted, a complete checklist proceeds directly to the Builder. `auto_execute` defaults
to `true`; set it to `false` to require `/approve` before the Builder receives the finalized
checklist. Defaults for budgets:
`max_iterations = 3`, `generation_rounds = 3`, `ideas_per_round = 4`,
`auto_execute = true`, `auto_verify = true`.
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
| `builder`           | review/plan      | Review: implements. Plan: required checklist executor.      |
| `reviewer`          | review/plan      | Emits verdicts; optional Plan-only checklist audit.         |
| `leader`            | plan             | Member who writes and revises the checklist                 |
| `participants`      | brainstorm       | Roster for all generation waves                             |
| `generation_rounds` | brainstorm       | Seed/build/stretch wave budget (default 3, minimum 2)       |
| `ideas_per_round`   | brainstorm       | Requested idea cards per member/wave (default 4, minimum 3) |
| `coordinator`       | team             | Member who coordinates the whole-team run                   |
| `max_iterations`    | review/plan/team | Loop budget before blocking or failing verify (def 3)       |
| `auto_execute`      | plan             | Auto-dispatch final plan (default); false needs `/approve`  |
| `auto_verify`       | review/plan/team | Runs after Review approval or Plan Builder completion.      |
| `verify_command`    | review/plan/team | Explicit auto-verify shell command (else heuristic)         |

Each mode has its own configuration shape; unrelated fields are rejected by
Serde instead of being silently accepted and ignored.

The `/mode` panel can change these knobs without editing the file by hand:
`s` selects the current mode and applies its pending overrides to this
conversation, while `w` writes the current mode's conversation overrides into
`team.json`.

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
| `cwd`             | No                          | Advanced per-member working-directory override          |
| `model`           | No                          | Omitted delegates to the backend CLI                    |
| `effort`          | No                          | Chosen inside the selected model picker when supported. |
| `system_prompt`   | No                          | Additional member instructions                          |
| `sandbox`         | No                          | Codex; mapped to Grok/Agy and ignored by Claude         |
| `permission_mode` | No                          | Backend control native in `/team`                       |
| `allowed_tools`   | No                          | Backend-specific tool allowlist                         |
| `session_policy`  | No                          | `resume` (default) or `fresh`                           |
| `session_id`      | No                          | Native CLI session/conversation ID to resume            |

On startup, a `resume` member with a bound session is scanned for native
transcript rows written while Asterline was closed (Grok CLI, Codex, or
Claude). Only unseen messages are imported into the current chat.

In `/team`, a `resume` member displays its bound native session ID directly.
Without one, it says `select a session` to make the required picker/manual-ID
choice explicit; a `fresh` member says `not set (fresh)`. The UI never calls an
unbound session ID `default`.

`cwd` is deliberately not editable in `/team`: members created there use the
team workspace. Keep the optional `team.json` field only for an advanced
multi-repository or monorepo setup where a member must run in a different
working directory. It also determines the backend session project and the
model-catalog cache key for that member.

Both policies pin and reuse the backend session ID after the first call.
`resume` keeps an existing persisted ID when available. Switching a member to
`fresh` discards its old ID once, so the next call creates a new native CLI
conversation; that newly discovered ID is then reused for subsequent calls.
`fresh` does not create a separate conversation for every turn.

Set `session_id` to bind a team member to a specific conversation from its
native CLI history. For Codex it is the App Server `thread.id`, resumed through
`thread/resume`. Claude, Grok, and Agy use `claude --resume`, ACP
`session/load`, and `agy --conversation` respectively. In the Team editor,
use `default` to clear the explicit ID.

Asterline names its Codex threads and Claude sessions `Asterline · <member>`
so they are recognizable in native history that exposes a title. A saved roster
at `<workspace>/.asterline/team.json` is also bound to that containing
workspace: on a project move, Asterline corrects a stale serialized workspace
before launching members, so new native transcripts remain associated with the
project you opened. Native session pickers filter by working directory. Claude
print-mode sessions are resumable with `claude --resume <id>`, but some Claude
Code versions intentionally omit print-mode sessions from their interactive
picker.

Permission modes, sandbox mappings, and allowed-tool behavior depend on the
backend. Do not assume a field has the same effect across all four CLIs.

## Backend setting support

This table describes what the current Asterline adapters actually pass to each
CLI. It is intentionally narrower than the union of fields accepted by the
Team editor.

| Setting                | Codex                                                              | Claude                                      | Grok ACP                                                        | Agy                                                                          |
| ---------------------- | ------------------------------------------------------------------ | ------------------------------------------- | --------------------------------------------------------------- | ---------------------------------------------------------------------------- |
| `cwd`                  | App Server `thread/start`/`thread/resume`                          | Process cwd                                 | ACP session `cwd`                                               | Process cwd plus `--add-dir`; prompt identifies the project workspace        |
| `model`                | App Server `model`                                                 | `--model`                                   | Agent `--model`                                                 | `--model`                                                                    |
| `effort`               | App Server `effort`; picker follows model metadata                 | `--effort` (through `max`)                  | Cache-defined levels pass as Agent `--reasoning-effort`         | Model-specific effort; defined only by its listed model (not a generic menu) |
| `sandbox`              | `read-only` / `workspace-write` / `danger-full-access`             | Not passed (not shown in `/team`)           | `read-only` / `workspace` / `off` profile mapping               | Terminal sandbox on/off; read-only intent also forces `--mode plan`          |
| `permission_mode`      | App Server `approvalPolicy` (`never` by default)                   | `--permission-mode` (default omitted)       | Mode plus ACP responses                                         | `--mode`; bypass requires terminal sandbox off                               |
| `allowed_tools`        | Not passed                                                         | `--tools` (hard built-in-tool allowlist)    | Added to ACP session rules; not a hard protocol-level allowlist | Not passed                                                                   |
| `system_prompt`        | App Server `developerInstructions`                                 | `--append-system-prompt`                    | ACP session `rules`                                             | Prepended to the print prompt                                                |
| `session_policy`       | Resume or fresh                                                    | Resume or fresh                             | ACP `session/load` or `session/new`                             | Resume or fresh conversation                                                 |
| `session_id`           | App Server `thread/resume <thread.id>`                             | `claude --resume <id>`                      | ACP `session/load`                                              | `agy --conversation <id>`                                                    |

For compatibility with existing `team.json` files, Codex's displayed policies
are stored through the shared adapter field: omitted/default,
`dontAsk`/`bypassPermissions` map to `never`; `plan`/`acceptEdits` map to
`untrusted`; and `auto` maps to `on-request`. Codex command, file-change, and permission-escalation callbacks are
shown as Asterline pending approvals and return your one-time decision to the
live App Server thread; the selected sandbox remains an independent boundary.
For Claude and Grok, choose only permission modes accepted by the installed CLI version. Asterline serializes
the configured value but does not negotiate vendor-version compatibility before
launch. Agy 1.1.12 or newer is required;
older versions are excluded from backend detection and rejected before a run
because earlier releases either lack structured streaming or ignore headless
`--mode` enforcement. Recent Claude CLIs no longer list
`default` as a `--permission-mode` choice; Asterline omits the flag when the
configured mode is `default` so the CLI default applies.

## Model discovery

Model choices are resolved in each member's effective working directory:

Asterline detects all four supported CLIs at launch, then starts one
workspace-scoped lookup asynchronously for every installed backend. The
queries do not depend on the configured roster, so a Codex, Claude, Grok, or
Agy member added later can use the already-loading result. Until a lookup
completes the Team editor shows `loading…`; once the CLI reports a concrete
default, it shows that model name rather than the placeholder `default`. The
result is held for the lifetime of that `ast` process: opening and closing
`/team` never re-runs it. A new working directory is shown as `not loaded ·
press Enter` rather than silently launching another background lookup; opening
that member's **Model** field explicitly loads one shared catalog for that
backend and working directory.
If a catalog loaded successfully but its CLI did not identify a default, the
field correctly shows `CLI default`; its picker still contains the discovered
models.

Focus `model` and press `t` to re-fetch that backend/workspace catalog whether
it previously succeeded or failed. The refreshed result is shared by all
matching members. If a load is already in progress, `t` leaves that shared
request in place rather than creating a duplicate.

| Backend | Source                                                          |
| ------- | --------------------------------------------------------------- |
| Codex   | App Server `model/list`; `codex debug models` only as fallback  |
| Claude  | Local settings/env; opt-in gateway `/v1/models`; Claude cache   |
| Grok    | `grok --no-auto-update models`                                  |
| Agy     | `agy models`                                                    |

Claude's catalog is local-configuration first. Asterline reads `model`,
`availableModels`, `modelOverrides`, and the corresponding `ANTHROPIC_*`
variables from the same user/project/local settings hierarchy as Claude Code.
For a `modelOverrides` entry it displays the provider ID while preserving the
Claude-side key passed to `claude --model`.

When that local configuration opts into Claude's gateway discovery with
`CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1` and a non-Anthropic
`ANTHROPIC_BASE_URL`, Asterline uses the same endpoint, authentication, and
custom headers to read `/v1/models`; a failed refresh uses Claude's local
`gateway-models.json` cache. Without a configured model or enabled gateway
discovery, it deliberately shows no invented Claude model list: use the native
CLI default or enter an explicit local/provider model ID.

Agy's local `~/.gemini/antigravity-cli/settings.json` `model` value is often
the human-readable label rather than the CLI ID. Asterline matches it against
the ID/label pairs returned by `agy models`, then displays that configured
model as the startup default.

The first-run Team builder also starts every installed catalog immediately;
`/team` reuses the catalog loaded once at `ast` startup. The Agent field lists
all four supported CLIs, includes installation status and
discovered model/effort summaries, and disables missing CLIs. Open the
member's `model` field to browse the already-loading catalog. Type to filter
by display name, model ID, or description, and use `↑`/`↓` to choose a model.
Only a picker whose selected model explicitly reports effort settings shows
`←`/`→`; `←` lowers and `→` raises the selected setting. Those settings are
applied with that model. Browsing with `↑`/`↓` never changes a saved effort
override. If the highlighted model does not advertise the existing override,
`Enter` keeps the current configuration and asks for an explicit `←`/`→`
choice instead of silently substituting a guessed default. Grok reads each listed
model's menu from its own CLI cache, and Agy reads model-qualified settings.
Neither gets a fabricated generic effort menu. Claude has no machine-readable
effort capability catalog, so Asterline does not guess from a model name. A
configured Claude alias remains exactly as configured, while a gateway model
uses the gateway's `display_name`. The picker selects the CLI-marked default
when discovery returns models or, if none is marked, the first discovered
model. It shows only actual model entries in that case. `default` is available
only when discovery returns no models. Focus `model` and press `t` to re-fetch
that catalog; the refresh is shared by matching backend/workspace members.
Press `e` on the field to enter a model ID
manually.

Reasoning effort is model-aware only when discovery returns capability
metadata. Unsupported levels are omitted and the model's reported default is
shown directly as a native default when available; it is not an override.
Agy exposes only the effort encoded by a
discovered model; Grok has no independent Team effort control.

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
├── roster.md
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

Reopening Asterline restores the selected conversation by default. `/new` and
`/clear` both create a clean conversation in normal mode and new backend
sessions while retaining older database records. They are rejected while
members, runs, or verification are active; press `Esc` and wait for
cancellation first.
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
ordinary chat turn. Codex only sees `$asterline-team` again on team runs and
teammate relays, where loading the skill is actually needed. Live roster identity and member status are rewritten to
`.asterline/roster.md` whenever the team or a member's status changes. The
team skill tells agents to read that file; it is not copied into each prompt.

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

### The model picker has no detected models

Wait for the startup catalog-loading notice to finish, then reopen the
`model` field. Verify the selected CLI is authenticated and can list models in
the member's working directory, then focus `model` and press `t` to re-fetch.
Press `e` to enter a model name manually.

### The wrong roster opens

Run `asterline --pick-team` to rebuild the saved roster, or use `/team` and
press `s` to apply changes.

### Start without the previous transcript

Use `asterline --no-restore`. This skips replay but does not delete SQLite data.
Use `/new` or `/clear` for a clean conversation with new backend sessions.

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
