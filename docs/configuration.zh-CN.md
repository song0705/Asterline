# Asterline 配置与运维

[English](configuration.md)

本页涵盖团队文件、运行时数据、权限、CLI 参数、Agent 协作和故障排查。产品概览请返回
[中文 README](../README.zh-CN.md)；交互控制请参阅[命令参考](commands.zh-CN.md)。

## Asterline 如何解析团队

启动时，Asterline 按下列顺序选择 roster：

1. `--team <PATH>` 加载指定 JSON 文件。
2. 除非设置 `--pick-team`，否则复用 `<workspace>/.asterline/team.json`。
3. 在 `PATH` 检测受支持后端可执行文件，并打开 Team builder。
4. 若没有已保存团队且没有受支持可执行文件，启动会显示设置提示后停止。

用 `/team` 修改正在运行的 roster。Asterline 在启动时一次性加载已安装 Agent CLI 与它们的
模型目录；之后打开 `/team` 会复用缓存。同一次查询也会读取非 Codex CLI 的本地权限默认值
用于显示，但不会将它们复制进 `team.json`。Codex 则会在 thread start/resume 时接收
Asterline 显式设置的默认 `approvalPolicy: "never"`。聚焦成员的 **model** 字段并按
`t` 可随时重新拉取该目录。按 `s` 应用更改、替换成员 runner 并保存更新后的团队。

### 平台路径与后端历史

Unix 上，Asterline 使用 `HOME` 作为用户级配置目录；Windows 上优先使用 `USERPROFILE`。
两个平台都接受另一个变量作为后备。`CODEX_HOME` 覆盖默认 `.codex` 目录，影响 session
选择、attach 后 transcript 导入以及全局 Codex skill/plugin 发现。

默认历史根目录为 `<Codex home>/sessions`、`<user home>/.claude/projects` 和
`<user home>/.grok/sessions`。Windows 项目匹配会把盘符大小写及 `/`、`\\` 分隔符视为
等价。后端发现使用平台 `PATH`；Windows 还遵循 `PATHEXT`，并启动解析出的 `.exe`、`.cmd`
或 `.bat` 路径。离开附加的 CLI 时，在 Unix 使用 `Ctrl+D`，在 Windows 使用 `Ctrl+Z` 后
按 `Enter`，或输入 `/exit`。

### 感知安装方式的更新

只有由 Windows Setup 安装的副本会自动更新。便携 ZIP 与源码构建永不会自行改写。已安装的
副本最多每 24 小时检查一次 GitHub 最新稳定 Release；有新版本时，它会下载 Setup，依据
同一 Release 的 `SHA256SUMS` 验证，然后在当前 Asterline 进程退出后静默启动 Setup。

运行 `ast update` 可立即强制检查。在 macOS 和 Linux，它只更新能够证明属于官方
Homebrew Formula 的安装：先执行 `brew update`，再只升级目标 Formula。它不会覆盖便携
归档、源码构建、直接安装的 macOS 包或直接安装的 `.deb` / `.rpm`；这些安装方式各自使用
明确的替换路径。`ast --update` 仍是别名。

使用 `ast --no-auto-update` 可跳过一次 Windows 自动检查。后台检查发生网络故障会被忽略，
不会阻止 Asterline 启动；强制 Windows 检查会将错误报告到终端。

## 团队文件

