# cc-partner｜LINUX DO 开源推广事实清单

> 核验日期：2026-08-21。此文件只服务 LINUX DO 宣传物料，不作为许可证或社区规则的法律解释。

## 仓库状态

- GitHub 仓库：<https://github.com/mmletgo/cc-partner>
- 仓库当前为 Public。
- GitHub API 返回 `licenseInfo: null`，本地仓库也没有 `LICENSE` / `COPYING` 文件。公开可见不等于已经授予开源许可证；添加许可证前，不应勾选“完整开源，无未开源部分：是”。
- README 当前没有 `linux.do` / `LINUX DO` 链接。添加社区认可或致谢链接前，不应勾选“已链接认可 LINUX DO 社区：是”。
- 帖子尚未发布，因此“已经打上 #开源推广 标签”只能在实际发帖并选择标签后勾选“是”。
- AI 生成、润色的项目介绍已经生成对应截图 `assets/linuxdo-02-ai-copy.png`；实际发帖上传该图后，才满足对应披露动作。
- “永久有效”是发布者本人的持续承诺，必须由用户亲自确认，不能由 AI 代为承诺。

## 可核验产品能力

- local-first 多设备项目工作台，桌面端支持 macOS / Windows / Ubuntu；安装包从 GitHub Releases 获取。
- Workbench 以 Project → Worktree → Window → Pane 组织项目、Git worktree 和 tmux 终端。
- worktree 支持按前缀创建、切换、移除，显示 clean / dirty / conflict 与 Git 提交树，并提供 AI commit、一键合并和源 worktree 清理。
- 文件工作区支持代码、Markdown、HTML、CSV、SQLite、图片等；浏览器工作区支持受控 smoke，保留截图、console 与断言结果。
- Mobile Workbench 允许同一局域网中的手机进入项目，操作终端、文件、Git、worktree 和自动化。
- Orchestrator 提供任务看板、专用 worktree、可见 Runner、验证 evidence、Rework、Human Review 与显式开启的自动交付。
- Agent Hub 管理 Claude / Codex / OpenCode / Grok Build / Gemini CLI / Cursor CLI / Pi 的本机用户级、远端设备与项目级指令；提示词按公共 / 适配 / 独有三槽组织，可移植资产包括 Skill / Command / Agent / MCP。
- Prompt 库支持标题、正文、标签、搜索、复制、版本历史与恢复，并纳入局域网和 GitHub 私有仓库同步；Prompt 优化结果只展示和复制，不自动入库。
- 局域网文件传输支持任意大小分块传输、SHA256 完整性校验；双方支持 `transfer.resume.v1` 且源文件指纹匹配时可断点续传。
- 区域截图支持可配置全局快捷键、框选、矩形/箭头标注、6 色色板、撤销，并将合成 PNG 写入剪贴板。
- 健康提醒支持久坐、喝水、全屏休息遮罩、免打扰、暂停与延迟；久坐检测在 macOS 依赖辅助功能权限，未授权时降级。
- 活动统计包括活跃/闲置分钟、应用使用时长排行、窗口标题排行与 24 小时活跃分布；默认明细保留 90 天后清理。
- 完整 window / pane 与重启恢复依赖 tmux；没有 tmux 时回退普通 PTY，不承诺恢复语义。Windows 使用默认 WSL 发行版内的 tmux。
- LAN 业务 API 没有调用者身份校验；同一可达网络内的设备可能读写执行，只适合可信局域网。

## 宣传边界

- 不把“公开仓库”写成“已经采用某个开源许可证”。
- 不宣称零冲突、零人工或全自动；worktree 只减少并行执行时互相污染，合并仍可能冲突。
- 不把 smoke / CI 覆盖写成全部平台、全部功能均完成真机验证。
- 不隐去 tmux、WSL、macOS ad-hoc 签名和 LAN 无身份校验等实际使用边界。
