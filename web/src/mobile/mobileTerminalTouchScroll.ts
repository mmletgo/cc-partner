import { encodeTerminalSgrWheelReports } from '@/pages/Workbench/terminalWheel';

export interface MobileTerminalTouchScrollState {
  lastClientY: number;
  remainderPx: number;
}

export interface MobileTerminalTouchScrollUpdate {
  state: MobileTerminalTouchScrollState;
  lines: number;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端用户在终端输出区一指滑动时，应回看终端历史，而不是让浏览器页面滚动并触发地址栏显示/隐藏。
 *
 * Code Logic（这个函数做什么）:
 *   用触点起始 Y 坐标创建一次触摸滚动状态，后续 move 事件会基于该坐标计算行滚动量。
 */
export function beginMobileTerminalTouchScroll(clientY: number): MobileTerminalTouchScrollState {
  return { lastClientY: clientY, remainderPx: 0 };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   xterm 的实际行高随视口 fit 后的 rows 变化，触摸滚动需要按当前可见行高换算成 scrollLines。
 *
 * Code Logic（这个函数做什么）:
 *   优先用 viewportHeight / rows 计算行高；尺寸不可用时回退到调用方提供的字体行高，最终至少返回 1px。
 */
export function mobileTerminalTouchLineHeight(
  viewportHeight: number,
  rows: number,
  fallbackPx: number,
): number {
  if (Number.isFinite(viewportHeight) && viewportHeight > 0 && Number.isFinite(rows) && rows > 0) {
    return Math.max(1, viewportHeight / rows);
  }
  return Number.isFinite(fallbackPx) && fallbackPx > 0 ? fallbackPx : 1;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   手指滑动通常是连续小位移，不能把不足一行的像素丢掉，否则慢速滑动会无响应。
 *
 * Code Logic（这个函数做什么）:
 *   将本次 Y 位移叠加上上次剩余像素后换算为整数行；正数表示向终端底部滚动，负数表示向历史顶部滚动。
 */
export function updateMobileTerminalTouchScroll(
  state: MobileTerminalTouchScrollState,
  clientY: number,
  lineHeightPx: number,
): MobileTerminalTouchScrollUpdate {
  const safeLineHeight = Number.isFinite(lineHeightPx) && lineHeightPx > 0 ? lineHeightPx : 1;
  const deltaPx = state.lastClientY - clientY;
  const totalPx = state.remainderPx + deltaPx;
  const lines = Math.trunc(totalPx / safeLineHeight);
  return {
    lines,
    state: {
      lastClientY: clientY,
      remainderPx: totalPx - lines * safeLineHeight,
    },
  };
}

export type MobileTerminalBufferType = 'normal' | 'alternate';

export type MobileTerminalScrollMode = 'scrollback' | 'tuiWheel';

/** 单次 touchmove 最多向 PTY 注入的滚轮事件数，防止高速滑动打爆输入流。 */
export const MOBILE_TERMINAL_TUI_WHEEL_EVENTS_CAP = 8;

/**
 * Business Logic（为什么需要这个函数）:
 *   PC 端 xterm 在 TUI（alternate buffer / 无 scrollback）时，滚轮不会 scrollLines：
 *   开启 mouse tracking 时发 SGR 滚轮报告（button 64/65），TUI 自己滚内部历史。
 *   移动端若一律 scrollLines 或误发方向键，Claude Code 会变成「上下选择」而非滚动。
 *
 * Code Logic（这个函数做什么）:
 *   alternate → tuiWheel；normal 且 baseY>0 → scrollback；否则 tuiWheel。
 */
export function resolveMobileTerminalScrollMode(
  bufferType: MobileTerminalBufferType,
  baseY: number,
): MobileTerminalScrollMode {
  if (bufferType === 'alternate') return 'tuiWheel';
  if (Number.isFinite(baseY) && baseY > 0) return 'scrollback';
  return 'tuiWheel';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Claude Code 等 TUI 在 PC 上靠「鼠标滚轮」滚 transcript，不是方向键（方向键是列表导航）。
 *   移动端手指滑动必须编码成与 xterm mouse tracking 相同的 SGR wheel 报告。
 *
 * Code Logic（这个函数做什么）:
 *   SGR：`CSI < Pb ; Px ; Py M`，wheel up Pb=64、wheel down Pb=65（xterm CoreMouseService）。
 *   lines>0（上滑）→ 65（看更新内容）；lines<0（下滑）→ 64（看更旧历史）。
 *   col/row 为 1-based 字符格；|lines| 截断到 cap。
 */
export function encodeMobileTerminalWheelReports(
  lines: number,
  col = 1,
  row = 1,
  maxEvents: number = MOBILE_TERMINAL_TUI_WHEEL_EVENTS_CAP,
): string {
  return encodeTerminalSgrWheelReports(lines, col, row, maxEvents);
}