```json
{
  "name": "product-team",
  "workspace": "/path/to/project",
  "default_target": { "member": "builder" },
  "max_auto_relays": 6,
  "members": [
    {
      "display_name": "Builder",
      "backend": "codex",
      "role": "implementation",
      "sandbox": "workspace-write",
      "effort": "high"
    },
    {
      "display_name": "Reviewer",
      "backend": "claude",
      "role": "review and risk analysis",
      "permission_mode": "plan",
      "effort": "medium"
    },
    {
      "display_name": "Grok",
      "backend": "grok",
      "role": "implementation",
      "sandbox": "workspace-write",
      "permission_mode": "auto"
    }
  ],
  "modes": {
    "review": {
      "builder": "builder",
      "reviewer": "reviewer",
      "max_iterations": 3
    },
    "plan": {
      "leader": "builder",
      "builder": "builder",
      "reviewer": "reviewer",
      "max_iterations": 3,
      "auto_execute": true
    },
    "brainstorm": {
      "participants": ["builder", "reviewer", "grok"],
      "generation_rounds": 3,
      "ideas_per_round": 4
    },
    "team": {
      "coordinator": "builder",
      "allow_add_members": false
    }
  },
  "approvals": {
    "gate": ["git", "shell", "file"],
    "apply_to": ["user", "relay", "mode"]
  }
}
```

`id` 可选。Asterline 从 `display_name` 推导稳定 handle，因此 `QA Lead` 会变为 `qa-lead`。
只有需要自定义 `@handle` 时才设置 `id`。显式 ID 只能包含 ASCII 字母、数字、`-` 和 `_`。
用于路由的 ID 与显示名都必须唯一，`all` 保留给广播目标（不区分大小写）。

### 团队字段

| 字段 | 必需 | 含义 |
| --- | --- | --- |
| `name` | 是 | Asterline 中显示的团队名称 |
| `workspace` | 是 | 默认工作目录 |
| `members` | 是 | 非空成员列表 |
| `default_target` | 否 | `{"member":"id"}`、`"all"` 或第一位成员 |
| `max_auto_relays` | 否 | 自动队友转交上限；默认 `6` |
| `modes` | 否 | 协作模式的角色绑定与预算 |
| `approvals` | 否 | 审批门类别和界面 |

### 协作模式（`modes`）

可为 `/mode review`、`/mode plan`、`/mode brainstorm` 和 `/mode team` 设置可选绑定。除
Plan 的 `builder` 和 `reviewer` 外，省略的角色字段会从成员 role 和 `default_target` 推导（Review 的
builder ≈ 默认目标或第一位非 reviewer；reviewer ≈ role 含 `review`；leader ≈ role 含
`plan` 或 `lead`，否则为第一位 participant；participants = 完整 roster）。Plan 的
`builder` 是必填项且不会自动推导。`reviewer` 可省略，省略后可执行 checklist 会直接交给
Builder。`auto_execute` 默认 `true`；设为 `false` 时，最终 checklist 会等待 `/approve` 后才派给
Builder。预算默认值为：`max_iterations = 3`、`generation_rounds = 3`、
`ideas_per_round = 4`、`auto_execute = true`。Brainstorm 至少要求两位
不同的已解析 participant；重复 ID 或同一成员同时以 ID 和显示名引用都会被拒绝。
`/mode` 面板里 `s` 选择当前模式并应用其 pending 覆盖，`w` 把当前模式的本对话覆盖写入
`team.json`。

Brainstorm 将发散与收敛分开。第一轮收集独立种子；后续轮向每位 participant 展示轮转的
匿名同伴样本，用于构建、合并、变异和延展想法。早期贡献留在已持久化的 idea set 中。
最终生成轮后，每位 participant 私下对带标签的 idea 排名。Asterline 用确定性的 Borda
计票汇总选票，再派发一次中性的排序综合。每个生成 idea 都从 `@@brainstorm_card` envelope
提取并分配 canonical candidate ID，因此选票不依赖模型特定的 Markdown 编号。

