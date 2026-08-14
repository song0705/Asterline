# Asterline

[English](README.md) · [简体中文](README.zh-CN.md)

[![CI](https://github.com/song0705/Asterline/actions/workflows/ci.yml/badge.svg)](https://github.com/song0705/Asterline/actions/workflows/ci.yml)
[![最新版本](https://img.shields.io/github/v/release/song0705/Asterline)](https://github.com/song0705/Asterline/releases/latest)
[![许可证：MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**在一个终端里，运行一支看得见、能恢复的编程 Agent 团队。**

Asterline 统一调度电脑上已经安装的 Codex、Claude、Grok 和 Agy 官方 CLI。它将各后端的原生事件汇入同一工作台，在成员之间路由任务，并把多步骤协作记录为结构化 Runs。

[安装](#安装) · [开始使用](#开始使用) · [文档](#文档) · [版本发布](https://github.com/song0705/Asterline/releases/latest)

![Codex 将前端设计方案发送给 Agy](docs/assets/asterline-codex-to-agy.webp)

## 为什么选择 Asterline

- **统一工作台。** 消息、思考、工具、diff、错误、审批和交接都归属于实际产生它的成员。
- **结构化协作。** Review、Plan、Brainstorm 和 Team 工作流为 Agent 任务增加负责人、有限重试、审阅结论和验证结果。
- **沿用原生 CLI。** Asterline 直接启动各厂商 CLI，保留原有认证、模型、权限设置与可恢复会话。
- **本地掌控。** 团队配置和运行记录保存在项目工作区，并提供明确的审批与取消入口。

## 安装

Asterline 支持 macOS、Linux 和 Windows 10/11。使用前，至少需要安装并登录一个受支持的 CLI。

| 平台    | 推荐安装方式                                                                         |
| ------- | ------------------------------------------------------------------------------------ |
| macOS   | 下载 `asterline-<version>-macos-universal.dmg`，打开后运行 `Install Asterline.pkg`。 |
| Windows | 下载并运行 `asterline-<version>-x86_64-windows-setup.exe`。                          |
| Linux   | 下载 `x86_64-unknown-linux-gnu` 或 `aarch64-unknown-linux-gnu` 对应的压缩包。        |

Linux 发布包要求 GNU/glibc 2.28 或更高版本，并内置 SQLite；目前不提供 Alpine/musl 发布目标。

安装器会同时提供完整命令 `asterline` 和短命令 `ast`。便携安装、源码构建、版本校验、自动更新、卸载与故障排查请参阅[安装指南](docs/installation.zh-CN.md)。

## 开始使用

进入希望 Agent 工作的项目，然后启动 Asterline：

```bash
cd /path/to/your/project
ast
```

首次启动时，Team 编辑器会发现 `PATH` 中受支持的 CLI。使用 `↑`/`↓` 选择成员，按 `Enter` 编辑，再按 `s` 保存团队。Asterline 不会代为安装厂商 CLI，也不会代替用户登录账号。

把第一项任务发给团队中显示的成员：

```text
@builder 审计这个仓库，并修复风险最高的缺陷
```

也可以先选择一个可追踪工作流，再输入任务：

```text
/mode review
修复支付回调的竞态问题，并补充回归测试
```

输入 `/help` 打开命令面板；输入 `/runs` 查看正在运行和已经保存的工作。

## 为可追踪的 Agent 协作而设计

### 基于原生 CLI 的实时团队

同一团队可以混用多个提供商，也可以多次使用同一后端。每个成员都可以独立设置职责、模型、推理强度、工作目录、系统提示、沙箱、权限模式、工具白名单与会话策略。

![Asterline Team 编辑器](docs/assets/asterline-team.webp)

| 后端   | 接入方式               | 会话恢复 | 模型发现                       |
| ------ | ---------------------- | -------- | ------------------------------ |
| Codex  | `codex exec --json`    | 支持     | `codex debug models`           |
| Claude | 流式 JSON              | 支持     | CLI 设置与模型别名             |
| Grok   | `grok agent stdio` ACP | 支持     | `grok --no-auto-update models` |
| Agy    | `stream-json` 事件     | 支持     | `agy models`                   |

Asterline 不代替各提供商的认证、计费、模型授权或用量限制。

### 用 Runs 管理工作，而不是堆叠对话回合

Runs 会保存当前阶段、清单负责人、尝试次数、阻塞原因、备注、验证结果、审阅结论和下一步操作。

| 模式         | 适合场景               | 执行方式                             |
| ------------ | ---------------------- | ------------------------------------ |
| `normal`     | 直接对话与任务转交     | 路由给一个成员或整个团队             |
| `review`     | 带质量门的实现任务     | Builder、Reviewer 结论、有限修改循环 |
| `plan`       | 多步骤且有负责人的工作 | 规划、分配、执行、审阅、验证         |
| `brainstorm` | 先探索、后判断         | 生成、私密投票、排名、综合           |
| `team`       | 端到端协调交付         | Coordinator 负责执行与整合           |

工作暂停后，Run 仍然可以继续推进：

```text
/block 等待 staging client secret
/note 已向平台团队申请 secret
/continue secret 已经可用
/verify cargo test
```

### 保存在本地、可以恢复的历史记录

`/new` 创建干净的新对话；`/resume` 恢复已保存的聊天、团队、后端会话、模式与 Runs。项目状态默认保存在：

```text
<workspace>/.asterline/
├── team.json
└── asterline.sqlite3
```

数据库可能包含提示、回复、工具输出、路由、审批、日志与会话标识。请将它视为敏感开发数据；除非项目明确需要版本管理，否则应把 `.asterline/` 加入 `.gitignore`。

## 安全边界

Asterline 在本机启动后端进程，并继承其凭据、环境变量、文件系统权限与网络权限。每个后端仍然受对应提供商的数据政策和权限模型约束。

Asterline 为高风险请求、Agent 间转发、工作流派发和 Agent 发起的成员变更增加审批门，但不会在所选后端之外再提供一层进程沙箱。放宽权限前，请阅读[审批与工具控制](docs/approvals.md)。

## 常用命令

| 命令                   | 用途                     |
| ---------------------- | ------------------------ |
| `@<member> <message>`  | 向一个成员发送任务       |
| `@all <message>`       | 向整个团队广播           |
| `/mode`                | 选择普通对话或协作模式   |
| `/runs`                | 查看工作状态和下一步     |
| `/team`                | 编辑当前团队             |
| `/resume`              | 恢复已保存的对话         |
| `/approve` / `/reject` | 处理待审批请求           |
| `/abort`               | 取消正在运行的工作与验证 |
| `/help`                | 打开命令面板             |

[完整命令与键盘参考](docs/commands.zh-CN.md)还包含原生会话接入、导航、Run 步骤、Skills、日志、搜索和 diff。

## 文档

- [安装与更新](docs/installation.zh-CN.md)
- [命令与键盘参考](docs/commands.zh-CN.md)
- [配置与本地数据](docs/configuration.md)
- [审批与工具控制](docs/approvals.md)
- [版本发布说明](docs/releases/v0.2.5.md)
- [维护者发布流程](docs/releasing.md)

程序内可通过 `/help` 和 `asterline --help` 查看帮助。

## 开发

无需调用真实后端即可运行 Asterline：

```bash
cargo run -- --fake
```

运行本地质量检查：

```bash
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked --no-fail-fast
cargo audit
```

完整质量检查需要 Rust 1.88 或更高版本以及 `cargo-audit` 0.22.2。真实后端 smoke 测试需要显式启用，受控的本地与 Actions 入口见[说明文档](docs/real-smoke.md)。可复现的问题和范围明确的建议请提交到 [GitHub Issues](https://github.com/song0705/Asterline/issues)。

## 项目状态

Asterline 仍处于 1.0 之前的积极开发阶段。配置、持久化数据、命令和界面细节可能在版本之间发生变化。

## 许可证

Asterline 使用 [MIT License](LICENSE)。
