# 游戏插件对外开放设计

日期：2026-08-14  
状态：已批准，进入实现

## 1. 问题

游戏大厅第一期只有内置记单词。用户希望自己 vibe coding 小游戏，丢进本机目录就能在 cc-partner 里玩，并适配充电模式与全应用半透明遮罩。

## 2. 范围

- 设置常规可配置游戏插件目录，默认 `~/.cc-partner/plugins`（`data_dir()/plugins`）。
- 扫描该目录每个一级子文件夹为一个 game。
- 静态网页与需要构建的前端工程都支持；宿主不执行 npm，只加载 `game.json` 的 `entry`。
- 大厅列出内置记单词 + 插件；提供写给 AI 的规范 prompt，一键复制。
- 插件在全应用半透明 scrim Dialog 的沙箱 iframe 中运行。
- 插件可按清单 `rewardMinutes` 给充电账本充值；完全信任游戏自报完成；可选 `sourceId` 走账本幂等。无日上限。余额仍钳 `max_balance_minutes`。

不包含：把记单词改成插件、宿主代跑构建、独立 Tauri 透明窗、新路由、卫星窗/手机入口、插件商店、局域网同步插件目录。

## 3. 目录与清单

字段名 `game_plugin_dir` / `gamePluginDir`，避免和 Agent Hub plugin 撞车。

```
~/.cc-partner/plugins/
  snake/
    game.json
    index.html
  tower-defense/
    game.json
    package.json
    src/...
    dist/index.html
```

`game.json`：

```json
{
  "id": "snake",
  "name": "Snake",
  "description": "一句话",
  "entry": "index.html",
  "rewardMinutes": 5
}
```

- `id` 必须等于文件夹名，`^[a-z0-9]+(?:-[a-z0-9]+)*$`。
- `entry` 相对路径，禁止 `..` 与绝对路径；缺省 `index.html`。构建工程写 `dist/index.html`。
- `rewardMinutes` 缺省 0；负值当 0。分钟数只信清单，不信 postMessage 里的数字。
- 无 `game.json`：不是游戏，忽略。点目录忽略。
- 坏 JSON / id 不匹配 / entry 非法 / 文件不存在：大厅显示该行，按钮禁用，带原因。

## 4. 入口与浮层

- 无新路由，不进主导航。卫星窗无入口。
- AppShell footer `game` 打开共享 Dialog。
- 大厅：Escape / 点遮罩关闭。列出记单词（门槛不变）与插件。底部「写给 AI 的游戏规范」可复制。
- 记单词：现有 560px 两态不变。
- 插件游戏：同一 Dialog 切全应用 scrim 遮罩（`backdropVariant=scrim`，半透明无 blur）。点遮罩不退出；Escape / 返回回大厅。

## 5. 运行与安全

- iframe `sandbox="allow-scripts allow-pointer-lock"`，不加 `allow-same-origin`。
- 资源 `gameplugin://localhost/<id>/<entry>`。Rust 只读 `<pluginDir>/<id>/`；拒绝 `..` 与 symlink 逃逸。
- 不执行插件里的 Node / npm / 原生代码。

宿主 → 游戏：

```json
{
  "type": "cc-partner:host",
  "version": 1,
  "theme": "light",
  "batteryMode": "charging",
  "remainingMs": 0,
  "locale": "zh"
}
```

打开发一次；主题或充电快照变化再发。

游戏 → 宿主（仅接受该 iframe `event.source`）：

```json
{ "type": "cc-partner:game", "action": "ready" }
{ "type": "cc-partner:game", "action": "close" }
{ "type": "cc-partner:game", "action": "complete", "sourceId": "optional" }
```

## 6. 充电

- 新来源 `BatteryCreditSource::GamePlugin`。
- 日上限视为无。分钟数读该游戏清单，不进设置 `battery.rewards`。
- `source_id`：有客户端 `sourceId` 则为 `game-plugin:<id>:<sourceId>`（重复不入账）；否则每次新 UUID。
- ledger `kind = credit_game_plugin`。打满余额 delta=0，无 `+Xm` toast。
- `rewardMinutes=0` 不写流水。

## 7. 失败

- 目录不存在：扫描时创建；创建失败则大厅危险提示。
- 无合法插件：只显示记单词 + 空态 + 当前路径。
- iframe 加载失败：播放器内提示，可回大厅。
- 非该 iframe 的 message：丢弃。

## 8. 非目标

宿主代跑 npm、热更新、商店、记单词插件化、独立透明窗、新路由、卫星窗/手机、插件访问工作台/文件系统/P2P、局域网同步插件目录。
