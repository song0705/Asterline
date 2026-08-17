# Approvals and tool-level control

[简体中文](approvals.zh-CN.md)

Asterline no longer holds a prompt because the text mentions `git`, `shell`,
or `file`. User messages, relays, and mode dispatches start immediately.
Codex tool asks are **approved automatically** unless you turn on manual
approval with `--manual-approvals` or `team.json` `"approvals": { "manual": true }`.

When manual approval is on and something is waiting, a card appears above the
composer. Press `y` or Enter to agree, `n` to deny. `/approve` and `/reject`
still resolve the oldest request if you prefer to type them. Multiple cards
cycle with `←`/`→` while the composer is empty.

The remaining holds (only with manual approval, plus optional Plan confirm) are:

| Hold              | When                                                        |
| ----------------- | ----------------------------------------------------------- |
| Codex native tool | App Server asks about a command, file change, or escalation |
| Plan execute      | `auto_execute` is off; Builder waits for the card           |

`@@team_member` is not an approval. In `/mode team` the default is the current
roster only (the add is refused). Enable `allow_add_members` to join
immediately.

## Tool-level control

Once a member runs, tool-by-tool enforcement belongs to the backend CLI:

| Backend | Controls passed through by Asterline                                              |
| ------- | --------------------------------------------------------------------------------- |
| codex   | `sandbox` plus App Server approval policy and callback responses                  |
| claude  | `permission_mode`, hard `allowed_tools`, plus `.claude/settings.json` policies    |
| grok    | `sandbox`, `permission_mode`, and ACP permission responses; tool list is advisory |
| agy     | `--sandbox`; `accept-edits`/`plan` modes; bypass only when configured             |

Configure these per member in the Team editor (`/team`) or `team.json`. A
member with `sandbox: read-only` cannot write regardless of what a prompt asks;
a claude member with `allowed_tools: ["Read", "Grep"]` cannot run Bash at all.
Claude tool lists are passed with `--tools`; `--allowed-tools` is not used
because that vendor flag only removes permission prompts rather than tools.
Agy requires CLI 1.1.12 or newer so headless `--mode plan` is actually applied.

## Interactive per-tool behavior

We verified the Claude control protocol on claude 2.1.207 (2026-07): in
headless `--print --input-format stream-json` mode, a Bash tool call executes
according to the CLI's own permission configuration and **no
`control_request` / `can_use_tool` round-trip is offered** — even under
`--permission-mode manual`. Claude still offers `--permission-prompt-tool` for
an MCP-based permission callback, but Asterline does not configure that bridge.
Codex uses App Server by default. Its structured command, file-change, and
permission-escalation requests become normal Asterline pending approvals.
Agree or deny in the card above the composer (`y` / `n`); `/approve` and
`/reject` still send the same one-time decision back to the live Codex thread.
The request body is recorded with the approval. The Team
editor's **approval policy** is passed to App Server as Codex's native policy:
omitted/default, `dontAsk`, and `bypassPermissions` map to `never`;
`plan`/`acceptEdits` map to `untrusted`; and `auto` maps to `on-request`.
Asterline never writes Codex's
session or persistent policy amendments for an approval, and the selected
sandbox still bounds every accepted request. Tool-input questions and MCP
elicitations need richer answer UIs and remain explicitly declined for now.

Grok is different: Asterline uses its bidirectional ACP server and answers
`session/request_permission` callbacks. `bypassPermissions` allows requests,
`acceptEdits` allows edit/delete/move requests, and
`default`/`dontAsk`/`plan` reject requests that reach the client. In `auto`
mode Grok handles safe operations itself; a request that still reaches the
client is rejected instead of being silently elevated. These are
automatic policy responses, not a modal user prompt. The structured ACP stream
also carries Grok tool starts, progress, completion, diffs, and thought chunks.

For backends without a callback, this release uses their native
non-interactive controls only. Prompt text is not held for `/approve`.

## Practical recipes

- **Cautious reviewer**: `permission_mode: plan` (claude) or
  `sandbox: read-only` (codex/grok) — the member can read and reason but not
  mutate the tree.
- **Trusted builder**: `sandbox: workspace-write` (or the matching Codex
  permission preset). Tool asks still appear in the approval card.
- **Demo / offline**: `--fake` never launches real CLIs at all.
