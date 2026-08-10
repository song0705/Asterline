# Asterline

[English](README.md) · [简体中文](README.zh-CN.md)

[![CI](https://github.com/song0705/Asterline/actions/workflows/ci.yml/badge.svg)](https://github.com/song0705/Asterline/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/song0705/Asterline)](https://github.com/song0705/Asterline/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**One terminal for a visible, persistent team of coding agents.**

Asterline is a local-first terminal workspace that coordinates the official
Codex, Claude, Grok, and Agy CLIs already installed on your machine. It turns
their native event streams into one auditable conversation, routes work between
members, and persists collaboration as structured Runs.

[Download](https://github.com/song0705/Asterline/releases/latest) ·
[Quick start](#quick-start) ·
[Commands](docs/commands.md) ·
[Configuration](docs/configuration.md)

![Codex handing a frontend design proposal to Agy](docs/assets/asterline-codex-to-agy.webp)

## Quick start

### Requirements

- Linux, macOS, or Windows 10/11 with a color-capable terminal
- At least one installed and authenticated CLI: `codex`, `claude`, `grok`, or
  `agy`
- Rust 1.85 or newer only when building from source

### Install a release build

Open [GitHub Releases](https://github.com/song0705/Asterline/releases/latest),
then follow the section for your operating system. Asterline ships both the
`asterline` command and its shorter alias, `ast`; the steps below install `ast`.

#### macOS

Download the `.tar.gz` archive that matches the Mac:

- Apple silicon: `aarch64-apple-darwin`
- Intel: `x86_64-apple-darwin`

Extract it, open Terminal in the extracted directory, and run:

```bash
mkdir -p "$HOME/.local/bin"
install -m 755 ast "$HOME/.local/bin/ast"
"$HOME/.local/bin/ast" --help
```

Add `$HOME/.local/bin` to `PATH` in `~/.zprofile` if `ast` is not found in a
new terminal.

#### Linux

Download the `.tar.gz` archive for the machine architecture:

- Intel/AMD 64-bit: `x86_64-unknown-linux-gnu`
- ARM64: `aarch64-unknown-linux-gnu`

Extract it, open a shell in the extracted directory, and run:

```bash
mkdir -p "$HOME/.local/bin"
install -m 755 ast "$HOME/.local/bin/ast"
"$HOME/.local/bin/ast" --help
```

Add `$HOME/.local/bin` to the shell's `PATH` configuration, commonly
`~/.profile`, if `ast` is not found in a new shell.

#### Windows

Download `x86_64-pc-windows-msvc.zip`, extract it, open PowerShell in the
extracted directory, and run:

```powershell
New-Item -ItemType Directory -Force "$HOME\bin" | Out-Null
Copy-Item .\ast.exe "$HOME\bin\ast.exe"
& "$HOME\bin\ast.exe" --help
```

Add `%USERPROFILE%\bin` to the user `Path` in Windows Environment Variables,
then open a new PowerShell window before running `ast` elsewhere.

Each release also includes `SHA256SUMS` and GitHub build provenance
attestations.

### Build from source

On any supported operating system with Rust 1.85 or newer, run this from an
Asterline source checkout:

```bash
cargo install --path . --force
```

Cargo installs both commands into `$HOME/.cargo/bin` on macOS/Linux and
`%USERPROFILE%\.cargo\bin` on Windows. Ensure that directory is on `PATH`.

### Start a team

Run Asterline from the project the agents should work on.

On macOS or Linux:

```bash
cd /path/to/your/project
ast
```

On Windows PowerShell:

```powershell
Set-Location C:\path\to\your-project
ast.exe
```

On first launch, Asterline discovers the supported CLIs on `PATH` and opens the
Team editor. Select a member with `↑`/`↓`, press `Enter` to edit its fields, and
press `s` to save and start. The editor discovers available models and reasoning
effort for each installed backend; it never installs a CLI or signs in for you.

Send the first task to a displayed member handle:

```text
@builder audit this repository and identify the highest-risk code path
```

Or select a tracked workflow, then send the task as the next message:

```text
/mode review
fix the payment callback race and add a regression test
```

A new normal conversation requires an explicit first target. Later plain text
continues to the previous target; `@all` broadcasts to the roster. Open `/help`
inside Asterline for the command palette.

## What Asterline adds

### One transcript, not a wall of terminals

Every message, reasoning update, tool call, diff, error, and handoff stays on the
member that produced it. Markdown, code blocks, tables, and working-tree diffs
render directly in the TUI. `/logs` keeps raw diagnostics available without
flooding the main conversation, while `/focus <member>` isolates one member.

### Native CLIs, one live roster

A team may mix providers or use the same backend more than once. Members can
carry a role, model, reasoning effort, working directory, system prompt,
sandbox, permission mode, tool allowlist, and session policy. `/team` updates
the roster while Asterline is running.

![Asterline Team editor](docs/assets/asterline-team.webp)

| Backend | Executable | Streaming                         | Resume | Model choices                  |
| ------- | ---------- | --------------------------------- | ------ | ------------------------------ |
| Codex   | `codex`    | `codex exec --json`               | Yes    | `codex debug models`           |
| Claude  | `claude`   | stream JSON with partial messages | Yes    | aliases and `availableModels`  |
| Grok    | `grok`     | ACP over `grok agent stdio`       | Yes    | `grok --no-auto-update models` |
| Agy     | `agy`      | `stream-json` print events        | Yes    | `agy models`                   |

Asterline does not replace provider authentication, billing, model access, or
usage limits. Those remain properties of each CLI account.

### Runs instead of loose agent turns

Runs preserve phase, checklist ownership, attempts, blockers, notes,
verification, verdicts, and the next action. Select a mode first; the following
message becomes its task.

| Mode         | Use it for                         | What Asterline does                                    |
| ------------ | ---------------------------------- | ------------------------------------------------------ |
| `normal`     | Direct work with one/all members   | Routes ordinary messages and remembers the last target |
| `review`     | Implementation with a quality gate | Loops builder → structured reviewer verdict → revision |
| `plan`       | Multi-step owned work              | Plans a checklist, dispatches owners, then reviews     |
| `brainstorm` | Broad exploration before judgment  | Runs seed/build/stretch, private vote, rank, synthesis |
| `team`       | End-to-end coordinated delivery    | Lets a coordinator own steps, integrate, and verify    |

Review and plan runs enforce bounded iteration. Brainstorm separates generation
from private voting and deterministic ranking. Team mode gives a coordinator
ownership of the checklist, integration, and verification. `/runs` shows only
the Runs attached to the current conversation.

When work must wait or needs an explicit check:

```text
/block waiting for the staging client secret
/note secret requested from the platform team
/continue secret is now available
/verify cargo test
```

Without an explicit command, Asterline can detect common checks such as
`cargo test`, `npm test`, and `pytest`.

### Local state that can be resumed and inspected

`/new` saves the current conversation and starts clean backend sessions.
`/resume` restores a selected transcript together with its roster, native
session IDs, mode, and Runs. `Ctrl+N` or `Ctrl+B` focuses a member; pressing
`Enter` opens that member's native interactive CLI and resumes its session when
supported.

By default, operational state stays inside the project:

```text
<workspace>/.asterline/
├── team.json
└── asterline.sqlite3
```

The database contains prompts, responses, tool events, routes, raw backend
events, logs, approvals, sessions, and Run history. Treat it as sensitive
development data and normally add `.asterline/` to `.gitignore`.

Asterline may also install workspace-local team and brainstorm Skills under
`.agents/skills/`. These are integration files rather than runtime history;
review them and decide whether the project should version them.

## How it works

```text
You
 └─ target a member, the whole team, or a tracked workflow
     ├─ Asterline launches or resumes the selected backend CLI
     ├─ native events become chat, tools, diffs, logs, and session state
     ├─ valid teammate envelopes are routed to other members
     └─ messages, routes, Runs, approvals, and verification persist to SQLite
```

Automatic teammate relays are bounded. When a turn reaches its configured
limit, Asterline pauses the route and makes that state visible instead of
allowing an uncontrolled agent loop.

## Trust model and boundaries

Asterline starts backend processes locally and inherits their credentials,
environment variables, filesystem access, and network access. No Asterline
cloud service receives the workspace, but each backend still follows its own
vendor data policy and network behavior.

Members may use backend-native sandbox and permission settings. Asterline adds
a configurable approval gate for risky user requests, agent-to-agent relays,
workflow dispatches, and agent-originated roster changes. It does not provide a
second process-level sandbox beyond the selected backend. `--debug` disables
the Asterline gate and is intended only for controlled development.

Read [approvals and tool-level control](docs/approvals.md) before relaxing
permissions. Choose a different setup if every agent must receive an isolated
worktree, if you need a hosted dashboard or remote queue, if you require direct
provider APIs, or if unattended merge automation is the goal.

## Essential commands

| Command                | Purpose                                       |
| ---------------------- | --------------------------------------------- |
| `@<member> <message>`  | Send to one member                            |
| `@all <message>`       | Broadcast to the team                         |
| `/mode`                | Choose normal or a collaboration mode         |
| `/runs`                | Inspect Run state, phase, and next actions    |
| `/team`                | Edit the live roster                          |
| `/skills`              | Select a Skill for the next prompt            |
| `/find <text>`         | Search the transcript                         |
| `/diff`                | Inspect unstaged changes and untracked files  |
| `/logs`                | Open persisted diagnostics                    |
| `/new`                 | Start a new conversation and backend sessions |
| `/resume`              | Choose and restore a saved team conversation  |
| `/approve` / `/reject` | Resolve a pending approval                    |
| `/abort`               | Cancel running work, modes, and verification  |
| `/help`                | Open the command palette                      |

The [complete command and keyboard reference](docs/commands.md) covers Run
steps, Team controls, history, session attach, and navigation.

## Documentation

- [Commands and keyboard](docs/commands.md)
- [Configuration, local data, permissions, and troubleshooting](docs/configuration.md)
- [Approval layers and tool-level control](docs/approvals.md)
- [Latest release notes](docs/releases/v0.2.2.md)
- [Release process for maintainers](docs/releasing.md)
- Built-in help: `/help` and `asterline --help`

## Development and contributing

Run the product without calling real backends:

```bash
cargo run -- --fake
```

Run the local quality gate:

```bash
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked --no-fail-fast
```

If `just` is installed, use `just run --fake`, `just install`, or `just check`.
Real-backend smoke tests are opt-in and are not run by default.

Use [GitHub Issues](https://github.com/song0705/Asterline/issues) for
reproducible bugs and focused feature proposals. Include the operating system,
terminal, Asterline version, backend CLI/version, relevant sanitized `/logs`,
and the smallest reproduction.

## Project status

Asterline is currently version `0.2.2` and under active development. Tagged
versions publish prebuilt Linux, macOS, and Windows archives. Before a stable
release, configuration, persisted data, commands, and UI details may change
without backward compatibility.

## License

Asterline is available under the [MIT License](LICENSE).
