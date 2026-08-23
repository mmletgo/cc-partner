import { describe, test } from 'vitest';
import { MOBILE_TERMINAL_EXTRA_KEY_LONG_PRESS_MS } from './mobileTerminalExtraKeys';
import {
  beginPress,
  cellsToXtermSelect,
  countSelectedLines,
  dragSelecting,
  edgeScrollDelta,
  MOBILE_TERMINAL_SELECT_MOVE_PX,
  noteMove,
  pointerToCell,
  resetGesture,
  shouldBecomeScrolling,
  shouldEnterSelecting,
  startSelecting,
  travelPx,
  type CellPos,
} from './mobileTerminalSelection';

/**
 * Business Logic（为什么需要这个函数）:
 *   当前 web tsconfig 会编译 src 下测试文件，但未启用 Node 类型；测试断言需要避免依赖 node:assert。
 *
 * Code Logic（这个函数做什么）:
 *   比较 actual 与 expected，不一致时抛出 Error 让用例失败。
 */
function assertEqual<T>(actual: T, expected: T, message: string): void {
  if (actual !== expected) {
    throw new Error(`${message}: expected ${String(expected)}, received ${String(actual)}`);
  }
}

function assertTrue(value: boolean, message: string): void {
  if (!value) throw new Error(message);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   手势状态与单元格坐标是对象；需要字段级比较，不能只做引用相等。
 *
 * Code Logic（这个函数做什么）:
 *   JSON 序列化后比较 actual 与 expected。
 */
function assertDeepEqual(actual: unknown, expected: unknown, message: string): void {
  const actualJson = JSON.stringify(actual);
  const expectedJson = JSON.stringify(expected);
  if (actualJson !== expectedJson) {
    throw new Error(`${message}: expected ${expectedJson}, received ${actualJson}`);
  }
}

describe('mobileTerminalSelection', () => {
  test('beginPress enters pressPending with origin and null cells', () => {
    const state = beginPress(12, 34);
    assertEqual(state.phase, 'pressPending', 'phase');
    assertEqual(state.originX, 12, 'originX');
    assertEqual(state.originY, 34, 'originY');
    assertEqual(state.anchor, null, 'anchor');
    assertEqual(state.focus, null, 'focus');
  });

  test('travel 9px becomes scrolling; 8px does not (strict greater than threshold)', () => {
    assertEqual(MOBILE_TERMINAL_SELECT_MOVE_PX, 8, 'move threshold');
    assertEqual(shouldBecomeScrolling(9), true, '9px scrolls');
    assertEqual(shouldBecomeScrolling(8), false, '8px stays pending');
    assertEqual(shouldBecomeScrolling(8.1), true, '8.1px scrolls');
    assertEqual(shouldBecomeScrolling(0), false, '0px stays pending');
  });

  test('400ms + travel 0 enters selecting; 399ms does not; 400ms + travel 9 does not', () => {
    assertEqual(MOBILE_TERMINAL_EXTRA_KEY_LONG_PRESS_MS, 400, 'long-press ms');
    assertEqual(shouldEnterSelecting(400, 0), true, '400ms still');
    assertEqual(shouldEnterSelecting(399, 0), false, '399ms too early');
    assertEqual(shouldEnterSelecting(400, 9), false, 'moved too far');
    assertEqual(shouldEnterSelecting(400, 8), true, 'travel at threshold still selects');
  });

  test('pointerToCell maps 80x24 / 800x240 click 10,10 to col 1 row 1; click 0,0 to origin cell', () => {
    const rect = { left: 0, top: 0, width: 800, height: 240 };
    assertDeepEqual(
      pointerToCell(10, 10, rect, 80, 24, 0),
      { col: 1, row: 1 },
      '10px cell at 10,10',
    );
    assertDeepEqual(
      pointerToCell(0, 0, rect, 80, 24, 0),
      { col: 0, row: 0 },
      'origin cell',
    );
  });

  test('pointerToCell adds viewportY to the buffer row', () => {
    const rect = { left: 0, top: 0, width: 800, height: 240 };
    assertDeepEqual(
      pointerToCell(10, 10, rect, 80, 24, 40),
      { col: 1, row: 41 },
      'viewportY 40 plus screen row 1',
    );
    assertDeepEqual(
      pointerToCell(0, 0, rect, 80, 24, 40),
      { col: 0, row: 40 },
      'origin row is viewportY',
    );
  });

  test('cellsToXtermSelect same cell has length 1', () => {
    const cell: CellPos = { col: 3, row: 8 };
    const range = cellsToXtermSelect(cell, cell, 80);
    assertEqual(range.column, 3, 'column');
    assertEqual(range.row, 8, 'row');
    assertEqual(range.length, 1, 'length');
  });

  test('cellsToXtermSelect wraps across cols=80 from (70,5) to (10,6)', () => {
    const range = cellsToXtermSelect({ col: 70, row: 5 }, { col: 10, row: 6 }, 80);
    assertEqual(range.column, 70, 'start column');
    assertEqual(range.row, 5, 'start row');
    assertEqual(range.length, 21, 'wrapped length');
  });

  test('cellsToXtermSelect reverse order yields the same range', () => {
    const forward = cellsToXtermSelect({ col: 70, row: 5 }, { col: 10, row: 6 }, 80);
    const reverse = cellsToXtermSelect({ col: 10, row: 6 }, { col: 70, row: 5 }, 80);
    assertDeepEqual(reverse, forward, 'anchor/focus order independent');
  });

  test('countSelectedLines is 1 on the same row and 3 for rows 5 and 7', () => {
    assertEqual(
      countSelectedLines({ col: 0, row: 5 }, { col: 10, row: 5 }),
      1,
      'same row',
    );
    assertEqual(
      countSelectedLines({ col: 0, row: 5 }, { col: 0, row: 7 }),
      3,
      'rows 5 and 7',
    );
    assertEqual(
      countSelectedLines({ col: 0, row: 7 }, { col: 0, row: 5 }),
      3,
      'order independent',
    );
  });

  test('noteMove from pressPending with 20px y becomes scrolling', () => {
    const pending = beginPress(100, 100);
    const moved = noteMove(pending, 100, 120);
    assertEqual(moved.phase, 'scrolling', '20px y travel');
    assertEqual(moved.originX, 100, 'originX kept');
    assertEqual(moved.originY, 100, 'originY kept');
  });

  test('startSelecting then dragSelecting updates focus and keeps anchor', () => {
    const selecting = startSelecting(beginPress(0, 0), { col: 2, row: 4 });
    assertEqual(selecting.phase, 'selecting', 'phase');
    assertDeepEqual(selecting.anchor, { col: 2, row: 4 }, 'anchor');
    assertDeepEqual(selecting.focus, { col: 2, row: 4 }, 'initial focus');

    const dragged = dragSelecting(selecting, { col: 9, row: 6 });
    assertEqual(dragged.phase, 'selecting', 'still selecting');
    assertDeepEqual(dragged.anchor, { col: 2, row: 4 }, 'anchor unchanged');
    assertDeepEqual(dragged.focus, { col: 9, row: 6 }, 'focus updated');
  });

  test('resetGesture returns idle zeros and null cells', () => {
    const idle = resetGesture();
    assertEqual(idle.phase, 'idle', 'phase');
    assertEqual(idle.originX, 0, 'originX');
    assertEqual(idle.originY, 0, 'originY');
    assertEqual(idle.anchor, null, 'anchor');
    assertEqual(idle.focus, null, 'focus');
  });

  test('edgeScrollDelta is -1 at top, +1 at bottom, 0 in the middle', () => {
    const rect = { top: 0, height: 240 };
    assertEqual(edgeScrollDelta(5, rect, 24), -1, 'top zone');
    assertEqual(edgeScrollDelta(235, rect, 24), 1, 'bottom zone');
    assertEqual(edgeScrollDelta(120, rect, 24), 0, 'middle');
  });

  test('travelPx is Euclidean distance', () => {
    assertEqual(travelPx(0, 0, 3, 4), 5, '3-4-5');
    assertEqual(travelPx(10, 10, 10, 18), 8, 'vertical 8');
  });

  test('noteMove does not leave pressPending under the move threshold', () => {
    const pending = beginPress(0, 0);
    const still = noteMove(pending, 0, 8);
    assertEqual(still.phase, 'pressPending', '8px is not scrolling');
  });

  test('noteMove leaves scrolling/selecting/idle unchanged', () => {
    const scrolling = noteMove(beginPress(0, 0), 0, 20);
    assertEqual(noteMove(scrolling, 40, 40).phase, 'scrolling', 'scrolling unchanged');

    const selecting = startSelecting(beginPress(1, 1), { col: 0, row: 0 });
    const afterSelectMove = noteMove(selecting, 50, 50);
    assertEqual(afterSelectMove.phase, 'selecting', 'selecting unchanged');
    assertDeepEqual(afterSelectMove.anchor, { col: 0, row: 0 }, 'selecting cells kept');

    const idle = resetGesture();
    assertEqual(noteMove(idle, 9, 9).phase, 'idle', 'idle unchanged');
  });

  test('dragSelecting is a no-op unless already selecting', () => {
    const pending = beginPress(0, 0);
    const ignored = dragSelecting(pending, { col: 3, row: 3 });
    assertEqual(ignored.phase, 'pressPending', 'pending not dragged');
    assertEqual(ignored.anchor, null, 'pending anchor');
  });

  test('pointerToCell clamps and returns origin cell for invalid sizes', () => {
    const rect = { left: 0, top: 0, width: 800, height: 240 };
    assertDeepEqual(
      pointerToCell(800, 240, rect, 80, 24, 7),
      { col: 79, row: 30 },
      'clamp to last cell plus viewportY',
    );
    assertDeepEqual(
      pointerToCell(10, 10, { left: 0, top: 0, width: 0, height: 240 }, 80, 24, 12),
      { col: 0, row: 12 },
      'invalid width',
    );
  });

  test('edgeScrollDelta returns 0 for invalid sizes', () => {
    assertEqual(edgeScrollDelta(5, { top: 0, height: 0 }, 24), 0, 'zero height');
    assertEqual(edgeScrollDelta(5, { top: 0, height: 240 }, 0), 0, 'zero rows');
  });
});
