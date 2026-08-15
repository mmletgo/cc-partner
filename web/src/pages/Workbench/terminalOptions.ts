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
 *   xterm 的 ThemeService 只能解析 `#rgb[a]` / `#rrggbb[aa]` / `rgb()` / `rgba()`；
 *   token 里的 `color-mix(...)` 会被静默回退到默认色。浅色主题下默认白色半透明选区几乎看不见，
 *   用户会以为“选中立刻消失 / 无法复制”。
 *
 * Code Logic（这个函数做什么）:
 *   已是 hex/rgb(a) 则原样返回；否则挂临时节点让浏览器 resolve 计算色（含 color-mix），
 *   再读 getComputedStyle().color；失败回退 fallback。
 */
export function resolveCssColorForXterm(value: string, fallback: string): string {
  const trimmed = value.trim();
  if (!trimmed) return fallback;
  if (/^#([\da-f]{3}|[\da-f]{4}|[\da-f]{6}|[\da-f]{8})$/i.test(trimmed)) return trimmed;
  if (/^rgba?\(/i.test(trimmed)) return trimmed;
  if (typeof document === 'undefined') return fallback;
  try {
    const probe = document.createElement('span');
    probe.style.color = trimmed;
    probe.style.display = 'none';
    document.documentElement.appendChild(probe);
    const resolved = window.getComputedStyle(probe).color;
    probe.remove();
    if (
      resolved &&
      resolved !== 'rgba(0, 0, 0, 0)' &&
      resolved !== 'transparent' &&
      /^rgba?\(/i.test(resolved)
    ) {
      return resolved;
    }
  } catch {
    // ignore probe failures
  }
  return fallback;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   选区需要固定 alpha，才能在浅色/深色 terminal 背景上都清晰可见。
 *
 * Code Logic（这个函数做什么）:
 *   解析 rgb/rgba/#hex，输出 `rgba(r,g,b,a)`；解析失败回退 fallbackRgba。
 */
export function withAlphaCssColor(color: string, alpha: number, fallbackRgba: string): string {
  const a = Math.min(1, Math.max(0, alpha));
  const trimmed = color.trim();
  const rgbaMatch = trimmed.match(
    /^rgba?\(\s*(\d{1,3})\s*,\s*(\d{1,3})\s*,\s*(\d{1,3})(?:\s*,\s*([0-9.]+))?\s*\)$/i,
  );
  if (rgbaMatch) {
    return `rgba(${rgbaMatch[1]}, ${rgbaMatch[2]}, ${rgbaMatch[3]}, ${a})`;
  }
  const hex = trimmed.match(/^#([\da-f]{3,8})$/i);
  if (hex) {
    let h = hex[1] ?? '';
    if (h.length === 3 || h.length === 4) {
      h = h
        .split('')
        .map((c) => c + c)
        .join('');
    }
    if (h.length >= 6) {
      const r = parseInt(h.slice(0, 2), 16);
      const g = parseInt(h.slice(2, 4), 16);
      const b = parseInt(h.slice(4, 6), 16);
      return `rgba(${r}, ${g}, ${b}, ${a})`;
    }
  }
  return fallbackRgba;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   终端颜色需要跟随当前主题 token，且主题切换后已存在的 xterm 也要同步更新。
 *
 * Code Logic（这个函数做什么）:
 *   读取 terminal 相关 CSS token 并组装 xterm ITheme；测试可传入 token reader stub。
 *   选区色经 resolve + withAlpha，保证 xterm 可解析且在浅/深背景下可见。
 */
export function workbenchTerminalTheme(readToken: TokenReader = readCssToken): ITheme {
  const background = resolveCssColorForXterm(readToken('--terminal-bg', '#141210'), '#141210');
  const foreground = resolveCssColorForXterm(readToken('--terminal-fg', '#f5f1e8'), '#f5f1e8');
  const accent = resolveCssColorForXterm(readToken('--accent', '#c96442'), '#c96442');
  const selectionBackground = withAlphaCssColor(accent, 0.4, 'rgba(201, 100, 66, 0.4)');
  const selectionInactiveBackground = withAlphaCssColor(accent, 0.25, 'rgba(201, 100, 66, 0.25)');
  return {
    background,
    foreground,
    cursor: accent,
    selectionBackground,
    selectionInactiveBackground,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   工作台渲染的是后端 PTY/tmux 的原始输出，前端必须像真实终端一样解释控制序列。
 *
 * Code Logic（这个函数做什么）:
 *   组装 Terminal 构造参数；不启用 convertEol，避免 tmux split 后的整屏重绘被换行改写破坏。
 *   macOptionClickForcesSelection：Claude/TUI 打开 mouse tracking 时，macOS 仍可用 Option+拖选保留选区。
 */
export function workbenchTerminalOptions(readToken: TokenReader = readCssToken): ITerminalOptions {
  return {
    cursorBlink: true,
    fontFamily: readToken('--font-mono', 'monospace'),
    fontSize: 13,
    // lineHeight 保持 1：>1 时 FitAddon 行高与 canvas 像素可能不一致，
    // tmux status 画在最后一行时会在容器底部上方留白，看起来像“悬空”。
    lineHeight: 1,
    // hydrated tmux history 会按手机窄列宽重排；3000 行会在长会话中把真正的早期消息
    // 裁掉，只留下靠后的 TUI 重绘帧。该容量覆盖 200k 字符 buffer 在最窄 20 列下的重排。
    scrollback: 20_000,
    // TUI mouse mode 会 disable selection；mac 上 Option+拖选强制选字（不发 mouse 到 PTY）。
    macOptionClickForcesSelection: true,
    rightClickSelectsWord: true,
    theme: workbenchTerminalTheme(readToken),
  };
}
