# Asterline

Asterline is a chat-first, TUI multi-agent coding console. You talk to a team
of coding agents from one terminal; each member is a backend (`codex`, `claude`,
or `agy`) bound to a role. You see every agent's streaming output, tool calls,
and the messages agents send each other — all in one conversation, persisted to
SQLite.

```text
┌ Asterline · /path/to/project · Builder·codex running  Reviewer·claude idle ┐
│ You  build the parser, then have the reviewer check it                      │
│ Builder · codex   I'll start by reading the project structure…              │
│   ⚙ shell: cargo test   [running]                                           │
│ Builder → reviewer   implementation done, please check edge cases           │
│ Reviewer · claude   Two issues: the lexer drops a trailing newline…         │
├─────────────────────────────────────────────────────────────────────────────┤
│ > @reviewer focus on the error paths                                        │
└─────────────────────────────────────────────────────────────────────────────┘
```

## Highlights

- **Single-column chat.** Agent text, tool calls, agent-to-agent routes, and
  errors are inline conversation blocks. Logs live in a drawer, not the main view.
- **Pick your team at startup.** With no `--team`, Asterline detects the
  installed backend CLIs and lets you choose which to include (or falls back to a
  default roster). A member is `backend + role + name`; mixed teams are valid.
- **Three streaming backends.** Claude (`claude -p --output-format stream-json
  --include-partial-messages`), Codex (`codex exec --json`), and Agy
  (`agy --print`). Each backend keeps a resumable session when the CLI exposes
  one.
- **Per-member reasoning effort.** `/effort <member> <level>` (low…max) maps to
  Claude's `--effort` and Codex's `model_reasoning_effort`, shown in the header.
- **Attach to a live session.** Select a member and press `Enter` to hand the
  terminal to its real interactive CLI, resuming that member's session; exit to
  return — and anything you said there is imported back into the transcript.
- **Real composer.** Multi-line input (`Shift+Enter`), shell-style prompt
  history (`↑`/`↓`), and reverse search (`Ctrl+R`).
- **Rich chat.** Agent output renders as Markdown with syntax-highlighted code
  (pulldown-cmark + syntect); `/diff` shows a syntax-highlighted working-tree
  diff; tool calls collapse to a single line; a live "working" timer shows
  elapsed time per member.
- **New chat / sessions.** `/new` starts a fresh conversation (cleared
  transcript, new backend sessions); transcripts are conversation-scoped, so a
  restart resumes the current chat.
- **Visible agent-to-agent messaging.** Agents get a compact `$asterline-team`
  skill trigger, then talk by emitting
  `@@team_message {"to":"reviewer","body":"…"}`; the runtime routes it, shows it
  in chat, and persists it. A relay guard pauses runaway loops.
- **Persisted + replayable.** Chat, tool events, routes, raw backend JSON, logs,
  approvals, and sessions are stored in SQLite (versioned + migrated) and
  replayed on startup.
- **No function keys.** All actions are plain keys and `Ctrl` chords; the mouse
  wheel scrolls the conversation.

## Install

```bash
cargo install --path .   # or: just install
```

This installs two binaries into `~/.cargo/bin`: `asterline` and the short alias
`ast`. Launch with either:

```bash
ast                       # auto-detect codex/claude/agy in the current directory
asterline --workspace ~/code/project
```

During development you can also run without installing:

```bash
cargo run            # or: just run
cargo run -- --fake  # offline fake agents (no real CLI usage)
```

## Usage

With no `--team`, Asterline detects which backend CLIs are installed and opens an
interactive team builder. You can add multiple members, choose each member's
backend, and set role, model, reasoning effort, sandbox, permission mode,
session policy, and cwd before starting. The builder seeds sensible defaults
(codex → builder, claude → reviewer, agy → researcher), but multiple members can
use the same backend with different roles/models. Use `↑/↓` to pick a member,
`Tab` to move fields, `Enter` to edit or cycle a field, `a` to add, `d` to
delete, and `s` to start. On a non-interactive stdout (or if you cancel) it
falls back to a default roster; with no backend found at all it prints a setup
hint and exits. Your choice is remembered in `<workspace>/.asterline/team.json`
and can be changed later from the `/team` drawer. Re-run with `--pick-team` only
when you want to rebuild the saved roster from scratch; `--team <PATH>` loads a
saved roster directly.

### Options

