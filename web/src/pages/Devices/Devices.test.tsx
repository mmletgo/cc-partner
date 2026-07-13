// @vitest-environment jsdom
/**
 * Devices 页面轮询合同测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   设备列表迁移到 useVisibilityPolling 后，必须保证隐藏不请求、可见立即刷新、
 *   重叠 tick 不并发、刷新失败保留旧数据。
 *
 * Code Logic（这个测试做什么）:
 *   mock devices/ssh API，使用真实 useVisibilityPolling + fake timers/visibilityState
 *   覆盖 hidden 暂停、visible 刷新、single-flight 与 stale 保留。
 */

import { afterEach, beforeAll, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, render, screen } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';

import i18n from '@/i18n';
import type { Device } from '@/lib/types';

const listDevicesMock = vi.fn();
const healthMock = vi.fn();
const listSshMock = vi.fn();
const getOsInfoMock = vi.fn();

vi.mock('@/api/devices', () => ({
  devicesApi: {
    list: (...args: unknown[]) => listDevicesMock(...args),
    health: (...args: unknown[]) => healthMock(...args),
  },
}));

vi.mock('@/api/ssh', () => ({
  sshApi: {
    list: (...args: unknown[]) => listSshMock(...args),
    getOsInfo: (...args: unknown[]) => getOsInfoMock(...args),
    upsert: vi.fn(),
    remove: vi.fn(),
    sync: vi.fn(),
  },
}));

import { Devices } from './Devices';

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

/**
 * Business Logic（为什么需要这个函数）:
 *   poll/render 异步链需要冲刷 microtask。
 *
 * Code Logic（这个函数做什么）:
 *   在 act 内多次 await Promise.resolve。
 */
async function flushMicrotasks(times = 10): Promise<void> {
  for (let i = 0; i < times; i += 1) {
    await act(async () => {
      await Promise.resolve();
    });
  }
}

const deviceA: Device = {
  id: 'device-a',
  name: 'MacBook Pro',
  address: '192.168.1.10',
  port: 62116,
  status: 'online',
};

/**
 * Business Logic（为什么需要这个函数）:
 *   契约测试需统一 i18n 挂载。
 *
 * Code Logic（这个函数做什么）:
 *   用 I18nextProvider 渲染 Devices 页面。
 */
function renderDevices() {
  return render(
    <I18nextProvider i18n={i18n}>
      <Devices />
    </I18nextProvider>,
  );
}

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

beforeEach(() => {
  vi.useFakeTimers();
  setVisibilityState('visible');
  listDevicesMock.mockReset();
  healthMock.mockReset();
  listSshMock.mockReset();
  getOsInfoMock.mockReset();

  listDevicesMock.mockResolvedValue([deviceA]);
  healthMock.mockResolvedValue({
    ok: true,
    device_id: 'self',
    device_name: 'This Mac',
    http_port: 62116,
    ts: Date.now(),
  });
  listSshMock.mockResolvedValue([]);
  getOsInfoMock.mockResolvedValue({ platform: 'mac', raw: 'darwin' });
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  setVisibilityState('visible');
});

describe('Devices visibility polling', () => {
  test('does not request device list while document is hidden', async () => {
    setVisibilityState('hidden');
    listDevicesMock.mockClear();
    renderDevices();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(15_000);
      await Promise.resolve();
    });

    expect(listDevicesMock).not.toHaveBeenCalled();
  });

  test('runs once immediately when becoming visible', async () => {
    setVisibilityState('hidden');
    renderDevices();
    expect(listDevicesMock).not.toHaveBeenCalled();

    await act(async () => {
      setVisibilityState('visible');
      await Promise.resolve();
      await Promise.resolve();
    });
    await flushMicrotasks();

    expect(listDevicesMock).toHaveBeenCalledTimes(1);
  });

  test('does not start overlapping device polls while deferred list is pending', async () => {
    const pending = deferred<Device[]>();
    listDevicesMock.mockReturnValueOnce(pending.promise);

    renderDevices();

    await act(async () => {
      await Promise.resolve();
    });
    expect(listDevicesMock).toHaveBeenCalledTimes(1);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(15_000);
    });
    expect(listDevicesMock).toHaveBeenCalledTimes(1);

    await act(async () => {
      pending.resolve([deviceA]);
      await pending.promise;
      await Promise.resolve();
    });
  });

  test('preserves stale devices after later list failure', async () => {
    listDevicesMock
      .mockResolvedValueOnce([deviceA])
      .mockRejectedValueOnce(new Error('devices down'));

    renderDevices();
    await flushMicrotasks();

    expect(screen.getByText('192.168.1.10')).toBeTruthy();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000);
      await Promise.resolve();
      await Promise.resolve();
    });
    await flushMicrotasks();

    expect(screen.getByRole('alert')).toBeTruthy();
    expect(screen.getByText('192.168.1.10')).toBeTruthy();
  });
});
