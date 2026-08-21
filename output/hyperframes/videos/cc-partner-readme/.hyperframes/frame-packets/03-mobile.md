# Frame packet: 03-mobile

## Project inputs

- Project: /Users/hans/web_project/cc-partner/output/hyperframes/videos/cc-partner-readme
- Design tokens: /Users/hans/web_project/cc-partner/output/hyperframes/videos/cc-partner-readme/frame.md
- RULES_DIR: /Users/hans/.agents/skills/hyperframes-animation/rules

## Assigned storyboard block

## Frame 3 — 离开电脑也能继续

- status: outline
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
