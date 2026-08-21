# 小红书发布文案｜CLI Coding 痛点与 cc-partner

## 推荐标题

**多 Agent 并行后，我做了个工作台**

备选：

1. CLI Agent 很会写，但项目会散
2. Worktree 很好用，也真的难管
3. 把多 Agent 开发收进一个工作台

## 正文（可直接发布）

用 Claude Code 等 CLI Agent 做真实项目后，我发现：**最累的不是等模型写代码，而是把散落的工程过程和工具拼回来。** 所以我做了 cc-partner。

**1｜多 Agent 并行，worktree 好用但难管**

多个 Agent 共用目录，会互相影响文件、Git index 和分支。秘诀是给每个 Agent 独立的 `worktree + branch`，但人又要管理创建、切换、状态、终端、提交、合并和清理。

cc-partner 把它 UI 化：终端归属、Git 状态、提交树、AI commit、合并和清理都在同一界面。它减少并行互踩；同段代码仍可能冲突。

**2｜离开电脑，长任务变黑盒**

Mobile Workbench 让手机接手终端、文件、Git 和自动化。

**3｜重启后，会话上下文丢了**

tmux 接回终端；`⌘K / Ctrl+K` 搜索历史 session 并 resume。

**4｜生成代码，不等于可以交付**

文件、Git、浏览器预览同屏；smoke 留下截图、console、断言和 evidence。

**5｜Agent 越多，人越像调度器**

Orchestrator 用看板、专用 worktree 和可见 Runner 管并行任务、返工与人工复核。

**6｜Agent 规则和资产越用越散**

Agent Hub 管 7 类 Agent 的用户级/项目级指令及 Skill、Command、Agent、MCP；Prompt 库管标签、搜索、复制和版本恢复。

**7｜开发周边的小事不断打断**

文件传输支持分块/续传；区域截图框选标注后直进剪贴板；健康提醒管久坐、喝水、休息；活动统计看应用/窗口排行与 24 小时分布。

这就是 cc-partner 想补上的“CLI Coding 另一半”：**让过程可见、可接手、可交付，也少一点工具切换。**

GLM-5.1、5.2、5.3 都参与过开发。这里不做横评，主角仍是产品。

说明：非 GLM 原生集成；LAN API 无身份校验，仅在可信局域网使用。

项目地址：<https://github.com/mmletgo/cc-partner>

#ClaudeCode #GLM5.3 #ccpartner

## 短版正文

用 Claude Code、Codex、OpenCode 这类 CLI Coding Agent 做真实项目后，我发现最累的经常不是等它写代码，而是把工程过程和周边工具重新拼回来：

1. 多 Agent 并行需要独立 worktree，但 worktree 和分支本身又带来一整套管理负担
2. 长任务离开电脑就变成黑盒
3. 会话重启后，很难找回之前的上下文
4. “生成了代码”不等于测试过、可合并、可交付
5. 并行 Agent 越多，人越像全职调度器
6. 不同 Agent 的项目指令、Skill、Command、Agent、MCP 资产各自散落
7. 常用 Prompt、传文件、贴截图、久坐提醒和使用复盘不断打断主流程

所以我做了 cc-partner：Workbench 把 worktree、终端、文件、Git、浏览器验证和交付收进同一界面；Mobile Workbench 可以在手机接手；Orchestrator 管并行任务、Rework 与 Human Review；Agent Hub 统一管理 7 类 CLI Agent 的指令与可移植资产；Prompt 库负责标签、搜索、复制和历史恢复。文件传输、区域截图、健康提醒、活动统计也都放在这条开发流程周围。

从 GLM-5.1 到 5.3，三代模型都参与过这个项目的开发。模型在变，我想解决的问题没变：让 Agent 的真实工程过程可见、可接手、可交付。

项目：<https://github.com/mmletgo/cc-partner>

#ClaudeCode #Codex #OpenCode #GLM5.3 #AICoding #ccpartner #开源项目

## 配图顺序（9 张）

1. `assets/social/xhs-01.png`：封面——CLI Coding 的另一半问题
2. `assets/social/xhs-02.png`：痛点 1——worktree 让多 Agent 隔离并行，却把创建 / 分支 / 状态 / 合并 / 清理成本交给人；对应 Workbench 的 UI 化管理
3. `assets/social/xhs-03.png`：痛点 2——长任务离开电脑变黑盒；对应 Mobile Workbench + Attention
4. `assets/social/xhs-04.png`：痛点 3——会话上下文难找回；对应 tmux 恢复 + `⌘K` session 搜索
5. `assets/social/xhs-05.png`：痛点 4——生成代码不等于交付；对应文件 / Git / 浏览器 smoke / evidence
6. `assets/social/xhs-06.png`：痛点 5——并行 Agent 人工调度；对应 Orchestrator 看板与 Human Review
7. `assets/social/xhs-07.png`：痛点 6——多 Agent 指令与资产散落；对应 Agent Hub + Prompt 库
8. `assets/social/xhs-08.png`：痛点 7——开发周边小事频繁打断；对应文件传输 + 区域截图 + 健康提醒 + 活动统计
9. `assets/social/xhs-09.png`：总结——GLM-5.1→5.2→5.3 开发背景 + 开源地址 + 安全边界

## 发布前检查

- 图片按 01 → 09 顺序上传；每一张都能独立读懂“痛点 → 功能”。
- 正文中的体验应以你实际用过的场景为准；如果某项功能尚未亲自使用，删掉对应主观句子。
- 不展开 GLM 的宣传信息；GLM 只作为 cc-partner 的开发背景。
- GitHub 链接可放正文末尾或首条评论，避免打断前半段叙事。
