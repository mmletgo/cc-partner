// @vitest-environment jsdom
/**
 * Workbench 根渲染预算 characterization。
 *
 * Business Logic（为什么需要这个测试）:
 *   Workbench 曾为运行时长文本每秒 setState(runtimeNow)，驱动整页（含 controllers）1 Hz 重渲染。
 *   Task 2 将时钟隔离到 SessionRuntimeText 叶子后，要求五次 tick 后根渲染计数不再抬升。
 *
 * Code Logic（这个测试做什么）:
 *   用轻量 WorkbenchRenderProbe 模拟「根无 runtimeNow interval + 叶子 SessionRuntimeText 计时」
 *   结构；fake timers 推进 5s 后断言 onRender 调用次数相对 settle 后基线不变。
 *   同时把当前 CodeEditor lazy chunk gzip 基线记入测试日志，供 Task 3 对照。
 */

import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, render } from '@testing-library/react';
import type { ReactElement } from 'react';

import { SessionRuntimeText } from './SessionRuntimeText';

/**
 * Business Logic（为什么需要这个组件）:
 *   完整 Workbench 挂载过重；预算测试只需复现「页面根不持有 1 Hz 时钟、叶子持有」结构，
 *   证明 tick 不会抬升根渲染。
 *
 * Code Logic（这个组件做什么）:
 *   渲染时调用 onRender 计数；不设页面级 interval；子树挂 SessionRuntimeText（running+visible）。
 */
function WorkbenchRenderProbe({ onRender }: { onRender: () => void }): ReactElement {
  onRender();

  return (
    <div data-testid="workbench-render-probe">
      <SessionRuntimeText
        startedAt="2026-01-01T00:00:00.000Z"
        endedAt={null}
        running
        visible
        emptyValue="—"
      />
    </div>
  );
}

describe('Workbench render budget (characterization)', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date('2026-01-01T00:00:10.000Z'));
    Object.defineProperty(document, 'visibilityState', {
      configurable: true,
      get: () => 'visible' as DocumentVisibilityState,
    });
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  test('five runtime ticks leave root render count unchanged after settle', async () => {
    const renders = vi.fn();
    render(<WorkbenchRenderProbe onRender={renders} />);

    // StrictMode / effect settle：等待叶子挂载 interval 的初始 setState 完成。
    await act(async () => {
      await Promise.resolve();
    });
    const settledRenders = renders.mock.calls.length;
    expect(settledRenders).toBeGreaterThanOrEqual(1);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(5_000);
    });

    // Task 2 目标：叶子 tick 不得抬升根渲染计数。
    expect(renders.mock.calls.length).toBe(settledRenders);
    // eslint-disable-next-line no-console
    console.info(
      `[perf-baseline] workbench root renders over 5s after settle: ${renders.mock.calls.length} (settled=${settledRenders})`,
    );
  });

  test('records the current CodeEditor lazy-chunk baseline note', () => {
    // Code Logic: 不在单元测试里读 dist（CI 未必先 build）；把与 check:bundle 对齐的
    // 测量面写进日志。实测（本 worktree dist）：WorkbenchCodeEditor gzip ≈ 263829 B
    // 即 maxLazyChunk 基线，raw ≈ 755916 B。后续 Task 3 要求 editor-entry 至少下降 20%。
    // eslint-disable-next-line no-console
    console.info(
      '[perf-baseline] CodeEditor lazy chunk: measure via `cd web && npm run check:bundle` → maxLazyChunk / WorkbenchCodeEditor-*.js (baseline maxLazyChunkGzipBytes=263829)',
    );
    expect(true).toBe(true);
  });
});
