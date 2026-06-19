# Asterline

Asterline is a TUI-first multi-agent coding console for coordinating a user,
Codex, and Claude Code from one terminal UI. It keeps inter-agent messages
visible, routed through the local runtime, and persisted to SQLite.

The current implementation is a working prototype based on `PLAN.md`. Fake
agents are the default so the UI, routing, persistence, relay guard, approvals,
and terminal log flows can be tested without spending real Codex or Claude
usage.

## Current Capabilities

- Rust + Ratatui terminal UI with a composer, event log, and agent status pane.
- Route selection for:
  - `You -> Team`
  - `You -> Codex`
  - `You -> Claude`
  - `Codex -> Claude`
  - `Claude -> Codex`
- Fake Codex and fake Claude adapters for deterministic collaboration tests.
- Structured inter-agent envelope parsing:

  ```text
  @@team_message {"to":"claude","kind":"question","body":"Should we write tests first?"}
  ```

- SQLite event log for messages, terminal output, inter-agent messages, and approvals.
- Auto-relay guard that pauses a thread after too many agent-to-agent hops.
- User-controlled relay pause, pending relay queue, replay, and rejection.
- Approval queue for risky-looking requests involving shell/file/git actions.
- Real non-interactive adapters:
  - Codex: `codex exec --json ...`
  - Claude Code: `claude -p --output-format json ...`
- PTY adapter support for launching CLI programs, injecting input, draining raw output,
  resizing the PTY, waiting for exit, and stopping a running session.
- Runtime PTY session manager for starting, writing to, draining, polling, and
  stopping per-agent PTY sessions.

## Run

From the repository root:

```bash
cargo run
```

After the TUI opens, type a task in the bottom Composer and press `Enter`.
Use `Tab` to choose whether the message goes to Team, Codex, Claude, or an
agent-to-agent route. Press `F1` inside the TUI for the built-in help view.

By default this starts fake Codex and fake Claude and stores data in:

```text
.asterline/asterline.sqlite3
```

Use a custom database path:

```bash
cargo run -- --db /tmp/asterline.sqlite3
```

Show CLI help:

```bash
cargo run -- --help
```

## Backend Modes

Fake agents are the default:

```bash
cargo run
```

Use real non-interactive Codex and Claude Code backends:

```bash
cargo run -- --real-agents
```

Use one real backend at a time:

```bash
cargo run -- --real-codex
cargo run -- --real-claude
```

Use PTY-backed CLI execution:

```bash
cargo run -- --pty-agents
cargo run -- --pty-codex
cargo run -- --pty-claude
```

Real and PTY modes call local `codex` and/or `claude` binaries. They may require
login and may consume real usage. The ignored smoke tests for those backends are
kept opt-in for that reason.

## TUI Shortcuts

- `Tab`: cycle the current route target.
- `Enter`: submit the composer text.
- `Ctrl-A`: show the agent list view.
- `Ctrl-L`: toggle persisted message log view.
- `Ctrl-T`: show raw terminal output view.
- `Ctrl-P`: show approval queue view.
- `Ctrl-R`: pause or resume automatic agent-to-agent relay.
- `Ctrl-E`: show pending relay queue view.
- `Ctrl-Y`: approve the next approval, or replay the next relay in relay view.
- `Ctrl-N`: reject the next approval, or reject the next relay in relay view.
- `F1`: show built-in help.
- `Esc`: return to the events view.
- `Ctrl-C`: exit.

## Test

Run the standard checks:

```bash
cargo fmt --check
cargo check
cargo test
```

The full test suite uses fake agents and local shell PTY tests by default. Real
Codex and Claude Code smoke tests are marked `#[ignore]` because they can consume
real service usage.

Opt-in real smoke tests only when you intend to call the installed CLIs:

```bash
ASTERLINE_RUN_CODEX_EXEC_SMOKE=1 cargo test real_codex_exec_smoke_test -- --ignored
ASTERLINE_RUN_CLAUDE_PRINT_SMOKE=1 cargo test real_claude_print_smoke_test -- --ignored
```

## Project Layout

```text
src/
  adapter/   fake agents, Codex exec, Claude print, and PTY adapters
  router/    team-message envelope parsing and relay guard
  runtime/   workflow orchestration and runtime events
  store/     SQLite persistence
  tui/       Ratatui state, input handling, and widgets
  types.rs   shared IDs, statuses, participants, and route targets
```

## Current Limits

- This is still a prototype, not a full replacement for Codex or Claude Code.
- Fake agents remain the default for repeatable local development.
- Real CLI integration is intentionally explicit to avoid accidental usage.
- PTY backends now use runtime-managed per-agent sessions, but the TUI still
  captures each submitted turn with a short synchronous output window rather
  than a fully asynchronous live terminal stream.
