# Asterline

[English](README.md) · [简体中文](README.zh-CN.md)

[![CI](https://github.com/song0705/Asterline/actions/workflows/ci.yml/badge.svg)](https://github.com/song0705/Asterline/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/song0705/Asterline)](https://github.com/song0705/Asterline/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**Run a visible, persistent team of coding agents in one terminal.**

Asterline coordinates the official Codex, Claude, Grok, and Agy CLIs already
installed on your machine. It combines their native event streams into one
auditable workspace, routes work between team members, and records multi-step
work as structured Runs.

[Install](#installation) · [Get started](#get-started) ·
[Documentation](#documentation) ·
[Releases](https://github.com/song0705/Asterline/releases/latest)

![Codex handing a frontend design proposal to Agy](docs/assets/asterline-codex-to-agy.webp)

## Why Asterline

- **One workspace.** Messages, reasoning, tools, diffs, errors, approvals, and
  handoffs stay attached to the member that produced them.
- **Structured collaboration.** Review, plan, brainstorm, and team workflows add
  ownership, bounded retries, verdicts, and verification to agent work.
- **Native integrations.** Asterline launches the provider CLIs directly and
  preserves their authentication, models, permissions, and resumable sessions.
- **Local control.** Team configuration and operational history remain in the
  project workspace, with explicit approval and cancellation controls.

## Installation

Asterline supports macOS, Linux, and Windows 10/11. At least one supported CLI
must already be installed and authenticated.

- **macOS:** Download `asterline-<version>-macos-universal.dmg`, open it, and
  run `Install Asterline.pkg`.
- **Windows:** Download and run `asterline-<version>-x86_64-windows-setup.exe`.
- **Linux:** Choose `asterline-v<version>-Linux-x86_64` or
  `asterline-v<version>-Linux-arm64` as a `.tar.gz`, `.deb`, or `.rpm`.
- **Homebrew (macOS and Linux):** `brew install song0705/asterline/asterline`.

Linux archives target GNU/glibc 2.28 or newer and embed SQLite; Alpine/musl is
not a supported release target.

The installers provide both `asterline` and the shorter `ast` command. See the
[installation guide](docs/installation.md) for Homebrew, portable packages, source builds,
release verification, updates, uninstallation, and troubleshooting.

## Get started

Open the project that the agents should work on, then start Asterline:

```bash
cd /path/to/your/project
ast
```

On first launch, the Team editor discovers supported CLIs on `PATH`. Select a
member with `↑`/`↓`, press `Enter` to edit, and press `s` to save the roster.
Asterline never installs a provider CLI or signs in on your behalf.

Send the first task to a member shown in the roster:

```text
@builder audit this repository and fix the highest-risk defect
```

Or select a tracked workflow before entering the task:

```text
/mode review
fix the payment callback race and add a regression test
```

Use `/help` for the command palette and `/runs` to inspect active and saved
work.

## Built for traceable agent work

### Live teams over native CLIs

A team can mix providers or use the same backend more than once. Each member can
have its own role, model, supported reasoning setting, working directory, system prompt,
sandbox, permission mode, tool allowlist, and session policy.

![Asterline Team editor](docs/assets/asterline-team.webp)

| Backend | Integration                                         | Session resume    | Model discovery                               |
| ------- | --------------------------------------------------- | ----------------- | --------------------------------------------- |
| Codex   | Persistent App Server                               | Yes               | App Server `model/list`                       |
| Claude  | Streaming JSON                                      | Yes               | CLI settings and aliases                      |
| Grok    | ACP over `grok agent stdio`                         | Yes               | `grok --no-auto-update models`                |
| Agy     | `stream-json` events                                | Yes               | `agy models`                                  |

Asterline does not replace provider authentication, billing, model access, or
usage limits.

### Runs instead of loose turns

Runs preserve the current phase, checklist owners, attempts, blockers, notes,
verification results, verdicts, and next action.

| Mode         | Best for                           | Execution model                                  |
| ------------ | ---------------------------------- | ------------------------------------------------ |
| `normal`     | Direct chat and delegation         | Route to one member or the full roster           |
| `review`     | Implementation with a quality gate | Builder, reviewer verdict, bounded revision loop |
| `plan`       | Multi-step owned work              | Plan, assign, execute, review, verify            |
| `brainstorm` | Exploration before judgment        | Generate, vote privately, rank, synthesize       |
| `team`       | End-to-end coordinated delivery    | Coordinator-owned execution and integration      |

Runs remain actionable when work pauses:

```text
/block waiting for the staging client secret
/note secret requested from the platform team
/continue secret is now available
/verify cargo test
```

### Local, resumable history

Reopening Asterline resumes the last conversation by default. `/new` and
`/clear` are equivalent: each starts a clean conversation. `/resume` restores a
saved transcript, roster, backend sessions, mode, and Runs. Project state is
stored by default in:

```text
<workspace>/.asterline/
├── team.json
└── asterline.sqlite3
```

The database may contain prompts, responses, tool output, routes, approvals,
logs, and session identifiers. Treat it as sensitive development data and add
`.asterline/` to `.gitignore` unless the project has a deliberate reason to
version it.

## Safety model

Asterline runs backend processes locally and inherits their credentials,
environment, filesystem access, and network access. Each backend remains subject
to its provider's data policy and permission model.

Asterline adds approval gates for risky requests, agent-to-agent relays,
workflow dispatches, and agent-originated roster changes. It does not add a
second process sandbox beyond the selected backend. Read
[Approvals and tool control](docs/approvals.md) before relaxing permissions.

## Essential commands

| Command                | Purpose                                     |
| ---------------------- | ------------------------------------------- |
| `@<member> <message>`  | Send a task to one member                   |
| `@all <message>`       | Broadcast to the roster                     |
| `/mode`                | Select a conversation or collaboration mode |
| `/runs`                | Inspect work state and next actions         |
| `/team`                | Edit the live roster                        |
| `/resume`              | Restore a saved conversation                |
| `/approve` / `/reject` | Resolve a pending approval                  |
| `/help`                | Open the command palette                    |

See the [complete command and keyboard reference](docs/commands.md) for session
attach, navigation, Run steps, targeted skills, logs, search, and diffs.

## Documentation

- [Installation and updates](docs/installation.md)
- [Commands and keyboard](docs/commands.md)
- [Configuration and local data](docs/configuration.md)
- [Approvals and tool control](docs/approvals.md)
- [Real-backend smoke tests](docs/real-smoke.md)
- [Release notes](docs/releases/v0.2.8.md)
- [Maintainer release process](docs/releasing.md)
- [Third-party package definitions](packaging/README.md)

Built-in help is available through `/help` and `asterline --help`.

## Development

Run Asterline without invoking real backends:

```bash
cargo run -- --fake
```

Run the local quality gate:

```bash
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked --no-fail-fast
cargo audit
```

Rust 1.88 or newer and `cargo-audit` 0.22.2 are required for this complete gate.
Real-backend smoke tests are opt-in; see the [controlled local and Actions
entrypoints](docs/real-smoke.md). Please use
[GitHub Issues](https://github.com/song0705/Asterline/issues) for reproducible
bugs and focused proposals.

## Project status

Asterline is pre-1.0 and under active development. Configuration, persisted
data, commands, and interface details may change between releases.

## License

Asterline is available under the [MIT License](LICENSE).
