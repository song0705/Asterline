# Asterline command and keyboard reference

This is the complete reference for Asterline's startup options, composer
syntax, slash commands, drawers, and keyboard controls. For installation and
the product overview, return to the [main README](../README.md). A
[Chinese version](commands.zh-CN.md) is also available.

## Quick start

```text
@builder inspect the repository and explain the architecture
/mode brainstorm
How could we reduce indexing latency?
/runs
/verify run-2 cargo test
/new
/resume
```

Type `/` to list Asterline commands and `@` to list team members. Use `↑` and
`↓` to select a suggestion, `Tab` or `Enter` to insert it, and `Esc` to close
the suggestion list.

## Syntax used on this page

- `<value>` is required; `[value]` is optional. Do not type the angle or square
  brackets.
- `run-<id>` means a displayed run ID such as `run-2`.
- `<message>`, `<note>`, `<reason>`, `<title>`, and `[command]` consume the
  remainder of the line, so they may contain spaces.
- Commands and mode names are lowercase.
- When `[run-<id>]` is omitted, the command selects the latest run in the
  current conversation.
- Missing required arguments, invalid arguments, and unknown slash commands
  open `/help` instead of being sent to an agent.

## Starting Asterline

```text
asterline [OPTIONS]
```

Both `--option value` and `--option=value` are accepted for `--team`,
`--workspace`, and `--db`.

### `--team <PATH>`

Load a JSON team configuration from `PATH` and skip the interactive team
builder.

```bash
asterline --team .asterline/team.json
```

### `--pick-team`

Open the interactive team builder even when Asterline has a saved team.

```bash
asterline --pick-team
```

### `--workspace <PATH>`

Set the working directory inherited by team members. The default is the
directory in which Asterline is started.

```bash
asterline --workspace /path/to/project
```

### `--db <PATH>`

Set the SQLite database path. By default it is
`<workspace>/.asterline/asterline.sqlite3`.

```bash
asterline --db /path/to/asterline.sqlite3
```

### `--no-restore`

Start without replaying the most recently persisted conversation. This does
not delete saved conversations; use `/resume` to select one later.

```bash
asterline --no-restore
```

### `--debug`

Enable developer mode and disable Asterline's risky-action approval gate.
Backend-native permissions still apply. Use this only in a controlled
environment.

```bash
asterline --debug
```

### `--fake`

Use deterministic offline fake agents instead of installed backend CLIs. This
is intended for development, demos, and tests.

```bash
asterline --fake
```

### `--banner`

Print a compact startup banner before entering the TUI.

```bash
asterline --banner
```

### `-h`, `--help`

Print command-line help and exit. This is different from the in-product
`/help` command.

```bash
asterline --help
```

## Sending messages

### `@<member> <message>`

Send a message to one team member. The member name must exist in `/team`.

```text
@builder implement the parser change and run its tests
```

In a fresh normal conversation, the first message needs an explicit target.
Later plain text reuses the previous target. In a collaboration mode, plain
text starts or continues that mode using its configured participants.

### `@all <message>`

Send the same message to every enabled member.

```text
@all review the proposed API and identify one risk
```

### `@<member> /<skill> [arguments]`

Invoke a discovered skill through a specific member's native CLI. Type `/`
after the member prefix to open skill completion.

```text
@builder /asterline-team inspect the active run
```

