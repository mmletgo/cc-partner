import type { ITheme, ITerminalOptions } from '@xterm/xterm';

type TokenReader = (name: string, fallback: string) => string;

/**
 * Business Logic（为什么需要这个函数）:
 *   xterm 的主题需要跟随应用设计 token，而不是写死另一套终端色板。
 *
 * Code Logic（这个函数做什么）:
 *   从 documentElement 的 CSS 变量读取颜色；缺失时回退调用方给出的默认值。
 */
function readCssToken(name: string, fallback: string): string {
  const value = window.getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return value || fallback;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   终端颜色需要跟随当前主题 token，且主题切换后已存在的 xterm 也要同步更新。
 *
 * Code Logic（这个函数做什么）:
 *   读取 terminal 相关 CSS token 并组装 xterm ITheme；测试可传入 token reader stub。
 */
export function workbenchTerminalTheme(readToken: TokenReader = readCssToken): ITheme {
  return {
    background: readToken('--terminal-bg', 'Canvas'),
    foreground: readToken('--terminal-fg', 'CanvasText'),
    cursor: readToken('--accent', 'CanvasText'),
    selectionBackground: readToken('--accent-soft', 'Highlight'),
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   工作台渲染的是后端 PTY/tmux 的原始输出，前端必须像真实终端一样解释控制序列。
 *
 * Code Logic（这个函数做什么）:
 *   组装 Terminal 构造参数；不启用 convertEol，避免 tmux split 后的整屏重绘被换行改写破坏。
 */
export function workbenchTerminalOptions(readToken: TokenReader = readCssToken): ITerminalOptions {
  return {
    cursorBlink: true,
    fontFamily: readToken('--font-mono', 'monospace'),
    fontSize: 13,
    // lineHeight 保持 1：>1 时 FitAddon 行高与 canvas 像素可能不一致，
    // tmux status 画在最后一行时会在容器底部上方留白，看起来像“悬空”。
    lineHeight: 1,
    scrollback: 3000,
    theme: workbenchTerminalTheme(readToken),
  };
}
