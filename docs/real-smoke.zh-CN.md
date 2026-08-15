# 真实后端冒烟测试

[English](real-smoke.md)

`tests/real_smoke.rs` 会测试 Asterline 的真实提供商进程和事件解析器。正常测试套件会
忽略这些测试，因为它们会发起真实模型调用并可能消耗付费额度。维护者可在
**Actions → Real backend smoke → Run workflow** 中一次运行一个提供商。

该 workflow 只接受仓库默认分支发起的 dispatch。请为它配置名为 `real-smoke` 的受保护
GitHub Environment：要求审批者、启用 **Prevent self-review**，并仅允许受保护的默认
分支部署。workflow 只有只读仓库权限，checkout 不保存 GitHub token，串行运行测试，
且永不在 pull request 上运行。参阅 GitHub 的
[环境保护参考](https://docs.github.com/en/actions/reference/workflows-and-actions/deployments-and-environments)。

## 托管提供商

Codex、Claude 与 Grok 在临时的 GitHub-hosted Ubuntu runner 上运行。只把当前要测试
的提供商凭据放进受保护的 `real-smoke` 环境，并使用该提供商说明的环境变量名：

- Codex：`OPENAI_API_KEY`；
- Claude：`ANTHROPIC_API_KEY`；
- Grok：`XAI_API_KEY`。

若选择的凭据不存在，workflow 会在测试前失败。它会用提供商官方安装方式安装当前 CLI、
打印被测版本，然后运行名称以 `real_<provider>_` 开头的所有 ignored 测试。

这些名称和安装命令来自提供商自己的资料：

- [Codex CLI 安装](https://learn.chatgpt.com/docs/codex/cli)与
  [CI API key 登录](https://learn.chatgpt.com/docs/auth)；
- [Claude Code 安装](https://code.claude.com/docs/en/getting-started)与
  [认证优先级](https://code.claude.com/docs/en/authentication)；
- [Grok Build 安装](https://github.com/xai-org/grok-build)与
  [headless API key 认证](https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-pager/docs/user-guide/02-authentication.md)。

不要向不可信输入开放此 workflow，也不要添加自动的 pull-request 触发器。提供商 CLI 会
运行仓库代码、发起网络请求，并使用可计费的凭据。

## Agy

Agy 目前没有可靠的公开、纯 headless 认证流程可供全新托管 runner 使用。因此它只会派发
到同时具有 `self-hosted` 和 `asterline-real-smoke` 标签的 runner。派发前，请安装 Agy
1.1.12 或更高版本，并以 runner 服务账号交互式登录。同时预装固定工具链：

```bash
rustup toolchain install 1.93.1 --profile minimal
```

Agy job 特意不在含凭据环境中运行第三方 toolchain 或 cache Action。不要把凭据存储复制到
仓库或 workflow artifact。

让该 runner 专用、加固，并在不用时离线。任何能够修改会在带凭据 self-hosted runner 上
执行的 workflow 代码的人，都在这个 runner 的信任边界内。本仓库是公开仓库，GitHub
明确警告 fork PR 可能攻破不受限制的 self-hosted runner。优先使用通过
`config.sh --ephemeral` 注册的一任务临时 runner，任务后擦除主机，并将 runner 日志保留在
机器外。如有 organization runner group，把它限制到本仓库和
`song0705/Asterline/.github/workflows/real-smoke.yml@refs/heads/main`；不要把 runner
留在可被任意 workflow 使用的组中。参阅 GitHub 的
[runner-group 访问控制](https://docs.github.com/en/enterprise-cloud@latest/actions/how-tos/manage-runners/self-hosted-runners/manage-access)
和[临时 runner 指引](https://docs.github.com/en/actions/reference/runners/self-hosted-runners#ephemeral-runners-for-autoscaling)。

## 本地等价操作

本地运行某一提供商前，先登录其 CLI 并显式 opt in。例如：

```bash
ASTERLINE_SMOKE_CODEX=1 \
  cargo test --locked --test real_smoke real_codex_ -- \
  --ignored --nocapture --test-threads=1
```

将 `CODEX` 和 `real_codex_` 分别替换为 `CLAUDE` / `real_claude_`、
`GROK` / `real_grok_` 或 `AGY` / `real_agy_`。
