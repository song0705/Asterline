# Asterline

Asterline is a chat-first, TUI multi-agent coding console. You talk to a team
of coding agents from one terminal; each member is a backend (`codex` or
`claude`) bound to a role. You see every agent's streaming output, tool calls,
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
- **Generic team roster.** A member is `backend + role + name`. All-Codex,
  all-Claude, and mixed teams are all valid; role is not tied to backend.
- **Three streaming backends.** Claude (`claude -p --output-format stream-json
  --include-partial-messages`), Codex (`codex exec --json`), and Gemini
  (`gemini -p -o text`). Codex/Claude keep a resumable session — no `--ephemeral`,
  no `--no-session-persistence`.
- **Per-member reasoning effort.** `/effort <member> <level>` (low…max) maps to
  Claude's `--effort` and Codex's `model_reasoning_effort`, shown in the header.
- **Attach to a live session.** Select a member and press `Enter` to hand the
  terminal to its real interactive CLI, resuming that member's session; exit to
  return.
- **Rich chat.** Agent output renders as Markdown; Codex file changes show as
  diff cards; tool calls collapse to a single line.
- **Visible agent-to-agent messaging.** Agents talk by emitting
  `@@team_message {"to":"reviewer","body":"…"}`; the runtime routes it, shows it
  in chat, and persists it. A relay guard pauses runaway loops.
- **Persisted + replayable.** Chat, tool events, routes, raw backend JSON, logs,
  approvals, and sessions are stored in SQLite and replayed on startup.
- **No function keys.** All actions are plain keys and `Ctrl` chords.

## Install

```bash
cargo install --path .   # or: just install
```

This installs two binaries into `~/.cargo/bin`: `asterline` and the short alias
`ast`. Launch with either:

```bash
ast                       # auto-detect codex/claude in the current directory
asterline --workspace ~/code/project
```

During development you can also run without installing:

```bash
cargo run            # or: just run
cargo run -- --fake  # offline fake agents (no real CLI usage)
```

## Usage

With no `--team`, Asterline detects which backends are installed and builds a
default roster:

- both `codex` and `claude` → a mixed team (`builder`·codex + `reviewer`·claude)
- only one → a single-member team
- neither → it prints a setup hint and exits

### Options

| Flag | Meaning |
| --- | --- |
| `--team <PATH>` | Load a team config (JSON). |
| `--workspace <PATH>` | Working directory for members (default: cwd). |
| `--db <PATH>` | SQLite path (default: `<workspace>/.asterline/asterline.sqlite3`). |
| `--no-restore` | Don't replay persisted chat on startup. |
| `--debug` | Disable the approval gate (developer mode). |
| `--fake` | Use offline fake agents instead of real CLIs. |
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
      "id": "builder",
      "display_name": "Builder",
      "backend": "codex",
      "role": "implementation",
      "sandbox": "workspace-write"
    },
    {
      "id": "reviewer",
      "display_name": "Reviewer",
      "backend": "claude",
      "role": "review",
      "permission_mode": "plan"
    }
  ]
}
```

### Keys

- `Enter` — send the composer.
- `Esc` — close the open drawer or cancel roster selection.
- `Ctrl+L` — logs drawer · `Ctrl+R` — team drawer · `Ctrl+P` — command palette.
- `Ctrl+C` — cancel running members, else clear the composer, else quit.
- `Ctrl+U` — clear line · `Ctrl+W` — delete word · `Ctrl+A`/`Ctrl+E` — line start/end.
- `Ctrl+N` / `Ctrl+B` — start cycling focus to next / previous member in the top roster.
- `←`/`→` — cycle member selection (when roster focus is active).
- `Enter` (when a member is selected) — attach to that member's live backend
  session (exit the CLI to return).
- `↑`/`↓` — recall previous submissions (shell-style prompt history; preserves
  your in-progress draft), or move the selection when a popup is open.
- `PageUp`/`PageDown` — scroll the conversation.

### Slash commands

Start typing `/` to see the available commands, or `@` to see members — a
popup filters as you type. `↑`/`↓` move the selection, `Tab`/`Enter` accept, and
`Esc` dismisses it.

- `/ask <member> <message>` or `@<member> <message>` — send to one member. Supports `all` as member to broadcast (e.g. `/ask all` or `@all`).
- `/all <message>` — send to everyone.
- `/effort <member> <level>` — set reasoning effort (`low`…`max`).
- `/workflow <goal>` — have a coordinator plan a goal and delegate to teammates.
- `/focus <member>` — view a member's logs.
- `/team`, `/sessions`, `/status` — open the team drawer.
- `/logs` — open the logs drawer.
- `/retry` — resume a paused route, or re-run the last turn.
- `/abort` — cancel running members.
- `/approve` · `/reject` — decide the first pending approval.
- `/help` — show the command palette.

### How agents talk to each other

An agent sends a teammate a message by emitting a line:

```text
@@team_message {"to":"reviewer","body":"please review the parser"}
@@team_message {"to":["builder","reviewer"],"body":"let's agree on the data model"}
@@team_message {"to":"all","body":"status?"}
```

Asterline parses it, shows `from → to` in the chat, persists it, and delivers it
to the target members. Auto-relays are bounded per turn; when the limit is hit
(or you pause relay), the route is queued and you continue it with `/retry`.

## Develop

```bash
just check     # cargo fmt --check + clippy -D warnings + tests
cargo test
```

The default test suite uses fake agents and local shell PTY tests. It never
calls the real `codex`/`claude` CLIs.

## Layout

```text
src/
  domain/    team roster + structured event vocabulary (no I/O)
  router/    @@team_message parsing + relay guard + target resolution
  adapter/   streaming claude/codex adapters, process runner, fake; cli_pty (debug)
  runtime/   TeamRuntime orchestration core + transport, sessions, approvals
  store/     event-source SQLite schema + chat replay
  tui/       chat-first UI: state, rendering, composer, drawers, commands, keymap
  app.rs     bootstrap: CLI args, default roster, runtime + TUI wiring
```
