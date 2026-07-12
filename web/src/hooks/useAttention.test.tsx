// @vitest-environment jsdom
/**
 * AttentionProvider 异步行为单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   request sequencing、unmount ignore、stale-on-refresh-failure、可见轮询是 Inbox 正确性核心。
 *
 * Code Logic（这个测试做什么）:
 *   用 deferred Promise + fake timers 覆盖并发请求、失败态、focus/visibility/interval。
 */

import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';

import type { AttentionSnapshot } from '@/lib/types';
import { ATTENTION_POLL_INTERVAL_MS, AttentionProvider } from './useAttention';
import { useAttention } from './attentionContext';

/**
 * Business Logic（为什么需要这个函数）:
 *   异步 loader 测试需要手动 resolve/reject。
 *
 * Code Logic（这个函数做什么）:
 *   返回 promise 与 resolve/reject 控制器。
 */
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Provider 测试需要最小合法快照区分不同请求。
 *
 * Code Logic（这个函数做什么）:
 *   返回可覆盖 generatedAt/total 的 AttentionSnapshot。
 */
function buildSnapshot(
  overrides: Partial<AttentionSnapshot> & {
    total?: number;
  } = {},
): AttentionSnapshot {
  const total = overrides.total ?? overrides.counts?.total ?? 0;
  return {
    generatedAt: overrides.generatedAt ?? '2026-07-11T10:00:00.000Z',
    counts: overrides.counts ?? {
      total,
      decision: total,
      blocked: 0,
      environment: 0,
    },
    items: overrides.items ?? [],
  };
}

/**
 * Business Logic（为什么需要这个组件）:
 *   renderHook 之外用简单探针读取 context 字段，便于 query DOM。
 *
 * Code Logic（这个组件做什么）:
 *   渲染 loading/refreshing/stale/error/total/lastSucceededAt。
 */
