/**
 * Workbench 桌面终端滚轮判定与 SGR 编码。
 *
 * Business Logic（为什么需要这个模块）:
 *   Claude Code 在 alternate screen 里用滚轮回看 transcript。xterm 只有在
 *   DECSET mouse tracking 真正落地时才会发 SGR 64/65；否则把滚轮译成 ↑/↓，
 *   被 Claude 输入框当成历史 prompt。Workbench 强制 tmux `mouse off` 后，
 *   默认 `terminal-features` 又不宣告 `xterm*:mouse`，DECSET 会被 tmux 吞掉，
 *   于是 xterm 永远走方向键回退。桌面必须自己按 buffer/mouse 模式分流。
 *
 * Code Logic（这个模块做什么）:
 *   - resolveWorkbenchTerminalWheelAction：scrollback / protocol / pageFallback；
 *   - encodeTerminalSgrWheelReports：与 xterm CoreMouseService 相同的 SGR 64/65；
 *   - encodeTerminalPageScrollKeys：tmux mouse off 时 Claude 认的 PageUp/PageDown。
 */

export type WorkbenchTerminalBufferType = 'normal' | 'alternate';

export type WorkbenchTerminalMouseTrackingMode = 'none' | 'x10' | 'vt200' | 'drag' | 'any';

export type WorkbenchTerminalWheelAction = 'scrollback' | 'protocol' | 'pageFallback';

export interface WorkbenchTerminalWheelInput {
  bufferType: WorkbenchTerminalBufferType;
  baseY: number;
  mouseTrackingMode: WorkbenchTerminalMouseTrackingMode;
}

/** 单次桌面滚轮最多向 PTY 注入的 SGR 事件数。 */
export const WORKBENCH_TERMINAL_SGR_WHEEL_EVENTS_CAP = 8;

/**
 * Business Logic（为什么需要这个函数）:
 *   桌面滚轮有三条互斥路径：本地 scrollback、xterm 已协商的 mouse protocol、
 *   以及「TUI 已在 alt screen 但 xterm 没看到 DECSET」时的 PageUp/PageDown 回退。
 *   Claude 在 tmux mouse off 时官方提示用 PgUp/PgDn 滚 transcript；
 *   裸 SGR 64/65 会当 mouse 打到输入框，看起来像滚轮没反应。
 *   回退绝不能发方向键，否则 Claude 输入框会翻历史 prompt。
 *
 * Code Logic（这个函数做什么）:
 *   mouse tracking ≠ none → protocol（交给 xterm 发报告）；
 *   normal buffer → scrollback（即使 baseY=0 也交给 xterm，禁止把控制序列打进 shell）；
 *   alternate + none → pageFallback。
 */
export function resolveWorkbenchTerminalWheelAction(
  input: WorkbenchTerminalWheelInput,
): WorkbenchTerminalWheelAction {
  if (input.mouseTrackingMode !== 'none') return 'protocol';
  if (input.bufferType === 'normal') return 'scrollback';
  return 'pageFallback';
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

/**
 * Business Logic（为什么需要这个函数）:
 *   Claude 在 tmux `mouse off` 时把 SGR 滚轮当 mouse 打到焦点输入框，transcript 不滚。
 *   它自己的提示是 PageUp/PageDown（`scroll:pageUp` / `scroll:pageDown`），不是方向键。
 *
 * Code Logic（这个函数做什么）:
 *   一次累计滚动只发一个 CSI：负向 `\x1b[5~`（PageUp），正向 `\x1b[6~`（PageDown）。
 *   不按 |lines| 重复，避免触控板一次轻扫翻很多页。
 */
export function encodeTerminalPageScrollKeys(lines: number): string {
  if (!Number.isFinite(lines) || lines === 0) return '';
  return lines < 0 ? '\x1b[5~' : '\x1b[6~';
}
