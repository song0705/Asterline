# Asterline

[English](README.md) · [简体中文](README.zh-CN.md)

[![CI](https://github.com/song0705/Asterline/actions/workflows/ci.yml/badge.svg)](https://github.com/song0705/Asterline/actions/workflows/ci.yml)
[![最新版本](https://img.shields.io/github/v/release/song0705/Asterline)](https://github.com/song0705/Asterline/releases/latest)
[![许可证：MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**在一个终端里，运行一支看得见、能恢复的编程 Agent 团队。**

Asterline 是一个本地优先的终端协作工作台，统一调度电脑上已经安装的 Codex、
Claude、Grok 和 Agy 官方 CLI。它把各后端的原生事件整理成一段可审计对话，在成员
之间路由任务，并用结构化 Runs 持久保存协作过程。

[下载](https://github.com/song0705/Asterline/releases/latest) ·
[快速开始](#快速开始) ·
[命令参考](docs/commands.zh-CN.md) ·
[配置参考](docs/configuration.md)

![Codex 将前端设计方案发送给 Agy](docs/assets/asterline-codex-to-agy.webp)

## 快速开始

### 环境要求

- Linux、macOS 或 Windows 10/11，以及支持颜色的终端
- 至少安装并登录一个 CLI：`codex`、`claude`、`grok` 或 `agy`
- 仅从源码构建时需要 Rust 1.85 或更高版本

### 安装发布版本

打开 [GitHub Releases](https://github.com/song0705/Asterline/releases/latest)，
然后按照自己的操作系统选择步骤。发布包同时包含完整命令 `asterline` 和短命令
`ast`；以下步骤安装 `ast`。

#### macOS

根据 Mac 处理器下载对应的 `.tar.gz`：

- Apple silicon：`aarch64-apple-darwin`
- Intel：`x86_64-apple-darwin`

解压后，在解压目录中打开终端并运行：

```bash
mkdir -p "$HOME/.local/bin"
install -m 755 ast "$HOME/.local/bin/ast"
"$HOME/.local/bin/ast" --help
```

如果新终端找不到 `ast`，请在 `~/.zprofile` 中把 `$HOME/.local/bin` 加入 `PATH`。

#### Linux

根据机器架构下载对应的 `.tar.gz`：

- Intel/AMD 64 位：`x86_64-unknown-linux-gnu`
- ARM64：`aarch64-unknown-linux-gnu`

解压后，在解压目录中打开 shell 并运行：

```bash
mkdir -p "$HOME/.local/bin"
install -m 755 ast "$HOME/.local/bin/ast"
"$HOME/.local/bin/ast" --help
```

如果新 shell 找不到 `ast`，请在 shell 的 `PATH` 配置（通常为 `~/.profile`）中加入
`$HOME/.local/bin`。

#### Windows

下载 `x86_64-pc-windows-msvc.zip`，解压后在解压目录中打开 PowerShell 并运行：

```powershell
New-Item -ItemType Directory -Force "$HOME\bin" | Out-Null
Copy-Item .\ast.exe "$HOME\bin\ast.exe"
& "$HOME\bin\ast.exe" --help
```

随后在 Windows“环境变量”中把 `%USERPROFILE%\bin` 加入用户 `Path`，再打开新的
PowerShell 窗口，即可在其他目录运行 `ast`。

每个 Release 还附带 `SHA256SUMS` 与 GitHub 构建来源证明。

### 从源码构建

在任一受支持的操作系统中安装 Rust 1.85 或更高版本，然后在 Asterline 源码目录运行：

```bash
cargo install --path . --force
```

Cargo 会把两个命令安装到 macOS/Linux 的 `$HOME/.cargo/bin` 或 Windows 的
`%USERPROFILE%\.cargo\bin`。请确认对应目录已经加入 `PATH`。

### 启动团队

进入希望 Agent 工作的项目，然后启动 Asterline。

macOS 或 Linux：

```bash
cd /path/to/your/project
ast
```

Windows PowerShell：

```powershell
Set-Location C:\path\to\your-project
ast.exe
```

首次启动时，Asterline 会发现 `PATH` 中受支持的 CLI，并打开 Team 编辑器。使用
`↑`/`↓` 选择成员，按 `Enter` 编辑字段，再按 `s` 保存并启动。编辑器会发现每个已
安装后端可用的模型与 reasoning effort；Asterline 不会代为安装 CLI 或登录账号。

把第一项任务发给界面中显示的成员 handle：

```text
@builder 审计这个仓库，并找出风险最高的代码路径
```

也可以先选择一个可追踪工作流，再把下一条消息作为任务：

```text
/mode review
修复支付回调的竞态问题，并补充回归测试
```

新的 normal 对话要求第一条消息明确目标；之后的普通文本会继续发给上一次目标，
`@all` 会广播给整个团队。在 Asterline 中输入 `/help` 可打开命令面板。

## Asterline 带来了什么

### 一份记录，而不是一墙终端

成员消息、思考、工具调用、diff、错误和交接始终归属于实际产生它的成员。Markdown、
代码块、表格和工作区差异会直接在 TUI 中渲染；`/logs` 保留原始诊断信息但不淹没
主对话，`/focus <member>` 可以只查看一个成员。

### 沿用原生 CLI，组成实时团队

同一团队可以混用不同后端，也可以多次使用同一后端。成员可配置职责、模型、推理
强度、工作目录、系统提示、沙箱、权限模式、工具白名单和会话策略。运行中输入
`/team` 即可更新成员列表。

![Asterline Team 编辑器](docs/assets/asterline-team.webp)

| 后端   | 可执行文件 | 流式接口                 | 会话恢复 | 模型来源                       |
| ------ | ---------- | ------------------------ | -------- | ------------------------------ |
| Codex  | `codex`    | `codex exec --json`      | 支持     | `codex debug models`           |
| Claude | `claude`   | 带增量消息的流式 JSON    | 支持     | 别名与 `availableModels`       |
| Grok   | `grok`     | `grok agent stdio` ACP   | 支持     | `grok --no-auto-update models` |
| Agy    | `agy`      | print `stream-json` 事件 | 支持     | `agy models`                   |

Asterline 不代替各厂商的认证、计费、模型授权或用量限制；这些仍由对应 CLI 账号决定。

### 用 Runs 管理工作，而不是堆叠回合

Runs 会保存阶段、清单负责人、尝试次数、阻塞、备注、验证、审阅结论和下一步。先选择
模式，下一条消息就会成为该模式的任务。

| 模式         | 适合场景                     | Asterline 的运行方式                              |
| ------------ | ---------------------------- | ------------------------------------------------- |
| `normal`     | 直接与一个/全部成员工作      | 路由普通消息，并记住上一次目标                    |
| `review`     | 带质量门的实现               | builder → 结构化 reviewer verdict → 修改循环      |
| `plan`       | 多步骤、带负责人的工作       | 规划清单、派发负责人，最后进入审阅                |
| `brainstorm` | 先广泛探索，再进行判断       | seed/build/stretch、私密投票、排名、综合          |
| `team`       | 端到端协调交付               | Coordinator 负责步骤、整合和验证                  |

Review 和 Plan 模式使用有次数上限的审阅循环；Brainstorm 将想法生成与私密投票、确定性
排名分开；Team 模式由 Coordinator 负责清单、整合与验证。`/runs` 只展示当前对话的
Runs。

工作需要等待或显式检查时，可以直接记录状态：

```text
/block 等待 staging client secret
/note 已向平台团队申请 secret
/continue secret 已经可用
/verify cargo test
```

没有指定验证命令时，Asterline 可以识别 `cargo test`、`npm test`、`pytest` 等常见
检查。

### 本地保存，可以恢复，也可以检查

`/new` 会保存当前对话并启动全新的后端会话；`/resume` 会恢复选中的聊天、Roster、
原生 session ID、模式和 Runs。使用 `Ctrl+N` 或 `Ctrl+B` 聚焦成员后按 `Enter`，
还可以打开该成员的原生交互式 CLI，并在支持时恢复其会话。

默认情况下，运行状态保存在项目内：

```text
<workspace>/.asterline/
├── team.json
└── asterline.sqlite3
```

数据库包含提示、回复、工具事件、路由、原始后端事件、日志、审批、会话和 Run 历史。
这些都是敏感开发数据，通常应把 `.asterline/` 加入 `.gitignore`。

Asterline 还可能在 `.agents/skills/` 下安装工作区级 Team 和 Brainstorm Skills。这些是
集成文件而不是运行历史；请审查内容并自行决定是否纳入版本控制。

## 工作方式

```text
你
 └─ 指定一个成员、整个团队或一个可追踪工作流
     ├─ Asterline 启动或恢复对应的后端 CLI
     ├─ 原生事件变成对话、工具、差异、日志和会话状态
     ├─ 合法的队友消息被路由给其他成员
     └─ 消息、路由、Runs、审批和验证结果持久化到 SQLite
```

自动转交有明确上限。一次任务达到配置的转交次数后，Asterline 会暂停路由并显示
当前状态，而不是允许 Agent 之间形成失控循环。

## 信任模型与边界

Asterline 在本机启动后端进程，并继承其凭据、环境变量、文件系统访问和网络访问。
Asterline 没有接收工作区的云服务，但每个后端仍遵循其厂商自己的网络行为与数据
政策。

成员可以使用后端原生的沙箱和权限设置。Asterline 另外提供可配置审批门，覆盖高风险
用户请求、Agent 间转发、工作流派发和 Agent 发起的成员变更；它不会在后端之外再
提供一层进程级沙箱。`--debug` 会关闭 Asterline 审批门，只适合受控开发环境。

放宽权限前请阅读[审批与工具级控制](docs/approvals.md)（英文）。如果每个 Agent 都
必须自动拥有隔离 worktree、需要托管式控制台或远程队列、必须直接调用厂商 API，
或者目标是无人值守自动合并，应选择其他方案。

## 常用命令

| 命令                   | 用途                              |
| ---------------------- | --------------------------------- |
| `@<member> <message>`  | 向一个成员发送消息                |
| `@all <message>`       | 向全队广播                        |
| `/mode`                | 选择 normal 或协作模式            |
| `/runs`                | 查看 Run 状态、阶段和下一步       |
| `/team`                | 编辑当前团队                      |
| `/skills`              | 为下一条提示选择 Skill            |
| `/find <text>`         | 搜索当前对话记录                  |
| `/diff`                | 查看未暂存修改和未跟踪文件        |
| `/logs`                | 打开持久化诊断日志                |
| `/new`                 | 创建新对话和新的后端会话          |
| `/resume`              | 选择并恢复之前的团队对话          |
| `/approve` / `/reject` | 处理待审批请求                    |
| `/abort`               | 取消运行中的任务、模式和验证      |
| `/help`                | 打开命令面板                      |

[完整命令与键盘参考](docs/commands.zh-CN.md)包含 Run 步骤、Team 操作、历史记录、
原生会话接入和导航方式。

## 文档

- [命令与键盘参考](docs/commands.zh-CN.md)
- [配置、本地数据、权限与故障排查](docs/configuration.md)
- [审批层与工具级控制](docs/approvals.md)
- [最新版本发布说明](docs/releases/v0.2.2.md)
- [维护者发布流程](docs/releasing.md)
- 内置帮助：`/help` 与 `asterline --help`

## 开发与贡献

不调用真实后端即可运行产品：

```bash
cargo run -- --fake
```

运行本地质量检查：

```bash
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked --no-fail-fast
```

如果已经安装 `just`，可以使用 `just run --fake`、`just install` 或 `just check`。
真实后端 smoke 测试需要显式启用，默认不会运行。

请通过 [GitHub Issues](https://github.com/song0705/Asterline/issues) 提交可复现的 Bug
和范围明确的功能建议，并附上操作系统、终端、Asterline 版本、后端 CLI/版本、经过
脱敏的 `/logs` 和最短复现步骤。

## 项目状态

Asterline 当前版本为 `0.2.2`，仍在积极开发。带版本标签的提交会发布 Linux、macOS
和 Windows 预编译包。在稳定版之前，配置、持久化数据、命令和界面细节都可能发生
不兼容变化。

## 许可证

Asterline 使用 [MIT License](LICENSE)。
