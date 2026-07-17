# Asterline command reference

This page is the complete in-product command and keyboard reference. For
installation and the product overview, return to the [main README](../README.md).

## Composer and targeting

Type `/` to open command completion or `@` to complete member handles. Use `↑`
and `↓` to move through suggestions, `Tab` or `Enter` to accept, and `Esc` to
dismiss the popup.

A fresh conversation requires an explicit target. After the first message,
plain text reuses the previous target.

Use `@<member> /<skill>` to invoke a skill through that member's native CLI.
Typing `/` after the explicit member prefix opens discovered skill completion.
Codex invocations are translated to its native `$skill` syntax; Claude, Grok,
and Agy keep `/skill`. Slash commands without a member prefix remain Asterline
commands. Interactive-only backend commands such as model pickers require
native session attach and are not available through headless dispatch.

| Input                     | Action                         |
| ------------------------- | ------------------------------ |
| `@<member> <message>`     | Send to one member             |
| `@all <message>`          | Send to every member           |
| `/ask <member> <message>` | Explicit single-member message |
| `/ask all <message>`      | Broadcast using `/ask`         |
| `/all <message>`          | Broadcast to every member      |

## Conversation and team commands

| Command                                            | Action                                              |
| -------------------------------------------------- | --------------------------------------------------- |
| `/new`                                             | Start a new conversation and new backend sessions   |
| `/effort <member> <low\|medium\|high\|xhigh\|max>` | Change reasoning effort                             |
| `/skills`                                          | Select a Skill for the next prompt                  |
| `/team`                                            | Edit members, backends, roles, models, and defaults |
| `/status`                                          | Open the Team view                                  |
| `/sessions`                                        | Open the Team view with session identifiers         |
| `/focus <member>`                                  | View one member's logs                              |
| `/logs`                                            | Open persisted runtime logs                         |
| `/diff`                                            | Show the unstaged diff and list untracked files     |
| `/approve`                                         | Approve the first pending request                   |
| `/reject`                                          | Reject the first pending request                    |
| `/retry`                                           | Resume a paused route or retry the previous turn    |
| `/abort`                                           | Cancel running members and active verification      |
| `/help`                                            | Open the command palette                            |

## Collaboration modes

First-class mode runs use the runtime engine (builder/review loops, lead
checklists, roundtable discussion). Type `/mode`, accept it, then select a
concrete mode from the second completion list. The selection remains active
for every later message in this terminal until another `/mode` replaces it;
`/new` starts a new chat without resetting the selected mode.

| Command                             | Action                                                                   |
| ----------------------------------- | ------------------------------------------------------------------------ |
| `/mode normal`                      | Select normal direct-message dispatch                                    |
| `/mode review`                      | Select builder/reviewer mode                                             |
| `/mode plan`                        | Select leader/checklist/reviewer mode                                    |
| `/mode roundtable`                  | Select multi-agent discussion mode                                       |
| `/mode workflow`                    | Select legacy prompt-driven team workflow                                |
| `/runs`                             | Inspect runs, mode phase, checklist, timeline, and next action           |
| `/continue [run-<id>] [note]`       | Continue a blocked or failed run                                         |
| `/note [run-<id>] <text>`           | Record a checkpoint without waking an agent                              |
| `/block [run-<id>] <reason>`        | Mark a run blocked                                                       |
| `/verify [run-<id>] [command]`      | Run verification in the background                                       |
| `/find <text>`                      | Search the transcript (case-insensitive); empty query clears             |

Mode defaults come from `team.json`. Legacy one-shot mode commands remain
available for inline overrides. Override keys: `builder`, `reviewer`, `leader`,
`moderator`, `participants` (comma list or `all`), `max_iterations`, `rounds`,
`auto_verify`. Example:

```text
/review reviewer=claude builder=@codex max_iterations=5 fix the parser
```

Review and lead modes loop on `@@review` verdicts until approve or
`max_iterations` is exhausted (then the run blocks). A one-line agent verdict:

```text
@@review {"verdict":"approve","summary":"LGTM"}
```

If `/verify` has no command, Asterline detects common checks such as
`cargo test`, `npm test`, and `pytest`.

### Checklist steps

| Command                                 | Action                  |
| --------------------------------------- | ----------------------- |
| `/step add [run-<id>] [@owner] <title>` | Add a step              |
| `/step todo [run-<id>] <n> [note]`      | Return a step to todo   |
| `/step doing [run-<id>] <n> [note]`     | Mark a step in progress |
| `/step done [run-<id>] <n> [note]`      | Mark a step complete    |
| `/step block [run-<id>] <n> [note]`     | Mark a step blocked     |
| `/step assign [run-<id>] <n> <member>`  | Assign a step           |
| `/step unassign [run-<id>] <n>`         | Clear step ownership    |
| `/step rename [run-<id>] <n> <title>`   | Rename a step           |
| `/step remove [run-<id>] <n>`           | Remove a step           |