| 字段 | 模式 | 含义 |
| --- | --- | --- |
| `builder` | review/plan | Review 中实现更改的成员；Plan 中必填，负责执行最终 checklist |
| `reviewer` | review/plan | 发出 `@@review` verdict；Plan 中可选，仅审核 checklist |
| `leader` | plan | 编写并修订 checklist 的成员 |
| `participants` | brainstorm | 所有生成轮的 roster |
| `generation_rounds` | brainstorm | 种子/构建/延展轮预算（默认 3，最少 2） |
| `ideas_per_round` | brainstorm | 每位成员/每轮固定输出的 idea card（默认 4，最少 3）。多出来的卡片会被丢掉。 |
| `coordinator` | team | 协调整个团队 Run 的成员 |
| `allow_add_members` | team | 允许用 `@@team_member` 自由加人（默认只能用当前队员） |
| `max_iterations` | review/plan/team | 阻塞前的循环预算（默认 3） |
| `auto_execute` | plan | 自动派发最终计划（默认 true）；false 时须 `/approve` |
| `reviewer_hint` | review | 可选，追加到审阅成员提示里的说明 |

每种 mode 都有自己的配置形状；不相关字段会被 Serde 拒绝，而不是被静默接受后忽略。

### 审批（`approvals`）

旧的 `team.json` 里可能还有 `approvals` 段（`gate`、`keywords`、`apply_to`）。
这些字段只为兼容保留，不再拦住用户消息、转交或模式派发。写
`"approvals": { "manual": true }` 或加上 `--manual-approvals` 时，Codex 工具
询问会显示输入框上方的卡片；默认自动通过。Plan 的 `auto_execute: false`
是另一套确认。`@@team_member` 不再审批。设置 `ASTERLINE_NO_BELL=1` 可关闭
审批、暂停路由、阻塞 Run 和成员错误事件的终端 BEL/OSC 9 通知。

审批门与后端原生 sandbox/权限执行的关系，请参阅[审批与工具级控制](approvals.zh-CN.md)。

### 成员字段

| 字段 | 必需 | 含义 |
| --- | --- | --- |
| `display_name` | 除非由 `id` 提供，否则是 | 可见成员名 |
| `backend` | 是 | `codex`、`claude`、`grok` 或 `agy` |
| `role` | 是 | 自由文本的团队职责 |
| `id` | 否 | `@member` 和路由使用的稳定 handle |
| `cwd` | 否 | 高级的逐成员工作目录覆盖 |
| `model` | 否 | 省略时交给后端 CLI |
| `effort` | 否 | 支持时在已选模型内选择 |
| `system_prompt` | 否 | 额外成员指令 |
| `sandbox` | 否 | Codex；映射给 Grok/Agy，对 Claude 忽略 |
| `permission_mode` | 否 | `/team` 中的后端原生控制 |
| `allowed_tools` | 否 | 后端特定的工具 allowlist |
| `session_policy` | 否 | `resume`（默认）或 `fresh` |
| `session_id` | 否 | 要恢复的原生 CLI session/conversation ID |

启动时，已绑定 session 的 `resume` 成员会扫描 Asterline 关闭期间写进原生会话的新记录
（Grok CLI、Codex、Claude），只把尚未出现过的消息导入当前对话。

在 `/team` 中，`resume` 成员直接显示已绑定的原生 session ID。尚未绑定时显示
`select a session`，明确要求使用 picker 或手动填写 ID；`fresh` 成员显示 `not set (fresh)`。
UI 不会把未绑定 session ID 称为 `default`。

`cwd` 有意不能在 `/team` 编辑：其中创建的成员使用团队 workspace。只有在高级多仓库或
monorepo 设置中，某成员必须在不同工作目录运行时，才保留可选 `team.json` 字段。它也决定
该成员的后端 session project 和 model catalog cache key。

两种策略都会在首次调用后固定并复用后端 session ID。`resume` 会在存在时保留已持久化 ID。
把成员切为 `fresh` 会一次性丢弃旧 ID，因此下一次调用会创建新的原生 CLI conversation；
该新发现的 ID 会被后续调用复用。`fresh` 不会每一轮都创建独立 conversation。

设置 `session_id` 可将成员绑定到原生 CLI 历史中的特定 conversation。Codex 使用 App
Server 的 `thread.id`，通过 `thread/resume` 恢复；Claude、Grok、Agy 分别使用
`claude --resume`、ACP `session/load` 和 `agy --conversation`。Team editor 中使用 `default`
可清除显式 ID。

