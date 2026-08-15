# Real-backend smoke tests

[简体中文](real-smoke.zh-CN.md)

`tests/real_smoke.rs` exercises Asterline's real provider processes and event
parsers. The tests are ignored by the normal test suite because they make live
model calls and may consume paid usage. Maintainers can run one provider at a
time from **Actions → Real backend smoke → Run workflow**.

The workflow accepts dispatches only from the repository's default branch. Put
its jobs behind a protected GitHub Environment named `real-smoke` with required
reviewers, enable **Prevent self-review**, and restrict deployments to the
protected default branch. The workflow has read-only repository permission,
checks out without persisting the GitHub token, runs tests serially, and never
runs for pull requests. See [GitHub's environment protection
reference](https://docs.github.com/en/actions/reference/workflows-and-actions/deployments-and-environments).

## Hosted providers

Codex, Claude, and Grok run on an ephemeral GitHub-hosted Ubuntu runner. Add only
the credential for the provider being tested to the protected `real-smoke`
environment, using the environment-variable name documented by that provider:

- `OPENAI_API_KEY` for Codex;
- `ANTHROPIC_API_KEY` for Claude;
- `XAI_API_KEY` for Grok.

The workflow fails before the test when the selected credential is absent. It
installs each current CLI using the provider's official installer, prints the
version under test, and then runs every ignored test whose name starts with that
provider's `real_<provider>_` prefix.

These names and installation commands come from the providers' own material:

- [Codex CLI installation](https://learn.chatgpt.com/docs/codex/cli) and
  [API-key login for CI](https://learn.chatgpt.com/docs/auth);
- [Claude Code installation](https://code.claude.com/docs/en/getting-started) and
  [authentication precedence](https://code.claude.com/docs/en/authentication);
- [Grok Build installation](https://github.com/xai-org/grok-build) and
  [headless API-key authentication](https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-pager/docs/user-guide/02-authentication.md).

Do not expose this workflow to untrusted input or add an automatic pull-request
trigger. The provider CLIs run repository code, make network calls, and use
billable credentials.

## Agy

Agy does not currently have a reliable public, purely headless authentication
flow for a fresh hosted runner. Its dispatch therefore routes only to a
self-hosted runner carrying both labels `self-hosted` and
`asterline-real-smoke`. Install Agy 1.1.12 or newer and authenticate it
interactively under the runner service account before dispatching the job. Also
preinstall the pinned toolchain:

```bash
rustup toolchain install 1.93.1 --profile minimal
```

The Agy job deliberately does not run a third-party toolchain or cache Action
in the credentialed environment. Do not copy its credential store into the
repository or a workflow artifact.

Keep this runner dedicated, locked down, and offline when it is not needed.
Anyone allowed to change workflow code that executes on a credentialed
self-hosted runner is inside that runner's trust boundary. This is a public
repository, and GitHub explicitly warns that fork pull requests can compromise
an unrestricted self-hosted runner. Prefer a one-job ephemeral runner registered
with `config.sh --ephemeral`, wipe its host after the job, and preserve runner
logs outside the machine. If an organization runner group is available, limit
it to this repository and exactly
`song0705/Asterline/.github/workflows/real-smoke.yml@refs/heads/main`; do not
leave the runner in a group usable by arbitrary workflows. See GitHub's
[runner-group access controls](https://docs.github.com/en/enterprise-cloud@latest/actions/how-tos/manage-runners/self-hosted-runners/manage-access)
and [ephemeral-runner guidance](https://docs.github.com/en/actions/reference/runners/self-hosted-runners#ephemeral-runners-for-autoscaling).

## Local equivalent

To run one provider locally, authenticate its CLI and opt in explicitly. For
example:

```bash
ASTERLINE_SMOKE_CODEX=1 \
  cargo test --locked --test real_smoke real_codex_ -- \
  --ignored --nocapture --test-threads=1
```

Replace `CODEX` and `real_codex_` with `CLAUDE` / `real_claude_`, `GROK` /
`real_grok_`, or `AGY` / `real_agy_`.
