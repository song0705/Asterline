# Asterline 文档

[English documentation](README.en.md)

根目录的 [`README.md`](../README.md) 适合第一次了解项目。本目录按使用目标展开：怎么装、怎么操作、数据在哪、以及怎么发版。想直接看英文产品页，用 [`README.en.md`](../README.en.md)。

## 按目标阅读

普通使用从[安装指南](installation.zh-CN.md)和[命令参考](commands.zh-CN.md)开始即可，不需要读维护者文档。

### 用户文档

| 文档                                     | 你会得到什么                                 |
| ---------------------------------------- | -------------------------------------------- |
| [安装指南](installation.zh-CN.md)        | 各平台安装包、Homebrew、更新、卸载和故障排查 |
| [命令与键盘](commands.zh-CN.md)          | 启动参数、`@成员`、斜杠命令、抽屉和按键      |
| [配置与本地数据](configuration.zh-CN.md) | `team.json`、权限、本地数据库和排错          |
| [审批与工具控制](approvals.zh-CN.md)     | 默认自动通过、手动审批卡片、各后端权限边界   |
| [v1.0.4 发布说明](releases/v1.0.4.md)    | 这一版对使用者可见的变化                     |

### 开发者与维护者文档

| 文档                                         | 你会得到什么                        |
| -------------------------------------------- | ----------------------------------- |
| [真实后端冒烟测试](real-smoke.zh-CN.md)      | 付费、需人工批准的真实 CLI 检查入口 |
| [维护者发布流程](releasing.zh-CN.md)         | 预检、annotated tag、不可变 Release |
| [第三方包定义](../packaging/README.zh-CN.md) | Homebrew、deb、rpm 等包装说明       |

## README 怎么分工

- [`README.md`](../README.md)：中文产品入口，也是 GitHub 和 crates.io 的默认说明。
- [`README.en.md`](../README.en.md)：英文产品入口，和中文 README 覆盖同一产品范围。

`docs/*.md` 仍是英文详述，对应的中文在 `docs/*.zh-CN.md`。发布说明以中文 `docs/releases/vX.Y.Z.md` 为准，英文对照是 `vX.Y.Z.en.md`。

## 界面截图

根 README 先用 Team 编辑器讲名册，再用对话、模式选择和四种工作模式的主图。其余截图也在 `docs/assets/`，文件名按画面本身起：

| 文件                                                | 画面                        |
| --------------------------------------------------- | --------------------------- |
| [chat.webp](assets/chat.webp)                       | `normal` 对话里成员互相交接 |
| [mode.webp](assets/mode.webp)                       | `/mode` 选择器              |
| [mode-fields.webp](assets/mode-fields.webp)         | 模式字段编辑                |
| [team.webp](assets/team.webp)                       | Team 编辑器                 |
| [team-run.webp](assets/team-run.webp)               | Team 模式拆任务并加人       |
| [team-done.webp](assets/team-done.webp)             | Team 模式验收通过           |
| [runs.webp](assets/runs.webp)                       | `/runs` 里查看清单和下一步  |
| [review.webp](assets/review.webp)                   | Review 实现并提交审阅       |
| [review-done.webp](assets/review-done.webp)         | Reviewer 对照仓库给出结论   |
| [plan.webp](assets/plan.webp)                       | Plan 写出步骤并开始执行     |
| [plan-done.webp](assets/plan-done.webp)             | Plan 步骤全部完成           |
| [brainstorm.webp](assets/brainstorm.webp)           | Brainstorm 生成想法卡片     |
| [brainstorm-vote.webp](assets/brainstorm-vote.webp) | 私下投票与综合              |
| [brainstorm-done.webp](assets/brainstorm-done.webp) | Brainstorm 排名结果         |

## 状态约定

文档会区分当前行为和版本之间可能变化的细节。1.0.x 已经发布，但配置、持久化数据和界面仍可能随版本调整。不要把 roadmap 或讨论中的想法写成已经装上的功能。
