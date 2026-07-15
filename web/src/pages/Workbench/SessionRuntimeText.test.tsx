// @vitest-environment jsdom
/**
 * SessionRuntimeText 叶子组件契约测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   运行时长文本原先由 Workbench 根 1 Hz setState 驱动整页重渲染；隔离到叶子后必须锁住
 *   interval 仅在 running + surface visible + document visible 时启动、stopped 冻结 endedAt、
 *   unmount 清理等行为，防止性能回归。
 *
 * Code Logic（这个测试做什么）:
 *   fake timers + document.visibilityState mock + setInterval spy；渲染 SessionRuntimeText
 *   断言 interval 启停、文本每秒更新、stopped 冻结与 unmount cleanup。
 */

import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, render, screen } from '@testing-library/react';

import { SessionRuntimeText } from './SessionRuntimeText';

/**
 * Business Logic（为什么需要这个函数）:
 *   jsdom 默认 visibilityState 固定，测试需要模拟 hidden/visible 切换。
 *
 * Code Logic（这个函数做什么）:
 *   用 configurable getter 覆盖 document.visibilityState，并派发 visibilitychange。
 */
function setVisibilityState(state: DocumentVisibilityState): void {
  Object.defineProperty(document, 'visibilityState', {
    configurable: true,
    get: () => state,
  });
  document.dispatchEvent(new Event('visibilitychange'));
}

const FIXED_NOW = new Date('2026-01-01T00:00:10.000Z');

describe('SessionRuntimeText', () => {
  let setIntervalSpy: ReturnType<typeof vi.spyOn>;
  let clearIntervalSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(FIXED_NOW);
    setVisibilityState('visible');
    setIntervalSpy = vi.spyOn(window, 'setInterval');
    clearIntervalSpy = vi.spyOn(window, 'clearInterval');
  });

  afterEach(() => {
    cleanup();
    setIntervalSpy.mockRestore();
    clearIntervalSpy.mockRestore();
    vi.useRealTimers();
    setVisibilityState('visible');
  });

  test('does not start interval when session is stopped; freezes duration at endedAt', () => {
    const startedAt = '2026-01-01T00:00:00.000Z';
    const endedAt = '2026-01-01T00:00:03.000Z';

    render(
      <SessionRuntimeText
        startedAt={startedAt}
        endedAt={endedAt}
        running={false}
        visible
        emptyValue="—"
      />,
    );

    expect(screen.getByTestId('session-runtime-text').textContent).toBe('3s');
    expect(setIntervalSpy).not.toHaveBeenCalled();

    // 推进时钟不应改变 stopped 时长
    void act(() => {
      vi.advanceTimersByTime(5_000);
    });
    expect(screen.getByTestId('session-runtime-text').textContent).toBe('3s');
    expect(setIntervalSpy).not.toHaveBeenCalled();
  });

  test('does not start interval when document is hidden', () => {
    setVisibilityState('hidden');

    render(
      <SessionRuntimeText
        startedAt="2026-01-01T00:00:00.000Z"
        endedAt={null}
        running
        visible
        emptyValue="—"
      />,
    );

    expect(setIntervalSpy).not.toHaveBeenCalled();
  });

  test('does not start interval when owning surface is not visible', () => {
    render(
      <SessionRuntimeText
        startedAt="2026-01-01T00:00:00.000Z"
        endedAt={null}
        running
        visible={false}
        emptyValue="—"
      />,
    );

    expect(setIntervalSpy).not.toHaveBeenCalled();
  });

  test('updates formatted runtime once per second when running and both visibility conditions hold', async () => {
    render(
      <SessionRuntimeText
        startedAt="2026-01-01T00:00:00.000Z"
        endedAt={null}
        running
        visible
        emptyValue="—"
      />,
    );

    // FIXED_NOW - startedAt = 10s
    expect(screen.getByTestId('session-runtime-text').textContent).toBe('10s');
    expect(setIntervalSpy).toHaveBeenCalled();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(1_000);
    });
    expect(screen.getByTestId('session-runtime-text').textContent).toBe('11s');

    await act(async () => {
      await vi.advanceTimersByTimeAsync(1_000);
    });
    expect(screen.getByTestId('session-runtime-text').textContent).toBe('12s');
  });

  test('clears interval on unmount', () => {
    const { unmount } = render(
      <SessionRuntimeText
        startedAt="2026-01-01T00:00:00.000Z"
        endedAt={null}
        running
        visible
        emptyValue="—"
      />,
    );

    expect(setIntervalSpy).toHaveBeenCalled();
    const timerId = setIntervalSpy.mock.results[0]?.value as number;

    unmount();
    expect(clearIntervalSpy).toHaveBeenCalledWith(timerId);
  });

  test('stops ticking when document becomes hidden and resumes when visible again', async () => {
    render(
      <SessionRuntimeText
        startedAt="2026-01-01T00:00:00.000Z"
        endedAt={null}
        running
        visible
        emptyValue="—"
      />,
    );

    expect(screen.getByTestId('session-runtime-text').textContent).toBe('10s');

    await act(async () => {
      setVisibilityState('hidden');
    });

    // hidden 后 interval 应被清理；推进时间文本冻结
    const afterHideCalls = setIntervalSpy.mock.calls.length;
    await act(async () => {
      await vi.advanceTimersByTimeAsync(3_000);
    });
    expect(screen.getByTestId('session-runtime-text').textContent).toBe('10s');
    // 不应再新建 interval（hidden 期间）
    expect(setIntervalSpy.mock.calls.length).toBe(afterHideCalls);

    await act(async () => {
      setVisibilityState('visible');
    });
    // 恢复可见后应重新启动 interval 并刷新到当前时刻（10s + 3s = 13s）
    expect(screen.getByTestId('session-runtime-text').textContent).toBe('13s');

    await act(async () => {
      await vi.advanceTimersByTimeAsync(1_000);
    });
    expect(screen.getByTestId('session-runtime-text').textContent).toBe('14s');
  });
});
