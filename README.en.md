<h1 align="center">Asterline</h1>

<p align="center">
  <strong>Run a visible, persistent team of coding agents in one terminal.</strong><br>
  Coordinates the Codex, Claude, Grok, and Agy CLIs already on your machine.
</p>

<p align="center"><sub><a href="README.md">中文</a> · English</sub></p>

<p align="center">
  <img src="docs/assets/chat.webp" alt="Asterline teammates handing work to each other" width="100%">
</p>

<p align="center">
  <a href="https://github.com/song0705/Asterline/actions/workflows/ci.yml"><img src="https://github.com/song0705/Asterline/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/song0705/Asterline/releases/latest"><img src="https://img.shields.io/github/v/release/song0705/Asterline" alt="Latest release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT License"></a>
</p>

Asterline folds each provider's native event stream into one workspace. Messages, reasoning, tools, diffs, approvals, and handoffs stay attached to the member that produced them. Multi-step work is recorded as structured Runs, so you can close the terminal and continue later.

[Install](#installation) · [Get started](#get-started) · [Modes](#modes) · [Documentation](#documentation) · [Releases](https://github.com/song0705/Asterline/releases/latest)

The default product README is [Chinese](README.md). This page is the English counterpart.

## Installation

Asterline supports macOS, Linux, and Windows 10/11. It coordinates provider CLIs; it does not install or sign in to Codex, Claude, Grok, or Agy for you. Have at least one of those CLIs installed and authenticated first.

| Platform | How to install                                                                              |
| -------- | ------------------------------------------------------------------------------------------- |
| macOS    | Open `asterline-<version>-macos-universal.dmg` and run `Install Asterline.pkg`              |
| Windows  | Run `asterline-<version>-x86_64-windows-setup.exe`                                          |
| Linux    | Choose `asterline-v<version>-Linux-x86_64` or `Linux-arm64` as `.tar.gz`, `.deb`, or `.rpm` |
| Homebrew | `brew install song0705/asterline/asterline`                                                 |

Installers provide both `asterline` and the shorter `ast` command. Linux archives target GNU/glibc 2.28 or newer and embed SQLite; Alpine/musl is not a release target. See the [installation guide](docs/installation.md) for Homebrew, portable packages, source builds, verification, updates, and uninstall.

## Get started

Open the project the agents should work on:

```bash
cd /path/to/your/project
ast
```

On first launch, the Team editor discovers supported CLIs on `PATH`. Select a member with `↑`/`↓`, press `Enter` to edit, and press `s` to save.

<p align="center">
  <img src="docs/assets/team.webp" alt="Asterline Team editor" width="100%">
</p>

In a normal conversation, send the first task to a member on the roster:

```text
@builder audit this repository and fix the highest-risk defect
```

For structured work, open the mode picker first:

```text
/mode
```

<p align="center">
  <img src="docs/assets/mode.webp" alt="Asterline mode picker" width="100%">
</p>

Use `↑`/`↓` to highlight a mode, `Enter` to edit optional fields, and `s` to apply it to this conversation. `/mode review` still works as a direct shortcut. After selecting `review`, `plan`, `brainstorm`, or `team`, type the task as plain text — no `@member` prefix. A fresh `normal` conversation still needs an explicit target.

```text
/mode review
fix the payment callback race and add a regression test
```

Use `/help` for the command palette and `/runs` to inspect active and saved work.

## Modes

Asterline is not several chat windows stacked together. Each mode has owners, steps, a retry bound, and a next action.

<table>
  <tr>
    <td valign="top">
      <h3>Review</h3>
      <p>One member implements; another verdicts against the tree. Bounded revision until pass or the iteration limit.</p>
      <img src="docs/assets/review.webp" alt="Review mode implementing and handing off to a reviewer" width="100%">
      <img src="docs/assets/review-done.webp" alt="Review mode verdict against the tree" width="100%">
    </td>
  </tr>
  <tr>
    <td valign="top">
      <h3>Plan</h3>
      <p>Write an executable plan, optionally review it, then execute and verify step by step.</p>
      <img src="docs/assets/plan.webp" alt="Plan mode executing and verifying steps" width="100%">
      <img src="docs/assets/plan-done.webp" alt="Plan mode with all steps completed" width="100%">
    </td>
  </tr>
  <tr>
    <td valign="top">
      <h3>Brainstorm</h3>
      <p>Generate idea cards first, then vote privately, rank, and synthesize. Judgment comes later.</p>
      <img src="docs/assets/brainstorm.webp" alt="Brainstorm mode generating idea cards" width="100%">
      <img src="docs/assets/brainstorm-vote.webp" alt="Brainstorm mode private voting and synthesis" width="100%">
    </td>
  </tr>
  <tr>
    <td valign="top">
      <h3>Team</h3>
      <p>A coordinator splits work, assigns it, and integrates results. The default roster is locked.</p>
      <img src="docs/assets/team-run.webp" alt="Team mode: coordinator splits work and adds teammates" width="100%">
      <img src="docs/assets/team-done.webp" alt="Team mode: review passed and handed back" width="100%">
    </td>
  </tr>
</table>

`/runs` records that work as something you can reopen: goal, phase, checklist, owners, and the next action. Close the terminal and continue later.

<p align="center">
  <img src="docs/assets/runs.webp" alt="Asterline /runs panel" width="100%">
</p>

`normal` remains direct chat and delegation. Runs stay actionable when work pauses:

```text
/block waiting for the staging client secret
/note secret requested from the platform team
/continue secret is now available
```

A team can mix providers or use the same backend more than once. Each member can have its own role, model, reasoning setting, working directory, system prompt, sandbox, permission mode, tool allowlist, and session policy.

| Backend | Integration                 | Session resume | Model discovery                |
| ------- | --------------------------- | -------------- | ------------------------------ |
| Codex   | Persistent App Server       | Yes            | App Server `model/list`        |
| Claude  | Streaming JSON              | Yes            | CLI settings and aliases       |
| Grok    | ACP over `grok agent stdio` | Yes            | `grok --no-auto-update models` |
| Agy     | `stream-json` events        | Yes            | `agy models`                   |

Asterline does not replace provider authentication, billing, model access, or usage limits.

Reopening Asterline resumes the last conversation by default. `/new` and `/clear` both start a clean conversation and keep the mode settings. `/resume` restores a saved transcript, roster, backend sessions, mode, and Runs. Project state lives in:

```text
<workspace>/.asterline/
├── team.json
├── roster.md
└── asterline.sqlite3
```

The database may contain prompts, responses, tool output, routes, approvals, logs, and session identifiers. Treat it as sensitive development data and add `.asterline/` to `.gitignore` unless you intend to version it.

## Safety model

Asterline starts backend processes locally and inherits their credentials, environment, filesystem access, and network access. Each backend remains subject to its provider's data policy and permission model.

Codex tool asks are approved automatically unless you pass `--manual-approvals` or set `"approvals": { "manual": true }` in `team.json`. Then a card appears above the composer; press `y` or `n`. Asterline does not add a second process sandbox beyond the selected backend. Read [Approvals and tool control](docs/approvals.md) before relaxing permissions.

## Essential commands

| Command                | Purpose                             |
| ---------------------- | ----------------------------------- |
| `@<member> <message>`  | Send a task to one member           |
| `@all <message>`       | Broadcast to the roster             |
| `/mode`                | Open the mode picker                |
| `/runs`                | Inspect work state and next actions |
| `/team`                | Edit the live roster                |
| `/resume`              | Restore a saved conversation        |
| `/approve` / `/reject` | Resolve a pending approval          |
| `/help`                | Open the command palette            |

See the [complete command and keyboard reference](docs/commands.md) for drawers, Run steps, targeted skills, logs, search, and diffs.

## Documentation

This page is the English product overview. Read by goal:

### User documentation

| Goal                                | Entry                                              |
| ----------------------------------- | -------------------------------------------------- |
| Install, update, uninstall          | [Installation](docs/installation.md)               |
| Commands and shortcuts              | [Commands and keyboard](docs/commands.md)          |
| Team files, permissions, local data | [Configuration](docs/configuration.md)             |
| Who asks, and what auto-passes      | [Approvals and tool control](docs/approvals.md)    |
| What changed in this version        | [v1.0.4 release notes](docs/releases/v1.0.4.en.md) |
| Full documentation map              | [Documentation index](docs/README.en.md)           |

### Developer and maintainer documentation

| Goal                                 | Entry                                           |
| ------------------------------------ | ----------------------------------------------- |
| Local quality gate and fake backends | [Development](#development) below               |
| Real CLI smoke tests                 | [Real-backend smoke tests](docs/real-smoke.md)  |
| Tags, preflight, and publishing      | [Maintainer release process](docs/releasing.md) |
| Third-party package definitions      | [packaging](packaging/README.md)                |

Built-in help is also available through `/help` and `asterline --help`.

## Development

Run Asterline without invoking real backends:

```bash
cargo run -- --fake
```

Local quality gate:

```bash
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked --no-fail-fast
cargo audit
```

Rust 1.88 or newer and `cargo-audit` 0.22.2 are required for this complete gate. Real-backend smoke tests are opt-in; see the [controlled local and Actions entrypoints](docs/real-smoke.md). Use [GitHub Issues](https://github.com/song0705/Asterline/issues) for reproducible bugs and focused proposals.

## Project status

Asterline is on the 1.0.x line and still under active development. Configuration, persisted data, commands, and interface details may change between releases.

## Acknowledgments

Thanks to the [LINUX DO](https://linux.do) community.

## License

Asterline is available under the [MIT License](LICENSE).
