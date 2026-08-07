import { describe, expect, it } from 'vitest';
import {
  isSameTerminalCell,
  shouldSelectPaneOnClick,
  terminalCellFromPointer,
  type TerminalCellMetrics,
} from './terminalPaneClick';

const metrics: TerminalCellMetrics = { left: 100, top: 50, cellWidth: 8, cellHeight: 16 };

describe('terminalCellFromPointer', () => {
  /**
   * Business Logic: 后端按 tmux 字符格做 pane 命中，像素换算必须与 xterm 网格一致。
   */
  it('maps client pixels to zero-based terminal cells', () => {
    expect(terminalCellFromPointer(metrics, 100, 50, 80, 24)).toEqual({ col: 0, row: 0 });
    expect(terminalCellFromPointer(metrics, 107, 65, 80, 24)).toEqual({ col: 0, row: 0 });
    expect(terminalCellFromPointer(metrics, 108, 66, 80, 24)).toEqual({ col: 1, row: 1 });
  });

  /**
   * Business Logic: 边缘 1px 抖动不得产生越界坐标，否则后端命中判定会读到无效格子。
   */
  it('clamps out-of-range pointers into the terminal grid', () => {
    expect(terminalCellFromPointer(metrics, 0, 0, 80, 24)).toEqual({ col: 0, row: 0 });
    expect(terminalCellFromPointer(metrics, 99999, 99999, 80, 24)).toEqual({ col: 79, row: 23 });
  });

  /**
   * Business Logic: cell 尺寸缓存失效期间可能为 0，此时换算无意义，必须拒绝而不是除零。
   */
  it('returns null for degenerate metrics or grid sizes', () => {
    expect(terminalCellFromPointer({ ...metrics, cellWidth: 0 }, 120, 60, 80, 24)).toBeNull();
    expect(terminalCellFromPointer({ ...metrics, cellHeight: -1 }, 120, 60, 80, 24)).toBeNull();
    expect(terminalCellFromPointer(metrics, Number.NaN, 60, 80, 24)).toBeNull();
    expect(terminalCellFromPointer(metrics, 120, 60, 0, 24)).toBeNull();
  });
});

describe('isSameTerminalCell', () => {
  it('requires both cells present and equal', () => {
    expect(isSameTerminalCell({ col: 3, row: 4 }, { col: 3, row: 4 })).toBe(true);
    expect(isSameTerminalCell({ col: 3, row: 4 }, { col: 3, row: 5 })).toBe(false);
    expect(isSameTerminalCell(null, { col: 3, row: 4 })).toBe(false);
    expect(isSameTerminalCell({ col: 3, row: 4 }, null)).toBe(false);
  });
});

describe('shouldSelectPaneOnClick', () => {
  const base = {
    down: { col: 5, row: 6 },
    up: { col: 5, row: 6 },
    hasSelection: false,
    atBottom: true,
    writeEnabled: true,
  };

  /**
   * Business Logic: 未拖拽的左键点击是切换分栏的唯一手势。
   */
  it('accepts a click that stays inside one cell', () => {
    expect(shouldSelectPaneOnClick(base)).toEqual({ col: 5, row: 6 });
  });

  /**
   * Business Logic: 拖拽选中文字必须保持原生行为，不得顺带切走 active pane。
   */
  it('rejects drags and existing selections', () => {
    expect(shouldSelectPaneOnClick({ ...base, up: { col: 9, row: 6 } })).toBeNull();
    expect(shouldSelectPaneOnClick({ ...base, hasSelection: true })).toBeNull();
    expect(shouldSelectPaneOnClick({ ...base, down: null })).toBeNull();
  });

  /**
   * Business Logic: 同格内仍可拖出几像素形成选区；位移超过阈值时不得切分栏，否则选中立刻被清掉。
   */
  it('rejects same-cell gestures with pointer travel beyond the click threshold', () => {
    expect(shouldSelectPaneOnClick({ ...base, pointerTravelPx: 1 })).toEqual({
      col: 5,
      row: 6,
    });
    expect(shouldSelectPaneOnClick({ ...base, pointerTravelPx: 5 })).toBeNull();
  });

  /**
   * Business Logic: 视口滚到历史后屏幕行不对应 tmux 当前屏幕，坐标会命中错误 pane。
   */
  it('rejects clicks while scrolled back in the xterm scrollback', () => {
    expect(shouldSelectPaneOnClick({ ...base, atBottom: false })).toBeNull();
  });

  /**
   * Business Logic: 远端项目离线时与 split/close pane 一致地禁写。
   */
  it('rejects clicks when writes are disabled', () => {
    expect(shouldSelectPaneOnClick({ ...base, writeEnabled: false })).toBeNull();
  });
});
