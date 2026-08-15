# Approvals and tool-level control

Asterline gates work at two layers. This page explains what each layer covers,
how to configure the Asterline layer, and why per-tool interactive approval is
delegated to the backends in this release.

## Layer 1: the Asterline approval gate

Before a prompt reaches a backend process, Asterline classifies it against the
`approvals` policy in `team.json` (see the
[configuration reference](configuration.md#approvals-approvals)). A match holds
the dispatch until you `/approve` or `/reject` it. The gate covers three
surfaces:

| Surface | What is gated                                             |
| ------- | --------------------------------------------------------- |
| `user`  | Messages you type (`@member …`, `/ask …`)                 |
| `relay` | Agent handoffs and agent-requested roster additions       |
| `mode`  | Engine dispatches inside selected collaboration-mode runs |

Rejecting a gated mode dispatch blocks the run (resume later with `/continue`).
A route resumed explicitly with `/retry` is not re-gated: the resume itself is
your decision. An `@@team_member` request is always held when the `relay`
surface is enabled, regardless of prompt keyword categories, because it can
change backend, model, working-directory, sandbox, and permission settings.
`--debug` disables this layer entirely.

The gate classifies **prompts**, not tool calls: it decides whether an
instruction may start, not what a running agent may execute.

## Layer 2: backend-native tool control

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
Use `/approve` or `/reject` to send a one-time decision back to the same live
Codex thread; the request body is recorded with the approval. The Team
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

For backends without a callback, this release uses prompt-surface gating
(layer 1) plus their native non-interactive controls (layer 2).

## Practical recipes

- **Cautious reviewer**: `permission_mode: plan` (claude) or
  `sandbox: read-only` (codex/grok) — the member can read and reason but not
  mutate the tree.
- **Trusted builder, gated intents**: `sandbox: workspace-write` plus an
  `approvals.keywords` category for the commands you care about
  (`{"deploy": ["kubectl", "terraform"]}`).
- **Demo / offline**: `--fake` never launches real CLIs at all.
