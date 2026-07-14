/**
 * pendingWrites 注册表单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   GUI 关闭依赖 register/unregister/flushAll 语义正确，否则会丢数据或误阻断退出。
 *
 * Code Logic（这个测试做什么）:
 *   覆盖登记 flush、unregister 移除、flushAll 聚合失败。
 */

import { describe, expect, test, vi } from 'vitest';

import {
  PendingWriteFlushError,
  createPendingWriteRegistry,
} from './pendingWrites';

/**
 * Business Logic（为什么需要这个函数）:
 *   测试需要可控 settle 的异步 flush。
 *
 * Code Logic（这个函数做什么）:
 *   返回 promise 与 resolve/reject。
 */
function deferred<T>(): {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (reason?: unknown) => void;
} {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

describe('pendingWrites registry', () => {
  test('flushAll runs all registered writers', async () => {
    const registry = createPendingWriteRegistry();
    const a = vi.fn(async () => undefined);
    const b = vi.fn(async () => undefined);
    registry.register('a', a);
    registry.register('b', b);

    await registry.flushAll();

    expect(a).toHaveBeenCalledTimes(1);
    expect(b).toHaveBeenCalledTimes(1);
  });

  test('unregister removes a writer so flushAll skips it', async () => {
    const registry = createPendingWriteRegistry();
    const keep = vi.fn(async () => undefined);
    const drop = vi.fn(async () => undefined);
    registry.register('keep', keep);
    const unregister = registry.register('drop', drop);
    unregister();

    await registry.flushAll();

    expect(keep).toHaveBeenCalledTimes(1);
    expect(drop).not.toHaveBeenCalled();
  });

  test('unregister is a no-op after the same id is re-registered', async () => {
    const registry = createPendingWriteRegistry();
    const first = vi.fn(async () => undefined);
    const second = vi.fn(async () => undefined);
    const unregisterFirst = registry.register('writer', first);
    registry.register('writer', second);
    unregisterFirst();

    await registry.flushAll();

    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledTimes(1);
  });

  test('flushAll aggregates failures from multiple writers', async () => {
    const registry = createPendingWriteRegistry();
    registry.register('ok', async () => undefined);
    registry.register('bad-a', async () => {
      throw new Error('save A failed');
    });
    registry.register('bad-b', async () => {
      throw new Error('save B failed');
    });

    await expect(registry.flushAll()).rejects.toBeInstanceOf(PendingWriteFlushError);
    try {
      await registry.flushAll();
    } catch (error) {
      expect(error).toBeInstanceOf(PendingWriteFlushError);
      const flushError = error as PendingWriteFlushError;
      expect(flushError.errors).toHaveLength(2);
      expect(flushError.message).toContain('save A failed');
      expect(flushError.message).toContain('save B failed');
    }
  });

  test('flushAll waits for in-flight writers', async () => {
    const registry = createPendingWriteRegistry();
    const gate = deferred<void>();
    const flush = vi.fn(() => gate.promise);
    registry.register('slow', flush);

    let settled = false;
    const all = registry.flushAll().then(() => {
      settled = true;
    });

    await Promise.resolve();
    expect(flush).toHaveBeenCalledTimes(1);
    expect(settled).toBe(false);

    gate.resolve();
    await all;
    expect(settled).toBe(true);
  });
});