Asterline 会把自己的 Codex thread 和 Claude session 命名为 `Asterline · <成员名>`，以便在
会显示标题的原生历史中辨认。`<workspace>/.asterline/team.json` 保存的 roster 也绑定到其
所在 workspace：项目移动后，Asterline 会在启动成员前修正过期的已序列化 workspace，使新的
原生 transcript 仍归属你实际打开的项目。原生 session picker 会按工作目录过滤。Claude
Code 会有意不在交互式 picker 中列出由 `claude -p` 或 Agent SDK 创建的 session。
Asterline 的 Team editor 不做这项过滤：选中该成员的 **session id** 字段并按 `Enter`
即可选择这些 transcript，再按 `s` 绑定。Asterline 会直接执行
`claude --resume <id>` 恢复所选会话；`/attach <member>` 也使用同一条指定 ID 的路径。

权限模式、sandbox 映射和 allowed-tool 行为取决于后端。不要假定同一个字段在四个 CLI 上
有相同效果。

## 后端设置支持

下表描述当前 Asterline adapter 实际传递给每个 CLI 的内容。它有意窄于 Team editor 接受
字段的并集。

| 设置 | Codex | Claude | Grok ACP | Agy |
| --- | --- | --- | --- | --- |
| `cwd` | App Server `thread/start` / `thread/resume` | 进程 cwd | ACP session `cwd` | 进程 cwd 加 `--add-dir`；prompt 标识项目 workspace |
| `model` | App Server `model` | `--model` | Agent `--model` | `--model` |
| `effort` | App Server `effort`；picker 跟随模型 metadata | `--effort`（经 `max`） | cache-defined level 作为 Agent `--reasoning-effort` | 模型特定 effort；仅由列出的模型定义 |
| `sandbox` | 并进 `/approvals` 预设 | 不传递（`/team` 不显示） | 默认 `workspace`；`/team` 不显示 | 默认 off；`/team` 不显示 |
| `permission_mode` | Codex `/approvals` 预设（默认 `Ask for approval`） | `acceptEdits` / `plan` / `auto` / `dontAsk` / `bypassPermissions` | `default` / `auto` / `plan` / `--always-approve` | `--mode accept-edits`/`plan`；skip-permissions 仅在 `off` 时 |
| `allowed_tools` | 不传递 | `--tools`（硬 built-in-tool allowlist） | 加入 ACP session rules；不是硬协议级 allowlist | 不传递 |
| `system_prompt` | App Server `developerInstructions` | `--append-system-prompt` | ACP session `rules` | 加在 print prompt 前 |
| `session_policy` | Resume 或 fresh | Resume 或 fresh | ACP `session/load` 或 `session/new` | Resume 或 fresh conversation |
| `session_id` | App Server `thread/resume <thread.id>` | `claude --resume <id>` | ACP `session/load` | `agy --conversation <id>` |

`/team` 显示的是各本机 CLI 真正接受的名字。`team.json` 仍用原来的共用字段，旧
roster 可以继续读：省略/`default`、`dontAsk` / `bypassPermissions` 映射为 Codex
`never`；`plan` / `acceptEdits` 映射为 `untrusted`；`auto` 映射为
`on-request`。Codex 的 command、file-change、permission-escalation 回调显示为
Asterline 待审批，并把你的一次性决定返回给 live App Server thread；已选 sandbox 仍是独立
边界。对 Claude 和 Grok，只选择已安装 CLI 版本接受的 permission mode。Asterline 会序列化
配置值，但不会在启动前协商厂商版本兼容性。Agy 要求 1.1.12 或更高版本；更早版本会在
后端发现阶段排除，并在运行前拒绝，因为它们缺少结构化 streaming 或忽略 headless
`--mode` 执行。近期 Claude CLI 不再把 `default` 列为 `--permission-mode` 选项；配置为
`default` 时 Asterline 会省略该 flag，让 CLI 默认值生效。

