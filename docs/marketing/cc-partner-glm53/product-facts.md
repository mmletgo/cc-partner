# cc-partner × GLM 开发故事事实清单

> 核验日期：2026-08-21。此文件只服务小红书发布物料，不进入 cc-partner 的 GitHub 产品介绍。

## GLM 版本时间线（官方可核验）

- GLM-5.1：Z.ai Release Notes 记录发布日期为 2026-04-07。
- GLM-5.2：Z.ai Release Notes 记录发布日期为 2026-06-16；官方另有 [GLM-5.2 发布介绍](https://z.ai/blog/glm-5.2)。
- GLM-5.3：Z.ai Release Notes 记录发布日期为 2026-08-18；官方博客 [GLM-5.3: Frontier Coding with Emergent Cyber Capabilities](https://z.ai/blog/glm-5.3) 发布于 2026-08-14。
- 统一时间线来源：[Z.ai Release Notes](https://docs.z.ai/release-notes/new-released)。

## 用户提供的开发经历口径

- cc-partner 不是一口气完成的；从 GLM-5.1、GLM-5.2 到 GLM-5.3，三代模型都参与过项目开发过程。
- 对外内容只描述“三代模型参与过需求梳理、实现、调试与文档等开发环节”的累计经历，不虚构某个版本独占完成了某个具体模块。
- 发布重点是 cc-partner 解决的真实问题与最终产品能力，不做模型横评，也不复述活动奖励。

## cc-partner（仓库可核验）

- cc-partner 是 local-first 多设备项目工作台，Workbench 将本机/远端项目、Git worktree、终端、文件、Git、浏览器预览和自动化集中在一个界面。
- Mobile Workbench 可通过同一局域网的手机浏览器访问，支持终端、worktree、Git、文件、Prompt 和自动化面板。
- Orchestrator 提供任务看板、可见 Runner、验证 evidence、修复循环与可选 full-auto 交付。
- Agent Hub 统一管理 Claude、Codex、OpenCode、Grok Build、Gemini CLI、Cursor CLI、Pi 的指令和可移植资产。
- Agent Hub 支持本机用户级、局域网远端设备与项目级指令；提示词按公共 / 适配 / 独有三槽组织，可移植资产包括 Skill / Command / Agent / MCP。
- Prompt 库与 Agent Hub 是两套互补能力：Prompt 库支持标题、正文、标签、搜索、复制、版本历史与恢复，可通过局域网或 GitHub 私有仓库同步；Prompt 优化结果仅展示和复制，不自动入库同步。
- 局域网文件传输支持任意大小文件分块传输、SHA256 校验与能力匹配时的断点续传；不得宣称所有旧版本对端都支持续传。
- 区域截图支持可配置全局快捷键、框选、矩形/箭头标注、6 色色板与撤销，确认后把 PNG 写入剪贴板，可直接粘贴到 CLI Agent。
- 健康提醒支持久坐、喝水、全屏休息遮罩、免打扰、暂停/延迟，以及今日和近 7 天习惯统计；久坐前台活跃度检测在 macOS 需要辅助功能权限。
- 活动统计包括活跃/闲置分钟、应用使用时长排行、窗口标题排行与 24 小时活跃分布；默认明细保留 90 天后自动清理。
- cc-partner 不是模型提供方，也不是 GLM 的原生集成；可以表述为“开发 cc-partner 的过程中使用了 GLM-5.1、5.2、5.3”。
- 固定局域网边界：业务 API 无调用者身份校验；同一可达网络中的任意设备均可能读写执行。宣传中必须保留“只在可信局域网使用”的提醒。

## Git worktree 表述口径

- Git 官方文档将 `git worktree` 定义为管理“连接到同一仓库的多个工作树”；同一仓库可以同时检出多个分支，每个 linked worktree 有自己的 `HEAD`、index 等 per-worktree 文件。来源：[Git `worktree` 官方文档](https://git-scm.com/docs/git-worktree)。
- 宣传中可以表述为“给每个 Agent 独立 worktree + branch，减少并行执行时互相污染工作目录与 Git 现场”，但不能表述为“保证最终不会产生合并冲突”。多个 Agent 修改相同代码时仍可能在合并阶段冲突。
- cc-partner 仓库可核验的 worktree 管理能力：按前缀创建、切换、移除，实时展示 clean / dirty / conflict，关联终端 window / pane，显示 Git 提交树，提供 AI commit、一键合并与合并后的源 worktree 清理。

## 禁止夸大

- 不宣称“cc-partner 原生集成 GLM”或“由 GLM 独立完成”。
- 不把官方基准直接等同于个人项目中的必然体验。
- 不给 GLM-5.1、5.2、5.3 虚构一对一的模块分工或未经记录的性能结论。
- 不宣称完全自动、零人工、零风险或全平台全部经过真机验证。
- 不将 GLM 或活动写进 README；README 只介绍 cc-partner 本身。