| Flag | Meaning |
| --- | --- |
| `--team <PATH>` | Load a team config (JSON); skips the builder. |
| `--pick-team` | Re-open the team builder, ignoring the saved team. |
| `--workspace <PATH>` | Working directory for members (default: cwd). |
| `--db <PATH>` | SQLite path (default: `<workspace>/.asterline/asterline.sqlite3`). |
| `--no-restore` | Don't replay persisted chat on startup. |
| `--debug` | Disable the approval gate (developer mode). |
| `--fake` | Use offline fake agents instead of real CLIs. |
| `--banner` | Print a compact one-line startup banner before the TUI. |
| `-h`, `--help` | Show help. |

### Team config

```json
{
  "name": "my-team",
  "workspace": "/path/to/project",
  "default_target": { "member": "builder" },
  "max_auto_relays": 6,
  "members": [
    {
      "display_name": "Builder",
      "backend": "codex",
      "role": "implementation",
      "model": "gpt-5-codex",
      "effort": "high",
      "sandbox": "workspace-write"
    },
    {
      "display_name": "Reviewer",
      "backend": "claude",
      "role": "review",
      "model": "sonnet",
      "effort": "medium",
      "permission_mode": "plan"
    }
  ]
}
```

Member `id` is optional. When omitted, Asterline derives the handle from
`display_name` (`Builder` -> `builder`, `QA Lead` -> `qa-lead`). Add `id` only
when you need a custom handle for `@member` or `default_target`.

Old saved configs and history rows that still say `backend: "gemini"` are
automatically migrated to `backend: "agy"` on startup.

### Keys

- `Enter` — send the composer.
- `Shift+Enter` — insert a newline (the composer is multi-line and grows with
  its content). `Alt+Enter` remains as a fallback on terminals that cannot
  report `Shift+Enter`.
- `↑`/`↓` — recall previous submissions (shell-style prompt history; preserves
  your in-progress draft), move between composer lines, or move the popup selection.
- `Tab` — accept completion; in `/runs`, stage a dispatch/assign draft for the
  selected checklist step.
- `x` — in `/runs`, toggle between compact history and full details.
- `Ctrl+R` — reverse-search prompt history (type to match, `Ctrl+R` for older,
  `Enter` to accept, `Esc` to cancel).
- `PageUp`/`PageDown` or the mouse wheel — scroll the conversation (or the open drawer).
- `Esc` — close the open drawer or cancel roster selection.
- `Ctrl+L` — logs drawer · `Ctrl+P` — command palette (team drawer via `/team`).
- `Ctrl+C` — cancel running members, else clear the composer; press twice on an
  empty composer to quit.
- `Ctrl+U` — clear line · `Ctrl+W` — delete word · `Ctrl+A`/`Ctrl+E` — line start/end.
- `Ctrl+N` / `Ctrl+B` — start cycling focus to next / previous member in the top roster.
- `←`/`→` — cycle member selection (when roster focus is active).
- `Enter` (when a member is selected) — attach to that member's live backend
  session; exit the CLI to return (messages you exchanged there are imported).

### Slash commands

Start typing `/` to see the available commands, or `@` to see members — a
popup filters as you type. `↑`/`↓` move the selection, `Tab`/`Enter` accept, and
`Esc` dismisses it. Plain text without `@...` or `/...` is not sent, and the
draft is kept so you can add a target prefix.

- `/ask <member> <message>` or `@<member> <message>` — send to one member. Supports `all` as member to broadcast (e.g. `/ask all` or `@all`).
- `/all <message>` — send to everyone.
- `/new` — start a fresh chat: a new conversation (cleared transcript) and new
  backend sessions for every member.
- `/effort <member> <level>` — set reasoning effort (`low`…`max`).
- `/plan <goal>` or `/workflow <goal>` — start a tracked team workflow; Asterline asks a
  coordinator to plan the goal and delegate to teammates.
- `/runs` — open workflow run history, including status, created/updated time,
  outcome summary, verification result, plan/work/verify stage progress, the
  next suggested action, and the command to run next. The drawer shows the
  run counts by outcome, total attempts, and the selected run's goal, owner,
  attempt number, outcome, next step, action command, checklist progress,
  owner workload summary, checklist steps, and recent run timeline above the
  history table. `/runs` opens in a compact scanning mode; press `x` to expand
  full details, and `x` again to collapse. Active
  workflow progress is also summarized in the footer and the history table so
  you can scan `done/total`, doing, and blocked steps without opening the full
  detail. The timeline records creation, continuation notes, status changes,
  checklist changes, and verification results per attempt. When Asterline
  detects a default check (`cargo test`, `npm test`, or `pytest`), the action
  shows that command explicitly. Use `←`/`→` to select a run. Use `↑`/`↓` to
  select one of that run's checklist steps; `Enter` then places the selected
  step's next status command into the composer. Pressing `←`/`→` also clears the
  step focus, so with no step selected `Enter` stages the selected run's normal
  action (`/verify`, `/continue`, `/abort`, etc.). Press `Tab` on a selected
  step to stage an editable `@owner ...` dispatch message; if the step has no
  owner yet, `Tab` stages `/step assign run-<id> <n>`.