## 模型发现

模型选项在每位成员的有效工作目录中解析。

Asterline 在启动时检测全部四个受支持 CLI，并为每个已安装 backend 异步启动一份 workspace
目录查询。查询不依赖已配置 roster，因此后来新增的 Codex、Claude、Grok 或 Agy 成员也会
直接使用已在加载的结果。查询完成前 Team editor 显示 `loading…`；CLI 报告具体默认模型后
显示该模型名，而非占位值 `default`。结果在该 `ast` 进程生命周期中保存；打开和关闭
`/team` 都不会重新运行。新工作目录显示 `not loaded · press Enter`，不会静默启动另一项
后台查询；明确打开该成员的 **Model** 字段时，才会为该 backend 与工作目录加载一份可共享
的 catalog。
目录成功加载但 CLI 未识别默认值时，字段正确显示 `CLI default`，其 picker 仍包含发现的
模型。

聚焦 `model` 后按 `t`，无论之前查询成功或失败，都会重新拉取该 backend/workspace 的目录。
刷新结果会在所有匹配成员间共享；若已经在加载，`t` 会保留这份共享请求，不会重复创建。

| 后端 | 来源 |
| --- | --- |
| Codex | App Server `model/list`；仅后备时用 `codex debug models` |
| Claude | 本地 settings/env；选择加入的 gateway `/v1/models`；Claude cache |
| Grok | `grok --no-auto-update models` |
| Agy | `agy models` |

Claude 的目录以本地配置优先。Asterline 会从与 Claude Code 相同的 user/project/local settings
层级读取 `model`、`availableModels`、`modelOverrides` 及相应的 `ANTHROPIC_*` 变量。对于
`modelOverrides` 条目，UI 显示 provider ID，同时保留传给 `claude --model` 的 Claude 端 key。

当本地配置通过 `CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1` 和非 Anthropic 的
`ANTHROPIC_BASE_URL` 选择加入 Claude gateway discovery 时，Asterline 使用相同 endpoint、
认证和 custom header 读取 `/v1/models`；刷新失败时使用 Claude 本地 `gateway-models.json`
cache。没有配置模型或启用 gateway discovery 时，它有意不虚构 Claude 模型列表：请使用
原生 CLI 默认值，或输入显式的本地/provider model ID。

Agy 本地 `~/.gemini/antigravity-cli/settings.json` 的 `model` 值常是人类可读标签而非 CLI ID。
Asterline 将它与 `agy models` 返回的 ID/label 对匹配，再将该配置模型显示为启动默认值。

首次运行的 Team builder 也会立即加载每个已安装 CLI 的目录；`/team` 复用 `ast` 启动时加载
一次的目录。Agent 字段列出四个支持的 CLI，包含安装状态和发现的 model/effort 摘要，并禁用缺失
CLI。打开成员的 `model` 字段即可浏览已在加载的目录。输入可按显示名、model ID 或 description
过滤，使用 `↑` / `↓` 选择模型。只有所选模型明确报告 effort 设置时才显示 `←` / `→`；
`←` 降低、`→` 提高所选设置。这些设置随该模型应用。使用 `↑` / `↓` 浏览不会更改已保存的
effort override。高亮模型若未声明现有 override，`Enter` 保留当前配置并要求显式 `←` / `→`
选择，而不是静默替换为猜测的默认值。Grok 从自己的 CLI cache 读取每个列出模型的菜单，
Agy 读取模型限定的设置；两者都不会得到虚构的通用 effort 菜单。Claude 没有机器可读的
effort capability catalog，因此 Asterline 不会从模型名猜测。已配置 Claude alias 保持原样，
gateway model 使用 gateway 的 `display_name`。发现模型时 picker 选择 CLI 标记的默认模型；
若没有标记则选第一个发现的模型。此时它只显示真实模型条目。只有发现不到模型时才可用
`default`。聚焦 `model` 并按 `t` 可重新拉取目录；刷新会由相同 backend/workspace 成员共享。
按 `e` 可手动输入 model ID。

