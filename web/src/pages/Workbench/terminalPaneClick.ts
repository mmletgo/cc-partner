/**
 * Workbench 终端点击切换 pane 的纯几何/手势判定。
 *
 * Business Logic（为什么需要这个模块）:
 *   tmux 的多个 pane 只是同一个 xterm 字符网格上的分区，前端唯一能提供的信息是
 *   “用户点了第几行第几列”。把像素→字符格换算与“这次点击算不算切换分栏”的判定
 *   抽成纯函数，才能脱离 DOM 做单元测试，并保证不误伤拖拽选中文字。
 *
 * Code Logic（这个模块做什么）:
 *   - terminalCellFromPointer：用 viewport 原点 + cell 尺寸把 client 坐标换算为 clamp 后的 (col,row)；
 *   - isSameTerminalCell / shouldSelectPaneOnClick：判定 mousedown→mouseup 是否构成一次纯点击。
 */

/**
 * 终端 viewport 的像素度量（与 WorkbenchTerminalPane 的 cursor metrics 同构）。
 */
export interface TerminalCellMetrics {
  left: number;
  top: number;
  cellWidth: number;
  cellHeight: number;
}

/**
 * 终端字符格坐标（0 基，col 为列、row 为行）。
 */
export interface TerminalCell {
  col: number;
  row: number;
}

/**
 * 一次点击是否应触发切换分栏的判定输入。
 */
export interface TerminalPaneClickInput {
  /** mousedown 落点；未记录到按下（例如按下发生在别处）时为 null。 */
  down: TerminalCell | null;
  /** mouseup 落点。 */
  up: TerminalCell | null;
  /** xterm 当前是否存在文本选区。 */
  hasSelection: boolean;
  /** xterm 视口是否停在最底部（滚了历史后行号与 tmux 屏幕不对应）。 */
  atBottom: boolean;
  /** 是否允许写操作（远端离线等情况下必须禁止）。 */
  writeEnabled: boolean;
  /**
   * 指针按下到松开的像素位移。
   * 即使落在同一字符格，拖出超过阈值也视为“拖拽选字”，不得切分栏。
   */
  pointerTravelPx?: number;
  /**
   * TUI 已协商 mouse tracking 时，点击必须交给应用（按钮/菜单），
   * 不得再抢去 tmux select-pane，否则 Grok/Claude 点按会变成切分栏或被重绘吃掉。
   */
  mouseTrackingActive: boolean;
}

/**
 * 拖拽选字与纯点击分栏的像素位移阈值。
 * 小于等于该值且同格才允许 select-pane；更大位移一律交给 xterm 选区。
 */
export const TERMINAL_PANE_CLICK_MAX_TRAVEL_PX = 4;

/**
 * Business Logic（为什么需要这个函数）:
 *   后端按 tmux 字符格坐标做 pane 命中，前端必须把鼠标像素位置换算成同一坐标系，
 *   并 clamp 在终端范围内，避免边缘 1px 抖动产生越界坐标。
 *
 * Code Logic（这个函数做什么）:
 *   减去 viewport 原点后除以 cell 宽高取整；cell 尺寸非正或非有限时返回 null；
 *   结果 clamp 到 [0, cols-1] / [0, rows-1]。
 */
export function terminalCellFromPointer(
  metrics: TerminalCellMetrics,
  clientX: number,
  clientY: number,
  cols: number,
  rows: number,
): TerminalCell | null {
  const { left, top, cellWidth, cellHeight } = metrics;
  if (!Number.isFinite(cellWidth) || !Number.isFinite(cellHeight)) return null;
  if (cellWidth <= 0 || cellHeight <= 0) return null;
  if (!Number.isFinite(clientX) || !Number.isFinite(clientY)) return null;
  if (!Number.isFinite(cols) || !Number.isFinite(rows) || cols <= 0 || rows <= 0) return null;
  const rawCol = Math.floor((clientX - left) / cellWidth);
  const rawRow = Math.floor((clientY - top) / cellHeight);
  return {
    col: Math.min(Math.max(rawCol, 0), Math.floor(cols) - 1),
    row: Math.min(Math.max(rawRow, 0), Math.floor(rows) - 1),
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   拖拽选中文字与点击切换分栏共用同一套鼠标事件，必须用“按下与松开落在同一个字符格”
 *   区分二者，否则每次选中文字都会顺带切走 active pane。
 *
 * Code Logic（这个函数做什么）:
 *   两个坐标都存在且 col/row 全等时返回 true。
 */
export function isSameTerminalCell(a: TerminalCell | null, b: TerminalCell | null): boolean {
  if (!a || !b) return false;
  return a.col === b.col && a.row === b.row;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   只有“未拖拽、无选区、视口在底部、允许写、TUI 未接管鼠标”的左键点击才代表用户想切换分栏。
 *   视口滚上去看历史时，屏幕行不再对应 tmux 当前屏幕，坐标会命中错误 pane，必须拒绝。
 *   拖选文字后若误触发 select-pane，tmux/PTY 重绘或 mouse-mode 切换会立刻清掉 xterm 选区，导致无法复制。
 *   mouse tracking 开启时点击属于 TUI 按钮，必须让 SGR 到达 PTY，而不是 select-pane。
 *
 * Code Logic（这个函数做什么）:
 *   逐条检查 !mouseTrackingActive / writeEnabled / atBottom / !hasSelection / 像素位移阈值 / down 与 up 同格；
 *   全部满足才返回该字符格，否则返回 null。
 */
export function shouldSelectPaneOnClick(input: TerminalPaneClickInput): TerminalCell | null {
  if (input.mouseTrackingActive) return null;
  if (!input.writeEnabled) return null;
  if (!input.atBottom) return null;
  if (input.hasSelection) return null;
  if ((input.pointerTravelPx ?? 0) > TERMINAL_PANE_CLICK_MAX_TRAVEL_PX) return null;
  if (!isSameTerminalCell(input.down, input.up)) return null;
  return input.up;
}
