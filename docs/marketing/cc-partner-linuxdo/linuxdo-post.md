# LINUX DO 开源推广文案｜cc-partner

## 推荐标题（补齐社区条件后使用）

**[开源] cc-partner：本地优先的多 Agent 项目工作台**

备选：

1. **[开源] 用 UI 管理 worktree、终端和多个 Coding Agent**
2. **[开源] cc-partner：把 CLI Agent 的工程现场收进一个工作台**

## 发布前核验（不要贴进正文）

当前仓库还不能直接使用“全部勾是”的社区声明：

| 社区要求 | 当前核验 | 发布前动作 |
|---|---|---|
| 已打 `#开源推广` 标签 | 否，尚未发帖 | 发布时选择该标签 |
| 完整开源，无未开源部分 | 否，Public 仓库但没有 LICENSE | 由项目所有者选择并添加许可证，再确认是否确实无闭源部分 |
| 已链接认可 LINUX DO 社区 | 否，README 未发现社区链接 | 在 README 增加真实的社区认可 / 致谢链接并发布到 GitHub |
| AI 内容已截图发出 | 素材已生成，尚未上传 | 上传 `assets/linuxdo-02-ai-copy.png` |
| 永久有效并接受监督 | 待项目所有者确认 | 发布者本人确认后勾选 |

README 可使用的社区认可文案示例：

> 本项目在 [LINUX DO](https://linux.do) 社区分享开发过程并接受社区监督，感谢社区成员提供的讨论与反馈。

如果希望链接到具体推广帖，可在发布后把上面的社区首页链接替换为帖子地址。许可证则需要项目所有者根据预期授权范围自行选择，不能由宣传文案代为决定。

## 合规条件满足后可粘贴的帖子模板

> 下面五项只有在对应动作真实完成后才能保留“是”；否则请诚实改成“否”，不要直接发布模板默认值。

#### 本帖使用社区开源推广，符合推广要求。我申明并遵守社区要求的以下内容：

- **我的帖子已经打上 #开源推广 标签：** 是
- **我的开源项目完整开源，无未开源部分：** 是
- **我的开源项目已链接认可 LINUX DO 社区：** 是
- **我帖子内的项目介绍，AI生成、润色内容部分已截图发出：** 是
- **以上选择我承诺是永久有效的，接受社区和佬友监督：** 是

*以下为项目介绍正文内容，AI生成、润色内容已使用截图方式发出*

<!-- 先上传 assets/linuxdo-01-overview.png -->

<!-- 再上传 assets/linuxdo-03-toolbox.png，展示完整能力范围 -->

<!-- 最后上传 assets/linuxdo-02-ai-copy.png；该图与下方“项目介绍文字源稿”逐字一致 -->

项目地址：<https://github.com/mmletgo/cc-partner>

Releases：<https://github.com/mmletgo/cc-partner/releases>

欢迎直接在帖子或 GitHub Issues 里反馈，尤其想听听大家对 worktree 工作流、跨平台安装和局域网使用边界的意见。

## 项目介绍文字源稿（与 AI 内容截图一致）

### 本地优先的多 Agent 项目工作台

cc-partner 不是新的 Coding Agent，也不替你选择模型。它处理的是 Claude Code、Codex、OpenCode 这类 CLI Agent 写代码之外的工程现场：项目、分支、终端、文件、验证与交付。

### 为什么做它

多个 Agent 并行时，比较稳妥的做法是给每个任务独立的 **worktree + branch**，避免它们共用一个目录、Git index 和当前分支。但 worktree 一多，人就要维护创建、切换、状态、终端对应、提交、合并和清理。

cc-partner 把这些操作放进 Workbench：项目下直接创建和切换 worktree，每个 worktree 管理自己的 terminal window / pane；旁边同步显示 Git 状态、文件和提交树，完成后可以 AI commit、一键合并并清理源 worktree。它减少并行执行时互相踩现场，但修改同一段代码时仍可能出现合并冲突。

### 目前主要能做什么

- **Workbench：**统一管理本机或局域网远端项目、worktree、多个终端、文件、Git 和浏览器预览。
- **终端恢复：**安装 tmux 后，重启可重新 attach 原 window / pane；断线后 replay，并可搜索历史 session。
- **Mobile Workbench：**用手机进入同一个项目，查看并接手终端、文件、Git、worktree 和自动化。
- **浏览器验证：**执行受控 smoke，保存 screenshot、console 错误、断言和 evidence。
- **Orchestrator：**专用 worktree + 可见 Runner；失败进入 Rework，需要决定时进入 Human Review。
- **Agent Hub：**统一管理 Claude、Codex、OpenCode、Grok Build、Gemini CLI、Cursor CLI、Pi 的用户级/项目级指令，以及 Skill、Command、Agent、MCP 资产。
- **Prompt 库：**常用 Prompt 可打标签、搜索、复制、查看版本历史并恢复，也可在局域网或 GitHub 私有仓库间同步。
- **局域网文件传输：**任意大小文件分块传输、SHA256 校验；双方能力匹配时支持断点续传。
- **区域截图：**全局快捷键框选，支持矩形/箭头标注与撤销，确认后直接写入剪贴板。
- **健康提醒：**久坐、喝水、休息倒计时、免打扰和近 7 天习惯统计。
- **活动统计：**查看活跃/闲置时间、应用和窗口排行，以及 24 小时活跃分布。

### 实际边界

- 项目是 local-first，没有公网中转；远端和手机能力走局域网。
- LAN 业务 API 没有调用者身份校验，只应在可信局域网使用。
- 完整 window / pane 和重启恢复依赖 tmux；无 tmux 时回退普通 PTY。Windows 使用默认 WSL 发行版内的 tmux。
- 公开 macOS 包目前为 ad-hoc 签名且未公证，第一次打开需按 README 操作。
- 久坐检测在 macOS 需要辅助功能权限；未授权会降级，不影响其它功能。

目前提供 macOS、Windows、Ubuntu / Linux 安装包。源码、构建方式、测试范围和未完成的真机验证都放在仓库文档里。

**GitHub：**github.com/mmletgo/cc-partner

项目仍在持续迭代，欢迎试用、提 Issue，也欢迎直接指出设计或实现里不合理的地方。
