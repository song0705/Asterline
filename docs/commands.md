# Asterline command and keyboard reference

This is the complete reference for Asterline's startup options, composer
syntax, slash commands, drawers, and keyboard controls. For installation and
the product overview, return to the [English README](../README.en.md). A
[Chinese version](commands.zh-CN.md) is also available. The default product
page is the [Chinese README](../README.md).

## Quick start

```text
@builder inspect the repository and explain the architecture
/mode brainstorm
How could we reduce indexing latency?
/runs
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

### `--manual-approvals`

Show the approval card above the composer for Codex tool asks. Off by
default: those asks are approved automatically. `team.json` can set the
same switch with `"approvals": { "manual": true }`.

```bash
asterline --manual-approvals
```

### `--debug`

Developer mode. It no longer changes approval behavior.

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

### `update`

Run an explicit update for the installation method that owns the currently
running binary:

- A Windows Setup-managed installation keeps the verified installer flow: it
  checks the latest stable Release, verifies Setup against that Release's
  `SHA256SUMS`, schedules it after Asterline exits, then exits.
- A Homebrew installation on macOS or Linux runs `brew update` followed by
  `brew upgrade song0705/asterline/asterline`. Asterline first verifies that
  its own executable is inside that Formula's installed prefix.

Portable archives, direct `.deb`/`.rpm` installs, macOS packages, and source
builds are intentionally not overwritten. They print the appropriate manual
path instead of guessing where to replace files.

```bash
ast update
```

`ast --update` remains a backward-compatible alias.

### `--no-auto-update`

Skip the once-per-24-hours background update check for this launch of a Windows
Setup-managed copy. It does not disable future checks or affect an explicit
`--update`; it has no effect on platforms and copies that never self-update.

```powershell
ast --no-auto-update
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

