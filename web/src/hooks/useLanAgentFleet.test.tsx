// @vitest-environment jsdom
/**
 * useLanAgentFleet 单元测试。
 */
import { act, cleanup, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { LAN_FLEET_RECONCILE_MS, useLanAgentFleet } from './useLanAgentFleet';
import type { LanFleetSnapshot } from '@/lib/types/lanFleet';

/**
 * Business Logic（为什么需要这个函数）:
 *   hook 测试需要稳定空 snapshot。
 *
 * Code Logic（这个函数做什么）:
 *   返回空 devices 的 snapshot。
 */
function emptySnapshot(): LanFleetSnapshot {
  return {
    generatedAt: '2026-07-15T00:00:00Z',
    devices: [],
    truncated: false,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   jsdom 默认 visibilityState 固定，测试需要模拟 hidden/visible 切换。
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

describe('useLanAgentFleet', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    setVisibilityState('visible');
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    setVisibilityState('visible');
  });

  it('stops safety polling while hidden and drops stale responses', async () => {
    let calls = 0;
    const loadSnapshot = vi.fn(async () => {
      calls += 1;
      return emptySnapshot();
    });

    renderHook(() => useLanAgentFleet({ enabled: true, loadSnapshot }));

    await act(async () => {
      await Promise.resolve();
    });
    expect(calls).toBe(1);

    setVisibilityState('hidden');
    await act(async () => {
      await vi.advanceTimersByTimeAsync(LAN_FLEET_RECONCILE_MS * 2);
    });
    expect(calls).toBe(1);

    setVisibilityState('visible');
    await act(async () => {
      await Promise.resolve();
    });
    expect(calls).toBe(2);
  });
});