只有发现能力 metadata 时 reasoning effort 才按模型生效。不支持的级别会省略；若可用，模型
报告的默认值直接显示为原生默认值，不是 override。Agy 只公开已发现模型中编码的 effort；
Grok 没有独立的 Team effort 控件。

## Streaming 与资源限制

Asterline 在进程、adapter、runtime、import 和 UI 边界应用明确上限，避免格式错误或过于冗长
的后端导致内存无限增长：

- JSON 协议记录最多 8 MiB；stderr 记录最多 1 MiB。
- 可见 assistant message 最多 4 MiB；单个 tool detail 最多 1 MiB；PTY 输出保留最多 4 MiB
  未读取数据。
- 验证输出最多 1 MiB，并保留流开头和结尾的有用内容。
- 产品 runtime 到 TUI 的队列保留 2,048 个事件并施加 backpressure。Abort 和 shutdown 使用
  独立 control channel，因此 stream 流量饱和时仍能响应。
- 导入的 JSONL 记录最多 8 MiB；导入的单条消息最多 1 MiB。

内容被缩短时，Asterline 会插入明确的 truncation marker，而不会把截断值展示为完整内容。

## 运行时数据

默认 workspace 状态为：

```text
<workspace>/.asterline/
├── team.json
├── roster.md
└── asterline.sqlite3
```

SQLite 保存会话、工具事件、队友路由、原始后端事件、日志、审批、session 标识符、Runs、
checklist、timeline 和验证结果。

像保护其他开发 transcript 一样保护此目录。大多数仓库应忽略它：

```gitignore
.asterline/
```

重新打开 Asterline 默认恢复当前选中的对话。`/new` 与 `/clear` 都会在普通模式创建干净会话和
新的后端 session，同时保留旧数据库记录。成员、Runs 或验证处于活动状态时它们会被拒绝；先按
`Esc` 并等待取消。`--no-restore` 跳过启动 replay，不删除数据。`--db <PATH>` 将数据库移出
workspace。

`/resume` 打开已保存聊天的 picker。恢复聊天也会恢复该聊天所属的 roster、完整成员配置和
每位成员的原生后端 session ID。

## 终端颜色主题

Asterline 为深色和浅色终端背景使用不同的后端身份 palette。默认读取惯用的 `COLORFGBG`
值；终端不暴露背景时回退到深色 palette。

自动检测不匹配终端时，可设置 `ASTERLINE_THEME`：

```bash
ASTERLINE_THEME=dark asterline
ASTERLINE_THEME=light asterline
```

`auto` 恢复检测。后端身份还通过成员名、后端标签与持续的会话 rail 表达，因此颜色不是唯一
线索。

## 权限与安全

Asterline 本地启动后端 CLI，并继承其凭据、环境变量、文件系统访问和网络访问。它不为后端
进程提供安全边界。

后端原生权限和 sandbox 设置仍然适用。Codex 工具询问默认自动通过；打开
`--manual-approvals` 或 `approvals.manual` 后才用输入框上方的卡片。

`danger-full-access` sandbox 与 bypass 风格 permission mode 应视为明确的信任决定。不要假定
团队 role 或模型名限制了底层进程可访问的内容。

## Agent 间协作

Asterline 缺失时会创建 `.agents/skills/asterline-team/SKILL.md`，并告诉每位成员这个 team
skill 位于该路径。完整协议留在 workspace，普通聊天不再每轮重复；该提示刻意不调用
`$asterline-team`，因此不会强制 Codex 展开整份 skill。自身和队员的实时身份与状态写在
`.asterline/roster.md`，团队或成员状态变化时更新。team skill 会说明该文件的用途，不会再写进
每一轮 prompt。