Targeted completion inserts the native invocation discovered for that member.
Codex uses `$skill`; Claude plugin skills keep their `/plugin-name:skill`
namespace. For convenience, a manually typed
`@<codex-member> /skill` is converted only when it exactly matches a
discovered Codex skill. `@member /` offers the real `/attach` action and only
that member's discovered skills. `/attach` opens the [native
session](#native-session-attach), where the backend's own interactive
slash-command menu is available. Unknown targeted slash commands are kept out
of noninteractive runners unless they exactly match a discovered skill. Slash
controls always need one member; Asterline rejects `@all /…` rather than
broadcasting it.

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
the terminal mode to `normal`. Mode field settings from the previous chat
(reviewer, builder, limits, and other this-chat overrides) stay in effect for
the new conversation. New runs in that chat start again at `run-1`. If a
member, collaboration run, or verification is active, `/new` is rejected;
press `Esc` and wait for cancellation first.

`/clear` is a direct alias for `/new`; both perform the same full
new-conversation operation rather than merely hiding history. A normal restart
reopens the currently selected conversation instead of clearing it.

### `/resume`

```text
/resume
```

Open the saved-conversation picker. Select a conversation with `↑` or `↓` and
press `Enter` to restore its transcript, team configuration, roster, native
backend session IDs, active mode, and conversation-scoped runs. Press `Esc` to
cancel.

`/resume` accepts no ID or other argument. Asterline refuses to switch while
members or verification are active; press `Esc` first.

### `/retry`

```text
/retry
```

Re-send the most recent user request through the currently active mode. It
does not resume a blocked run or a paused approval route; use `/continue` or
`/approve` for those cases. If the conversation has no previous user request,
nothing is sent.

### `/attach`

```text
/attach <member>
@member /attach
```

Suspend Asterline and open that member's real interactive CLI, resuming its
existing backend session when one is available. Exit using the method supported
by that native CLI (usually its own `/exit`) to return automatically to
Asterline. A fresh Claude attach receives an Asterline-generated UUID through
`claude --session-id`, so its transcript is automatically imported and bound
to that member when you return. Codex imports only an already-bound session it
can identify safely; a Claude fork is imported only when its prior transcript
proves the lineage. Ambiguous native sessions are never guessed. Grok and Agy
can resume their sessions but do not yet import attached messages.

Claude's own interactive resume picker intentionally hides sessions created by
`claude -p` or the Agent SDK. To attach one of those Asterline sessions, open
`/team`, select the member's **session id** field, press `Enter`, choose the
session, and press `s`; then run `/attach <member>`. Asterline discovers the
transcript itself and resumes its UUID directly.

### `/import`

```text
/import <session_id>
/import <member> <session_id>
@member /import <session_id>
```

Import messages from a native Claude Code, Codex, or Grok session transcript into the
Asterline chat and bind that session to the member.

### `/export`

```text
/export
/export claude
```

Export the current Asterline conversation history into Claude Code's native session
JSONL storage (`~/.claude/projects/.../<session_id>.jsonl`), making it directly
visible and resumable via `claude --resume` (`claude -r`) in the project directory.

### `/exit`

```text
/exit
```

Exit Asterline immediately. Its normal shutdown path cancels active backend
work and restores the terminal. This is an Asterline command only; while
attached to a native backend CLI, that CLI's own `/exit` returns to Asterline.

### `/approve`

```text
/approve
```

Approve the selected pending request (oldest if you have not switched).
The usual path is the card above the composer: `y` or Enter to agree, `n`
to deny. If no request is pending, Asterline reports that there is nothing
to approve.

### `/reject`

```text
/reject
```

Reject the oldest pending Asterline approval request. If no request is
pending, Asterline reports that there is nothing to reject.

## Team and diagnostics commands

### `/team`

```text
/team
```

Open the live Team editor. Opening it refreshes installed Codex, Claude, Grok,
and Agy executables. Model catalogs are loaded asynchronously once at `ast`
startup, so the editor stays responsive and reports the actual detected model.
Focus the member's `model` field and press `t` to re-fetch it at any time; the
result is shared by matching backend/workspace members.
Missing CLIs remain visible for diagnosis but cannot be selected.

The editor changes the roster, backend, role, model, effort, native session ID,
approval behavior, and default target. Changes stay in a
draft until `s` applies and saves them. See [Team editor keys](#team-editor).

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
/mode
/mode <normal|review|plan|brainstorm|team>
```

`/mode` with no argument opens a small overlay on the chat (the same kind
of drawer as `/team`). The first layer lists every mode with its resolved
binding line and lands on the current conversation mode. Enter is the only
key that opens the highlighted mode's fields. Esc or q closes without
applying unsaved edits.

Each field shows the value currently in effect and where it came from:
`default`, `team.json`, or `this chat`. Edits stay pending until you apply
them. `s` selects that mode and applies its pending overrides to this
conversation. `w` writes the current mode's conversation overrides into
`team.json` as that team's defaults. `r`
clears the selected field's this-chat override so it falls back. Closing
without `s` or `w` discards pending edits.

`/mode <name>` stays the keyboard fast path: it switches immediately and
does not open the panel. After selecting `review`, `plan`, `brainstorm`, or
`team`, enter the task as the next plain message — do **not** add an
`@member` prefix. That message starts the selected mode with its configured
participants. `@member <message>` deliberately remains a one-to-one
instruction and bypasses the collaboration run. `/new` starts the new
conversation in `normal` but keeps the previous chat's mode field settings
(reviewer, builder, limits, …) so you do not have to reconfigure them in the
same project. `/resume` restores the selected conversation's mode and
overrides.

#### `/mode normal`

Use ordinary direct-message dispatch. A fresh chat requires `@member`,
`@all`, `/ask`, or `/all`; later plain text can reuse the previous target.

#### `/mode review`

Start a builder/reviewer loop. The builder works, the reviewer emits a
structured `@@review` verdict, and revision continues until approval or
`max_iterations`. If the limit is reached, the run becomes blocked. The
reviewer is asked to inspect the working tree rather than trusting the
builder's report. An optional `reviewer_hint` is appended to that prompt.
A reviewer reply
without a structured verdict is nudged once, then treated as
`request_changes` (a notice says so) and costs an iteration.

```text
/mode review
Refactor the parser without changing its public behavior
```

#### `/mode plan`

Start a leader-driven planning run. Plan needs a configured Builder; the leader
creates a checklist (steps do not need owners), then the complete checklist is
sent to that Builder. The Reviewer is optional. When configured, it audits only
the plan — completeness, ordering, risks, acceptance criteria, testability —
never code or diffs; requested changes return to the Leader for a revised
checklist. `modes.plan.auto_execute` defaults to `true`; set it to `false` to
pause at an explicit `/approve` before the Builder is sent the finalized plan.
Builder completion then runs verification when configured.

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
checklist, dispatches work to teammates, and integrates results. Repair
loops continue until the configured iteration limit is reached. Team mode
defaults to the current roster; turn on `allow_add_members` in the Mode
panel if the coordinator may add teammates (they join immediately, with
no `/approve` step).

```text
/mode team
Implement the feature, review it, update docs, and run the test suite
```

Mode roles, participants, and iteration limits are defined in `team.json`.
Reviewers communicate a one-line verdict such as:

```text
@@review {"verdict":"approve","summary":"LGTM"}
```

## Run commands

Runs belong to the current conversation. `/new` starts with no runs, and
the next run in that chat is `run-1`. `/resume` restores only the selected
conversation's runs.

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

Resume re-enters the phase the run was blocked in: review runs re-dispatch the
builder (or reviewer, if a review was pending), plan runs return to the leader
or re-dispatch unfinished owned steps, brainstorm runs re-run the current
generation wave (or voting/synthesis), and verification restarts when the run
was mid-verify. All roster members recorded in the run must still exist.

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

While a member is working, `Enter` queues the next message instead of
starting a second run. `Esc` interrupts the live member and then sends the
queued message. If that queued text has not started yet and you want to
change it, `Shift+←` pulls it back into the composer. The current
conversation can be scrolled all the way to its first message.

| Key                            | Action                                                        |
| ------------------------------ | ------------------------------------------------------------- |
| `Enter`                        | Send or accept the active selection                           |
| `Shift+Enter`                  | Insert a newline                                              |
| `Alt+Enter`                    | Newline fallback for terminals without distinct Shift+Enter   |
| `↑` / `↓`                      | Move in composer, history, or active selection                |
| `Tab`                          | Accept completion                                             |
| `Ctrl+R`                       | Reverse-search prompt history                                 |
| `n` / `p`                      | Next/previous `/find` match when composer is empty            |
| `PageUp` / `PageDown`          | Scroll chat or the open drawer by a page                      |
| `Shift+←`                      | Pull the last queued, not-yet-started message back to edit    |
| Mouse drag                     | Select and copy chat, composer, status-bar, or drawer text    |
| Mouse wheel                    | Scroll chat or the open drawer                                |
| `Esc`                          | Close overlay/find, or stop work and send the queued message  |
| `Ctrl+T`                       | Expand or collapse thinking                                   |
| `Ctrl+G`                       | Expand or collapse file-change diffs                          |
| `Ctrl+O`                       | Expand or collapse tool output                                |
| `Ctrl+L`                       | Open logs                                                     |
| `Ctrl+P`                       | Open command palette                                          |
| `Ctrl+N` / `Ctrl+B`            | Focus next/previous member                                    |
| `Ctrl+A` / `Ctrl+E`            | Move to line start/end                                        |
| `Ctrl+U` / `Cmd+Backspace`     | Clear the current composer line (other lines stay)            |
| `Ctrl+W`                       | Delete previous word                                          |
| `Ctrl+C`                       | Cancel, clear composer, or arm quit when idle                 |
| `Ctrl+V` / `Cmd+V`             | Attach a clipboard image to the next send (not Ctrl+C)        |

Screenshots and copied images are copied under the process temp directory
(`std::env::temp_dir()/asterline-pasted/<pid>/`: `%TEMP%` on Windows, `$TMPDIR`
on macOS, `/tmp` on Linux). Asterline deletes them itself — unused attachments
when you backspace or clear the composer, this process's directory on exit,
and leftover directories from dead processes on the next start. They are sent
natively: Codex `localImage`, Grok ACP image blocks, Claude/Agy as a readable
attached-image path. Up to four images per message. Pasting a PNG/JPEG/GIF/WebP
file path copies that file into the temp dir instead of inserting the path as
text.

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
| `t`       | Make member the default target              | Retry failed Model catalog               |
| `*`       | Make all members the default target         | —                                        |
| `s`       | Apply and save                              | Apply and save                           |
| `e`       | —                                           | Manually enter model or session ID       |

Text fields open in a focused input box. Press `Enter` to commit or `Esc` to
cancel. Model pickers use `↑`/`↓` for model, `←`/`→` for that model's effort,
and `Enter` to apply both. When discovery returns models, its actual default
is selected directly; the generic `default` entry appears only for an empty
catalog. Browsing never alters an existing effort override. If a different
model does not advertise that override, `Enter` leaves the current setting in
place until you explicitly choose an advertised effort with `←`/`→`.

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
immediately. Changing runs clears the selected step. To cancel active work,
close the drawer with `Esc`, then press `Esc` again.

## Other drawers

All drawers support `PageUp`/`PageDown` or the mouse wheel for scrolling and
`Esc` to close. Text in chat, status bars, and drawers can be selected with a
mouse drag and copied using the terminal's normal copy shortcut.

## Native session attach

Press `Ctrl+N` or `Ctrl+B` to focus the roster, move with `←` or `→`, and press
`Enter`; alternatively use `/attach <member>` or `@member /attach`. Asterline
suspends its TUI and opens that member's native interactive CLI. Use that CLI's
supported exit method—normally `/exit`; EOF works only when the backend accepts
it—to return to Asterline.

Fresh Claude attach sessions are created with an Asterline-generated UUID, then
automatically imported and bound on return. Codex messages are imported only
for an already-bound session that Asterline can identify safely; a Claude fork
is imported only when its prior transcript proves the lineage. Ambiguous native
sessions are never guessed. Grok and Agy resume their native sessions but
attached messages are not currently imported.
