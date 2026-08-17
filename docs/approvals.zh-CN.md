# 审批与工具级控制

[English](approvals.md)

Asterline 不再因为句子里出现 `git`、`shell`、`file` 就拦住整段 prompt。
用户消息、队员转交和模式派发会立刻开始。Codex 工具询问**默认自动通过**；只有打开
`--manual-approvals` 或 `team.json` 的 `"approvals": { "manual": true }` 才会
弹出卡片。

手动审批打开且有待批事项时，输入框上方会出现卡片：`y` 或 Enter 同意，`n` 拒绝。
也可以继续用 `/approve` / `/reject`。多条时，输入框为空可用 `←` / `→` 切换。

仍会停下的（手动审批，外加可选的 Plan 确认）只有：

| 拦截 | 何时 |
| --- | --- |
| Codex 原生工具 | App Server 询问命令、改文件或提权 |
| Plan 执行 | `auto_execute` 关闭，且 checklist 已准备好交给 Builder |

`@@team_member` 不再审批。`/mode team` 默认只能用当前队员；打开
`allow_add_members` 后立刻加入。

## 工具级控制

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
Asterline 待审批项。在输入框上方的卡片里按 `y` / `n` 决定；`/approve` 和
`/reject` 仍会把一次性决定发回同一个仍存活的 Codex thread。请求正文会随审批一起记录。Team 编辑器的 **approval policy** 会作为
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

对于没有回调的后端，本版本只使用它们的原生非交互控制。Prompt 文本不会再为
`/approve` 而挂起。

## 实用配方

- **谨慎审查者**：Claude 使用 `permission_mode: plan`，或 Codex/Grok 使用
  `sandbox: read-only`——成员能读取和推理，但不能修改工作树。
- **可信构建者**：`sandbox: workspace-write`（或对应的 Codex 权限预设）。工具询问仍会出现在审批卡片里。
- **演示 / 离线**：`--fake` 完全不会启动真实 CLI。
