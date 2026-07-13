// @vitest-environment jsdom
/**
 * useVisibilityPolling 单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   可见性感知 + single-flight 轮询是 Transfer/Devices/Health 等页面的共享正确性基础，
 *   必须保证后台停轮询、恢复立即刷新、重叠 tick 不并发、卸载不回写。
 *
 * Code Logic（这个测试做什么）:
 *   用 fake timers + deferred Promise + visibilityState mock 覆盖立即执行、
 *   single-flight、hidden 暂停、visible 刷新、enabled、task 身份更新与 unmount。
 */

import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, renderHook } from '@testing-library/react';

import { useVisibilityPolling } from './useVisibilityPolling';

/**
 * Business Logic（为什么需要这个函数）:
 *   异步 poll task 测试需要手动 resolve/reject，才能卡住 in-flight 窗口。
 *
 * Code Logic（这个函数做什么）:
 *   返回 promise 与 resolve/reject 控制器。
 */
function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

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

beforeEach(() => {
  vi.useFakeTimers();
  setVisibilityState('visible');
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  setVisibilityState('visible');
});

describe('useVisibilityPolling', () => {
  test('runs immediately by default on mount', async () => {
    const task = vi.fn(async () => undefined);

    renderHook(() => useVisibilityPolling(task, { intervalMs: 3000 }));

    await act(async () => {
      await Promise.resolve();
    });

    expect(task).toHaveBeenCalledTimes(1);
  });

  test('does not start overlapping ticks while a deferred task is pending', async () => {
    const first = deferred<void>();
    const task = vi.fn(() => first.promise);

    renderHook(() => useVisibilityPolling(task, { intervalMs: 3000 }));

    await act(async () => {
      await Promise.resolve();
    });
    expect(task).toHaveBeenCalledTimes(1);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(9000);
    });
    expect(task).toHaveBeenCalledTimes(1);

    await act(async () => {
      first.resolve();
      await first.promise;
    });
  });

  test('pauses polling while document is hidden', async () => {
    const task = vi.fn(async () => undefined);

    renderHook(() =>
      useVisibilityPolling(task, { intervalMs: 1000, runImmediately: false }),
    );

    await act(async () => {
      setVisibilityState('hidden');
      await vi.advanceTimersByTimeAsync(5000);
    });

    expect(task).not.toHaveBeenCalled();
  });

  test('runs once immediately when becoming visible if refreshOnVisible', async () => {
    const task = vi.fn(async () => undefined);

    renderHook(() =>
      useVisibilityPolling(task, {
        intervalMs: 5000,
        runImmediately: false,
        refreshOnVisible: true,
      }),
    );

    await act(async () => {
      setVisibilityState('hidden');
      await Promise.resolve();
    });
    expect(task).not.toHaveBeenCalled();

    await act(async () => {
      setVisibilityState('visible');
      await Promise.resolve();
    });
    expect(task).toHaveBeenCalledTimes(1);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(4000);
    });
    expect(task).toHaveBeenCalledTimes(1);
  });

  test('enabled=false stops immediate and interval polling', async () => {
    const task = vi.fn(async () => undefined);

    renderHook(() =>
      useVisibilityPolling(task, {
        intervalMs: 1000,
        enabled: false,
      }),
    );

    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000);
      await Promise.resolve();
    });

    expect(task).not.toHaveBeenCalled();
  });

  test('updates task identity without resetting the interval timer', async () => {
    const task1 = vi.fn(async () => undefined);
    const task2 = vi.fn(async () => undefined);

    const { rerender } = renderHook(
      ({ task }) =>
        useVisibilityPolling(task, {
          intervalMs: 3000,
          runImmediately: false,
        }),
      { initialProps: { task: task1 } },
    );

    await act(async () => {
      await vi.advanceTimersByTimeAsync(2000);
    });
    expect(task1).not.toHaveBeenCalled();
    expect(task2).not.toHaveBeenCalled();

    rerender({ task: task2 });

    await act(async () => {
      await vi.advanceTimersByTimeAsync(1000);
    });

    expect(task1).not.toHaveBeenCalled();
    expect(task2).toHaveBeenCalledTimes(1);
  });

  test('does not write state after unmount', async () => {
    const pending = deferred<void>();
    const task = vi.fn(() => pending.promise);
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => undefined);

    const { result, unmount } = renderHook(() =>
      useVisibilityPolling(task, { intervalMs: 3000 }),
    );

    await act(async () => {
      await Promise.resolve();
    });
    expect(result.current.inFlight).toBe(true);

    unmount();

    await act(async () => {
      pending.resolve();
      await pending.promise;
      await Promise.resolve();
    });

    const reactUnmountWarnings = consoleError.mock.calls.filter((args) =>
      args.some(
        (arg) =>
          typeof arg === 'string' &&
          (arg.includes('unmounted') || arg.includes('not mounted')),
      ),
    );
    expect(reactUnmountWarnings).toHaveLength(0);
    consoleError.mockRestore();
  });

  test('runNow returns the same in-flight promise and does not swallow errors', async () => {
    const pending = deferred<void>();
    const task = vi.fn(() => pending.promise);

    const { result } = renderHook(() =>
      useVisibilityPolling(task, {
        intervalMs: 10_000,
        runImmediately: false,
      }),
    );

    let first!: Promise<void>;
    let second!: Promise<void>;
    await act(async () => {
      first = result.current.runNow();
      second = result.current.runNow();
    });

    expect(second).toBe(first);
    expect(task).toHaveBeenCalledTimes(1);
    expect(result.current.inFlight).toBe(true);

    await act(async () => {
      pending.reject(new Error('boom'));
      await expect(first).rejects.toThrow('boom');
    });

    expect(result.current.inFlight).toBe(false);
  });

  test('force runNow after in-flight poll executes a second task run', async () => {
    const first = deferred<void>();
    const second = deferred<void>();
    let call = 0;
    const task = vi.fn(() => {
      call += 1;
      return call === 1 ? first.promise : second.promise;
    });

    const { result } = renderHook(() =>
      useVisibilityPolling(task, {
        intervalMs: 10_000,
        runImmediately: false,
      }),
    );

    let pollPromise!: Promise<void>;
    let forcePromise!: Promise<void>;
    await act(async () => {
      pollPromise = result.current.runNow();
      forcePromise = result.current.runNow({ force: true });
    });

    expect(task).toHaveBeenCalledTimes(1);
    expect(forcePromise).toBe(pollPromise);

    await act(async () => {
      first.resolve();
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(task).toHaveBeenCalledTimes(2);
    expect(result.current.inFlight).toBe(true);

    await act(async () => {
      second.resolve();
      await forcePromise;
    });

    expect(result.current.inFlight).toBe(false);
    expect(task).toHaveBeenCalledTimes(2);
  });

  test('plain runNow still joins in-flight without forcing a second run', async () => {
    const pending = deferred<void>();
    const task = vi.fn(() => pending.promise);

    const { result } = renderHook(() =>
      useVisibilityPolling(task, {
        intervalMs: 10_000,
        runImmediately: false,
      }),
    );

    let first!: Promise<void>;
    let second!: Promise<void>;
    await act(async () => {
      first = result.current.runNow();
      second = result.current.runNow();
    });

    expect(task).toHaveBeenCalledTimes(1);
    expect(second).toBe(first);

    await act(async () => {
      pending.resolve();
      await first;
      await Promise.resolve();
    });

    expect(task).toHaveBeenCalledTimes(1);
  });

  test('interval ticks swallow rejections so they stay unhandled-safe', async () => {
    const task = vi
      .fn()
      .mockRejectedValueOnce(new Error('interval boom'))
      .mockResolvedValue(undefined);

    const unhandled: unknown[] = [];
    const onUnhandled = (reason: unknown) => {
      unhandled.push(reason);
    };
    process.on('unhandledRejection', onUnhandled);

    try {
      renderHook(() =>
        useVisibilityPolling(task, {
          intervalMs: 1000,
          runImmediately: false,
        }),
      );

      await act(async () => {
        await vi.advanceTimersByTimeAsync(1000);
        await Promise.resolve();
        await Promise.resolve();
      });

      expect(task).toHaveBeenCalledTimes(1);
      expect(unhandled).toHaveLength(0);
    } finally {
      process.off('unhandledRejection', onUnhandled);
    }
  });
});
