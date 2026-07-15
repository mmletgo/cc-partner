// @vitest-environment jsdom
/**
 * useSettingsUpdatePermissions characterization 测试
 *
 * Business Logic（为什么需要这个测试文件）:
 *   权限/更新域从巨型 controller 拆出后，必须锁定 checkUpdate 写结果、
 *   download 乐观状态与 install retry 派生标记，防止回归。
 *
 * Code Logic（这个测试文件做什么）:
 *   mock configApi 与 usePermissions；renderHook 断言 check/download/install 合同。
 */
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, renderHook, waitFor } from '@testing-library/react';

const checkUpdateMock = vi.fn();
const downloadUpdateMock = vi.fn();
const getDownloadStatusMock = vi.fn();
const cancelDownloadMock = vi.fn();
const installUpdateMock = vi.fn();

vi.mock('@/api/config', () => ({
  configApi: {
    checkUpdate: (...args: unknown[]) => checkUpdateMock(...args),
    downloadUpdate: (...args: unknown[]) => downloadUpdateMock(...args),
    getDownloadStatus: (...args: unknown[]) => getDownloadStatusMock(...args),
    cancelDownload: (...args: unknown[]) => cancelDownloadMock(...args),
    installUpdate: (...args: unknown[]) => installUpdateMock(...args),
  },
}));

vi.mock('@/hooks/usePermissions', () => ({
  usePermissions: () => ({
    status: null,
    loading: false,
    refreshing: false,
    error: null,
    requesting: new Set(),
    request: vi.fn(async () => undefined),
    refresh: vi.fn(),
    allRequiredGranted: true,
    allGranted: true,
  }),
}));

vi.mock('@/hooks/useVisibilityPolling', () => ({
  useVisibilityPolling: () => undefined,
}));

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key: string, opts?: { version?: string }) =>
      opts?.version ? `${key}:${opts.version}` : key,
  }),
}));

import { useSettingsUpdatePermissions } from './useSettingsUpdatePermissions';

describe('useSettingsUpdatePermissions', () => {
  beforeEach(() => {
    checkUpdateMock.mockReset();
    downloadUpdateMock.mockReset();
    getDownloadStatusMock.mockReset();
    cancelDownloadMock.mockReset();
    installUpdateMock.mockReset();
    getDownloadStatusMock.mockResolvedValue({
      status: 'idle',
      progress: 0,
      error: '',
      filePath: '',
      url: '',
      filename: '',
      size: 0,
    });
  });

  afterEach(() => {
    cleanup();
  });

  test('checkUpdate 成功写入 updateResult', async () => {
    checkUpdateMock.mockResolvedValue({
      hasUpdate: true,
      version: '9.9.9',
      downloadUrl: 'https://example.com/app.dmg',
      filename: 'app.dmg',
      size: 1024,
    });

    const { result } = renderHook(() => useSettingsUpdatePermissions());

    await act(async () => {
      await result.current.handleCheckUpdate();
    });

    expect(result.current.updateResult?.hasUpdate).toBe(true);
    expect(result.current.updateResult?.version).toBe('9.9.9');
    expect(result.current.updateHint).toContain('9.9.9');
  });

  test('download 先乐观进入 downloading', async () => {
    checkUpdateMock.mockResolvedValue({
      hasUpdate: true,
      version: '9.9.9',
      downloadUrl: 'https://example.com/app.dmg',
      filename: 'app.dmg',
      size: 2048,
    });
    let resolveDownload: (() => void) | null = null;
    downloadUpdateMock.mockImplementation(
      () =>
        new Promise<void>((resolve) => {
          resolveDownload = resolve;
        }),
    );

    const { result } = renderHook(() => useSettingsUpdatePermissions());
    await act(async () => {
      await result.current.handleCheckUpdate();
    });

    let downloadPromise: Promise<void> | undefined;
    act(() => {
      downloadPromise = result.current.handleDownload();
    });

    await waitFor(() => {
      expect(result.current.downloadStatus?.status).toBe('downloading');
    });
    expect(result.current.downloadStatus?.filename).toBe('app.dmg');
    expect(result.current.updateDownloadDisabled).toBe(true);

    await act(async () => {
      resolveDownload?.();
      await downloadPromise;
    });
  });

  test('install 失败后可从 completed+error 派生 retry 模式', async () => {
    getDownloadStatusMock.mockResolvedValue({
      status: 'completed',
      progress: 100,
      error: 'install failed',
      filePath: '/tmp/app.dmg',
      url: 'https://example.com/app.dmg',
      filename: 'app.dmg',
      size: 1,
    });
    installUpdateMock.mockRejectedValue(new Error('boom'));

    const { result } = renderHook(() => useSettingsUpdatePermissions());

    // 先进入可安装态
    act(() => {
      // 通过 check 不必要；直接 install 会乐观 installing 后回填 completed+error
    });

    await act(async () => {
      await result.current.handleInstall();
    });

    expect(result.current.downloadStatus?.status).toBe('completed');
    expect(result.current.downloadStatus?.error).toBe('install failed');
    expect(result.current.updateInstallRetry).toBe(true);
  });
});
