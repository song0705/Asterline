# Asterline

[English](README.md) · [简体中文](README.zh-CN.md)

[![CI](https://github.com/song0705/Asterline/actions/workflows/ci.yml/badge.svg)](https://github.com/song0705/Asterline/actions/workflows/ci.yml)
[![最新版本](https://img.shields.io/github/v/release/song0705/Asterline)](https://github.com/song0705/Asterline/releases/latest)
[![许可证：MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**让本地编程 Agent 真正成为一支看得见的团队。**

Asterline 是一个本地优先的终端协作工作台，用于统一调度 Codex、Claude、Grok
和 Agy。它不只是把多个 Agent 放进不同窗口，而是提供一段共享对话、明确的成员
职责、可见的任务交接、可追踪的工作流，以及一份持久保存的执行记录。

Asterline 直接运行你电脑上已经安装的官方 CLI。它不是模型网关，不代替各厂商的
登录认证，也不会把工作区上传到 Asterline 云服务。

## 一眼了解

- **一个终端、一份记录：**成员消息、思考、工具、diff、错误和交接都有清晰归属。
- **沿用已经信任的 CLI：**Codex、Claude、Grok、Agy 可以混合组队，并自动发现
  已安装模型及 reasoning effort。
- **五种派发模式：**直接聊天、审阅循环、带负责人的规划、含私密投票的结构化
  头脑风暴，以及 Coordinator 驱动的团队执行。
- **用 Runs 管理工作，而不是堆叠散乱回合：**清单、负责人、尝试次数、阻塞、
  备注、验证和下一步都会保存。
- **本地保存、可以恢复：**团队配置和运行记录默认保留在工作区；`/resume` 可恢复
  指定对话及其原生后端会话。
- **人始终掌控：**审批门、后端原生权限、有限转交、`/abort` 和可见日志共同约束
  自动化范围。

![Codex 将前端设计方案发送给 Agy](docs/assets/asterline-codex-to-agy.webp)

## 快速开始

### 环境要求

- 从源码构建时需要 Rust 1.85 或更高版本
- Linux、macOS 或 Windows 10/11，以及支持颜色和备用屏幕的终端
- 至少安装并登录一个 CLI：`codex`、`claude`、`grok` 或 `agy`
- 推荐安装 Git，以便查看差异和运行验证流程

### 安装并启动

从 [GitHub Releases](https://github.com/song0705/Asterline/releases/latest)
下载对应平台的压缩包，解压后安装其中任意一个命令：

```bash
mkdir -p ~/.local/bin
install -m 755 ast ~/.local/bin/ast
ast --help
```

Windows PowerShell 用户可解压 `.zip` 并安装其中任意一个 `.exe`：

```powershell
New-Item -ItemType Directory -Force "$HOME\bin" | Out-Null
Copy-Item .\ast.exe "$HOME\bin\ast.exe"
& "$HOME\bin\ast.exe" --help
```

Release 提供 Linux x86-64、Linux ARM64、macOS Intel、macOS Apple silicon 和
Windows x86-64 构建，并附带 `SHA256SUMS` 与 GitHub 签名的构建来源证明。

如果希望从源码安装，克隆本仓库后执行：

```bash
cargo install --path . --force
cd ~/code/your-project
ast
```

安装会同时提供 `asterline` 和更短的 `ast` 命令。Asterline 会检测 `PATH` 中可用的
后端，打开 Team 编辑器，并将结果保存在
`<workspace>/.asterline/team.json`。

如果安装 Release 后找不到 `ast`，请把 `$HOME/.local/bin` 加入 `PATH` 并重新
打开 Shell。从源码安装时使用 Cargo 的二进制目录，通常是 `$HOME/.cargo/bin`。
Windows 用户应把 `$HOME\bin`（或 Cargo 的 `%USERPROFILE%\.cargo\bin`）加入
`PATH`；Asterline 会按 `PATHEXT` 查找 `.exe`、`.cmd`、`.bat` 等后端启动器。

在 Team 编辑器中：

1. 使用 `↑`、`↓` 选择成员。
2. 按 `Enter` 进入该成员的字段列表。
3. 使用 `↑`、`↓` 选择字段，按 `Enter` 编辑或打开选项列表。
4. 按 `Esc` 返回成员选择。
5. 按 `s` 应用、保存并启动团队。

Agent 字段会打开全部受支持 CLI 的列表：已安装的 Agent 可以选择，未安装的仍会
显示但不可选择。Team 编辑器打开时，Asterline 会自动加载每个已安装 Agent 的模型
和推理强度能力。模型列表中使用 `↑`/`↓` 选择模型、`←`/`→` 选择 effort，
按 `Enter` 同时应用。发现成功时会直接显示并选择实际默认模型；若 CLI 未标记默认
模型，则选择发现列表的第一项。自动发现期间字段显示 `loading…`，只有无法发现任何
模型时才显示 `default`。

### 第一个协作任务

下面假设团队中有一个 handle 为 `builder` 的成员；如果你的 Team 编辑器显示了
其他 handle，请替换它。

直接派发任务：

```text
@builder 检查这个仓库，并找出风险最高的代码路径
```

启动审阅循环（builder 实现，reviewer 以结构化 verdict 把关，直到通过）：

```text
/mode review
修复支付回调的竞态问题，并补充回归测试
```

或者让领队规划任务清单，由 Asterline 派发给团队执行：

```text
/mode plan
端到端交付支付回调修复
```

在决定实现方向前广泛探索：

```text
/mode brainstorm
寻找三种既降低索引延迟、又不削弱召回率的架构
```

协调一次完整的多角色交付：

```text
/mode team
实现选定方案、完成审阅、更新文档并运行验证
```

新对话的第一条消息必须明确目标成员。之后的普通文本会继续发给上一次目标；
`@all` 和 `/all` 会广播给整个团队。

需要调用某个成员 CLI 已安装的 Skill 时，先输入成员前缀，再输入 `/`：例如
`@codex /review-patch`。Asterline 会显示已发现的 Skill，并把命令交给该成员；Codex
会自动转换为其原生的 `$review-patch` 形式。没有成员前缀的 `/mode`、`/team` 等仍是
Asterline 自己的命令。

## 为什么是 Asterline

### 协作本身就是产品

不少多 Agent 终端工具本质上是会话管理器：创建窗格、工作树或并行任务，然后把
上下文搬运留给使用者。Asterline 聚焦的是协作层：

- 每个成员拥有稳定的名称、职责、模型、权限和会话；
- Agent 可以在同一段可见对话中把工作交给指定队友；
- 工具调用、输出、文件变更、路由和失败都有明确归属；
- 工作流持续记录负责人、尝试次数、阻塞、备注和验证结果；
- SQLite 在重启后仍保留完整操作记录。

### 适合这些场景

- 希望实现、审查、研究和验证由不同角色承担；
- 已经在使用受支持的编程 CLI，希望它们协同工作；
- 除了最终补丁，还需要知道工作为何在成员之间流转；
- 需要由人掌控的协作流程，又不想自己搭 Agent 框架；
- 重视本地存储和可恢复的后端会话。

### 这些需求应选择其他方案

- 每个 Agent 都必须自动拥有隔离的 Git worktree 或分支；
- 需要托管式 Agent 服务、网页控制台或远程任务队列；
- 需要直接调用模型厂商 API，而不是本机 CLI 订阅；
- 需要无人值守的自动合并流水线。

Asterline 成员默认共享同一个工作区。可以为成员设置不同的 `cwd`，但当前版本
不会自动创建或合并 worktree。

## 支持的后端

| 后端   | 可执行文件 | 流式接口                 | 会话恢复 | 模型来源                 |
| ------ | ---------- | ------------------------ | -------- | ------------------------ |
| Codex  | `codex`    | `codex exec --json`      | 支持     | `codex debug models`     |
| Claude | `claude`   | 带增量消息的流式 JSON    | 支持     | 别名与 `availableModels` |
| Grok   | `grok`     | `grok agent stdio` ACP   | 支持     | `grok models`            |
| Agy    | `agy`      | print `stream-json` 事件 | 支持     | `agy models`             |

Asterline 不负责安装、认证或计费。后端是否可用、能访问哪些模型、以及使用额度，均由
对应 CLI 账号决定。

## 工作方式

```text
你
 └─ 指定一个成员、整个团队或一个工作流
     ├─ Asterline 启动或恢复对应的后端 CLI
     ├─ 将流事件转换为对话、工具、差异、日志和会话状态
     ├─ 把合法的队友消息路由给其他成员
     └─ 将消息、路由、工作流和验证结果持久化到 SQLite
```

自动转交有明确上限。一次任务达到配置的转交次数后，Asterline 会暂停路由并向
使用者显示该状态，避免 Agent 之间出现失控循环。

## 产品体验

### 一段统一的对话

每个参与者都有清晰身份。工具调用、返回结果、diff、交接和错误始终位于对应成员的
对话轨道上。失败的工具输出会立即展示；较长的成功输出可用 `Ctrl+O` 展开或折叠。

Agent 返回的 Markdown、代码块、表格和工作区差异会直接在终端中渲染。原始诊断
信息保存在 `/logs` 中，不会淹没主对话。

后端身份色分别针对深色和浅色终端优化。自动判断不准确时，可使用
`ASTERLINE_THEME=dark` 或 `ASTERLINE_THEME=light` 强制选择；成员名称、后端标签
和连续对话轨道也会同时标识身份，不会只靠颜色区分。

### 运行中也能调整团队

一个团队可以混用不同后端，也可以多次使用同一后端。成员可配置职责、模型、推理
强度、工作目录、系统提示、沙箱、权限模式、工具白名单和会话策略。各后端实际支持
的字段不同，详见[配置参考](docs/configuration.md#backend-setting-support)。

输入 `/team` 即可修改当前团队。打开时，Asterline 会立即重新检测已安装的 Agent
CLI，并在各成员工作目录中预加载其模型和推理强度能力。Agent 字段通过列表选择，
不再循环切换；未安装的 CLI 会显示但不可选择。在模型字段按 `e` 可以手动输入，
按 `s` 应用并保存变更。

![Asterline Team 编辑器](docs/assets/asterline-team.webp)

### 带审计记录的协作模式

先选择模式，再在下一条消息输入任务。模式属于当前对话，会一直保持到另一个
`/mode` 替换它。`/new` 创建 normal 模式的新对话；`/resume` 恢复所选对话的模式。

| 模式         | 适合场景                     | Asterline 的运行方式                              |
| ------------ | ---------------------------- | ------------------------------------------------- |
| `normal`     | 直接与一个/全部成员工作      | 路由普通消息，并记住上一次目标                    |
| `review`     | 带质量门的实现               | builder → 结构化 reviewer verdict → 修改循环      |
| `plan`       | 多步骤、带负责人的工作       | 规划清单、派发负责人，最后进入审阅                |
| `brainstorm` | 先广泛探索，再进行判断       | seed/build/stretch、私密投票、排名、综合          |
| `team`       | 端到端协调交付               | Coordinator 负责步骤、整合和验证                  |

审阅模式要求 Reviewer 给出结构化 `@@review` 结论。`approve` 结束运行并可触发
验证；`request_changes` 把反馈送回 Builder，循环受 `max_iterations` 限制。
Plan 模式在同一质量门前增加由 Leader 维护并派发的任务清单。

Brainstorm 模式把发散和判断严格分开。成员先独立播种，再从轮换的匿名同伴样本中
组合、变形和补充方向，最后通过反转假设、移除约束和跨领域类比拓展空间。IdeaSet
只追加、不提前淘汰；生成结束后，每个成员才对稳定候选 ID 进行私密排序。
Asterline 使用确定性的 Borda 计分聚合选票，再要求 Synthesizer 输出 top-5、
主推荐、备选方案和最小验证实验。工作区内的 Brainstorm Skill 定义可提取的卡片
和选票格式，并允许部署方按领域定制。

谁实现、谁审阅、谁规划、谁协调、谁参与，都在 `team.json` 的 `modes` 配置中定义。
`/runs` 只显示当前对话的模式相位、迭代预算、步骤负责人、verdict 时间线、阻塞和
下一条建议命令。`/new` 的 run 列表为空；`/resume` 会恢复所选对话对应的 runs。

```text
/block 等待 staging client secret
/note 已向平台团队申请 secret
/continue secret 已经可用
/verify cargo test
```

没有指定验证命令时，Asterline 会自动识别 `cargo test`、`npm test`、`pytest` 等
常见检查。选择 `/mode team` 后，后续消息会交给协调者组织整个团队：协调者维护清单，
完成后自动验证，验证失败会在迭代预算内自动继续协调者修复。

### 对话生命周期

- `/new` 保存当前对话，创建全新的后端会话，清空当前聊天和 run 列表，并选择
  normal 模式。
- `/resume` 打开选择列表，而不是猜测要恢复哪一次。所选聊天、成员配置、Roster、
  原生 session ID、模式和 runs 会一起恢复。
- `/runs` 只显示当前对话的内容：新对话从空列表开始，恢复的对话只显示属于它的
  runs。
- `--no-restore` 只跳过启动时的自动重放，不会删除任何保存的对话。

### 原生会话接入

使用 `Ctrl+N` 或 `Ctrl+B` 聚焦成员，按 `←`、`→` 移动，再按 `Enter`。Asterline
会暂时挂起界面，打开该成员的原生交互式 CLI，并尽可能恢复已有会话。退出 CLI 后
即可回到 Asterline。

接入期间产生的 Codex 和 Claude 消息会导入 Asterline 对话记录。Grok 和 Agy 会
恢复原生会话，但不会导入接入期间的消息。

如需绑定已有 CLI 会话，请在 `/team` 中选中成员，并在 `session id` 字段按
`Enter`。Asterline 会提取本地 Codex、Claude 和 Grok 会话元数据，在自身界面中
仅显示属于该成员当前工作目录的可搜索表格；用 `↑`/`↓` 选择并按 `Enter` 绑定，
再按 `s` 保存。按 `e` 可手动输入（Agy 目前需要手动输入），输入 `default` 可取消
显式绑定。

### 本地、持久、可检查

默认情况下，团队配置和 SQLite 数据库保存在项目内：

```text
<workspace>/.asterline/
├── team.json
└── asterline.sqlite3
```

数据库包含提示、回复、工具事件、成员路由、原始后端事件、日志、审批、会话和运行
历史。这些都是敏感开发数据，通常应加入 `.gitignore`：

```gitignore
.asterline/
```

如果团队协议不存在，Asterline 会创建 `.agents/skills/asterline-team/SKILL.md`；
同时用 `.agents/skills/asterline-brainstorm/SKILL.md` 定义卡片、投票和综合规则。
brainstorm skill 仅在缺失时安装，不会覆盖部署方的本地修改，因此可以按领域定制方法，
同时保留结构化卡片和选票协议。这些是工作区集成文件，不是运行历史；请自行审查并决定
是否纳入版本控制。

## 常用命令

| 命令                   | 用途                              |
| ---------------------- | --------------------------------- |
| `@<member> <message>`  | 向一个成员发送消息                |
| `@all <message>`       | 向全队广播                        |
| `/mode review`         | 选择 builder/reviewer 审阅模式    |
| `/mode plan`           | 选择规划清单与审阅模式            |
| `/mode brainstorm`     | 选择多轮、无评判的想法生成模式    |
| `/mode team`           | 选择协调者驱动的全团队模式        |
| `/runs`                | 查看运行状态、相位和下一步操作    |
| `/team`                | 编辑当前团队                      |
| `/skills`              | 为下一条提示选择 Skill            |
| `/find <text>`         | 搜索当前对话记录                  |
| `/diff`                | 查看未暂存修改和未跟踪文件        |
| `/logs`                | 打开持久化诊断日志                |
| `/new`                 | 创建新对话和新的后端会话          |
| `/resume`              | 选择并恢复之前的团队对话          |
| `/approve` / `/reject` | 处理待审批请求                    |
| `/retry`               | 重新发送最近一条用户请求          |
| `/abort`               | 取消运行中的任务、模式和验证      |
| `/help`                | 打开命令面板                      |

完整的运行步骤命令、Team 操作、提示历史、原生会话接入和 `/runs` 导航，请查看
[命令与键盘参考](docs/commands.zh-CN.md)。

## 权限与安全

Asterline 在本机启动后端进程，并继承其凭据、环境变量、文件系统访问和网络访问。
除了后端自身支持的控制，Asterline 不会额外提供进程级沙箱。

成员可以使用后端原生的沙箱和权限设置。Asterline 还会按可配置策略拦截其判定为
高风险的请求，覆盖用户消息、Agent 间转发和协作模式派发三类入口，详见
[审批与工具级控制](docs/approvals.md)（英文）。`--debug` 会关闭 Asterline 审批门，
仅适合受控的开发环境。

使用 `danger-full-access`、绕过式权限模式、自定义系统提示或允许 Agent 管理团队前，
请先阅读[配置与运维参考](docs/configuration.md)。

## 常见问题

### Asterline 会上传我的仓库吗？

不会上传到 Asterline 云服务。Asterline 启动的是本机后端 CLI；这些 CLI 仍然使用
各自厂商的认证、网络行为、数据政策和计费方式。

### 为什么新对话要求输入 `@member`？

Normal 模式有意要求第一条消息明确目标。可以使用 `@builder`、`@all`、`/ask`
或 `/all`；之后的普通文本可以沿用该目标。协作模式已有配置好的参与者，因此允许
直接输入任务。

### 为什么上一次模式消失了，或者又恢复了？

`/mode` 属于具体对话。`/new` 创建 `normal` 模式的新对话；`/resume` 恢复所选
历史对话保存的模式。

### 是否支持 `/clear`？

没有只隐藏历史的 `/clear` 命令。输入 `/cl` 或 `/clear` 时会补全为 `/new`：
旧对话会保留，同时开始一个干净的新对话。

### 模型字段为什么显示 `loading…` 或 `default`？

`loading…` 表示自动模型发现仍在进行。只有发现结果为空时才显示 `default`。
请确认所选后端已经安装、登录，并能在该成员工作目录中列出模型；也可按 `e`
手动输入模型。

### Agent 输出看起来不完整时应该看哪里？

用 `/logs` 查看原始后端 stderr 和适配器警告，用 `/focus <member>` 只看一个
成员，用 `/runs` 检查阶段和阻塞。`/diff` 可独立于聊天显示检查真实工作树结果。

## 文档

- [命令与键盘参考](docs/commands.zh-CN.md)
- [配置、本地数据、权限与故障排查](docs/configuration.md)
- [审批层与工具级控制](docs/approvals.md)
- [v0.2.3 发布说明](docs/releases/v0.2.3.md)
- [v0.2.2 发布说明](docs/releases/v0.2.2.md)
- [v0.2.1 发布说明](docs/releases/v0.2.1.md)
- [v0.2.0 发布说明](docs/releases/v0.2.0.md)
- 内置命令面板：`/help`
- 命令行帮助：`asterline --help`

命令参考提供中英文版本；配置与审批参考目前以英文提供。

## 开发

使用离线 Fake Agent 运行：

```bash
cargo run -- --fake
```

运行完整本地质量检查：

```bash
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked --no-fail-fast
```

如果已安装 `just`，也可以使用 `just run --fake`、`just install` 和 `just check`。

```text
src/
├── adapter/   后端事件流、模型发现、PTY 和进程适配器
├── domain/    团队配置与结构化事件
├── router/    队友消息、目标和转交上限
├── runtime/   调度、审批、会话和工作流
├── store/     SQLite 持久化与重放
├── tui/       对话、输入框、抽屉、命令和 Team 编辑器
└── app.rs     CLI 启动与产品装配
```

## 项目状态

Asterline 当前版本为 `0.2.3`，仍在积极开发。带版本标签的提交会通过 GitHub
Actions 自动发布 Linux、macOS 和 Windows 预编译包。在稳定版之前，配置、持久化
数据、命令和界面细节都可能不兼容地变化。

发布维护者请参考[发布指南](docs/releasing.md)。

## 反馈与贡献

可通过 [GitHub Issues](https://github.com/song0705/Asterline/issues) 提交可复现的
Bug 和功能建议。请附上操作系统、终端、Asterline 版本、后端 CLI 及其版本、相关
`/logs` 内容和最短复现步骤；发布前请删除不应公开的凭据、提示、源代码和 session
ID。

提交修改前请运行 `just check`，或执行上面的三条质量检查命令。真实后端 smoke
测试需要显式选择，默认测试和 CI 不会调用真实 CLI。

## 许可证

Asterline 使用 [MIT License](LICENSE)。
