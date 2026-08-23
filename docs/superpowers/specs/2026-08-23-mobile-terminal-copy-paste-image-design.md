# Mobile Terminal：自管选区复制 + FAB 相册贴图

**Date:** 2026-08-23  
**Status:** Approved for implementation (user: 方案 1；非业务细节按推荐)

## Goal

`/mobile` 终端现在无法从输出里复制文字（系统 loupe / 选区手柄抢手势），也没有稳定的贴图入口（用户先全屏截图进相册，浏览器 paste 事件经常带不上 image file）。提供自管选区复制，以及 FAB 打开相册后立刻走现有 paste-image 通道。

## Decisions

| Topic | Choice |
| --- | --- |
| Copy interaction | 长按进入选区，拖出高亮，底栏点「复制」 |
| Native browser selection | 禁止；系统 loupe / 选区手柄不得出现 |
| Paste image entry | 右侧 FAB 最上方图片按钮，打开系统相册 |
| After pick | 选完立刻 paste-image，不预览、不裁剪 |
| Clipboard paste image | 现有 viewport `paste` 监听保留为隐藏通道，不当主入口 |
| Extra keys Copy | 不做 |
| Desktop | 不改桌面选区 / Ctrl+V / Cmd+V |
| Backend / protocol | 不改；复用 `POST /api/mobile/workbench/sessions/paste-image` |

## Copy / selection

长按判定与 extra keys `/` 相同：约 400ms（`MOBILE_TERMINAL_EXTRA_KEY_LONG_PRESS_MS`）。按下后位移超过现有 8px 阈值视为滚动，不进选区。进入选区后：

- 不弹软键盘，不进入打字态；这次长按后续的 click/focus 必须吞掉（复用 `suppressClickAfterScroll` 同类门闩）
- 一指拖动只改选区，不再 `scrollLines` / SGR 滚轮
- 触点贴近 viewport 上下边缘约 1 行时自动滚历史并延伸选区
- 再长按 = 从新格子重新起锚
- 高亮走 xterm `select` / `getSelection`（画布选区，不是 DOM 选字）
- 底栏显示「已选 N 行」、复制、取消；空选区复制禁用
- 退出：复制成功、取消、extra key Esc、换 session / 卸终端

viewport、canvas、helper textarea 必须：

- `user-select: none` 与 `-webkit-touch-callout: none`
- `contextmenu` `preventDefault`
- 移动端同样安装 `installWorkbenchTerminalSelectionOverrides`，避免 TUI mouse tracking `clearSelection`

复制写入手机剪贴板（不写 PTY）：

1. 优先 `navigator.clipboard.writeText`
2. `/mobile` 是局域网 HTTP，不是安全上下文；失败则隐藏 textarea + `document.execCommand('copy')`
3. 两路都失败 → 终端错误区 `role=alert`
4. 成功 → 短暂 `StatusMessage tone=success`，清选区并退出选区模式

## Paste image

FAB 组现有顺序是 Merge → Commit → Prompt 优化 → 收藏 Prompt。本功能在**最上方**加图片按钮（终端输入动作，排在 Git 动作之前）。

- 隐藏 `<input type="file" accept="image/*">`，**不**设 `capture`（避免直接开相机）
- 点 FAB = 触发该 input 的用户手势 `click()`，打开系统相册
- 选出文件后 `fileToPngDataUrl` → `httpWorkbenchTransport.sessions.pasteImage`
- 上限仍是 `MAX_TERMINAL_PASTE_IMAGE_BYTES`（8 MiB）
- 会话非 running、输入流未 ready、busy、或 write-blocked 时**贴图 FAB** 禁用（这是写 PTY / owning clipboard）
- 选区复制只读本地 xterm buffer，stopped / write-blocked 仍允许长按复制
- 贴图请求进行中 FAB `aria-busy`，忽略重复选择
- 失败写入已有 `workbench:errors.pasteImage` 错误区
- 现有 capture-phase `paste` 图片拦截保持不变

## Architecture

| Unit | Responsibility |
| --- | --- |
| `web/src/mobile/mobileTerminalSelection.ts` | 纯函数：长按判定、client 坐标 → 格子、锚点+拖动 → xterm select 参数、已选行数 |
| `web/src/mobile/mobileClipboard.ts` | 纯函数/薄包装：`writeText` + execCommand 兜底；不碰 React |
| `MobileTerminalPanel` | 手势接线、选区底栏、FAB + hidden file input、错误/成功反馈 |
| `lib/icons.tsx` | 新增 `ImageIcon`（与现有 `CopyIcon` 同规范） |
| CSS / i18n | tokens only；`workbench:mobile.terminalPanel.selection.*` 与 FAB aria |

选区底栏不是 Dialog，不抢 helper textarea 焦点。底栏贴 viewport 底部，右侧留出 FAB 宽度，避免盖住按钮。

手势状态机（纯函数，与 DOM 解耦）：

- `idle` → touchstart → `pressPending`
- `pressPending` + 8px 移动 → `scrolling`（现有 touch scroll）
- `pressPending` + 400ms 且几乎未移动 → `selecting`（锚点 = 按下格子）
- `selecting` + move → 更新终点；贴边产生 scroll 增量
- `selecting` + 再一次长按 → 新锚点
- 退出事件 → `idle` + `clearSelection`

## Error handling

| Case | UI |
| --- | --- |
| 复制两路都失败 | 终端错误区，停留选区模式，选区保留 |
| 空选区点复制 | 按钮 disabled，不报错 |
| 贴图过大 / 编码失败 / HTTP 失败 | 已有 pasteImage 错误区 |
| 相册取消 | 无操作、无错误 |
| 会话已停 / write-blocked | 贴图 FAB 禁用；选区复制仍可用 |

不新增 toast 系统。

## Non-goals

- 裁剪 / 预览 / 相机直拍
- extra key「Copy」或「复制可见屏」
- 桌面 Workbench 同款长按选区
- 改 paste-image 协议、capability 或 32 KiB 输入帧
- 宣称真实 iOS/Android 相册 / 剪贴板 L3
- 浮动选区手柄（v1 只靠拖动两端）

## Testing

- Pure unit：长按 vs 滚动分流、格子换算、select 参数、已选行数、clipboard 优先 writeText 再 execCommand
- `MobileTerminalPanel`：长按出现底栏且不 focus 打字；位移 8px+ 仍滚动且无底栏；复制调用写入 helper；stopped session 仍可进入选区并复制；FAB 选 PNG 走 `pasteImage`；input 无 `capture`；会话 stopped 时贴图 FAB disabled、选区底栏复制仍可用
- i18n zh/en 配对；`check:i18n` / `localeParity`
- 不宣称真机 loupe 消失或相册授权为自动化已验证

## PRD delta（实现时同步）

在「移动端终端 extra keys」条之后追加：

> 移动端终端复制与贴图：`/mobile` 终端输出区长按进入自管选区（拖出高亮后底栏复制到系统剪贴板），必须屏蔽浏览器 loupe / 选区手柄，不得把长按交给 xterm helper textarea。贴图主入口是终端右侧 FAB 最上方的图片按钮，打开系统相册后立刻经现有 `POST /api/mobile/workbench/sessions/paste-image` 发给 owning device Agent；不预览、不裁剪。浏览器 paste 事件中的图片仍可走隐藏通道。不宣称真实 iOS/Android 剪贴板或相册 L3。

同条「终端输入传输」里 mobile 图片粘贴句，补一句：移动端主入口是 FAB 相册，不是长按系统粘贴。