- `/continue [run-<id>] [note]` — continue the latest or specified workflow run
  after a blocker, failed work turn, or failed verification. Continuing keeps the
  same run history entry, increments its attempt number, sends the coordinator
  the previous outcome and optional note, and moves the run back to `running`.
- `/note [run-<id>] <note>` — add a human checkpoint to the latest or specified
  workflow run without waking an agent or changing the run status. The note is
  stored in the run timeline and shown in `/runs`.
- `/block [run-<id>] <reason>` — mark the latest or specified workflow run as
  blocked and record the reason in the timeline. Use `/continue` when the
  blocker is resolved.
- `/step add [run-<id>] [@owner] <title>` — add a checklist step to the latest
  or specified workflow run, optionally assigning it to a member handle. Use
  `/step todo|doing|done|block [run-<id>] <n> [note]` to update a step by its
  number in `/runs`; use `/step assign [run-<id>] <n> <member>` or `/step
  unassign [run-<id>] <n>` to update ownership; use `/step rename [run-<id>]
  <n> <title>` or `/step remove [run-<id>] <n>` to clean up duplicate or
  obsolete steps. Checklist changes are also recorded in the timeline.
- `/verify [run-<id>] [command]` — verify the latest workflow run, or the
  specified run when launched from `/runs`. Without a command, Asterline
  auto-detects common project checks (`cargo test`, `npm test`, `pytest`).
  Verification runs in the runtime background thread; use `/abort` to cancel an
  active check.
- `/focus <member>` — view a member's logs.
- `/team`, `/sessions`, `/status` — open the team drawer. Inside `/team`,
  use `a`/`d` to add or delete members, `←`/`→` and `Enter` to edit fields
  (`backend`, `model`, `effort`, `cwd`, etc.), `t` or `*` to set the default
  target, and `s` to apply and save.
- `/logs` — open the logs drawer.
- `/diff` — show the working-tree git diff (including untracked files), with
  syntax-highlighted code, in a scrollable overlay.
- `/retry` — resume a paused route, or re-run the last turn.
- `/abort` — cancel running members and any active workflow verification.
- `/approve` · `/reject` — decide the first pending approval.
- `/help` — show the command palette.

### How agents talk to each other

Asterline writes the protocol into the repo skill at
`.agents/skills/asterline-team/SKILL.md` and prompts agents with the compact
`$asterline-team` trigger instead of injecting the full protocol into every
member prompt.

An agent sends a teammate a message by emitting a line:

```text
@@team_message {"to":"reviewer","body":"please review the parser"}
@@team_message {"to":["builder","reviewer"],"body":"let's agree on the data model"}
@@team_message {"to":"all","body":"status?"}
```

Asterline parses it, shows `from → to` in the chat, persists it, and delivers it
to the target members. Auto-relays are bounded per turn; when the limit is hit
(or you pause relay), the route is queued and you continue it with `/retry`.

An agent can also request a new teammate when the current roster lacks a needed
specialty:

```text
@@team_member {"display_name":"QA","backend":"codex","role":"tests"}
@@team_member {"display_name":"Researcher","backend":"agy","role":"research","model":"default","effort":"high"}
```

The runtime validates the request, rejects duplicate ids/names, adds a runner,
saves the roster, and broadcasts the updated team to the TUI. Only adding is
supported through this protocol; deleting still goes through `/team`.

During `/plan` or `/continue`, an agent can keep the selected run's checklist in
sync:

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

These updates apply only to the active workflow turn, appear in `/runs`, and are
also recorded in the run timeline.

## Develop

```bash
just check     # cargo fmt --check + clippy -D warnings + tests
cargo test
```

The default test suite uses fake agents and local shell PTY tests. It never
calls the real `codex`/`claude`/`agy` CLIs.

## Layout

```text
src/
  domain/    team roster + structured event vocabulary (no I/O)
  router/    @@team_message/@@team_member parsing + relay guard + target resolution
  adapter/   claude/codex/agy adapters, process runner, fake; cli_pty (debug)
  runtime/   TeamRuntime orchestration core + transport, sessions, approvals
  store/     event-source SQLite schema + chat replay
  tui/       chat-first UI: state, rendering, composer, drawers, commands, keymap
  app.rs     bootstrap: CLI args, default roster, runtime + TUI wiring
```
