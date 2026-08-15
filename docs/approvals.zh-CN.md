# 审批与工具级控制

[English](approvals.md)

Asterline 在两个层级控制工作。本页说明每个层级覆盖的范围、如何配置
Asterline 层，以及为何本版本把逐工具的交互式审批交给各后端 CLI。

## 第一层：Asterline 审批门

在 prompt 进入后端进程前，Asterline 会依据 `team.json` 的 `approvals` 策略进行分类
（参阅[配置参考](configuration.zh-CN.md#审批approvals)）。命中规则的请求会暂停派发，
直到你执行 `/approve` 或 `/reject`。该门覆盖三个界面：

| 界面 | 被拦截的内容 |
| --- | --- |
| `user` | 你输入的消息（`@成员 …`、`/ask …`） |
| `relay` | Agent 转交和 Agent 请求添加成员 |
| `mode` | 已选择协作模式 Run 内部的引擎派发 |

拒绝被拦截的模式派发会阻塞该 Run（可稍后用 `/continue` 恢复）。通过 `/retry`
显式恢复的路由不会再次经过门：这次恢复本身就是你的决定。只要启用了 `relay` 界面，
`@@team_member` 请求一定会被暂停，而不取决于 prompt 关键词分类，因为它可能改变后端、
模型、工作目录、sandbox 和权限设置。`--debug` 会完全关闭这一层。

此门分类的是**提示词**，而不是工具调用：它决定一条指令是否可以开始，不能决定正在
运行的 Agent 具体可以执行什么。

## 第二层：后端原生工具控制

成员开始运行后，逐工具的强制控制属于后端 CLI：

| 后端 | Asterline 传递的控制 |
| --- | --- |
| codex | `sandbox`、App Server approval policy 与回调响应 |
| claude | `permission_mode`、硬性 `allowed_tools`，以及 `.claude/settings.json` 策略 |
| grok | `sandbox`、`permission_mode` 与 ACP 权限响应；工具列表只是建议 |
| agy | `--sandbox`、`accept-edits` / `plan` 模式；仅在配置时 bypass |

在 Team 编辑器（`/team`）或 `team.json` 为每位成员配置它们。`sandbox: read-only`
的成员无论 prompt 怎样要求都不能写入；`allowed_tools: ["Read", "Grep"]` 的 Claude
成员完全不能运行 Bash。Claude 的工具列表通过 `--tools` 传递；不使用
`--allowed-tools`，因为该厂商参数只是移除权限提示而非移除工具。Agy 需要 1.1.12
或更新版本，才能保证 headless `--mode plan` 真的生效。

## 逐工具的交互式行为

我们在 Claude 2.1.207（2026-07）上验证了控制协议：在 headless
`--print --input-format stream-json` 模式，Bash 工具调用依据 CLI 自己的权限配置执行，
即使使用 `--permission-mode manual` 也**不会**提供
`control_request` / `can_use_tool` 往返。Claude 仍提供基于 MCP 的
`--permission-prompt-tool` 权限回调，但 Asterline 尚未配置这座桥。

Codex 默认使用 App Server。其结构化的命令、文件变更和权限提升请求会成为普通的
Asterline 待审批项。使用 `/approve` 或 `/reject` 将一次性决定发回同一个仍存活的
Codex thread；请求正文会随审批一起记录。Team 编辑器的 **approval policy** 会作为
Codex 原生策略传入 App Server：省略/`default`、`dontAsk`、`bypassPermissions` 映射
为 `never`；`plan` / `acceptEdits` 映射为 `untrusted`；`auto` 映射为 `on-request`。
Asterline 不会为了单次审批写入 Codex 的会话或持久策略修正；选定的 sandbox 仍限制每一
个被接受的请求。工具输入问题和 MCP 追问需要更丰富的回答 UI，目前会被明确拒绝。

Grok 则不同：Asterline 使用其双向 ACP server，并响应
`session/request_permission` 回调。`bypassPermissions` 允许请求，`acceptEdits`
允许编辑、删除、移动请求，`default` / `dontAsk` / `plan` 会拒绝到达客户端的请求。
在 `auto` 模式下，Grok 自行处理安全操作；仍到达客户端的请求会被拒绝，而不是被静默
提权。这些是自动的策略响应，不是模态用户提示。结构化 ACP 流也携带 Grok 工具开始、
进度、完成、diff 和思考片段。

对于没有回调的后端，本版本使用提示词层面的门（第一层）加上它们的原生非交互控制
（第二层）。

## 实用配方

- **谨慎审查者**：Claude 使用 `permission_mode: plan`，或 Codex/Grok 使用
  `sandbox: read-only`——成员能读取和推理，但不能修改工作树。
- **可信构建者，但意图需审批**：`sandbox: workspace-write` 加一个关心命令的
  `approvals.keywords` 类别（`{"deploy": ["kubectl", "terraform"]}`）。
- **演示 / 离线**：`--fake` 完全不会启动真实 CLI。