## Global keyboard shortcuts

| Key                            | Action                                                        |
| ------------------------------ | ------------------------------------------------------------- |
| `Enter`                        | Send or accept the active selection                           |
| `Shift+Enter`                  | Insert a newline                                              |
| `Alt+Enter`                    | Newline fallback for terminals without distinct Shift+Enter   |
| `↑` / `↓`                      | Move in the composer, recall history, or move popup selection |
| `Tab`                          | Accept completion                                             |
| `Ctrl+R`                       | Reverse-search prompt history                                 |
| `n` / `p`                      | Next or previous `/find` match (composer empty, no drawer)    |
| `PageUp` / `PageDown`          | Scroll chat or the open drawer                                |
| Mouse drag                     | Select visible text and copy it to the clipboard              |
| Mouse wheel                    | Scroll chat or the open drawer                                |
| `Esc`                          | Close the active overlay, or cancel running work              |
| `Ctrl+O` / `Ctrl+G` / `Ctrl+T` | Expand or collapse successful tool output                     |
| `Ctrl+L`                       | Open logs                                                     |
| `Ctrl+P`                       | Open the command palette                                      |
| `Ctrl+N` / `Ctrl+B`            | Focus the next or previous member                             |
| `Ctrl+A` / `Ctrl+E`            | Move to line start or end                                     |
| `Ctrl+U`                       | Clear the current line                                        |
| `Ctrl+W`                       | Delete the previous word                                      |
| `Ctrl+C`                       | Cancel work, clear the composer, or arm quit when idle        |

Prompt history behaves like a shell: `↑` and `↓` preserve the current draft
while browsing older submissions. During `Ctrl+R`, type to refine the match,
press `Ctrl+R` again for an older match, `Enter` to accept, or `Esc` to cancel.

During `/find`, the footer shows `find: "query" (i/n)`. Press `n` or `p` to
jump matches (composer must be empty and no drawer open). `Esc` clears find.
`/find` with no argument also clears the search.

## Team editor

The Team editor has two navigation levels. Select a member first, then enter its
field list.

| Key       | Member selection                            | Field selection                                  |
| --------- | ------------------------------------------- | ------------------------------------------------ |
| `↑` / `↓` | Select a member                             | Select a field                                   |
| `Enter`   | Open member fields                          | Edit, cycle, or open a model/session table       |
| `Esc`     | Close Team                                  | Return to member selection                       |
| `a` / `d` | Add or delete a member                      | —                                                |
| `t`       | Make the selected member the default target | —                                                |
| `*`       | Make all members the default target         | —                                                |
| `s`       | Apply and save                              | Apply and save                                   |
| `e`       | —                                           | Manually enter a model or session ID             |

Text fields open in a focused input box. Press `Enter` to commit or `Esc` to
cancel; use `←`/`→`, `Home`/`End`, `Delete`/`Backspace`, `Ctrl+A/E/B/F`,
`Ctrl+U/K/W`, or `Alt+B/F` for shell-style cursor editing. Pasted multiline
text is inserted atomically with line breaks folded to spaces. Model pickers
use `↑`, `↓`, `Enter`, and `Esc`.

On the `session id` field, `Enter` opens Asterline's session table. It extracts
the title, project, update time, and native ID from local Codex, Claude, or Grok
history, then keeps only sessions belonging to that member's effective working
directory. Type to filter, use `↑`/`↓` or `PageUp`/`PageDown` to move, and press
`Enter` to place the selected ID in the Team draft. Press `s` to apply it. Use
`e` for manual entry; Agy currently requires this path because no verified
local history format is available.

## Workflow Runs drawer

| Key                   | Action                                                |
| --------------------- | ----------------------------------------------------- |
| `←` / `→`             | Select an older or newer run                          |
| `↑` / `↓`             | Select a checklist step                               |
| `x`                   | Toggle compact and detailed views                     |
| `Enter`               | Stage the selected step or run's next action          |
| `Tab`                 | Stage an editable dispatch to the selected step owner |
| `PageUp` / `PageDown` | Scroll details                                        |
| `Esc`                 | Close the drawer                                      |

Changing runs clears step focus. With no step selected, `Enter` stages the
run-level next action such as `/verify`, `/continue`, or `/abort`.

## Native session attach

Press `Ctrl+N` or `Ctrl+B` to focus the top roster, move with `←` or `→`, then
press `Enter`. Asterline suspends its TUI and opens the selected member's native
interactive CLI. Exit that CLI with `/exit` or `Ctrl+D` to return.

Codex and Claude messages created while attached are imported back into the
Asterline transcript (codex rollout diff; Claude session JSONL with timestamp
filtering for resume forks). Grok and Agy resume the native session but do not
import the attached transcript.
