# Asterline

[English](README.md) · [简体中文](README.zh-CN.md)

**Turn local coding agents into one visible team.**

Asterline is a local-first terminal workspace for coordinating Codex, Claude,
Grok, and Agy. Instead of placing agents in disconnected tabs, it gives the
operator one shared conversation and the agents explicit roles, visible
handoffs, tracked workflows, and a durable record of the work.

Asterline runs the official CLIs already installed on your machine. It is not a
model gateway, does not replace vendor authentication, and does not send your
workspace through an Asterline cloud service.

![Codex handing a frontend design proposal to Agy](docs/assets/asterline-codex-to-agy.webp)

## Quick start

### Requirements

- Rust 1.85 or newer when building from source
- A terminal with color and alternate-screen support
- At least one installed and authenticated CLI: `codex`, `claude`, `grok`, or `agy`
- Git is recommended for diff and verification workflows

### Install and launch

Download the archive for your platform from
[GitHub Releases](https://github.com/song0705/Asterline/releases/latest), then
install either binary from the extracted directory:

```bash
install -m 755 ast ~/.local/bin/ast
```

Release archives are published for Linux x86-64, Linux ARM64, macOS Intel, and
macOS Apple silicon. Every release includes `SHA256SUMS` and signed GitHub build
provenance.

To install from source instead, clone this repository and run:

```bash
cargo install --path . --force
cd ~/code/your-project
ast
```

This installs both `asterline` and the shorter `ast` command. Asterline detects
supported executables on `PATH`, opens the Team builder, and remembers the
result in `<workspace>/.asterline/team.json`.

In the Team builder:

1. Use `↑` and `↓` to select a member.
2. Press `Enter` to open that member's fields.
3. Use `↑` and `↓` to select a field; press `Enter` to edit or cycle it.
4. Press `Esc` to return to member selection.
5. Press `s` to save and start.

### First useful workflow

The examples below assume the roster contains a member with the `builder`
handle. Use the handle shown by your Team builder when it differs.

Send a direct task:

```text
@builder inspect this repository and identify the highest-risk code path
```

Run a review loop (the builder implements, the reviewer issues structured
verdicts until the work passes):

```text
/mode review
fix the payment callback race and add regression tests
```

Or have a leader plan an owned checklist that Asterline dispatches to the team:

```text
/mode plan
ship the payment callback fix end to end
```

A fresh conversation requires an explicit target. Later plain text reuses the
previous target; `@all` and `/all` broadcast to the team.

To invoke a skill installed for a member CLI, type the member prefix followed
by `/`, for example `@codex /review-patch`. Asterline completes discovered
skills and passes the invocation to that member; Codex is translated to its
native `$review-patch` form. Unprefixed `/mode`, `/team`, and similar commands
remain Asterline commands.

## Why Asterline

### Coordination is the product

Many multi-agent terminal tools are session managers: they create panes,
worktrees, or parallel tasks and leave the operator to move context between
them. Asterline focuses on the collaboration layer:

- members have stable names, roles, models, permissions, and sessions;
- agents can hand work to named teammates inside the same visible conversation;
- tool calls, output, file changes, routes, and failures stay attributed;
- workflows track ownership, attempts, blockers, notes, and verification;
- SQLite preserves the operational record across restarts.

### Use Asterline when

- implementation, review, research, and verification should be different roles;
- you already use supported coding CLIs and want them to collaborate;
- seeing why work moved between agents matters as much as the final patch;
- you want a human-controlled workflow without building an agent framework;
- local persistence and resumable sessions matter.

### Choose a different setup when

- every agent must work in an automatically isolated Git worktree or branch;
- you need a hosted agent service, web dashboard, or remote job queue;
- you need direct provider APIs rather than installed CLI subscriptions;
- you want fully unattended merge automation.

Asterline members share the configured workspace by default. A member may have a
different `cwd`, but Asterline does not currently create or merge worktrees.

## Supported backends

| Backend | Executable | Streaming                         | Resume | Model choices                 |
| ------- | ---------- | --------------------------------- | ------ | ----------------------------- |
| Codex   | `codex`    | `codex exec --json`               | Yes    | `codex debug models`          |
| Claude  | `claude`   | stream JSON with partial messages | Yes    | aliases and `availableModels` |
| Grok    | `grok`     | headless streaming JSON           | Yes    | `grok models`                 |
| Agy     | `agy`      | print/log stream                  | Yes    | `agy models`                  |

Asterline does not install, authenticate, or bill for these products. Backend
availability, model access, and usage limits remain properties of the
underlying CLI account.

## How it works

```text
You
 └─ target a member, the whole team, or a tracked workflow
     ├─ Asterline launches/resumes the selected backend CLI
     ├─ stream events become chat, tools, diffs, logs, and session state
     ├─ valid teammate envelopes are routed to other members
     └─ messages, routes, workflow state, and verification persist to SQLite
```

Automatic teammate relays are bounded. When a turn reaches the configured
limit, Asterline pauses the route and waits for `/retry` instead of allowing an
uncontrolled loop.

## Product experience

### One conversation

Each participant has a clear identity. Tool calls, returned output, diffs,
routes, and errors remain on the member's conversation rail. Failed tool output
is shown immediately; `Ctrl+O` expands or collapses longer successful output.

Agent Markdown, fenced code, tables, and working-tree diffs are rendered in the
terminal. Raw diagnostics remain available in `/logs` without flooding chat.

### A team you can change while it runs

Teams may mix backends or use the same backend more than once. Member
configuration can include a role, model, reasoning effort, working directory,
system prompt, sandbox, permission mode, tool allowlist, and session policy.
The exact settings passed through depend on the backend; the
[configuration reference](docs/configuration.md#backend-setting-support) lists
the current adapter behavior.

Open `/team` to update the live roster. Model choices are discovered in the
member's working directory, while `e` allows a custom model. Press `s` to apply
and save changes.

![Asterline Team editor](docs/assets/asterline-team.webp)

### Collaboration modes with an audit trail

`/mode review`, `/mode plan`, and `/mode roundtable` select the active mode for
the current terminal. Every later message uses that mode until another
`/mode` selection replaces it; `/new` does not reset the selection, and
`/mode normal` returns to regular direct messages. In review mode the builder implements and
the reviewer must answer with a structured `@@review` verdict; `approve`
finishes the run (optionally auto-verifying), `request_changes` loops the
feedback back to the builder, bounded by `max_iterations`. Lead mode has the
leader plan an owned checklist that Asterline dispatches to each owner before
the same review loop. Roundtable runs N discussion rounds where each member
sees the others' arguments, with an optional moderator synthesis.

Who builds, reviews, leads, or moderates is configurable per mode in
`team.json`. Legacy one-shot commands such as
`/review reviewer=claude builder=codex …` still accept inline overrides. `/runs`
shows the mode phase, iteration budget, checklist owners, verdict timeline,
blockers, and the next suggested command.

```text
/block waiting for the staging client secret
/note secret requested from the platform team
/continue secret is now available
/verify cargo test
```

Without an explicit verification command, Asterline detects common checks such
as `cargo test`, `npm test`, and `pytest`. Selecting `/mode workflow` sends
subsequent messages through the original prompt-driven coordination path.

### Native session attach

Focus a member with `Ctrl+N` or `Ctrl+B`, move with `←` or `→`, and press
`Enter`. Asterline suspends its interface and opens that member's native
interactive CLI, resuming its session when possible. Exit the CLI to return.

Codex and Claude messages created while attached are imported into the
Asterline transcript. Grok and Agy resume their native session but do not
import the attached transcript.

To bind a member to an existing native CLI conversation, open `/team`, select
the member and press `Enter` on `session id`. Asterline extracts local Codex,
Claude, and Grok history metadata for that member's working directory into its
own searchable session table; choose a row with `↑`/`↓` and `Enter`, then press
`s` to apply. Press `e` for manual entry (required for Agy), or enter `auto` to
remove an explicit binding. The selected ID is persisted in `team.json` and
passed to the backend's native resume command.

### Local, durable state

By default, Asterline stores the roster and SQLite database inside the project:

```text
<workspace>/.asterline/
├── team.json
└── asterline.sqlite3
```

The database contains prompts, responses, tool events, routes, raw backend
events, logs, approvals, sessions, and workflow history. Treat it as sensitive
development data and normally add this to the project `.gitignore`:

```gitignore
.asterline/
```

Asterline also creates `.agents/skills/asterline-team/SKILL.md` when the team
protocol is missing. This is a readable workspace integration file rather than
runtime history; review it and decide whether your project should version it.

## Essential commands

| Command                | Purpose                                        |
| ---------------------- | ---------------------------------------------- |
| `@<member> <message>`  | Send to one member                             |
| `@all <message>`       | Broadcast to the team                          |
| `/mode`                | Choose normal or a collaboration mode          |
| `/runs`                | Inspect run state, phase, and next actions     |
| `/team`                | Edit the live roster                           |
| `/skills`              | Select a Skill for the next prompt             |
| `/find <text>`         | Search the transcript                          |
| `/diff`                | Inspect unstaged changes and untracked files   |
| `/logs`                | Open persisted diagnostics                     |
| `/new`                 | Start a new conversation and backend sessions  |
| `/approve` / `/reject` | Resolve a pending approval                     |
| `/retry`               | Resume a paused route or retry a turn          |
| `/abort`               | Cancel running work, modes, and verification   |
| `/help`                | Open the command palette                       |

See the [complete command and keyboard reference](docs/commands.md) for workflow
step commands, Team controls, prompt history, session attach, and `/runs`
navigation.

## Permissions and safety

Asterline launches backend processes locally and inherits their credentials,
environment, filesystem access, and network access. It does not sandbox a
process beyond the controls supported by that backend.

Members may use backend-native sandbox and permission settings. Asterline also
gates requests it classifies as risky — user messages, agent-to-agent relays,
and collaboration-mode dispatches — with a configurable policy (see
[approvals and tool-level control](docs/approvals.md)). `--debug` disables the
Asterline approval gate and is intended only for controlled development
environments.

Read [configuration and operations](docs/configuration.md) before using
`danger-full-access`, bypass-style permission modes, custom system prompts, or
agent-managed roster changes.

## Documentation

- [Commands and keyboard](docs/commands.md)
- [Configuration, local data, permissions, and troubleshooting](docs/configuration.md)
- Built-in command palette: `/help`
- Command-line help: `asterline --help`

## Development

Run against offline fake agents:

```bash
cargo run -- --fake
```

Run the full local quality gate:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

If `just` is installed, `just run --fake`, `just install`, and `just check`
provide the same common workflows.

```text
src/
├── adapter/   backend streams, model discovery, PTY and process adapters
├── domain/    team configuration and structured events
├── router/    teammate envelopes, targets, and relay limits
├── runtime/   orchestration, approvals, sessions, and workflows
├── store/     SQLite persistence and replay
├── tui/       chat, composer, drawers, commands, and Team editor
└── app.rs     CLI bootstrap and product wiring
```

## Project status

Asterline is currently version `0.1.2` and under active development. Tagged
versions are published as prebuilt Linux and macOS archives through GitHub
Actions. Configuration and persisted data are migrated when possible, but
commands and UI details may continue to evolve before a stable release.

Release maintainers should follow the [release guide](docs/releasing.md).

## License

Asterline is available under the [MIT License](LICENSE).