For Codex, Asterline translates the invocation to native `$skill` syntax.
Claude, Grok, and Agy receive `/skill`. A bare `/skill` is not an Asterline
command; it must follow an explicit member target. Interactive backend-only
commands such as native model pickers require [native session
attach](#native-session-attach).

## Message and conversation commands

### `/ask`

```text
/ask <member|all> <message>
```

Send to one named member, or use `all` to broadcast. This is the slash-command
equivalent of `@member` and `@all`.

```text
/ask reviewer check the error handling
/ask all summarize your current progress
```

### `/all`

```text
/all <message>
```

Broadcast the message to every enabled member.

```text
/all stop editing and report findings
```

### `/new`

```text
/new
```

Persist the current conversation, create a new conversation, clear the visible
transcript and current run list, create fresh backend session IDs, and reset
the terminal mode to `normal`. Any active collaboration run is recorded as
superseded.

`/clear` is intentionally not a separate command. Typing `/cl` or `/clear`
offers `/new` in completion, so accepting it performs the full new-conversation
operation instead of merely hiding history.

### `/resume`

```text
/resume
```

Open the saved-conversation picker. Select a conversation with `↑` or `↓` and
press `Enter` to restore its transcript, team configuration, roster, native
backend session IDs, active mode, and conversation-scoped runs. Press `Esc` to
cancel.

`/resume` accepts no ID or other argument. Asterline refuses to switch while
members or verification are active; use `/abort` first.

### `/retry`

```text
/retry
```

Re-send the most recent user request through the currently active mode. It
does not resume a blocked run or a paused approval route; use `/continue` or
`/approve` for those cases. If the conversation has no previous user request,
nothing is sent.

### `/abort`

```text
/abort
```

Cancel all running members, queued dispatches, active verification, and paused
routes. Active collaboration or team runs are marked blocked with an
user-aborted reason. Use this before `/resume` when work is still active.

### `/approve`

```text
/approve
```

Approve the oldest pending Asterline approval request. If no request is
pending, Asterline reports that there is nothing to approve.

### `/reject`

```text
/reject
```

Reject the oldest pending Asterline approval request. If no request is
pending, Asterline reports that there is nothing to reject.

## Team, model, and diagnostics commands

### `/team`

```text
/team
```

Open the live Team editor. Opening it refreshes installed Codex, Claude, Grok,
and Agy executables and automatically discovers each available backend's
models and reasoning-effort choices. Missing CLIs remain visible for diagnosis
but cannot be selected.

The editor changes the roster, backend, role, model, effort, working directory,
native session ID, approval behavior, and default target. Changes stay in a
draft until `s` applies and saves them. See [Team editor keys](#team-editor).

### `/effort`

```text
/effort <member> <level>
```

Set one member's reasoning effort and persist it with the conversation.
Supported levels are model-dependent:

- General choices: `low`, `medium`, `high`, `xhigh`, `max`.
- Agy accepts `low`, `medium`, or `high`.
- Codex additionally accepts `ultra` when the selected model advertises it.

```text
/effort builder high
```

Use the model picker in `/team` when possible: it applies the model and one of
that model's discovered effort levels together. An unsupported level or an
unknown member is rejected.

### `/skills`

```text
/skills
```

Rescan workspace and user skill directories, then open the skill picker.
`Enter` or `Tab` stages the selected skill invocation in the composer for the
default target (or first member); it does not execute the skill until the
message is submitted.

### `/focus`

```text
/focus <member>
```

Open the log drawer filtered to one member. This is useful for inspecting its
stdout, stderr, tool activity, and adapter warnings.

```text
/focus builder
```

### `/logs`

```text
/logs
```

Open persisted runtime logs, including backend stderr and adapter/runtime
warnings. Use `/focus <member>` for a member-specific view.

### `/diff`

```text
/diff
```

Open a live working-tree view containing `git diff` output plus untracked file
information. It does not stage, revert, or otherwise modify files.

### `/find`

```text
/find [text]
```

Search the current transcript case-insensitively. The footer shows the current
match and total, such as `find: "timeout" (2/5)`. With an empty composer and no
drawer open, press `n` for the next match or `p` for the previous match.
`/find` with no text, or `Esc`, clears the search.

```text
/find timeout
```

### `/help`

```text
/help
```

Open the command palette. Unknown slash commands and commands with invalid or
missing required arguments also open this palette.

## Collaboration modes

### `/mode`

```text
/mode <normal|review|plan|brainstorm|team>
```

Choose how subsequent plain-text prompts are dispatched. `/mode` only selects
the mode; enter the task as the next message. The choice remains active in the
current conversation until another `/mode` changes it. `/new` resets the new
conversation to `normal`, while `/resume` restores the selected conversation's
mode.

#### `/mode normal`

Use ordinary direct-message dispatch. A fresh chat requires `@member`,
`@all`, `/ask`, or `/all`; later plain text can reuse the previous target.

#### `/mode review`

Start a builder/reviewer loop. The builder works, the reviewer emits a
structured `@@review` verdict, and revision continues until approval or
`max_iterations`. If the limit is reached, the run becomes blocked.

```text
/mode review
Refactor the parser without changing its public behavior
```

#### `/mode plan`

Start a leader-driven planning run. The leader creates a checklist, dispatches
work, tracks step state, and uses a reviewer loop before completion.

```text
/mode plan
Migrate the cache format and validate backward compatibility
```

#### `/mode brainstorm`

Start a structured multi-participant brainstorm. A complete run performs
judgment-free `seed`, `build`, and `stretch` generation waves, then collects
private ranked ballots, calculates the ranking, and synthesizes the selected
ideas while preserving dissent and evidence. Idea cards and ballots follow the
bundled Asterline brainstorm skill so they can be extracted reliably.

```text
/mode brainstorm
How could we make graph retrieval robust without node text?
```

#### `/mode team`

Start a coordinator-driven team run. The coordinator creates and owns the
checklist, dispatches work to teammates, integrates results, and can
automatically verify completion according to `modes.team` configuration.
Verification failure can return to the coordinator for repair until the
configured iteration limit is reached.

```text
/mode team
Implement the feature, review it, update docs, and run the test suite
```

Mode roles, participants, iteration limits, and verification settings are
defined in `team.json`. Reviewers communicate a one-line verdict such as:

```text
@@review {"verdict":"approve","summary":"LGTM"}
```

## Run commands

Runs belong to the current conversation. `/new` starts with no runs, and
`/resume` restores only the selected conversation's runs.

### `/runs`

```text
/runs
```

Open the Runs drawer to inspect run ID, mode, phase, status, contributions,
verification, checklist, timeline, and suggested next action. See [Runs drawer
keys](#runs-drawer).

### `/continue`

```text
/continue [run-<id>] [note]
```

Resume a blocked or failed mode/team run, optionally giving the coordinator or
mode engine a note. Without a run ID, the latest run in this conversation is
selected. An already active run cannot be continued, and legacy runs without
persisted mode state cannot be reconstructed.

```text
/continue run-4 retry with the newly installed dependency
/continue use the simpler fallback
```

### `/note`

```text
/note [run-<id>] <text>
```

Append a checkpoint to the run timeline without waking or dispatching an
agent.

```text
/note run-4 API contract confirmed with the reviewer
```

### `/block`

```text
/block [run-<id>] <reason>
```

Mark the selected run blocked and record the reason. A run currently being
verified must be aborted before it can be blocked manually.

```text
/block run-4 waiting for the schema decision
```

### `/verify`

```text
/verify [run-<id>] [command]
```

Run a verification command in the workspace in the background and store its
result on the run. Without a command, Asterline detects a suitable project
check such as `cargo test`, `npm test`, or `pytest`. Without a run ID, it uses
the latest run.

```text
/verify
/verify run-4 cargo test --all-targets
```

Verification cannot start while the selected run is active or another
verification is already running. In configured mode/team runs, failure may
trigger an automatic repair iteration.

### `/step`

Manage the checklist of the latest or explicitly selected run. Step numbers
start at `1`.

#### Add a step

```text
/step add [run-<id>] [@owner] <title>
```

Add a checklist item, optionally assigning it immediately.

```text
/step add run-4 @builder implement the migration
```

#### Change step status

```text
/step todo [run-<id>] <n> [note]
/step doing [run-<id>] <n> [note]
/step done [run-<id>] <n> [note]
/step block [run-<id>] <n> [note]
```

Set a step to todo, in progress, complete, or blocked. `/step blocked` is an
alias of `/step block`.

```text
/step doing run-4 2 started after API approval
/step done run-4 2 tests pass
```

#### Rename a step

```text
/step rename [run-<id>] <n> <title>
```

Replace a step's title. `/step edit` is an alias.

#### Remove a step

```text
/step remove [run-<id>] <n>
```

Delete a checklist item. `/step delete` and `/step drop` are aliases.

#### Assign or unassign a step

```text
/step assign [run-<id>] <n> <member>
/step unassign [run-<id>] <n>
```

Set or clear the owner. `member` may be written with or without `@`.
`/step owner` is an alias of `assign`; `/step clear-owner` and
`/step clear_owner` are aliases of `unassign`.

## Global keyboard controls

| Key                            | Action                                                        |
| ------------------------------ | ------------------------------------------------------------- |
| `Enter`                        | Send or accept the active selection                           |
| `Shift+Enter`                  | Insert a newline                                              |
| `Alt+Enter`                    | Newline fallback for terminals without distinct Shift+Enter   |
| `↑` / `↓`                      | Move in composer, history, or active selection                |
| `Tab`                          | Accept completion                                             |
| `Ctrl+R`                       | Reverse-search prompt history                                 |
| `n` / `p`                      | Next/previous `/find` match when composer is empty            |
| `PageUp` / `PageDown`          | Scroll chat or the open drawer                                |
| Mouse drag                     | Select and copy chat, status-bar, or drawer text              |
| Mouse wheel                    | Scroll chat or the open drawer                                |
| `Esc`                          | Close an overlay, clear find, or cancel running work          |
| `Ctrl+O` / `Ctrl+G` / `Ctrl+T` | Expand or collapse successful tool output                     |
| `Ctrl+L`                       | Open logs                                                     |
| `Ctrl+P`                       | Open command palette                                          |
| `Ctrl+N` / `Ctrl+B`            | Focus next/previous member                                    |
| `Ctrl+A` / `Ctrl+E`            | Move to line start/end                                        |
| `Ctrl+U`                       | Clear current line                                            |
| `Ctrl+W`                       | Delete previous word                                          |
| `Ctrl+C`                       | Cancel, clear composer, or arm quit when idle                 |

Prompt history behaves like a shell: `↑` and `↓` preserve the current draft
while browsing older submissions. During `Ctrl+R`, type to refine the match,
press `Ctrl+R` again for an older match, `Enter` to accept, or `Esc` to cancel.

## Team editor

The Team editor has member-selection and field-selection levels.

| Key       | Member selection                            | Field selection                          |
| --------- | ------------------------------------------- | ---------------------------------------- |
| `↑` / `↓` | Select a member                             | Select a field or model                  |
| `←` / `→` | —                                           | Select effort in the model list          |
| `Enter`   | Open member fields                          | Edit or open Agent/model/session picker  |
| `Esc`     | Close Team                                  | Return to member selection               |
| `a` / `d` | Add/delete a member                         | —                                        |
| `t`       | Make member the default target              | —                                        |
| `*`       | Make all members the default target         | —                                        |
| `s`       | Apply and save                              | Apply and save                           |
| `e`       | —                                           | Manually enter model or session ID       |

Text fields open in a focused input box. Press `Enter` to commit or `Esc` to
cancel. Model pickers use `↑`/`↓` for model, `←`/`→` for that model's effort,
and `Enter` to apply both. When discovery returns a catalog, the initial choice
is the CLI-marked default model or the first discovered model; `default`
appears only when no model is discovered.

On `session id`, `Enter` opens Asterline's native-session table. It reads local
Codex, Claude, or Grok history, shows title/project/update time/native ID, and
filters sessions to the member's effective working directory. Type to filter,
move with `↑`/`↓` or `PageUp`/`PageDown`, and press `Enter` to stage the ID;
press `s` to save it. Use `e` for manual entry. Agy currently requires manual
entry because Asterline has no verified local Agy history format.

## Runs drawer

| Key                   | Action                                                       |
| --------------------- | ------------------------------------------------------------ |
| `←` / `→`             | Select older/newer run                                       |
| `↑` / `↓`             | Select checklist step; select run when no step exists        |
| `x`                   | Toggle compact/detail view                                   |
| `Enter`               | Stage selected step status or run next action in composer    |
| `Tab`                 | Stage editable dispatch to selected step owner               |
| `PageUp` / `PageDown` | Scroll details                                               |
| `Esc`                 | Close drawer                                                 |

`Enter` and `Tab` stage text in the composer; they do not execute it
immediately. Changing runs clears the selected step.

## Other drawers

All drawers support `PageUp`/`PageDown` or the mouse wheel for scrolling and
`Esc` to close. Text in chat, status bars, and drawers can be selected with a
mouse drag and copied using the terminal's normal copy shortcut.

## Native session attach

Press `Ctrl+N` or `Ctrl+B` to focus the roster, move with `←` or `→`, and press
`Enter`. Asterline suspends its TUI and opens that member's native interactive
CLI. Exit the CLI with `/exit` or `Ctrl+D` to return.

Messages created while attached to Codex or Claude are imported into the
Asterline transcript. Grok and Agy resume their native sessions but attached
messages are not currently imported.
