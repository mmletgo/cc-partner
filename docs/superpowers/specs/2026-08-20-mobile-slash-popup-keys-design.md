# Mobile Extra Keys：`/` 长按弹出 slash 命令

**Date:** 2026-08-20  
**Status:** Approved for implementation (user: 方案 A，直接开发)

## Goal

在 `/mobile` 终端 extra keys 的 `/` 键上提供 iOS 键盘式长按弹出层，插入 Claude Code 常用 slash 命令，不自动回车。

## Decisions

| Topic | Choice |
| --- | --- |
| Reveal | 长按 `/` 约 400ms 后，在键上方弹出竖排命令 |
| Select | 按住滑动到目标再松手；松手仍在 `/` 上则插入 `/`；滑出弹出层则取消、不插入 |
| Short press | 400ms 内抬手仍只插入 `/` |
| Send | 只插入文本，不追加 CR/LF（与 `cd..` / `ls` snippet 一致） |
| Commands | 靠近 `/` 往上：`/rewind` → `/resume` → `/compact` |
| Scope | 仅 `/` 使用 popup；键定义预留 `popup` 字段，其它键暂不启用 |
| Overlay | `position: fixed` 挂到 `document.body`，避开键条 `overflow-x: auto` 裁切 |
| Keyboard | 无 pointer 的 click（Enter/Space）仍立即插入 `/`，不打开弹出层 |
| Modal | 不用 Dialog/focus trap，避免抢走终端 helper textarea 焦点 |

## Send payloads

| 动作 | Payload |
| --- | --- |
| 短按 `/` / 长按后松手仍在 `/` | `/` |
| `/rewind` | `/rewind` |
| `/resume` | `/resume` |
| `/compact` | `/compact` |

三者均无 `\r` / `\n`。

## Gesture state

纯函数状态机，与 DOM 解耦：

- `idle` → pointerDown → `pending`
- `pending` + 400ms → `open`（hover 初始为 trigger `/`）
- `open` + pointerMove → 按 hit-test 更新 hover（trigger / item id / null）
- pointerUp：`pending` 发送 trigger；`open` 且 hover 非空则发送对应项；hover 为空则取消
- pointerCancel：取消，不发送

## UI

- `/` 键带长按提示（角标）与 `aria-haspopup="menu"`
- 弹出层 `role="menu"`，项 `role="menuitem"`，当前 hover 高亮
- 触控目标高度 ≥ 44px；tokens only；文案 i18n
- 手指由 trigger 按钮 `setPointerCapture`，弹出层 `pointer-events: none`，命中靠 client 坐标 hit-test

## Non-goals

- 用户自定义 popup 内容
- 其它 extra key 的长按
- 选中后自动 Enter
- 桌面 Workbench 同款条
- 宣称真实 iOS/Android 软键盘 L3

## Testing

- Pure unit：`/` 的 popup 表、payload 无 CR、状态机（短按 / 长按滑动 / 滑出取消 / cancel）
- 组件：`/` 的 pointerDown 不立即发送；超时后出现三项；滑动到 `/rewind` 松手发送该项；Esc 等无 popup 键仍 pointerDown 即发