function AttentionProbe() {
  const {
    snapshot,
    loading,
    refreshing,
    stale,
    error,
    lastSucceededAt,
    refresh,
  } = useAttention();

  return (
    <div>
      <div data-testid="loading">{String(loading)}</div>
      <div data-testid="refreshing">{String(refreshing)}</div>
      <div data-testid="stale">{String(stale)}</div>
      <div data-testid="error">{error?.message ?? ''}</div>
      <div data-testid="total">{snapshot ? String(snapshot.counts.total) : 'null'}</div>
      <div data-testid="generatedAt">{snapshot?.generatedAt ?? ''}</div>
      <div data-testid="lastSucceededAt">{lastSucceededAt ?? ''}</div>
      <button type="button" onClick={() => void refresh()}>
        refresh
      </button>
    </div>
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   每个用例需要挂载 Provider + Probe。
 *
 * Code Logic（这个函数做什么）:
 *   render AttentionProvider，注入 loadSnapshot。
 */
function renderProvider(loadSnapshot: () => Promise<AttentionSnapshot>) {
  return render(
    <AttentionProvider loadSnapshot={loadSnapshot}>
      <AttentionProbe />
    </AttentionProvider>,
  );
}

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  vi.setSystemTime(new Date('2026-07-11T12:00:00.000Z'));
  Object.defineProperty(document, 'visibilityState', {
    configurable: true,
    get: () => 'visible',
  });
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe('AttentionProvider', () => {
  test('loads snapshot on mount', async () => {
    const loadSnapshot = vi.fn(async () => buildSnapshot({ total: 2, generatedAt: 't1' }));
    renderProvider(loadSnapshot);

    await waitFor(() => {
      expect(screen.getByTestId('loading').textContent).toBe('false');
    });
    expect(loadSnapshot).toHaveBeenCalled();
    expect(screen.getByTestId('total').textContent).toBe('2');
    expect(screen.getByTestId('generatedAt').textContent).toBe('t1');
    expect(screen.getByTestId('lastSucceededAt').textContent).toBe('2026-07-11T12:00:00.000Z');
  });

  test('first load failure keeps null total source', async () => {
    const loadSnapshot = vi.fn(async () => {
      throw new Error('boom');
    });
    renderProvider(loadSnapshot);

    await waitFor(() => {
      expect(screen.getByTestId('loading').textContent).toBe('false');
    });
    expect(screen.getByTestId('total').textContent).toBe('null');
    expect(screen.getByTestId('stale').textContent).toBe('false');
    expect(screen.getByTestId('error').textContent).toBe('boom');
  });

  test('request 2 beats late request 1', async () => {
    const first = deferred<AttentionSnapshot>();
    const second = deferred<AttentionSnapshot>();
    const loadSnapshot = vi
      .fn()
      .mockImplementationOnce(() => first.promise)
      .mockImplementationOnce(() => second.promise);

    renderProvider(loadSnapshot);

    await act(async () => {
      screen.getByRole('button', { name: 'refresh' }).click();
      await Promise.resolve();
    });

    await act(async () => {
      first.resolve(buildSnapshot({ total: 1, generatedAt: 'old' }));
      await Promise.resolve();
    });
    // 旧请求不得写入。
    expect(screen.getByTestId('total').textContent).toBe('null');

    await act(async () => {
      second.resolve(buildSnapshot({ total: 5, generatedAt: 'new' }));
      await Promise.resolve();
    });

    await waitFor(() => {
      expect(screen.getByTestId('total').textContent).toBe('5');
    });
    expect(screen.getByTestId('generatedAt').textContent).toBe('new');
  });

  test('unmount ignores late resolution', async () => {
    const pending = deferred<AttentionSnapshot>();
    const loadSnapshot = vi.fn(() => pending.promise);
    const view = renderProvider(loadSnapshot);

    view.unmount();

    await act(async () => {
      pending.resolve(buildSnapshot({ total: 9, generatedAt: 'late' }));
      await Promise.resolve();
    });

    // 已卸载，不应抛错；重新挂载后不应看到旧写入（新实例重新 load）。
    const next = deferred<AttentionSnapshot>();
    const load2 = vi.fn(() => next.promise);
    renderProvider(load2);
    expect(screen.getByTestId('total').textContent).toBe('null');
  });

  test('refresh failure keeps snapshot and marks stale; success clears stale', async () => {
    const loadSnapshot = vi
      .fn()
      .mockResolvedValueOnce(buildSnapshot({ total: 3, generatedAt: 'ok-1' }))
      .mockRejectedValueOnce(new Error('refresh failed'))
      .mockResolvedValueOnce(buildSnapshot({ total: 4, generatedAt: 'ok-2' }));

    renderProvider(loadSnapshot);

    await waitFor(() => {
      expect(screen.getByTestId('total').textContent).toBe('3');
    });

    await act(async () => {
      screen.getByRole('button', { name: 'refresh' }).click();
      await Promise.resolve();
    });

    await waitFor(() => {
      expect(screen.getByTestId('stale').textContent).toBe('true');
    });
    expect(screen.getByTestId('total').textContent).toBe('3');
    expect(screen.getByTestId('error').textContent).toBe('refresh failed');

    await act(async () => {
      screen.getByRole('button', { name: 'refresh' }).click();
      await Promise.resolve();
    });

    await waitFor(() => {
      expect(screen.getByTestId('stale').textContent).toBe('false');
    });
    expect(screen.getByTestId('total').textContent).toBe('4');
    expect(screen.getByTestId('error').textContent).toBe('');
  });

  test('invalidation event triggers refresh without waiting for poll', async () => {
    const { requestAttentionInvalidation } = await import('./attentionInvalidation');
    const loadSnapshot = vi
      .fn()
      .mockResolvedValueOnce(buildSnapshot({ total: 2, generatedAt: 'first' }))
      .mockResolvedValueOnce(buildSnapshot({ total: 0, generatedAt: 'after-action' }));

    renderProvider(loadSnapshot);

    await waitFor(() => {
      expect(screen.getByTestId('total').textContent).toBe('2');
    });
    const callsAfterMount = loadSnapshot.mock.calls.length;

    await act(async () => {
      requestAttentionInvalidation();
      await Promise.resolve();
    });

    await waitFor(() => {
      expect(screen.getByTestId('total').textContent).toBe('0');
    });
    expect(loadSnapshot.mock.calls.length).toBeGreaterThan(callsAfterMount);
  });

  test('hidden document pauses 10s interval; visible and focus trigger loads', async () => {
    let visibility: DocumentVisibilityState = 'visible';
    Object.defineProperty(document, 'visibilityState', {
      configurable: true,
      get: () => visibility,
    });

    const loadSnapshot = vi.fn(async () => buildSnapshot({ total: 1, generatedAt: 'tick' }));
    renderProvider(loadSnapshot);

    await waitFor(() => {
      expect(loadSnapshot.mock.calls.length).toBeGreaterThanOrEqual(1);
    });
    const afterMount = loadSnapshot.mock.calls.length;

    await act(async () => {
      await vi.advanceTimersByTimeAsync(ATTENTION_POLL_INTERVAL_MS);
    });
    expect(loadSnapshot.mock.calls.length).toBeGreaterThan(afterMount);
    const afterVisiblePoll = loadSnapshot.mock.calls.length;

    visibility = 'hidden';
    await act(async () => {
      document.dispatchEvent(new Event('visibilitychange'));
      await Promise.resolve();
    });

    await act(async () => {
      await vi.advanceTimersByTimeAsync(ATTENTION_POLL_INTERVAL_MS * 2);
    });
    // hidden 后 interval 应暂停，不再因时间推进增加调用（visibilitychange 本身可能触发一次 visible 检查但当前是 hidden 不 load）。
    expect(loadSnapshot.mock.calls.length).toBe(afterVisiblePoll);

    visibility = 'visible';
    await act(async () => {
      document.dispatchEvent(new Event('visibilitychange'));
      await Promise.resolve();
    });
    await waitFor(() => {
      expect(loadSnapshot.mock.calls.length).toBeGreaterThan(afterVisiblePoll);
    });
    const afterVisibleAgain = loadSnapshot.mock.calls.length;

    await act(async () => {
      window.dispatchEvent(new Event('focus'));
      await Promise.resolve();
    });
    await waitFor(() => {
      expect(loadSnapshot.mock.calls.length).toBeGreaterThan(afterVisibleAgain);
    });
  });

  test('slow load longer than poll interval is not starved by 10s timer', async () => {
    const slow = deferred<AttentionSnapshot>();
    let callCount = 0;
    const loadSnapshot = vi.fn(() => {
      callCount += 1;
      if (callCount === 1) {
        return slow.promise;
      }
      return Promise.resolve(buildSnapshot({ total: 7, generatedAt: 'after-slow' }));
    });

    renderProvider(loadSnapshot);

    // 首个慢请求已启动。
    await act(async () => {
      await Promise.resolve();
    });
    expect(loadSnapshot).toHaveBeenCalledTimes(1);
    expect(screen.getByTestId('loading').textContent).toBe('true');

    // 10s 轮询触发：in-flight 时只 coalesce，不启动第二请求。
    await act(async () => {
      await vi.advanceTimersByTimeAsync(ATTENTION_POLL_INTERVAL_MS);
      await Promise.resolve();
    });
    expect(loadSnapshot).toHaveBeenCalledTimes(1);
    expect(screen.getByTestId('total').textContent).toBe('null');

    // 再等一轮仍不得丢弃唯一 in-flight 请求。
    await act(async () => {
      await vi.advanceTimersByTimeAsync(ATTENTION_POLL_INTERVAL_MS);
      await Promise.resolve();
    });
    expect(loadSnapshot).toHaveBeenCalledTimes(1);

    // 慢请求完成后必须写入快照；若轮询有 pending，可再跑一次但不永久 loading。
    await act(async () => {
      slow.resolve(buildSnapshot({ total: 3, generatedAt: 'slow-ok' }));
      await Promise.resolve();
      await Promise.resolve();
    });

    await waitFor(() => {
      expect(screen.getByTestId('loading').textContent).toBe('false');
    });
    // 首个慢响应或随后的 coalesced 刷新都应让 total 变为非 null。
    await waitFor(() => {
      expect(screen.getByTestId('total').textContent).not.toBe('null');
    });
    const total = screen.getByTestId('total').textContent;
    expect(total === '3' || total === '7').toBe(true);
  });

  test('useAttention outside provider throws', () => {
    function Broken() {
      useAttention();
      return null;
    }
    expect(() => render(<Broken />)).toThrow(/AttentionProvider/);
  });
});
