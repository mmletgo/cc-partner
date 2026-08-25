// @vitest-environment jsdom
/**
 * useBattery / isBatteryConsumingRoute 合同测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   扣时只认工作台与 Inbox 前台；入账 toast 必须跟 snapshot 上升沿。
 *
 * Code Logic（这个测试做什么）:
 *   覆盖路由判定、setMode 走 API、creditMinutes 弹出 toast。
 */

import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, renderHook, waitFor } from '@testing-library/react';
import type { ReactNode } from 'react';
import { MemoryRouter } from 'react-router-dom';

import type { BatterySnapshot } from '@/lib/types/battery';

const getSnapshot = vi.fn();
const setModeApi = vi.fn();
const reportFocus = vi.fn();

vi.mock('@/api/battery', () => ({
  batteryApi: {
    getSnapshot: () => getSnapshot(),
    setMode: (mode: string) => setModeApi(mode),
    reportFocus: (label: string, consuming: boolean) => reportFocus(label, consuming),
  },
}));

vi.mock('@/hooks/useWorkbenchWindowRole', () => ({
  readCurrentWindowLabel: () => 'main',
}));

import { isBatteryConsumingRoute, useBattery } from './useBattery';

const chargingSnap: BatterySnapshot = {
  mode: 'charging',
  remainingMs: 23 * 60_000,
  maxBalanceMs: 240 * 60_000,
  todayEarnedMs: 8 * 60_000,
  todaySpentMs: 0,
  consuming: false,
};

function wrapper({ children }: { children: ReactNode }) {
  return <MemoryRouter initialEntries={['/workbench']}>{children}</MemoryRouter>;
}

describe('isBatteryConsumingRoute', () => {
  test('only workbench and attention consume when visible and focused', () => {
    expect(isBatteryConsumingRoute('/workbench', true, true)).toBe(true);
    expect(isBatteryConsumingRoute('/attention', true, true)).toBe(true);
    expect(isBatteryConsumingRoute('/health', true, true)).toBe(false);
    expect(isBatteryConsumingRoute('/workbench', false, true)).toBe(false);
    expect(isBatteryConsumingRoute('/workbench', true, false)).toBe(false);
  });
});

describe('useBattery', () => {
  beforeEach(() => {
    getSnapshot.mockReset();
    setModeApi.mockReset();
    reportFocus.mockReset();
    getSnapshot.mockResolvedValue(chargingSnap);
    reportFocus.mockResolvedValue(chargingSnap);
    setModeApi.mockResolvedValue({ ...chargingSnap, mode: 'unlimited' });
    Object.defineProperty(document, 'visibilityState', {
      configurable: true,
      value: 'visible',
    });
    vi.spyOn(document, 'hasFocus').mockReturnValue(false);
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  test('loads snapshot and toggles mode through the API', async () => {
    const { result } = renderHook(() => useBattery(), { wrapper });
    await waitFor(() => {
      expect(result.current.snapshot?.remainingMs).toBe(23 * 60_000);
    });

    await act(async () => {
      await result.current.setMode('unlimited');
    });
    expect(setModeApi).toHaveBeenCalledWith('unlimited');
    expect(result.current.snapshot?.mode).toBe('unlimited');
  });

  test('shows toast when credit minutes arrive', async () => {
    const credited: BatterySnapshot = {
      ...chargingSnap,
      remainingMs: 31 * 60_000,
      creditMinutes: 8,
      creditSource: 'health',
    };
    getSnapshot.mockResolvedValue(credited);
    reportFocus.mockResolvedValue(credited);
    const { result } = renderHook(() => useBattery(), { wrapper });
    await waitFor(() => {
      expect(result.current.toast?.minutes).toBe(8);
    });
    expect(result.current.toast?.source).toBe('health');
  });
});
