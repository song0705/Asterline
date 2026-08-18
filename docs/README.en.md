# Asterline documentation

[中文文档](README.md)

The root [`README.md`](../README.md) is the default Chinese product page. This directory is organized by goal: install, operate, inspect local data, and publish. The English product page is [`README.en.md`](../README.en.md).

## Read by goal

Everyday use starts at the [installation guide](installation.md) and the [command reference](commands.md). Maintainer docs are only needed when you are testing real backends or cutting a release.

### User documentation

| Document                                      | What you get                                                  |
| --------------------------------------------- | ------------------------------------------------------------- |
| [Installation](installation.md)               | Packages, Homebrew, updates, uninstall, troubleshooting       |
| [Commands and keyboard](commands.md)          | Startup flags, `@member`, slash commands, drawers, keys       |
| [Configuration](configuration.md)             | `team.json`, permissions, the local database, debugging       |
| [Approvals and tool control](approvals.md)    | Default auto-pass, the manual card, backend permission bounds |
| [v1.0.4 release notes](releases/v1.0.4.en.md) | User-visible changes in this version                          |

### Developer and maintainer documentation

| Document                                                  | What you get                                    |
| --------------------------------------------------------- | ----------------------------------------------- |
| [Real-backend smoke tests](real-smoke.md)                 | Paid, manually approved live CLI entrypoints    |
| [Maintainer release process](releasing.md)                | Preflight, annotated tags, immutable Releases   |
| [Third-party package definitions](../packaging/README.md) | Homebrew, deb, rpm, and related packaging notes |

## How the README files split

- [`README.md`](../README.md): Chinese product entry. This is also the GitHub and crates.io default.
- [`README.en.md`](../README.en.md): English product entry, covering the same product scope.

Detail pages stay English at `docs/*.md`, with Chinese counterparts at `docs/*.zh-CN.md`. Release notes use Chinese `docs/releases/vX.Y.Z.md` as the canonical file and `vX.Y.Z.en.md` as the English counterpart.

## Screenshots

The product README opens with the Team editor, then the conversation, mode picker, and one full-width block per structured mode. The rest of the set also lives in `docs/assets/`, named after the screen:

| File                                                | Screen                                 |
| --------------------------------------------------- | -------------------------------------- |
| [chat.webp](assets/chat.webp)                       | Teammates handing work off in `normal` |
| [mode.webp](assets/mode.webp)                       | `/mode` picker                         |
| [mode-fields.webp](assets/mode-fields.webp)         | Mode field editor                      |
| [team.webp](assets/team.webp)                       | Team editor                            |
| [team-run.webp](assets/team-run.webp)               | Team mode splitting work and adding    |
| [team-done.webp](assets/team-done.webp)             | Team mode review passed                |
| [runs.webp](assets/runs.webp)                       | `/runs` checklist and next action      |
| [review.webp](assets/review.webp)                   | Review implementation and handoff      |
| [review-done.webp](assets/review-done.webp)         | Reviewer verdict against the tree      |
| [plan.webp](assets/plan.webp)                       | Plan written and execution started     |
| [plan-done.webp](assets/plan-done.webp)             | Completed Plan checklist in `/runs`    |
| [brainstorm.webp](assets/brainstorm.webp)           | Brainstorm idea cards                  |
| [brainstorm-vote.webp](assets/brainstorm-vote.webp) | Private ballots and synthesis          |
| [brainstorm-done.webp](assets/brainstorm-done.webp) | Ranked brainstorm result               |

## Status convention

Docs distinguish current behavior from details that may change between releases. 1.0.x is published, but configuration, persisted data, and UI details can still move. Do not describe roadmap items as if they shipped.
