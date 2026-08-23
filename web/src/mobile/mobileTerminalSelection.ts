import { MOBILE_TERMINAL_EXTRA_KEY_LONG_PRESS_MS } from './mobileTerminalExtraKeys';

/**
 * 移动端终端长按划选纯逻辑。
 *
 * Business Logic（为什么需要这个模块）:
 *   `/mobile` xterm 需要长按进入划选、移动超过阈值改滚动、边缘滚历史；这些判定必须与 React/DOM
 *   解耦，才能单测并给面板复用。
 *
 * Code Logic（这个模块做什么）:
 *   导出手势状态机、像素位移、指针到单元格、xterm.select 线性范围与边缘滚动增量；不挂监听、不碰剪贴板。
 */

/** 手指位移超过该像素才从按住改为滚动（与现有面板 8px 阈值一致）。 */
export const MOBILE_TERMINAL_SELECT_MOVE_PX = 8;

export type MobileTerminalGesturePhase = 'idle' | 'pressPending' | 'scrolling' | 'selecting';

export interface CellPos {
  /** 0-based 列。 */
  col: number;
  /** 0-based BUFFER 行 = viewportY + screenRow。 */
  row: number;
}

export interface XtermSelectRange {
  column: number;
  row: number;
  length: number;
}

export interface MobileTerminalGestureState {
  phase: MobileTerminalGesturePhase;
  originX: number;
  originY: number;
  anchor: CellPos | null;
  focus: CellPos | null;
}

function copyCell(cell: CellPos): CellPos {
  return { col: cell.col, row: cell.row };
}

