---
format: 1920x1080
duration: 12s
message: "把散落的 AI 编码过程收进一个可见、可接手、可交付的本地工作台"
arc: Pain → Product proof → Mobile continuation → CTA
audience: 使用 CLI Coding Agent 推进真实项目的开发者
mode: autonomous
music: none
---

## Frame 1 — 不再拼窗口

- status: animated
- src: compositions/frames/01-pain.html
- duration: 2.8s
- transition_in: cut
- scene: 散落的项目上下文收拢成一条清晰问题陈述
- poster: 1.8s
- blueprint: compose
- focal: “任务散在五个窗口里？”短标题
- roles: 图标=品牌锚点；五个词块=项目/终端/worktree/Git/验证；收拢线=从散乱到工作台

Scene 1 (0.0–0.5s): 暖纸背景立即可见，cc-partner 图标从左上精确滑入；“AI 会写代码”短句在左侧锁定。  
Scene 2 (0.5–1.8s): 项目、终端、worktree、Git、验证五个等宽标签从不同方向分批滑入，位置略错开，表达上下文散落。  
Scene 3 (1.8–2.8s): 陶土橙结构线从这些标签指向右侧“任务还散在五个窗口里？”标题，最后一个“？”轻微放大，建立痛点并把视线推向下一帧。

## Frame 2 — 一个可见工作台

- status: animated
- src: compositions/frames/02-workbench.html
- duration: 5.2s
- transition_in: blur-crossfade
- scene: 真实 Workbench 界面被逐层标出 worktree、终端与 Git 交付上下文
- poster: 3.2s
- blueprint: compose
- focal: `assets/workbench.png`
- roles: 截图=产品证据；三枚 callout=Worktree/可见终端/Git evidence；标题=价值主张

Scene 1 (0.0–0.7s): “一个项目，一条可见流程。”从左上进入；Workbench 截图父层从右下放大到画面中心，0.5 秒内成为主角。  
Scene 2 (0.7–2.1s): 截图子图进行极慢推近；“WORKTREE”标记从左侧锁定顶部工作区标签，连接线只连接真实锚点。  
Scene 3 (2.1–3.6s): “VISIBLE TERMINAL”标记锁定中央终端；终端区域覆盖一层短暂的暖橙扫描高光后淡出。  
Scene 4 (3.6–5.2s): “GIT + EVIDENCE”标记锁定右侧提交 DAG；底部出现“可见 · 可接手 · 可交付”三段价值短语，依次落位并保持到切换。

## Frame 3 — 离开电脑也能继续

- status: animated
- src: compositions/frames/03-mobile.html
- duration: 4.0s
- transition_in: push-slide
- scene: 桌面工作台向左让位，真实 Mobile Workbench 在右侧接过同一项目
- poster: 2.4s
- blueprint: compose
- focal: `assets/mobile-workbench.png`
- roles: 桌面缩略图=任务来源；手机截图=接手动作；项目路径胶囊=连续上下文；项目地址=CTA

Scene 1 (0.0–0.7s): 桌面 Workbench 缩略图停在左侧，项目路径胶囊 `/workspace/cc-partner-demo` 横跨中部，表达上下文仍是同一个项目。  
Scene 2 (0.7–2.4s): Mobile Workbench 设备框从右侧滑入并轻微推近；路径胶囊的陶土橙端点从桌面移向手机，完成接力。  
Scene 3 (2.4–3.3s): “离开电脑，也能继续推进。”标题逐词落位；“终端 / 文件 / Git / 自动化”四项短标签在手机旁依次出现。  
Scene 4 (3.3–4.0s): cc-partner 图标与 `github.com/mmletgo/cc-partner` 在左下锁定，右侧手机保持可读；仅最终帧做轻微 settle，不加入音频或额外口号。

## Video direction

- 全片以真实界面为证据，不做抽象功能卡片堆叠。
- 过渡从散乱词块到桌面 Workbench，再由桌面路径接力到手机，形成连续的“收拢 → 看见 → 接手”运动叙事。
- 画面必须完全无声；不存在 `SCRIPT.md`、`audio_meta.json` 或任何 `<audio>` 元素。
- 所有截图均为演示数据，不在画面中出现真实用户路径、用户名、会话标题或个人项目。
