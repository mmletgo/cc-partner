# Mobile Terminal Extra Keys（Termux 风格）

**Date:** 2026-08-06  
**Status:** Approved for implementation (user: write spec then implement; skip writing-plans gate)

## Goal

在 `/mobile` 真实终端面板提供固定内置的额外按键条，补齐手机软键盘缺少的控制键、高频 Ctrl 组合，以及少量 shell 文本 snippet。参考 Termux extra-keys，但不兼容 `termux.properties` 自定义。

## Decisions

| Topic | Choice |
| --- | --- |
| Scope | 特殊键 + Ctrl 组合 + 少量 snippet |
| Layout | 单行主栏 + 分页（页 1 / 页 2） |
| Customization | 无；固定内置 |
| Snippet send | 只插入文本，不自动 Enter |
| Ctrl / Alt | Sticky 单次武装，3s 无后续自动解除 |
| Input path | 与 xterm 相同：`MobileTerminalInputStream.enqueue` 字符串帧 |
| Backend / protocol | 不改 |

## Default keys

**Page 1:** Esc · Tab · Ctrl · Alt · `/` · ↑ · ↓ · ← · → · (page 2)

**Page 2:** ^C · ^D · ^Z · ^L · Home · End · PgUp · PgDn · `cd ..` · `ls -la` · `clear` · (page 1)

## Send semantics

| Key | Payload |
| --- | --- |
| Esc | `\x1b` |
| Tab | `\t` |
| `/` | `/` |
| Arrows | CSI `A`/`B`/`C`/`D` |
| Home / End / PgUp / PgDn | CSI `H` / `F` / `5~` / `6~` |
| ^C / ^D / ^Z / ^L | `\x03` / `\x04` / `\x1a` / `\x0c` |
| cd.. / ls / clr | `cd ..` / `ls -la` / `clear`（无 `\r`） |
| Ctrl / Alt | 不发字节；sticky 武装 |

Sticky：武装后下一次 xterm `onData` 的单字符可打印输入转为 Ctrl（`code & 0x1f`）或 Alt（`\x1b`+char）后发送并解除；再点同修饰键取消；Ctrl/Alt 互斥。

## UI / placement

- 挂在 `MobileTerminalPanel` 终端 surface 底部，全屏与非全屏均显示
- 仅 running session 且输入流 ready、非 busy 时启用
- 触控目标高度 ≥ 44px；横向可滚动
- 软键盘策略：
  - 系统键盘只在用户轻点终端输入区时出现（`enterMobileTerminalTypingMode` + `terminal.focus()`）
  - 默认 / 按 extra key 时离开打字态：`readonly` + `inputmode=none` + blur（`leaveMobileTerminalTypingMode`）
  - shell 钉在 visualViewport（`position:fixed; top=offsetTop; height=vv.height`），键盘弹出时整体上移/压缩，不把终端挡在键盘下；padding 不叠 keyboard-inset
- tokens only；文案 i18n

## Non-goals (v1)

- 用户自定义布局 / popup 长按 / snippet 自动 Enter
- 桌面 Workbench 同款条
- 改 WS 协议或回退 `/sessions/write`

## Testing

- Pure unit：编码表、sticky 变换、超时策略、snippet 无 Enter
- 组件/面板：可选；不宣称真实 iOS/Android 软键盘 L3
