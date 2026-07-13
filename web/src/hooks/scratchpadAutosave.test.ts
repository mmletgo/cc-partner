/**
 * 速记本 autosave 队列单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   debounce 合并、多页隔离、in-flight 续 flush、失败保留与重试是数据不丢的核心合同。
 *
 * Code Logic（这个测试做什么）:
 *   用 fake timers + deferred save 覆盖 schedule/flushPage/flushAll 状态机。
 */

import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';

import {
  SCRATCHPAD_AUTOSAVE_DELAY_MS,
  createScratchpadAutosaveQueue,
  type ScratchpadAutosaveSaveFn,
} from './scratchpadAutosave';

/**
 * Business Logic（为什么需要这个函数）:
 *   模拟慢速/可失败后端保存。
 *
 * Code Logic（这个函数做什么）:
 *   返回可控 Promise 的 deferred 对象。
 */
function deferred<T>(): {
  promise: Promise<T>;
  resolve: (value: T | PromiseLike<T>) => void;
  reject: (reason?: unknown) => void;
} {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

describe('createScratchpadAutosaveQueue', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  test('debounce coalesces multiple edits into one save with latest content', async () => {
    const save = vi.fn<ScratchpadAutosaveSaveFn>(async () => undefined);
    const queue = createScratchpadAutosaveQueue(save, { delayMs: 500 });

    queue.schedule('p1', 'a');
    queue.schedule('p1', 'ab');
    queue.schedule('p1', 'abc');

    expect(save).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(499);
    expect(save).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1);
    await Promise.resolve();

    expect(save).toHaveBeenCalledTimes(1);
    expect(save).toHaveBeenCalledWith('p1', 'abc');
    const snap = queue.getSnapshot().pages.p1;
    expect(snap?.pendingVersion).toBe(3);
    expect(snap?.savedVersion).toBe(3);
    expect(snap?.error).toBeNull();
    expect(snap?.inFlight).toBe(false);
  });

  test('independent pages schedule and save without blocking each other', async () => {
    const save = vi.fn<ScratchpadAutosaveSaveFn>(async () => undefined);
    const queue = createScratchpadAutosaveQueue(save, { delayMs: 500 });

    queue.schedule('p1', 'one');
    queue.schedule('p2', 'two');
    await vi.advanceTimersByTimeAsync(500);
    await Promise.resolve();

    expect(save).toHaveBeenCalledTimes(2);
    expect(save).toHaveBeenCalledWith('p1', 'one');
    expect(save).toHaveBeenCalledWith('p2', 'two');
  });

  test('only one in-flight save per page; edit during save causes second flush', async () => {
    const first = deferred<void>();
    const second = deferred<void>();
    let call = 0;
    const save = vi.fn<ScratchpadAutosaveSaveFn>(async (_pageId, content) => {
      call += 1;
      if (call === 1) {
        expect(content).toBe('v1');
        await first.promise;
        return;
      }
      if (call === 2) {
        expect(content).toBe('v2');
        await second.promise;
        return;
      }
      throw new Error(`unexpected save #${call}`);
    });
    const queue = createScratchpadAutosaveQueue(save, { delayMs: 500 });

    queue.schedule('p1', 'v1');
    await vi.advanceTimersByTimeAsync(500);
    await Promise.resolve();
    expect(save).toHaveBeenCalledTimes(1);
    expect(queue.getSnapshot().pages.p1?.inFlight).toBe(true);

    // 保存进行中再次编辑
    queue.schedule('p1', 'v2');
    await vi.advanceTimersByTimeAsync(500);
    await Promise.resolve();
    // 仍只有第一次 in-flight，不会并行第二次
    expect(save).toHaveBeenCalledTimes(1);

    first.resolve();
    // 让 microtask / 链式 flush 跑完
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();

    expect(save).toHaveBeenCalledTimes(2);
    expect(save).toHaveBeenLastCalledWith('p1', 'v2');

    second.resolve();
    await queue.flushPage('p1');
    const snap = queue.getSnapshot().pages.p1;
    expect(snap?.savedVersion).toBe(snap?.pendingVersion);
    expect(snap?.inFlight).toBe(false);
    expect(snap?.error).toBeNull();
  });

  test('failed save retains pending content and error; retry succeeds', async () => {
    let shouldFail = true;
    const save = vi.fn<ScratchpadAutosaveSaveFn>(async () => {
      if (shouldFail) {
        throw new Error('disk full');
      }
    });
    const queue = createScratchpadAutosaveQueue(save, { delayMs: 500 });

    queue.schedule('p1', 'draft');
    await vi.advanceTimersByTimeAsync(500);
    await Promise.resolve();
    await Promise.resolve();

    let snap = queue.getSnapshot().pages.p1;
    expect(save).toHaveBeenCalledTimes(1);
    expect(snap?.error).toBe('disk full');
    expect(snap?.content).toBe('draft');
    expect(snap?.pendingVersion).toBeGreaterThan(snap?.savedVersion ?? 0);
    expect(snap?.inFlight).toBe(false);

    shouldFail = false;
    await queue.flushPage('p1');

    snap = queue.getSnapshot().pages.p1;
    expect(save).toHaveBeenCalledTimes(2);
    expect(snap?.error).toBeNull();
    expect(snap?.savedVersion).toBe(snap?.pendingVersion);
    expect(snap?.content).toBe('draft');
  });

  test('flushAll aggregates failures across pages', async () => {
    const save = vi.fn<ScratchpadAutosaveSaveFn>(async (pageId) => {
      if (pageId === 'p1') throw new Error('fail-1');
      if (pageId === 'p2') throw new Error('fail-2');
    });
    const queue = createScratchpadAutosaveQueue(save, { delayMs: 500 });
    queue.schedule('p1', 'a');
    queue.schedule('p2', 'b');

    await expect(queue.flushAll()).rejects.toSatisfy((error: unknown) => {
      expect(error).toBeInstanceOf(AggregateError);
      const agg = error as AggregateError;
      expect(agg.errors).toHaveLength(2);
      expect(agg.message).toContain('fail-1');
      expect(agg.message).toContain('fail-2');
      return true;
    });

    expect(queue.getSnapshot().pages.p1?.error).toBe('fail-1');
    expect(queue.getSnapshot().pages.p2?.error).toBe('fail-2');
  });

  test('flushPage cancels debounce and saves immediately', async () => {
    const save = vi.fn<ScratchpadAutosaveSaveFn>(async () => undefined);
    const queue = createScratchpadAutosaveQueue(save, {
      delayMs: SCRATCHPAD_AUTOSAVE_DELAY_MS,
    });

    queue.schedule('p1', 'immediate');
    expect(save).not.toHaveBeenCalled();
    await queue.flushPage('p1');
    expect(save).toHaveBeenCalledTimes(1);
    expect(save).toHaveBeenCalledWith('p1', 'immediate');
  });

  test('subscribe notifies on schedule and after save settles', async () => {
    const save = vi.fn<ScratchpadAutosaveSaveFn>(async () => undefined);
    const queue = createScratchpadAutosaveQueue(save, { delayMs: 100 });
    const listener = vi.fn();
    const unsubscribe = queue.subscribe(listener);

    queue.schedule('p1', 'x');
    expect(listener).toHaveBeenCalled();
    const afterSchedule = listener.mock.calls.length;

    await vi.advanceTimersByTimeAsync(100);
    await Promise.resolve();
    await Promise.resolve();
    expect(listener.mock.calls.length).toBeGreaterThan(afterSchedule);

    unsubscribe();
    const callsAfterUnsub = listener.mock.calls.length;
    queue.schedule('p1', 'y');
    await vi.advanceTimersByTimeAsync(100);
    await Promise.resolve();
    expect(listener.mock.calls.length).toBe(callsAfterUnsub);
  });

  test('only clears error after successful latest-version save', async () => {
    const first = deferred<void>();
    let call = 0;
    const save = vi.fn<ScratchpadAutosaveSaveFn>(async (_pageId, content) => {
      call += 1;
      if (call === 1) {
        await first.promise;
        // first.promise reject 后这里不会执行成功路径
        return;
      }
      if (content === 'latest') return;
      throw new Error(`unexpected ${content}`);
    });
    const queue = createScratchpadAutosaveQueue(save, { delayMs: 50 });

    queue.schedule('p1', 'old');
    await vi.advanceTimersByTimeAsync(50);
    await Promise.resolve();
    queue.schedule('p1', 'latest');

    first.reject(new Error('stale-fail'));
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
    await queue.flushPage('p1');

    const snap = queue.getSnapshot().pages.p1;
    expect(save).toHaveBeenCalledWith('p1', 'latest');
    expect(snap?.error).toBeNull();
    expect(snap?.content).toBe('latest');
    expect(snap?.savedVersion).toBe(snap?.pendingVersion);
  });
});
