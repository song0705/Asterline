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

| Surface | What is gated                                                       |
| ------- | ------------------------------------------------------------------- |
| `user`  | Messages you type (`@member …`, `/ask …`)                           |
| `relay` | Automatic agent-to-agent handoffs (`@@team_message` routes)         |
| `mode`  | Engine dispatches inside `/review`, `/plan`, and `/roundtable` runs |

Rejecting a gated mode dispatch blocks the run (resume later with `/continue`).
A route resumed explicitly with `/retry` is not re-gated: the resume itself is
your decision. `--debug` disables this layer entirely.

The gate classifies **prompts**, not tool calls: it decides whether an
instruction may start, not what a running agent may execute.

## Layer 2: backend-native tool control

Once a member runs, tool-by-tool enforcement belongs to the backend CLI:

| Backend | Controls passed through by Asterline                                        |
| ------- | --------------------------------------------------------------------------- |
| codex   | `sandbox` (`read-only` / `workspace-write` / `danger-full-access`)          |
| claude  | `permission_mode`, `allowed_tools`, plus `.claude/settings.json` allowlists |
| grok    | `sandbox`, `permission_mode`, `allowed_tools`                               |
| agy     | `--sandbox` (unless `danger-full-access`), bypass only when configured      |

Configure these per member in the Team editor (`/team`) or `team.json`. A
member with `sandbox: read-only` cannot write regardless of what a prompt asks;
a claude member with `allowed_tools: ["Read", "Grep"]` cannot run Bash at all.

## Why no interactive per-tool approval yet

We verified the Claude control protocol on claude 2.1.207 (2026-07): in
headless `--print --input-format stream-json` mode, a Bash tool call executes
according to the CLI's own permission configuration and **no
`control_request` / `can_use_tool` round-trip is offered** — even under
`--permission-mode manual` — and the former `--permission-prompt-tool` flag no
longer exists. Codex `exec` is likewise non-interactive by design.

Without a backend callback there is nothing for Asterline to intercept, so
this release ships prompt-surface gating (layer 1) plus pass-through of every
backend-native control (layer 2). If a headless approval callback returns to a
backend CLI, interactive per-tool approval is a candidate for a future
release — the runtime
already models held approvals and per-member dispatch, so the missing piece is
only the adapter round-trip.

## Practical recipes

- **Cautious reviewer**: `permission_mode: plan` (claude) or
  `sandbox: read-only` (codex/grok) — the member can read and reason but not
  mutate the tree.
- **Trusted builder, gated intents**: `sandbox: workspace-write` plus an
  `approvals.keywords` category for the commands you care about
  (`{"deploy": ["kubectl", "terraform"]}`).
- **Demo / offline**: `--fake` never launches real CLIs at all.
