# Asterline command reference

This page is the complete in-product command and keyboard reference. For
installation and the product overview, return to the [main README](../README.md).

## Composer and targeting

Type `/` to open command completion or `@` to complete member handles. Use `↑`
and `↓` to move through suggestions, `Tab` or `Enter` to accept, and `Esc` to
dismiss the popup.

A fresh conversation requires an explicit target. After the first message,
plain text reuses the previous target.

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

## Workflow commands

| Command                        | Action                                                   |
| ------------------------------ | -------------------------------------------------------- |
| `/plan <goal>`                 | Start a coordinated workflow                             |
| `/workflow <goal>`             | Alias for `/plan`                                        |
| `/runs`                        | Inspect runs, checklist steps, timeline, and next action |
| `/continue [run-<id>] [note]`  | Continue a blocked or failed run                         |
| `/note [run-<id>] <text>`      | Record a checkpoint without waking an agent              |
| `/block [run-<id>] <reason>`   | Mark a run blocked                                       |
| `/verify [run-<id>] [command]` | Run verification in the background                       |

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
| `PageUp` / `PageDown`          | Scroll chat or the open drawer                                |
| Mouse wheel                    | Scroll chat or the open drawer                                |
| `Esc`                          | Close or step back from the active overlay                    |
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

## Team editor

The Team editor has two navigation levels. Select a member first, then enter its
field list.

| Key       | Member selection                            | Field selection                                  |
| --------- | ------------------------------------------- | ------------------------------------------------ |
| `↑` / `↓` | Select a member                             | Select a field                                   |
| `Enter`   | Open member fields                          | Edit, cycle, or open model choices               |
| `Esc`     | Close Team                                  | Return to member selection                       |
| `a` / `d` | Add or delete a member                      | —                                                |
| `t`       | Make the selected member the default target | —                                                |
| `*`       | Make all members the default target         | —                                                |
| `s`       | Apply and save                              | Apply and save                                   |
| `e`       | —                                           | Manually enter a model name on the `model` field |

When editing text, press `Enter` to commit, `Esc` to cancel, and `Ctrl+U` to
clear the buffer. Model pickers use `↑`, `↓`, `Enter`, and `Esc`.

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

Codex messages created while attached are imported back into the Asterline
transcript. Other backends resume the native session without transcript import.
