---
canvas:
  background: "#F4F1E9"
colors:
  ink: "#1B1917"
  muted: "#6F6A63"
  accent: "#C96442"
  accentSoft: "#F1D7CB"
  surface: "#FFFDF8"
  line: "#D9D5CA"
  success: "#2A9D61"
typography:
  display: "CC Sans"
  body: "CC Sans"
  mono: "CC Mono"
spacing:
  frame: 92
  section: 52
  element: 24
components:
  panelRadius: 26
  panelBorder: 2
  uiShadow: "0 34px 90px rgba(40, 32, 24, 0.22)"
---

## Overview

cc-partner 的 README 视频沿用产品本身的暖纸色、墨色与陶土橙。整体像一张有编辑感的开发者产品海报：克制但不空，真实 UI 是主角，文字只负责给观众一个明确的阅读方向。

## The Frame

- 1920×1080，浅色暖纸画布，底色始终为 `#F4F1E9`。
- 主体内容保留在顶部约 83% 以内；底部留出播放器与 README 环境的呼吸空间。
- UI 截图使用 2px 暖灰边框、26px 圆角与真实投影，不使用玻璃拟态或紫色渐变。
- 每帧至少两个视觉焦点：一句短标题 + 一个真实产品界面或状态对象。

## Typography

- 中文与英文标题使用粗体无衬线；标题 72–108px，正文/标签不小于 26px。
- 元数据与项目路径使用等宽字体。
- 可见文案必须短：一行痛点、一行价值或一个功能标签，不重复旁白（本片无旁白）。

## Motion

- 所有画面由 seek-safe GSAP 时间线驱动。
- UI 截图使用父层入场 + 子图缓慢 1.00→1.035 放大；不同 transform 不落在同一节点上。
- 主运动语言：精确滑入、结构线拉开、局部 callout 锁定、桌面与手机界面形成接力。
- 不使用无限循环、不使用 CSS transition、不在非最终帧制作退出动画。

## Do

- 让真实界面足够大，终端、worktree、Git 历史、手机文件编辑均可辨认。
- 用陶土橙只强调核心状态和路径。
- 使用演示数据标记，保持产品事实诚实。

## Don't

- 不画假的应用界面代替已有截图。
- 不出现模型品牌、性能数字、活动信息、虚构口碑。
- 不用霓虹、赛博蓝紫或大面积渐变文字。
