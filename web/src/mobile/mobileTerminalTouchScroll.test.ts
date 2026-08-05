import { readFileSync } from 'node:fs';
import { describe, test } from 'vitest';
import {
  beginMobileTerminalTouchScroll,
  mobileTerminalTouchLineHeight,
  updateMobileTerminalTouchScroll,
} from './mobileTerminalTouchScroll';

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

/**
 * Business Logic（为什么需要这个函数）:
 *   源码合同测试需要确认关键 CSS/接线字符串仍然存在，避免再次回归到“手势被 xterm 原生滚动抢走”。
 *
 * Code Logic（这个函数做什么）:
 *   source 不含 needle 时抛出带 message 的 Error。
 */
function assertContains(source: string, needle: string, message: string): void {
  if (!source.includes(needle)) {
    throw new Error(message);
  }
}

describe('mobileTerminalTouchScroll', () => {
  test('upward and downward swipe scroll terminal by accumulated lines', () => {
    let state = beginMobileTerminalTouchScroll(200);
    let result = updateMobileTerminalTouchScroll(state, 164, 18);
    assertEqual(result.lines, 2, 'finger swipe up should scroll terminal down by lines');
    assertEqual(result.state.lastClientY, 164, 'touch state should track latest y after upward swipe');

    state = beginMobileTerminalTouchScroll(120);
    result = updateMobileTerminalTouchScroll(state, 150, 15);
    assertEqual(result.lines, -2, 'finger swipe down should scroll terminal up by lines');
    assertEqual(
      result.state.lastClientY,
      150,
      'touch state should track latest y after downward swipe',
    );
  });

  test('sub-line touch movement accumulates into full lines keeping remaining pixels', () => {
    const state = beginMobileTerminalTouchScroll(100);
    let result = updateMobileTerminalTouchScroll(state, 91, 20);
    assertEqual(result.lines, 0, 'sub-line touch movement should wait for accumulated pixels');
    result = updateMobileTerminalTouchScroll(result.state, 79, 20);
    assertEqual(result.lines, 1, 'sub-line touch movement should accumulate into full lines');
    assertEqual(
      result.state.remainderPx,
      1,
      'touch scroll should keep only remaining partial pixels',
    );
  });

  test('mobileTerminalTouchLineHeight prefers visible viewport rows and falls back', () => {
    assertEqual(
      mobileTerminalTouchLineHeight(360, 18, 17.5),
      20,
      'touch line height should prefer visible viewport rows',
    );
    assertEqual(
      mobileTerminalTouchLineHeight(0, 0, 17.5),
      17.5,
      'touch line height should fall back when viewport rows are unavailable',
    );
  });

  test('mobile terminal viewport CSS disables native xterm touch scrolling', () => {
    const stylesSource = readFileSync(new URL('./MobileWorkbench.module.css', import.meta.url), 'utf8');
    assertContains(
      stylesSource,
      '.mobileTerminalViewport :global(.xterm-viewport)',
      'CSS must target xterm-viewport so native overflow-y:scroll cannot steal mobile gestures',
    );
    assertContains(
      stylesSource,
      'overflow-y: hidden !important',
      'CSS must force-hide xterm-viewport overflow-y scroll for mobile terminal',
    );
    assertContains(
      stylesSource,
      '.mobileTerminalViewport :global(canvas)',
      'CSS must set touch-action:none on canvas where fingers actually land',
    );
    assertContains(
      stylesSource,
      'touch-action: none',
      'CSS must keep touch-action:none for mobile terminal touch ownership',
    );
  });

  test('MobileTerminalPanel owns touch scroll via capture-phase listeners and scrollLines', () => {
    const panelSource = readFileSync(
      new URL('./components/MobileTerminalPanel.tsx', import.meta.url),
      'utf8',
    );
    assertContains(
      panelSource,
      'capture: true',
      'touch listeners must use capture so canvas/xterm-viewport cannot consume gestures first',
    );
    assertContains(
      panelSource,
      'terminal.scrollLines(result.lines)',
      'touch move must map finger delta into xterm scrollLines',
    );
    assertContains(
      panelSource,
      'passive: false',
      'touchmove must be non-passive so preventDefault can cancel native page/xterm scrolling',
    );
  });
});