function isPositiveFinite(value: number): boolean {
  return Number.isFinite(value) && value > 0;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   长按复制从指针按下开始计时；需要记录原点以便后续区分微抖、滚动与划选。
 *
 * Code Logic（这个函数做什么）:
 *   返回 phase=pressPending、origin 为坐标、anchor/focus 为 null 的新手势状态。
 */
export function beginPress(clientX: number, clientY: number): MobileTerminalGestureState {
  return {
    phase: 'pressPending',
    originX: clientX,
    originY: clientY,
    anchor: null,
    focus: null,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   判定滚动还是长按划选都依赖位移；必须用欧氏距离，避免只看单轴漏掉斜向滑动。
 *
 * Code Logic（这个函数做什么）:
 *   返回 (fromX,fromY) 到 (toX,toY) 的欧氏距离。
 */
export function travelPx(fromX: number, fromY: number, toX: number, toY: number): number {
  return Math.hypot(toX - fromX, toY - fromY);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用户开始滑动历史时不应误进划选；超过阈值必须改走滚动。
 *
 * Code Logic（这个函数做什么）:
 *   travel 严格大于 movePx（默认 MOBILE_TERMINAL_SELECT_MOVE_PX）时为 true；等于阈值仍不算滚动。
 */
export function shouldBecomeScrolling(
  travel: number,
  movePx: number = MOBILE_TERMINAL_SELECT_MOVE_PX,
): boolean {
  return travel > movePx;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   长按且基本没动才进入划选，避免滚动途中误选。
 *
 * Code Logic（这个函数做什么）:
 *   elapsedMs >= longPressMs（默认 extra key 400ms）且 travel <= movePx 时为 true。
 */
export function shouldEnterSelecting(
  elapsedMs: number,
  travel: number,
  longPressMs: number = MOBILE_TERMINAL_EXTRA_KEY_LONG_PRESS_MS,
  movePx: number = MOBILE_TERMINAL_SELECT_MOVE_PX,
): boolean {
  return elapsedMs >= longPressMs && travel <= movePx;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   划选锚点/焦点必须落到 xterm buffer 单元格，才能调用 Terminal.select。
 *
 * Code Logic（这个函数做什么）:
 *   用视口矩形均分行宽列宽，floor 得到 screenCol/screenRow 并夹到 0..cols-1 / 0..rows-1，
 *   buffer 行 = viewportY + screenRow；宽高或行列非法时返回 {col:0,row:viewportY}。
 */
export function pointerToCell(
  clientX: number,
  clientY: number,
  rect: { left: number; top: number; width: number; height: number },
  cols: number,
  rows: number,
  viewportY: number,
): CellPos {
  if (
    !isPositiveFinite(rect.width) ||
    !isPositiveFinite(rect.height) ||
    !isPositiveFinite(cols) ||
    !isPositiveFinite(rows)
  ) {
    return { col: 0, row: viewportY };
  }
  const cellWidth = rect.width / cols;
  const cellHeight = rect.height / rows;
  const screenCol = Math.min(
    cols - 1,
    Math.max(0, Math.floor((clientX - rect.left) / cellWidth)),
  );
  const screenRow = Math.min(
    rows - 1,
    Math.max(0, Math.floor((clientY - rect.top) / cellHeight)),
  );
  return { col: screenCol, row: viewportY + screenRow };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   按住期间手指一旦滑出阈值，就应切到滚动，不再等待长按。
 *
 * Code Logic（这个函数做什么）:
 *   仅 pressPending 且相对 origin 的 travel 应滚动时改为 scrolling；scrolling/selecting/idle 原样返回。
 */
export function noteMove(
  state: MobileTerminalGestureState,
  clientX: number,
  clientY: number,
): MobileTerminalGestureState {
  if (state.phase !== 'pressPending') {
    return state;
  }
  const travel = travelPx(state.originX, state.originY, clientX, clientY);
  if (!shouldBecomeScrolling(travel)) {
    return state;
  }
  return { ...state, phase: 'scrolling' };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   长按成立后需要立刻钉住锚点，后续拖动才有选区起点。
 *
 * Code Logic（这个函数做什么）:
 *   返回 phase=selecting 且 anchor=focus=cell 的新状态（拷贝单元格，保留 origin）。
 */
export function startSelecting(
  state: MobileTerminalGestureState,
  cell: CellPos,
): MobileTerminalGestureState {
  const copied = copyCell(cell);
  return {
    ...state,
    phase: 'selecting',
    anchor: copied,
    focus: copyCell(copied),
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   划选拖动只应移动焦点，锚点保持按下时长按的那一格。
 *
 * Code Logic（这个函数做什么）:
 *   非 selecting 原样返回；否则只更新 focus=cell，保留 anchor。
 */
export function dragSelecting(
  state: MobileTerminalGestureState,
  cell: CellPos,
): MobileTerminalGestureState {
  if (state.phase !== 'selecting') {
    return state;
  }
  return { ...state, focus: copyCell(cell) };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   抬手、取消或开始新手势前必须清掉阶段与单元格，避免脏状态串到下一次。
 *
 * Code Logic（这个函数做什么）:
 *   返回 idle、origin 为 0、anchor/focus 为 null 的状态。
 */
export function resetGesture(): MobileTerminalGestureState {
  return {
    phase: 'idle',
    originX: 0,
    originY: 0,
    anchor: null,
    focus: null,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   xterm.Terminal.select(column, row, length) 吃的是线性字符长度，需要把两端单元格折成该三元组。
 *
 * Code Logic（这个函数做什么）:
 *   linear = row * max(cols,1) + col；startLin=min、endLin=max；length=endLin-startLin+1；
 *   column=startLin%cols，row=floor(startLin/cols)。
 */
export function cellsToXtermSelect(
  anchor: CellPos,
  focus: CellPos,
  cols: number,
): XtermSelectRange {
  const safeCols = Math.max(cols, 1);
  const startLin = Math.min(anchor.row * safeCols + anchor.col, focus.row * safeCols + focus.col);
  const endLin = Math.max(anchor.row * safeCols + anchor.col, focus.row * safeCols + focus.col);
  return {
    column: startLin % safeCols,
    row: Math.floor(startLin / safeCols),
    length: endLin - startLin + 1,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   复制 toast / 无障碍需要告诉用户选了几行，而不是线性字符数。
 *
 * Code Logic（这个函数做什么）:
 *   返回 abs(anchor.row - focus.row) + 1，至少为 1。
 */
export function countSelectedLines(anchor: CellPos, focus: CellPos): number {
  return Math.max(1, Math.abs(anchor.row - focus.row) + 1);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   划选拖到视口上下边缘时应滚动历史，才能选中当前屏以外的内容。
 *
 * Code Logic（这个函数做什么）:
 *   边缘带高度 = (height/rows)*(edgeRows 默认 1)；指针在顶带返回 -1、底带 +1、其余 0；尺寸非法返回 0。
 */
export function edgeScrollDelta(
  clientY: number,
  rect: { top: number; height: number },
  rows: number,
  edgeRows: number = 1,
): number {
  if (!isPositiveFinite(rect.height) || !isPositiveFinite(rows) || !Number.isFinite(clientY)) {
    return 0;
  }
  const safeEdgeRows = isPositiveFinite(edgeRows) ? edgeRows : 1;
  const zoneHeight = (rect.height / rows) * safeEdgeRows;
  if (!Number.isFinite(zoneHeight) || zoneHeight <= 0) {
    return 0;
  }
  if (clientY <= rect.top + zoneHeight) {
    return -1;
  }
  if (clientY >= rect.top + rect.height - zoneHeight) {
    return 1;
  }
  return 0;
}
