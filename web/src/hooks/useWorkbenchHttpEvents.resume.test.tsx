// @vitest-environment jsdom

import { afterEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, renderHook } from '@testing-library/react';
import { createWorkbenchTerminalBufferStore } from './workbenchTerminalBuffer';
import { useWorkbenchHttpEvents } from './useWorkbenchHttpEvents';

/**
 * Business Logic（为什么需要这个函数）:
 *   jsdom 默认 visibilityState 固定，测试需要模拟后台/前台切换。
 *
 * Code Logic（这个函数做什么）:
 *   覆盖 document.visibilityState 并派发 visibilitychange。
 */
function setVisibilityState(state: DocumentVisibilityState): void {
  Object.defineProperty(document, 'visibilityState', {
    configurable: true,
    get: () => state,
  });
  document.dispatchEvent(new Event('visibilitychange'));
}

describe('useWorkbenchHttpEvents resume reconnect', () => {
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   手机浏览器后台几分钟会冻结 setTimeout watchdog，半开 NDJSON 不会自己断开；
   *   回到前台必须立刻 abort 当前流并以 after 游标重连，不能再等 35s。
   *
   * Code Logic（这个测试做什么）:
   *   首连后写入 heartbeat 建立会话，切到 hidden 再 visible；断言第二次 fetch
   *   在 watchdog 到期前发生，且旧连接 signal 已 abort。
   */
  test('visibilitychange to visible aborts the half-open stream and reconnects immediately', async () => {
    vi.useFakeTimers();
    type StreamController = ReadableStreamDefaultController<Uint8Array>;
    const controllers: StreamController[] = [];
    const abortFlags: boolean[] = [];
    let fetchCount = 0;
    const encoder = new TextEncoder();

    const mockFetch = vi.fn((_input: RequestInfo | URL, init?: RequestInit) => {
      fetchCount += 1;
      const index = fetchCount - 1;
      abortFlags[index] = false;
      init?.signal?.addEventListener('abort', () => {
        abortFlags[index] = true;
      });
      const stream = new ReadableStream<Uint8Array>({
        start(controller) {
          controllers.push(controller);
          init?.signal?.addEventListener('abort', () => {
            try {
              controller.error(new DOMException('The operation was aborted.', 'AbortError'));
            } catch {
              // already closed
            }
          });
        },
      });
      return Promise.resolve({
        ok: true,
        body: stream,
      } as Response);
    });
    vi.stubGlobal('fetch', mockFetch);

    const store = createWorkbenchTerminalBufferStore();
    renderHook(() =>
      useWorkbenchHttpEvents({
        store,
        enabled: true,
        reconnectDelayMs: 2_000,
        watchdogMs: 35_000,
      }),
    );

    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(fetchCount).toBe(1);

    await act(async () => {
      controllers[0]?.enqueue(
        encoder.encode('{"type":"heartbeat","sentAt":"2026-08-24T00:00:00Z"}\n'),
      );
      await vi.advanceTimersByTimeAsync(0);
    });

    await act(async () => {
      setVisibilityState('hidden');
    });
    expect(fetchCount).toBe(1);

    await act(async () => {
      setVisibilityState('visible');
      await vi.advanceTimersByTimeAsync(0);
    });

    expect(fetchCount).toBe(2);
    expect(abortFlags[0]).toBe(true);
  });

  test('pageshow from bfcache also reconnects immediately', async () => {
    vi.useFakeTimers();
    let fetchCount = 0;
    const encoder = new TextEncoder();
    const controllers: ReadableStreamDefaultController<Uint8Array>[] = [];

    vi.stubGlobal(
      'fetch',
      vi.fn((_input: RequestInfo | URL, init?: RequestInit) => {
        fetchCount += 1;
        const stream = new ReadableStream<Uint8Array>({
          start(controller) {
            controllers.push(controller);
            init?.signal?.addEventListener('abort', () => {
              try {
                controller.error(new DOMException('The operation was aborted.', 'AbortError'));
              } catch {
                // already closed
              }
            });
          },
        });
        return Promise.resolve({
          ok: true,
          body: stream,
        } as Response);
      }),
    );

    const store = createWorkbenchTerminalBufferStore();
    renderHook(() =>
      useWorkbenchHttpEvents({
        store,
        enabled: true,
        reconnectDelayMs: 2_000,
        watchdogMs: 35_000,
      }),
    );

    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(fetchCount).toBe(1);
    await act(async () => {
      controllers[0]?.enqueue(
        encoder.encode('{"type":"heartbeat","sentAt":"2026-08-24T00:00:00Z"}\n'),
      );
      await vi.advanceTimersByTimeAsync(0);
    });

    await act(async () => {
      window.dispatchEvent(new Event('pageshow'));
      await vi.advanceTimersByTimeAsync(0);
    });

    expect(fetchCount).toBe(2);
  });

  test('reconnects after backgrounding even before the first heartbeat', async () => {
    vi.useFakeTimers();
    let fetchCount = 0;

    vi.stubGlobal(
      'fetch',
      vi.fn((_input: RequestInfo | URL, init?: RequestInit) => {
        fetchCount += 1;
        const stream = new ReadableStream<Uint8Array>({
          start(controller) {
            init?.signal?.addEventListener('abort', () => {
              try {
                controller.error(new DOMException('The operation was aborted.', 'AbortError'));
              } catch {
                // already closed
              }
            });
          },
        });
        return Promise.resolve({
          ok: true,
          body: stream,
        } as Response);
      }),
    );

    const store = createWorkbenchTerminalBufferStore();
    renderHook(() =>
      useWorkbenchHttpEvents({
        store,
        enabled: true,
        reconnectDelayMs: 2_000,
        watchdogMs: 35_000,
      }),
    );

    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(fetchCount).toBe(1);

    await act(async () => {
      setVisibilityState('hidden');
      setVisibilityState('visible');
      await vi.advanceTimersByTimeAsync(0);
    });

    expect(fetchCount).toBe(2);
  });
});
