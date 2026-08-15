# Asterline 命令与键盘完整参考

本文档完整说明 Asterline 的启动参数、输入语法、斜杠命令、抽屉界面和键盘操作。
安装与产品概览请返回[中文 README](../README.zh-CN.md)。也可查看
[英文版](commands.md)。

## 快速上手

```text
@builder 检查仓库并解释整体架构
/mode brainstorm
怎样降低索引延迟？
/runs
/verify run-2 cargo test
/new
/resume
```

输入 `/` 可列出 Asterline 命令，输入 `@` 可列出团队成员。用 `↑`、`↓` 选择，
按 `Tab` 或 `Enter` 插入，按 `Esc` 关闭候选列表。

## 本文的参数写法

- `<参数>` 表示必填，`[参数]` 表示可选；实际输入时不要键入尖括号或方括号。
- `run-<id>` 表示界面显示的运行编号，例如 `run-2`。
- `<消息>`、`<备注>`、`<原因>`、`<标题>`、`[命令]` 会读取这一行剩余的全部
  内容，因此可以包含空格。
- 命令名和模式名使用小写。
- 省略 `[run-<id>]` 时，命令作用于当前对话中最近的一次运行。
- 必填参数缺失、参数无效或命令不存在时，不会把内容发给 Agent，而会打开
  `/help`。

## 启动 Asterline

```text
asterline [OPTIONS]
```

`--team`、`--workspace` 和 `--db` 同时支持 `--参数 值` 与 `--参数=值` 两种写法。

### `--team <PATH>`

从 `PATH` 加载 JSON 团队配置并跳过交互式团队创建器。

```bash
asterline --team .asterline/team.json
```

### `--pick-team`

即使已有保存的团队，也重新打开交互式团队创建器。

```bash
asterline --pick-team
```

### `--workspace <PATH>`

设置各成员继承的工作目录。默认值是启动 Asterline 时所在的目录。

```bash
asterline --workspace /path/to/project
```

### `--db <PATH>`

设置 SQLite 数据库路径。默认是
`<workspace>/.asterline/asterline.sqlite3`。

```bash
asterline --db /path/to/asterline.sqlite3
```

### `--no-restore`

启动时不重放最近保存的对话。它不会删除历史对话，之后仍可用 `/resume` 选择恢复。

```bash
asterline --no-restore
```

### `--debug`

启用开发模式并关闭 Asterline 的高风险操作审批门。后端自身的权限控制仍然有效。
只应在受控环境中使用。

```bash
asterline --debug
```

### `--fake`

使用确定性的离线 Fake Agent，不启动真实后端 CLI。适用于开发、演示和测试。

```bash
asterline --fake
```

### `--banner`

进入 TUI 前打印精简启动横幅。

```bash
asterline --banner
```

### `update`

按当前正在运行的二进制所属安装方式执行显式更新：

- Windows Setup 管理的安装保留原有的校验安装器流程：检查最新稳定 Release，使用
  同一 Release 的 `SHA256SUMS` 校验 Setup，安排在 Asterline 退出后运行，然后退出。
- macOS 或 Linux 上的 Homebrew 安装会先执行 `brew update`，再执行
  `brew upgrade song0705/asterline/asterline`。Asterline 会先确认自己的可执行文件
  确实位于该 Formula 的安装前缀内。

便携归档、直接安装的 `.deb`/`.rpm`、macOS 安装包和源码构建版不会被猜测覆盖；
命令会明确提示对应的手动更新路径。

```bash
ast update
```

`ast --update` 继续作为向后兼容别名。

### `--no-auto-update`

本次启动跳过 Windows Setup 管理版本每 24 小时一次的后台更新检查。它不会永久
关闭更新，也不会阻止显式 `--update`；对于本就不会自动更新的平台和安装方式没有
效果。

```powershell
ast --no-auto-update
```

### `-h`、`--help`

打印命令行帮助并退出。它与产品内的 `/help` 不是同一个功能。

```bash
asterline --help
```

## 发送消息

### `@<member> <message>`

向一个团队成员发送消息。成员名必须存在于 `/team`。

```text
@builder 实现解析器修改并运行测试
```

普通模式的新对话中，第一条消息必须明确指定目标。之后的纯文本会沿用上一个目标。
在协作模式中，纯文本会按该模式配置的参与者开始或继续运行。

