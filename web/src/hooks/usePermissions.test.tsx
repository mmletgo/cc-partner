// @vitest-environment jsdom
/**
 * usePermissions 单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   权限引导不得永久「检查中」；首轮失败、重试、stale 保留、逐项请求与可见性轮询
 *   是 Welcome/Settings 的核心正确性合同。
 *
 * Code Logic（这个测试做什么）:
 *   mock configApi 与 notification helper；默认 real timers 覆盖状态机，
 *   仅 interval/visibility 用例启用 fake timers + act 冲刷。
 */

import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, renderHook } from '@testing-library/react';

import type { PermissionType, PermissionsStatus } from '@/lib/types';

const permissionsMock = vi.fn();
const requestPermissionMock = vi.fn();
const checkNotificationGrantedMock = vi.fn();
const requestNotificationPermissionMock = vi.fn();

vi.mock('@/api/config', () => ({
  configApi: {
    permissions: (...args: unknown[]) => permissionsMock(...args),
    requestPermission: (...args: unknown[]) => requestPermissionMock(...args),
  },
}));

vi.mock('@/lib/notification', () => ({
  checkNotificationGranted: (...args: unknown[]) => checkNotificationGrantedMock(...args),
  requestNotificationPermission: (...args: unknown[]) =>
    requestNotificationPermissionMock(...args),
}));

import { usePermissions } from './usePermissions';

/**
 * Business Logic（为什么需要这个函数）:
 *   异步权限 API 测试需要手动 resolve/reject 以卡住 in-flight 窗口。
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

/**
 * Business Logic（为什么需要这个函数）:
 *   多个用例共享最小合法权限 DTO。
 *
 * Code Logic（这个函数做什么）:
 *   返回可覆盖字段的 PermissionsStatus（TCC 部分；notification 由 check mock 合并）。
 */