它也会在缺失时创建 `.agents/skills/asterline-brainstorm/SKILL.md`。Brainstorm mode 会为每次
生成、投票和综合派发加载该文件。已有副本永不升级或覆盖，因此部署可以自定义方法，同时
保留 `@@brainstorm_card` 和 `@@brainstorm_vote` schema。

### 队友消息

```text
@@team_message {"to":"reviewer","body":"implementation is ready for review"}
@@team_message {"to":["builder","reviewer"],"body":"align on the API"}
@@team_message {"to":"all","body":"report status"}
```

Asterline 会从可见响应移除有效 envelope、渲染 handoff、持久化并把正文发给目标成员。自动
handoff 由 `max_auto_relays` 限制；`/retry` 恢复暂停路由。

### Roster 请求

Agent 可请求缺失的专长：

```text
@@team_member {"display_name":"QA","backend":"codex","role":"tests"}
```

Asterline 校验重复 ID 与名称、启动 runner、保存 roster 并广播更新团队。Agent envelope 可以
添加成员但不能删除；删除仍然是 `/team` 操作。

### Run checklist 更新

活动 Run 中，Agent 可以增加、更新、分配、重命名或删除 checklist step：

```text
@@run_step {"action":"add","owner":"builder","title":"Write tests"}
@@run_step {"action":"doing","step":1,"note":"Implementing edge cases"}
@@run_step {"action":"done","step":1,"note":"Tests pass"}
@@run_step {"action":"block","step":2,"note":"Waiting for credentials"}
@@run_step {"action":"assign","step":2,"owner":"reviewer"}
```

这些更新显示在 `/runs`，并记录在 Run timeline 中。

## CLI 参数

| 参数 | 描述 |
| --- | --- |
| `--team <PATH>` | 加载 JSON 团队并跳过 builder |
| `--pick-team` | 忽略已保存 roster 并打开 builder |
| `--workspace <PATH>` | 设置 workspace；默认当前目录 |
| `--db <PATH>` | 设置 SQLite 数据库路径 |
| `--no-restore` | 启动时不 replay 已持久化聊天 |
| `--debug` | 开发模式 |
| `--manual-approvals` | 显示 Codex 工具审批卡片（默认关闭） |
| `--fake` | 使用离线 fake agent 而不是后端 CLI |
| `--banner` | TUI 前打印紧凑启动横幅 |
| `-h`、`--help` | 打印命令行帮助 |

示例：

```bash
asterline --workspace ~/code/api
asterline --pick-team
asterline --team ./team.json --db ~/.local/share/asterline/api.sqlite3
asterline --fake --no-restore
```

## 故障排查

### 未找到受支持后端

确认 `codex`、`claude`、`grok`、`agy` 至少有一个已安装、已认证且在 `PATH` 上。也可传入
有效文件：`--team`。

### 模型 picker 没有检测到模型

等待启动目录加载提示完成，再打开 `model` 字段。发现失败时，确认选定 CLI 已认证，且能在
成员工作目录列出模型，然后聚焦 `model` 并按 `t` 重试。按 `e` 可手动输入模型名。

### 打开了错误 roster

运行 `asterline --pick-team` 重建保存的 roster，或使用 `/team` 后按 `s` 应用更改。

### 不加载之前的 transcript 启动

使用 `asterline --no-restore`。它只跳过 replay，不会删除 SQLite 数据。使用 `/new` 或 `/clear`
可创建具有新后端 session 的干净 conversation。

### 测试但不调用后端 CLI

运行 `asterline --fake`。fake mode 会演练 runtime 和 TUI，但不调用 Codex、Claude、Grok 或
Agy。

### 离开附加 CLI 后键盘输入异常

先安装当前 Asterline build；它会在挂起、恢复和退出时还原终端键盘状态，并在 VS Code 与
Cursor 终端禁用 enhanced keyboard reporting。旧 build 若遗留终端协议，请在受影响 shell
运行一次：

```bash
printf '\033[=0u'
```

然后在新的终端会话中启动新安装的二进制。