### `@all <message>`

向所有已启用成员发送同一条消息。

```text
@all 审查这个 API 方案并各自指出一个风险
```

### `@<member> /<skill> [arguments]`

通过指定成员的原生 CLI 调用已发现的 Skill。在成员前缀后输入 `/` 可打开 Skill
补全。

```text
@builder /asterline-team 检查当前运行
```

定向补全会填入该成员实际发现的原生调用语法：Codex 使用 `$skill`，Claude 插件 Skill
保留 `/插件名:skill` 命名空间。为兼容手动输入，只有精确匹配已发现
Codex Skill 的 `@<Codex 成员> /skill` 才会转换。`@member /` 会列出真正的 `/attach`，
以及仅属于该成员的已发现 Skill。`/attach` 会打开[原生会话](#接入原生会话)，在其中可以使用
后端自己的交互式斜杠命令菜单。除非精确匹配已发现的 Skill，未知定向 slash 命令不会进入
非交互 runner。Slash 控制命令必须指定一个成员；Asterline 会拒绝 `@all /…`，不会把它广播出去。

## 消息与对话命令

### `/ask`

```text
/ask <member|all> <message>
```

向一个指定成员发送消息；目标写成 `all` 时向全队广播。它分别等价于
`@member` 和 `@all`。

```text
/ask reviewer 检查错误处理
/ask all 汇报当前进度
```

### `/all`

```text
/all <message>
```

向所有已启用成员广播消息。

```text
/all 停止编辑并汇报发现
```

### `/new`

```text
/new
```

保存当前对话，创建一个新对话，清空当前显示的聊天记录和运行列表，为所有后端
创建新的会话 ID，并把终端模式重置为 `normal`。如果仍有成员、协作运行或验证
处于活动状态，`/new` 会被拒绝；请按 `Esc` 并等待取消完成。

`/clear` 有意不作为独立命令存在。输入 `/cl` 或 `/clear` 时，补全列表会显示
`/new`；接受补全后执行的是完整的新建对话操作，而不是仅隐藏屏幕历史。

### `/resume`

```text
/resume
```

打开历史对话选择器。用 `↑`、`↓` 选择，按 `Enter` 恢复该对话的聊天记录、
团队配置、成员列表、原生后端会话 ID、活动模式以及属于该对话的运行；按 `Esc`
取消。

`/resume` 不接受 ID 或其他参数。当成员或验证任务仍在运行时不能切换，应先使用
`Esc` 取消。

### `/retry`

```text
/retry
```

把最近一条用户请求按照当前活动模式重新发送。它不会恢复被阻塞的运行，也不会
处理暂停的审批路由；这两种情况分别使用 `/continue` 和 `/approve`。如果当前
对话没有历史用户请求，则不会发送任何内容。

### `/attach`

```text
/attach <member>
@member /attach
```

暂时挂起 Asterline，打开该成员真正的原生交互式 CLI；存在会话时会恢复该后端会话。
使用该原生 CLI 支持的退出方式（通常是它自己的 `/exit`）后，Asterline 会自动恢复。
新的 Claude 接入会由 Asterline 通过 `claude --session-id` 指定 UUID，因此返回后会自动导入
记录并绑定到该成员。Codex 只导入能安全识别的已绑定会话；Claude fork 只有在既有记录能
唯一证明谱系时才会导入。存在歧义的原生会话绝不靠猜测导入。Grok 和 Agy 可以恢复会话，
但目前尚不会导入接入期间的消息。

### `/exit`

```text
/exit
```

立即退出 Asterline。正常退出路径会取消正在执行的后端工作并恢复终端。它仅是
Asterline 的命令；接入原生后端 CLI 时，那个 CLI 自己的 `/exit` 会返回 Asterline。

### `/approve`

```text
/approve
```

批准最早的一条待处理 Asterline 审批请求。没有待审批项时会给出提示。

### `/reject`

```text
/reject
```

拒绝最早的一条待处理 Asterline 审批请求。没有待审批项时会给出提示。

## 团队与诊断命令

### `/team`

```text
/team
```

打开实时 Team 编辑器。打开时会刷新系统中可用的 Codex、Claude、Grok、Agy
可执行文件。模型目录会在 `ast` 启动时异步加载一次，因此编辑器不会卡住且会显示
实际检测到的模型。若初次加载失败，在该成员的 `model` 字段按 `t` 可重试；同一
后端和工作目录的成员会共享结果。缺失的 CLI 仍会显示以便诊断，但不能被选中。

可编辑成员列表、后端、角色、模型、effort、原生 session ID、审批行为
和默认目标。修改先保存在草稿中，按 `s` 才会应用并保存。完整按键见
[Team 编辑器](#team-编辑器)。

### `/focus`

```text
/focus <member>
```

打开仅显示指定成员内容的日志抽屉，可检查其 stdout、stderr、工具活动和适配器
警告。

```text
/focus builder
```

### `/logs`

```text
/logs
```

打开持久化运行日志，其中包括后端 stderr、适配器警告和运行时警告。若只看一个
成员，使用 `/focus <member>`。

### `/diff`

```text
/diff
```

打开实时工作树视图，显示 `git diff` 以及未跟踪文件信息。它不会暂存、还原或
修改任何文件。

### `/find`

```text
/find [text]
```

在当前聊天记录中执行不区分大小写的搜索。底栏显示当前匹配和总数，例如
`find: "timeout" (2/5)`。输入框为空且没有抽屉打开时，按 `n` 跳到下一项，
按 `p` 跳到上一项。不带文本的 `/find` 或 `Esc` 会清除搜索。

```text
/find timeout
```

### `/help`

```text
/help
```

打开命令面板。不存在的斜杠命令，以及缺少必填参数或参数无效的命令，也会打开
这个面板。

## 协作模式

### `/mode`

```text
/mode <normal|review|plan|brainstorm|team>
```

选择后续纯文本提示的派发方式。`/mode` 只选择模式，任务应在下一条消息输入。
模式在当前对话中持续有效，直到再次用 `/mode` 修改。`/new` 会把新对话重置为
`normal`，`/resume` 则恢复所选历史对话原来的模式。

#### `/mode normal`

使用普通的直接消息派发。新对话需要 `@member`、`@all`、`/ask` 或 `/all`；
之后的纯文本可以沿用上一次目标。

#### `/mode review`

启动 builder/reviewer 循环。Builder 执行工作，Reviewer 输出结构化
`@@review` 结论；未批准时继续修改，直到批准或用尽 `max_iterations`。用尽后
运行会进入 blocked。

```text
/mode review
重构解析器，但不要改变公开行为
```

#### `/mode plan`

启动由 Leader 驱动的规划运行。Leader 创建清单、派发工作、维护步骤状态，并在
完成前执行 Reviewer 循环。

```text
/mode plan
迁移缓存格式并验证向后兼容
```

#### `/mode brainstorm`

启动结构化、多参与者的头脑风暴。一次完整运行先进行不带评判的 `seed`、`build`、
`stretch` 三类生成波次，再收集私密排序选票、计算排名并综合入选想法，同时保留
异议和证据。想法卡片与选票遵循内置 Asterline brainstorm Skill，便于稳定提取。

```text
/mode brainstorm
怎样让没有节点文本的图检索更稳健？
```

#### `/mode team`

启动 Coordinator 驱动的团队运行。Coordinator 创建并负责清单、向其他成员派发
任务、整合结果，并可按 `modes.team` 配置在完成时自动验证。验证失败后可以回到
Coordinator 修复，直到达到配置的迭代上限。

```text
/mode team
实现功能、完成审查、更新文档并运行测试
```

模式角色、参与者、迭代上限和验证设置定义在 `team.json` 中。Reviewer 使用类似
下面的一行式结论通信：

```text
@@review {"verdict":"approve","summary":"LGTM"}
```

## 运行命令

运行只属于当前对话。`/new` 的新对话没有运行记录，`/resume` 只恢复所选对话的
运行。

### `/runs`

```text
/runs
```

打开 Runs 抽屉，查看运行 ID、模式、阶段、状态、贡献、验证结果、清单、时间线和
建议的下一步操作。按键见 [Runs 抽屉](#runs-抽屉)。

### `/continue`

```text
/continue [run-<id>] [note]
```

恢复 blocked 或 failed 的模式/团队运行，并可向 Coordinator 或模式引擎附带
备注。省略运行 ID 时选择本对话最近的运行。活动中的运行不能再次 continue；
没有持久化模式状态的旧版运行也无法重建。

```text
/continue run-4 使用刚安装的依赖重试
/continue 改用更简单的备用方案
```

### `/note`

```text
/note [run-<id>] <text>
```

向运行时间线添加检查点，但不会唤醒 Agent 或触发派发。

```text
/note run-4 已与 reviewer 确认 API 契约
```

### `/block`

```text
/block [run-<id>] <reason>
```

把所选运行标记为 blocked 并记录原因。正在验证的运行必须先按 `Esc`，才能手动
标记 blocked。

```text
/block run-4 等待 schema 决策
```

### `/verify`

```text
/verify [run-<id>] [command]
```

在工作区后台执行验证命令，并把结果保存到运行中。不提供命令时，Asterline 会
探测 `cargo test`、`npm test`、`pytest` 等常见项目检查。不提供运行 ID 时使用
最近的运行。

```text
/verify
/verify run-4 cargo test --all-targets
```

所选运行仍在活动，或已有另一个验证任务运行时，不能开始新的验证。在已配置的
模式/团队运行中，失败可以触发自动修复迭代。

### `/step`

管理最近一次运行或明确指定运行的清单。步骤编号从 `1` 开始。

#### 添加步骤

```text
/step add [run-<id>] [@owner] <title>
```

添加清单项，并可立即指定负责人。

```text
/step add run-4 @builder 实现迁移
```

#### 修改步骤状态

```text
/step todo [run-<id>] <n> [note]
/step doing [run-<id>] <n> [note]
/step done [run-<id>] <n> [note]
/step block [run-<id>] <n> [note]
```

分别把步骤设为待办、进行中、完成或阻塞。`/step blocked` 是 `/step block`
的别名。

```text
/step doing run-4 2 API 批准后已开始
/step done run-4 2 测试通过
```

#### 重命名步骤

```text
/step rename [run-<id>] <n> <title>
```

替换步骤标题。`/step edit` 是别名。

#### 删除步骤

```text
/step remove [run-<id>] <n>
```

删除清单项。`/step delete` 和 `/step drop` 是别名。

#### 分配或清除负责人

```text
/step assign [run-<id>] <n> <member>
/step unassign [run-<id>] <n>
```

设置或清除负责人。成员名可以带或不带 `@`。`/step owner` 是 `assign` 的
别名；`/step clear-owner` 和 `/step clear_owner` 是 `unassign` 的别名。

## 全局键盘操作

| 按键                           | 功能                                                        |
| ------------------------------ | ----------------------------------------------------------- |
| `Enter`                        | 发送，或接受当前选项                                        |
| `Shift+Enter`                  | 插入换行                                                    |
| `Alt+Enter`                    | 终端无法区分 Shift+Enter 时的换行备用键                     |
| `↑` / `↓`                      | 在输入框、历史或当前选择列表中移动                          |
| `Tab`                          | 接受补全                                                    |
| `Ctrl+R`                       | 反向搜索提示历史                                            |
| `n` / `p`                      | 输入框为空时跳到下一/上一条 `/find` 结果                    |
| `PageUp` / `PageDown`          | 滚动聊天或当前抽屉                                          |
| 鼠标拖动                       | 选择并复制聊天、状态栏或抽屉文字                            |
| 鼠标滚轮                       | 滚动聊天或当前抽屉                                          |
| `Esc`                          | 关闭浮层、清除搜索或取消运行中的工作                        |
| `Ctrl+O` / `Ctrl+G` / `Ctrl+T` | 展开或折叠成功的工具输出                                    |
| `Ctrl+L`                       | 打开日志                                                    |
| `Ctrl+P`                       | 打开命令面板                                                |
| `Ctrl+N` / `Ctrl+B`            | 聚焦下一/上一个成员                                         |
| `Ctrl+A` / `Ctrl+E`            | 移到行首/行尾                                               |
| `Ctrl+U`                       | 清空当前行                                                  |
| `Ctrl+W`                       | 删除前一个词                                                |
| `Ctrl+C`                       | 取消、清空输入框；空闲时第一次按键进入退出待确认状态        |

提示历史的行为与 Shell 类似：浏览旧记录时，`↑`、`↓` 会保留当前草稿。在
`Ctrl+R` 搜索中，继续输入可缩小范围，再按 `Ctrl+R` 找更早的匹配，按 `Enter`
接受，按 `Esc` 取消。

## Team 编辑器

Team 编辑器包含“选择成员”和“选择字段”两层。

| 按键      | 选择成员                         | 选择字段                         |
| --------- | -------------------------------- | -------------------------------- |
| `↑` / `↓` | 选择成员                         | 选择字段或模型                   |
| `←` / `→` | —                                | 在模型列表中选择 effort          |
| `Enter`   | 打开成员字段                     | 编辑或打开 Agent/模型/会话列表   |
| `Esc`     | 关闭 Team                        | 返回成员选择                     |
| `a` / `d` | 添加/删除成员                    | —                                |
| `t`       | 把成员设为默认目标               | 重试失败的模型目录               |
| `*`       | 把全体成员设为默认目标           | —                                |
| `s`       | 应用并保存                       | 应用并保存                       |
| `e`       | —                                | 手动输入模型或 session ID        |

文本字段会打开专用输入框；`Enter` 提交，`Esc` 取消。模型列表用 `↑`、`↓`
选择模型，用 `←`、`→` 选择该模型的 effort，按 `Enter` 同时应用。发现到模型时，
会直接选中 CLI 报告的实际默认模型；只有空目录才显示通用的 `default` 项。
只用 `↑`、`↓` 浏览不会改动已有的 effort 覆盖值；若新模型没有公布该覆盖值，
按 `Enter` 会保留当前设置，必须用 `←`、`→` 明确选择它公布的值，绝不会猜测替换。

在 `session id` 字段按 `Enter` 会打开 Asterline 原生会话表。它读取本地 Codex、
Claude 或 Grok 历史，显示标题、项目、更新时间和原生 ID，并只保留属于该成员
有效工作目录的会话。输入可筛选，用 `↑`、`↓` 或 `PageUp`、`PageDown` 移动，
按 `Enter` 把 ID 放入草稿，再按 `s` 保存。按 `e` 可手工输入。Agy 暂时必须
手工输入，因为 Asterline 尚无经过验证的本地 Agy 历史格式。

## Runs 抽屉

| 按键                  | 功能                                                        |
| --------------------- | ----------------------------------------------------------- |
| `←` / `→`             | 选择更早/更新的运行                                         |
| `↑` / `↓`             | 选择清单步骤；没有步骤时选择运行                            |
| `x`                   | 切换精简/详细视图                                           |
| `Enter`               | 把所选步骤状态或运行下一步填入输入框                        |
| `Tab`                 | 把发给所选步骤负责人的可编辑派发填入输入框                  |
| `PageUp` / `PageDown` | 滚动详情                                                    |
| `Esc`                 | 关闭抽屉                                                    |

`Enter` 和 `Tab` 只会把文字放入输入框，不会立即执行。切换运行会清除当前步骤
选择。要取消活动工作，先按 `Esc` 关闭抽屉，再按一次 `Esc`。

## 其他抽屉

所有抽屉均可用 `PageUp`、`PageDown` 或鼠标滚轮滚动，用 `Esc` 关闭。聊天、
状态栏和抽屉中的文字均可用鼠标拖动选择，再使用终端常规复制快捷键复制。

## 接入原生会话

按 `Ctrl+N` 或 `Ctrl+B` 聚焦顶部成员栏，用 `←`、`→` 移动，再按 `Enter`。
也可以使用 `/attach <member>` 或 `@member /attach`。Asterline 会暂时挂起 TUI，打开
所选成员的原生交互式 CLI。请使用该 CLI 支持的退出方式（通常是 `/exit`）；只有后端接受
EOF 时，EOF 才会返回 Asterline。

新的 Claude 接入会由 Asterline 通过 `claude --session-id` 指定 UUID，返回后会自动导入并绑定。
Codex 只导入能安全识别的已绑定会话；Claude fork 只有在既有记录能唯一证明谱系时才会导入。
存在歧义的原生会话绝不靠猜测导入。Grok 和 Agy 可以恢复原生会话，但目前不会导入接入期间的消息。
