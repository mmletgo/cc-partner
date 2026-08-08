// @vitest-environment jsdom
/**
 * Health 页面网络轮询合同测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   Health 5s 刷新迁移到 useVisibilityPolling 后，后台不得空转，恢复可见立即刷新，
 *   重叠 tick 不并发；刷新失败保留已有 status。HealthOverlay 本地倒计时不在本测试范围。
 *
 * Code Logic（这个测试做什么）:
 *   mock healthApi，用真实 useVisibilityPolling + fake timers/visibilityState
 *   覆盖 hidden 暂停、visible 刷新、single-flight 与 stale 保留。
 */

import { afterEach, beforeAll, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, render, screen } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';
import { MemoryRouter } from 'react-router-dom';

import i18n from '@/i18n';
import type { HealthStatus } from '@/lib/types';

const getStatusMock = vi.fn();
const getStatsMock = vi.fn();
const getDetailMock = vi.fn();
const getConfigMock = vi.fn();
const getHabitStatsMock = vi.fn();

vi.mock('@/api/health', () => ({
  healthApi: {
    getStatus: (...args: unknown[]) => getStatusMock(...args),
    getStats: (...args: unknown[]) => getStatsMock(...args),
    getDetail: (...args: unknown[]) => getDetailMock(...args),
    getConfig: (...args: unknown[]) => getConfigMock(...args),
    getHabitStats: (...args: unknown[]) => getHabitStatsMock(...args),
    toggleEnabled: vi.fn(),
    togglePaused: vi.fn(),
    addWaterManual: vi.fn(),
  },
}));

import { Health } from './Health';

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
async function flushMicrotasks(times = 12): Promise<void> {
  for (let i = 0; i < times; i += 1) {
    await act(async () => {
      await Promise.resolve();
    });
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   多个用例共享最小合法 HealthStatus。
 *
 * Code Logic（这个函数做什么）:
 *   返回可覆盖字段的 HealthStatus。
 */
function buildStatus(overrides: Partial<HealthStatus> = {}): HealthStatus {
  return {
    enabled: true,
    paused: false,
    phase: 'working',
    windowStartTs: Math.floor(Date.now() / 1000) - 60,
    workWindowSeconds: 1800,
    breakSeconds: 300,
    snoozeUntil: null,
    overlayRestEndTs: null,
    ...overrides,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   契约测试需统一 i18n + router 挂载。
 *
 * Code Logic（这个函数做什么）:
 *   用 MemoryRouter + I18nextProvider 渲染 Health 页面。
 */
function renderHealth() {
  return render(
    <MemoryRouter>
      <I18nextProvider i18n={i18n}>
        <Health />
      </I18nextProvider>
    </MemoryRouter>,
  );
}

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

beforeEach(() => {
  vi.useFakeTimers();
  setVisibilityState('visible');
  getStatusMock.mockReset();
  getStatsMock.mockReset();
  getDetailMock.mockReset();
  getConfigMock.mockReset();
  getHabitStatsMock.mockReset();

  getStatusMock.mockResolvedValue(buildStatus());
  getStatsMock.mockResolvedValue({ activeMinutes: 10, idleMinutes: 5 });
  getDetailMock.mockResolvedValue({ appUsage: [], hourly: Array.from({ length: 24 }, () => 0) });
  getConfigMock.mockResolvedValue({
    enabled: true,
    workWindowSeconds: 1800,
    breakSeconds: 300,
    recordWindowTitle: true,
    retainDays: 30,
    notifyEnabled: true,
    dndStart: null,
    dndEnd: null,
    waterEnabled: true,
    waterIntervalSeconds: 3600,
    reminderFullscreen: true,
  });
  getHabitStatsMock.mockResolvedValue({
    todayWaterCount: 0,
    waterDailyCounts: [0, 0, 0, 0, 0, 0, 0],
    lastWaterTs: null,
    todayRestCount: 0,
    todayRestTotalSeconds: 0,
    todayReminderCount: 0,
    restDailyCounts: [0, 0, 0, 0, 0, 0, 0],
  });
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  setVisibilityState('visible');
});

describe('Health visibility polling', () => {
  test('does not request health status while document is hidden', async () => {
    setVisibilityState('hidden');
    getStatusMock.mockClear();
    renderHealth();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(15_000);
      await Promise.resolve();
    });

    expect(getStatusMock).not.toHaveBeenCalled();
  });

  test('runs once immediately when becoming visible', async () => {
    setVisibilityState('hidden');
    renderHealth();
    expect(getStatusMock).not.toHaveBeenCalled();

    await act(async () => {
      setVisibilityState('visible');
      await Promise.resolve();
      await Promise.resolve();
    });
    await flushMicrotasks();

    expect(getStatusMock).toHaveBeenCalledTimes(1);
  });

  test('does not start overlapping health polls while deferred status is pending', async () => {
    const pending = deferred<HealthStatus>();
    getStatusMock.mockReturnValueOnce(pending.promise);

    renderHealth();

    await act(async () => {
      await Promise.resolve();
    });
    expect(getStatusMock).toHaveBeenCalledTimes(1);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(15_000);
    });
    expect(getStatusMock).toHaveBeenCalledTimes(1);

    await act(async () => {
      pending.resolve(buildStatus());
      await pending.promise;
      await Promise.resolve();
    });
  });

  test('preserves stale status after later refresh failure', async () => {
    getStatusMock
      .mockResolvedValueOnce(buildStatus({ enabled: true, phase: 'working' }))
      .mockRejectedValueOnce(new Error('health down'));

    renderHealth();
    await flushMicrotasks();

    expect(screen.getByText('健康提醒')).toBeTruthy();

    const afterSuccess = getStatusMock.mock.calls.length;

    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000);
      await Promise.resolve();
      await Promise.resolve();
    });
    await flushMicrotasks();

    expect(getStatusMock.mock.calls.length).toBeGreaterThan(afterSuccess);
    expect(screen.getByText('健康提醒')).toBeTruthy();
    expect(screen.queryByText('加载中…')).toBeNull();
    expect(screen.getByTestId('health-stale-banner').textContent).toMatch(/刷新失败|保留/);
    expect(screen.getByRole('button', { name: '重试' })).toBeTruthy();
  });

  test('first load status failure shows error and retry instead of permanent loading', async () => {
    getStatusMock.mockRejectedValue(new Error('status unavailable'));

    renderHealth();
    await flushMicrotasks(20);

    expect(screen.queryByText('加载中…')).toBeNull();
    expect(screen.getByRole('alert').textContent).toMatch(/加载失败|重试|status unavailable|健康状态/);
    expect(screen.getByRole('button', { name: '重试' })).toBeTruthy();
  });
});
