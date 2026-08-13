/**
 * Workbench 桌面终端滚轮判定与 SGR 编码。
 *
 * Business Logic（为什么需要这个模块）:
 *   Claude Code 会在 main/alternate screen 上用自己的虚拟 transcript。xterm 只有在
 *   DECSET mouse tracking 真正落地时才会发 SGR 64/65；否则把滚轮译成 ↑/↓，
 *   被 Claude 输入框当成历史 prompt。Workbench 强制 tmux `mouse off` 后，
 *   默认 `terminal-features` 又不宣告 `xterm*:mouse`，DECSET 会被 tmux 吞掉，
 *   于是 xterm 永远走方向键回退。桌面必须自己按 mouse/buffer 模式分流。
 *
 * Code Logic（这个模块做什么）:
 *   - resolveWorkbenchTerminalWheelAction：scrollback / sgrFallback；
 *   - encodeTerminalSgrWheelReports：与 xterm CoreMouseService 相同的 SGR 64/65。
 */

export type WorkbenchTerminalBufferType = 'normal' | 'alternate';

export type WorkbenchTerminalMouseTrackingMode = 'none' | 'x10' | 'vt200' | 'drag' | 'any';

export type WorkbenchTerminalWheelAction = 'scrollback' | 'sgrFallback';

export interface WorkbenchTerminalWheelInput {
  bufferType: WorkbenchTerminalBufferType;
  baseY: number;
  mouseTrackingMode: WorkbenchTerminalMouseTrackingMode;
}

/** 单次桌面滚轮最多向 PTY 注入的 SGR 事件数。 */
export const WORKBENCH_TERMINAL_SGR_WHEEL_EVENTS_CAP = 8;

/**
 * Business Logic（为什么需要这个函数）:
 *   桌面滚轮只有两条路径：应用启用 mouse tracking 时优先送给应用；
 *   只有未启用 mouse tracking 的普通 buffer 才交给 xterm 本地 scrollback。
 *   alternate screen 则始终自己发固定打在 transcript 左上角的 SGR 64/65。
 *   不能交给 xterm protocol：指针常在底部输入框，SGR 会按原坐标命中输入区。
 *   也不能按固定行数把指针上抬：resume 后输入区高度会随内容变化，估算仍可能命中输入区。
 *   也不能发 PageUp：Chat 上下文没有 pageup 绑定，输入框聚焦时整页不动。
 *
 * Code Logic（这个函数做什么）:
 *   mouseTrackingMode !== none → sgrFallback；否则 normal → scrollback、alternate → sgrFallback。
 */
export function resolveWorkbenchTerminalWheelAction(
  input: WorkbenchTerminalWheelInput,
): WorkbenchTerminalWheelAction {
  if (input.mouseTrackingMode !== 'none') return 'sgrFallback';
  if (input.bufferType === 'normal') return 'scrollback';
  return 'sgrFallback';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   触控板一次滑动会拆成很多小 delta，必须累计成整行再发 SGR，否则慢速滚动无响应。
 *
 * Code Logic（这个函数做什么）:
 *   remainder + deltaY 按 cellHeight 取整行；正数=wheel down(65)，负数=wheel up(64)。
 */
export function consumeWorkbenchTerminalWheelLines(
  deltaY: number,
  remainder: number,
  cellHeight: number,
): { lines: number; remainder: number } {
  if (!Number.isFinite(deltaY) || deltaY === 0) {
    return { lines: 0, remainder: Number.isFinite(remainder) ? remainder : 0 };
  }
  const safeCell = Number.isFinite(cellHeight) && cellHeight > 0 ? cellHeight : 16;
  const safeRemainder = Number.isFinite(remainder) ? remainder : 0;
  const total = safeRemainder + deltaY;
  const rawLines = Math.trunc(total / safeCell);
  const lines = rawLines === 0 ? 0 : rawLines;
  return { lines, remainder: total - lines * safeCell };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Claude Code 等 TUI 靠「鼠标滚轮」滚 transcript，不是方向键（方向键是列表/输入历史）。
 *
 * Code Logic（这个函数做什么）:
 *   SGR：`CSI < Pb ; Px ; Py M`，wheel up Pb=64、wheel down Pb=65。
 *   lines>0 → 65；lines<0 → 64。col/row 为 1-based；|lines| 截断到 cap。
 */
export function encodeTerminalSgrWheelReports(
  lines: number,
  col = 1,
  row = 1,
  maxEvents: number = WORKBENCH_TERMINAL_SGR_WHEEL_EVENTS_CAP,
): string {
  if (!Number.isFinite(lines) || lines === 0) return '';
  const cap =
    Number.isFinite(maxEvents) && maxEvents > 0
      ? Math.floor(maxEvents)
      : WORKBENCH_TERMINAL_SGR_WHEEL_EVENTS_CAP;
  const count = Math.min(Math.abs(Math.trunc(lines)), cap);
  if (count === 0) return '';
  const safeCol = Number.isFinite(col) && col >= 1 ? Math.floor(col) : 1;
  const safeRow = Number.isFinite(row) && row >= 1 ? Math.floor(row) : 1;
  const button = lines > 0 ? 65 : 64;
  return `\x1b[<${button};${safeCol};${safeRow}M`.repeat(count);
}
