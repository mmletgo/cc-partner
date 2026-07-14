/**
 * rovingTablist 纯函数契约测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   终端/inspector/文件 tablist 共用 getRovingTabIndex；必须锁住 wrap 与 Home/End 语义。
 *
 * Code Logic（这个测试做什么）:
 *   覆盖正常前进/后退 wrap、Home/End、空列表与越界 currentIndex。
 */

import { describe, expect, test } from 'vitest';

import { getRovingTabIndex } from './rovingTablist';

describe('getRovingTabIndex', () => {
  test('ArrowRight advances and wraps at end', () => {
    expect(getRovingTabIndex(0, 'ArrowRight', 3)).toBe(1);
    expect(getRovingTabIndex(1, 'ArrowRight', 3)).toBe(2);
    expect(getRovingTabIndex(2, 'ArrowRight', 3)).toBe(0);
  });

  test('ArrowLeft retreats and wraps at start', () => {
    expect(getRovingTabIndex(2, 'ArrowLeft', 3)).toBe(1);
    expect(getRovingTabIndex(1, 'ArrowLeft', 3)).toBe(0);
    expect(getRovingTabIndex(0, 'ArrowLeft', 3)).toBe(2);
  });

  test('Home and End jump to edges', () => {
    expect(getRovingTabIndex(2, 'Home', 4)).toBe(0);
    expect(getRovingTabIndex(0, 'End', 4)).toBe(3);
    expect(getRovingTabIndex(1, 'End', 1)).toBe(0);
  });

  test('empty count and out-of-range currentIndex stay safe', () => {
    expect(getRovingTabIndex(0, 'ArrowRight', 0)).toBe(0);
    expect(getRovingTabIndex(-1, 'ArrowRight', 3)).toBe(1);
    expect(getRovingTabIndex(99, 'ArrowLeft', 3)).toBe(2);
  });
});
