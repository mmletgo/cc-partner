// @vitest-environment jsdom
/**
 * useWorkbenchFreshRestart 单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   设备级「全新启动连接」弹窗状态机被 Workbench 页面与侧栏 Rail 两个入口共享；
 *   deviceId 解析、preview 自动拉取、confirm 成功/失败分支必须独立可测，
 *   防止后续改动破坏 busy 双锁或降级结果透传。
 *
 * Code Logic（这个测试做什么）:
 *   renderHook 挂 hook；mock workbenchApi.freshRestart 覆盖 preview/execute 的
 *   成功、失败与断言调用参数；jsdom 环境供 React hooks 运行。
 */
import { afterEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, renderHook } from '@testing-library/react';

const freshRestartPreviewMock = vi.fn();
const freshRestartExecuteMock = vi.fn();

vi.mock('@/api/workbench', () => ({
  workbenchApi: {
    freshRestart: {
      preview: (...args: unknown[]) => freshRestartPreviewMock(...args),
      execute: (...args: unknown[]) => freshRestartExecuteMock(...args),
    },
  },
}));

import { useWorkbenchFreshRestart } from './useWorkbenchFreshRestart';

const basePreview = {
  sessions: [{ sessionId: 's1', projectId: 'p1', name: 'demo', backend: 'tmux' }],
  workbenchTmuxSessionCount: 1,
  foreignSessionCount: 0,
  foreignSessionNames: [],
  sshBootstrapAvailable: true,
  sshBootstrapDetail: null,
};

const successResult = {
  terminatedSessionCount: 1,
  terminatedSessionIds: ['s1'],
  skippedSessionIds: [],
  serverRestarted: true,
  bootstrap: 'ssh' as const,
  degradedReason: null,
  degradedDetail: null,
  foreignSessionCount: 0,
  manualCommand: null,
};

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe('useWorkbenchFreshRestart', () => {
  test('open 解析 remote deviceId 并自动 preview', async () => {
    freshRestartPreviewMock.mockResolvedValue(basePreview);
    const { result } = renderHook(() => useWorkbenchFreshRestart({}));
    await act(async () => {
      result.current.openFreshRestartDialog({ deviceId: 'device-a' });
    });
    expect(freshRestartPreviewMock).toHaveBeenCalledWith('device-a');
    expect(result.current.dialog.open).toBe(true);
    expect(result.current.dialog.preview?.sessions).toHaveLength(1);
    expect(result.current.dialog.previewing).toBe(false);
  });

  test('open 无 deviceId（本机）时 preview 收到 undefined', async () => {
    freshRestartPreviewMock.mockResolvedValue(basePreview);
    const { result } = renderHook(() => useWorkbenchFreshRestart({}));
    await act(async () => {
      result.current.openFreshRestartDialog({});
    });
    expect(freshRestartPreviewMock).toHaveBeenCalledWith(undefined);
    expect(result.current.dialog.open).toBe(true);
  });

  test('preview 失败保留 error 弹窗并展示（不静默关闭）', async () => {
    freshRestartPreviewMock.mockRejectedValue(new Error('offline'));
    const { result } = renderHook(() => useWorkbenchFreshRestart({}));
    await act(async () => {
      result.current.openFreshRestartDialog({ deviceId: 'device-a' });
    });
    expect(result.current.dialog.error).toContain('offline');
    expect(result.current.dialog.preview).toBeNull();
    expect(result.current.dialog.open).toBe(true);
  });

  test('confirm 成功写 result 并回调 onCompleted', async () => {
    freshRestartPreviewMock.mockResolvedValue(basePreview);
    freshRestartExecuteMock.mockResolvedValue(successResult);
    const completed = vi.fn();
    const { result } = renderHook(() => useWorkbenchFreshRestart({ onCompleted: completed }));
    await act(async () => {
      result.current.openFreshRestartDialog({ deviceId: 'device-a' });
    });
    await act(async () => {
      await result.current.confirmFreshRestart(true);
    });
    expect(freshRestartExecuteMock).toHaveBeenCalledWith('device-a', true);
    expect(result.current.dialog.result?.serverRestarted).toBe(true);
    expect(completed).toHaveBeenCalledTimes(1);
  });

  test('confirm 传输失败保留 error 供重试且不回调 onCompleted', async () => {
    freshRestartPreviewMock.mockResolvedValue(basePreview);
    freshRestartExecuteMock.mockRejectedValue(new Error('boom'));
    const completed = vi.fn();
    const { result } = renderHook(() => useWorkbenchFreshRestart({ onCompleted: completed }));
    await act(async () => {
      result.current.openFreshRestartDialog({});
    });
    await act(async () => {
      await result.current.confirmFreshRestart(false);
    });
    expect(result.current.dialog.error).toContain('boom');
    expect(result.current.dialog.result).toBeNull();
    expect(completed).not.toHaveBeenCalled();
  });

  test('close 在非 busy 时清空状态；busy 期间执行后 result 保留供展示', async () => {
    freshRestartPreviewMock.mockResolvedValue(basePreview);
    freshRestartExecuteMock.mockResolvedValue(successResult);
    const { result } = renderHook(() => useWorkbenchFreshRestart({}));
    await act(async () => {
      result.current.openFreshRestartDialog({});
    });
    await act(async () => {
      await result.current.confirmFreshRestart(false);
    });
    await act(async () => {
      result.current.closeFreshRestartDialog();
    });
    expect(result.current.dialog.open).toBe(false);
    expect(result.current.dialog.result).toBeNull();
  });
});
