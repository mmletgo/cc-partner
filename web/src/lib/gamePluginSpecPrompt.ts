/**
 * 写给 AI 的游戏插件规范 prompt。
 *
 * Business Logic（为什么需要这个模块）:
 *     用户在大厅一键复制，交给 vibe coding 工具按同一合同生成游戏。
 *
 * Code Logic（这个模块做什么）:
 *     中英两份常量；按语言返回。正文必须覆盖清单、构建、遮罩、postMessage。
 */

export const GAME_PLUGIN_SPEC_PROMPT_ZH = `你正在为 cc-partner 写一个游戏插件。

# 交付
在用户的插件目录（默认 ~/.cc-partner/plugins）下创建一个子文件夹。每个子文件夹是一个独立游戏。
必须包含 game.json，以及浏览器能打开的入口 HTML。宿主不会执行 npm。

## 静态网页
plugins/my-game/
  game.json
  index.html
  style.css
  main.js

## 需要构建的前端工程
plugins/my-game/
  game.json
  package.json
  src/
  dist/index.html
  dist/assets/
你必须先构建出产物。没有 game.json 声明的 entry 文件时，大厅会显示「请先构建」。

# game.json
{
  "id": "my-game",
  "name": "我的游戏",
  "description": "一句话",
  "entry": "index.html",
  "rewardMinutes": 5
}
- id 必须等于文件夹名，kebab-case（小写字母、数字、单个连字符）
- entry 是相对路径，禁止 .. 与绝对路径；静态默认 index.html，工程用 dist/index.html
- rewardMinutes 是完成一局充入的分钟，可省略或 0 表示不充值

# 运行环境
- 游戏显示在全应用半透明遮罩里，必须能透过看到 cc-partner。
- 禁止铺满不透明背景。
- 监听宿主消息里的 theme（light/dark）和 batteryMode（charging/unlimited）。
- 不要访问 parent 的 DOM，不要调用 Tauri invoke。只通过 postMessage。

# 宿主协议
宿主 → 游戏（window.addEventListener('message')）:
{ "type": "cc-partner:host", "version": 1, "theme": "light"|"dark", "batteryMode": "charging"|"unlimited", "remainingMs": 0, "locale": "zh"|"en" }

游戏 → 宿主（window.parent.postMessage(..., '*')）:
{ "type": "cc-partner:game", "action": "ready" }
{ "type": "cc-partner:game", "action": "close" }
{ "type": "cc-partner:game", "action": "complete", "sourceId": "optional" }

complete 按 game.json 的 rewardMinutes 入账，消息里不要带分钟数。sourceId 相同则不重复入账；不传则每次都入账。余额有全局上限。
`;

export const GAME_PLUGIN_SPEC_PROMPT_EN = `You are writing a game plugin for cc-partner.

# Deliverable
Create a subfolder in the user's plugin directory (default ~/.cc-partner/plugins). Each subfolder is one game.
You must include game.json and an HTML entry the browser can open. The host will not run npm.

## Static page
plugins/my-game/
  game.json
  index.html
  style.css
  main.js

## Bundled frontend project
plugins/my-game/
  game.json
  package.json
  src/
  dist/index.html
  dist/assets/
You must build artifacts first. If the entry file declared in game.json is missing, the hub shows "build first".

# game.json
{
  "id": "my-game",
  "name": "My game",
  "description": "One line",
  "entry": "index.html",
  "rewardMinutes": 5
}
- id must equal the folder name, kebab-case
- entry is a relative path; no .. and no absolute path; default index.html, or dist/index.html for a build
- rewardMinutes is minutes credited on complete; omit or 0 for no credit

# Runtime
- The game is shown as a full-app semi-transparent overlay; cc-partner must show through.
- Do not paint an opaque full-viewport background.
- Listen for theme (light/dark) and batteryMode (charging/unlimited) from the host.
- Do not touch parent DOM. Do not call Tauri invoke. You must not use invoke. postMessage only.

# Host protocol
Host → game:
{ "type": "cc-partner:host", "version": 1, "theme": "light"|"dark", "batteryMode": "charging"|"unlimited", "remainingMs": 0, "locale": "zh"|"en" }

Game → host:
{ "type": "cc-partner:game", "action": "ready" }
{ "type": "cc-partner:game", "action": "close" }
{ "type": "cc-partner:game", "action": "complete", "sourceId": "optional" }

complete credits rewardMinutes from game.json, not a number in the message. The same sourceId is idempotent; omit it to credit every time. Balance has a global cap.
`;

/**
 * 按界面语言返回规范 prompt。
 */
export function gamePluginSpecPrompt(lang: 'zh' | 'en'): string {
  return lang === 'zh' ? GAME_PLUGIN_SPEC_PROMPT_ZH : GAME_PLUGIN_SPEC_PROMPT_EN;
}