function buildTccStatus(
  overrides: Partial<{
    screenCapture: boolean;
    accessibility: boolean;
    inputMonitoring: boolean;
  }> = {},
): Omit<PermissionsStatus, 'notification'> {
  return {
    screenCapture: { granted: overrides.screenCapture ?? false },
    accessibility: { granted: overrides.accessibility ?? false },
    inputMonitoring: { granted: overrides.inputMonitoring ?? false },
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   首轮 poll 是 async task，测试需要冲刷 microtask 直到状态稳定。
 *
 * Code Logic（这个函数做什么）:
 *   在 act 内多次 await Promise.resolve 推进 promise 链。
 */
async function flushMicrotasks(times = 8): Promise<void> {
  for (let i = 0; i < times; i += 1) {
    await act(async () => {
      await Promise.resolve();
    });
  }
}

beforeEach(() => {
  setVisibilityState('visible');
  permissionsMock.mockReset();
  requestPermissionMock.mockReset();
  checkNotificationGrantedMock.mockReset();
  requestNotificationPermissionMock.mockReset();
  checkNotificationGrantedMock.mockResolvedValue(false);
  requestPermissionMock.mockResolvedValue({ ok: true, requested: true, opened: true });
  requestNotificationPermissionMock.mockResolvedValue(undefined);
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  setVisibilityState('visible');
});

describe('usePermissions', () => {
  test('first failure ends loading with error and no permanent checking state', async () => {
    permissionsMock.mockRejectedValueOnce(new Error('perm down'));

    const { result } = renderHook(() => usePermissions());
    await flushMicrotasks();

    expect(result.current.loading).toBe(false);
    expect(result.current.status).toBeNull();
    expect(result.current.error).toBe('perm down');
    expect(result.current.refreshing).toBe(false);
  });

  test('retry after first failure succeeds and clears error', async () => {
    permissionsMock
      .mockRejectedValueOnce(new Error('perm down'))
      .mockResolvedValue(buildTccStatus({ screenCapture: true }));
    checkNotificationGrantedMock.mockResolvedValue(true);

    const { result } = renderHook(() => usePermissions());
    await flushMicrotasks();

    expect(result.current.loading).toBe(false);
    expect(result.current.error).toBe('perm down');

    await act(async () => {
      await result.current.refresh();
    });

    expect(result.current.error).toBeNull();
    expect(result.current.status?.screenCapture.granted).toBe(true);
    expect(result.current.status?.notification.granted).toBe(true);
    expect(result.current.loading).toBe(false);
  });

  test('later refresh failure preserves stale status and sets error', async () => {
    permissionsMock
      .mockResolvedValueOnce(buildTccStatus({ accessibility: true }))
      .mockRejectedValueOnce(new Error('stale refresh'));
    checkNotificationGrantedMock.mockResolvedValue(false);

    const { result } = renderHook(() => usePermissions());
    await flushMicrotasks();

    expect(result.current.status?.accessibility.granted).toBe(true);

    await act(async () => {
      await result.current.refresh();
    });

    expect(result.current.status?.accessibility.granted).toBe(true);
    expect(result.current.error).toBe('stale refresh');
  });

  test('request asks only the given type and refreshes status', async () => {
    permissionsMock
      .mockResolvedValueOnce(buildTccStatus())
      .mockResolvedValueOnce(buildTccStatus({ screenCapture: true }));
    checkNotificationGrantedMock.mockResolvedValue(false);

    const { result } = renderHook(() => usePermissions());
    await flushMicrotasks();

    expect(result.current.status).not.toBeNull();

    await act(async () => {
      await result.current.request('screenCapture', false);
    });

    expect(requestPermissionMock).toHaveBeenCalledTimes(1);
    expect(requestPermissionMock).toHaveBeenCalledWith('screenCapture', false);
    expect(requestNotificationPermissionMock).not.toHaveBeenCalled();
    expect(result.current.status?.screenCapture.granted).toBe(true);
    expect(result.current.requesting.has('screenCapture')).toBe(false);
  });

  test('request error surfaces error and clears requesting flag', async () => {
    permissionsMock.mockResolvedValue(buildTccStatus());
    checkNotificationGrantedMock.mockResolvedValue(false);
    requestPermissionMock.mockRejectedValueOnce(new Error('request boom'));

    const { result } = renderHook(() => usePermissions());
    await flushMicrotasks();

    await act(async () => {
      await expect(result.current.request('accessibility')).rejects.toThrow('request boom');
    });

    expect(result.current.error).toBe('request boom');
    expect(result.current.requesting.has('accessibility')).toBe(false);
  });

  test('duplicate same-type request reuses in-flight promise', async () => {
    permissionsMock.mockResolvedValue(buildTccStatus());
    checkNotificationGrantedMock.mockResolvedValue(false);
    const pending = deferred<{ ok: boolean; requested: boolean; opened: boolean }>();
    requestPermissionMock.mockReturnValueOnce(pending.promise);

    const { result } = renderHook(() => usePermissions());
    await flushMicrotasks();

    let first!: Promise<void>;
    let second!: Promise<void>;
    await act(async () => {
      first = result.current.request('inputMonitoring');
      second = result.current.request('inputMonitoring');
    });

    expect(second).toBe(first);
    expect(requestPermissionMock).toHaveBeenCalledTimes(1);
    expect(result.current.requesting.has('inputMonitoring')).toBe(true);

    await act(async () => {
      pending.resolve({ ok: true, requested: true, opened: true });
      await first;
    });

    expect(result.current.requesting.has('inputMonitoring')).toBe(false);
  });

  test('allRequiredGranted includes notification', async () => {
    permissionsMock.mockResolvedValue(
      buildTccStatus({
        screenCapture: true,
        accessibility: true,
        inputMonitoring: true,
      }),
    );
    checkNotificationGrantedMock.mockResolvedValue(false);

    const { result } = renderHook(() => usePermissions());
    await flushMicrotasks();

    expect(result.current.allRequiredGranted).toBe(false);
    expect(result.current.allGranted).toBe(false);
    expect(result.current.status?.notification.granted).toBe(false);

    checkNotificationGrantedMock.mockResolvedValue(true);
    await act(async () => {
      await result.current.refresh();
    });
    expect(result.current.allRequiredGranted).toBe(true);
    expect(result.current.allGranted).toBe(true);
    expect(result.current.status?.notification.granted).toBe(true);
  });

  test('pauses polling while hidden and refreshes once when visible', async () => {
    vi.useFakeTimers();
    permissionsMock.mockResolvedValue(buildTccStatus());
    checkNotificationGrantedMock.mockResolvedValue(false);

    renderHook(() => usePermissions());
    await flushMicrotasks();

    const afterMount = permissionsMock.mock.calls.length;
    expect(afterMount).toBeGreaterThanOrEqual(1);

    await act(async () => {
      setVisibilityState('hidden');
      await vi.advanceTimersByTimeAsync(10_000);
    });

    expect(permissionsMock.mock.calls.length).toBe(afterMount);

    await act(async () => {
      setVisibilityState('visible');
      await Promise.resolve();
      await Promise.resolve();
    });
    await flushMicrotasks();

    expect(permissionsMock.mock.calls.length).toBe(afterMount + 1);
  });

  test('requestMissing requests missing types sequentially', async () => {
    permissionsMock.mockResolvedValue(buildTccStatus());
    checkNotificationGrantedMock.mockResolvedValue(false);

    const order: PermissionType[] = [];
    requestPermissionMock.mockImplementation(async (type: PermissionType) => {
      order.push(type);
      return { ok: true, requested: true, opened: true };
    });
    requestNotificationPermissionMock.mockImplementation(async () => {
      order.push('notification');
    });

    const { result } = renderHook(() => usePermissions());
    await flushMicrotasks();

    await act(async () => {
      await result.current.requestMissing();
    });

    expect(order).toEqual([
      'screenCapture',
      'accessibility',
      'inputMonitoring',
      'notification',
    ]);
  });

  test('stopWhenGranted disables further polling after required granted', async () => {
    vi.useFakeTimers();
    permissionsMock.mockResolvedValue(
      buildTccStatus({
        screenCapture: true,
        accessibility: true,
        inputMonitoring: true,
      }),
    );
    checkNotificationGrantedMock.mockResolvedValue(true);

    renderHook(() => usePermissions({ stopWhenGranted: true }));
    await flushMicrotasks();

    const afterMount = permissionsMock.mock.calls.length;

    await act(async () => {
      await vi.advanceTimersByTimeAsync(10_000);
    });

    expect(permissionsMock.mock.calls.length).toBe(afterMount);
  });
});
