/**
 * 移动端传输 controller 空数组保护与对账契约。
 *
 * Business Logic（为什么需要这个测试）:
 *   刷新失败不得用空列表覆盖；uncertain 必须 get-operation，禁止盲重试 complete。
 *
 * Code Logic（这个测试做什么）:
 *   单测 retain helpers；renderHook 覆盖 refresh 失败与 send uncertain。
 */

// @vitest-environment jsdom

import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, renderHook, waitFor } from '@testing-library/react';
import type { ReactNode } from 'react';
import { I18nextProvider } from 'react-i18next';
import i18n from '@/i18n';
import type { TransferTask } from '@/lib/types';
import type { MobileTransferDevice } from '@/api/transferHttp';

const listDevicesMock = vi.fn();
const listTasksMock = vi.fn();
const initUploadMock = vi.fn();
const uploadChunkMock = vi.fn();
const completeUploadMock = vi.fn();
const getOperationMock = vi.fn();
const downloadMock = vi.fn();

vi.mock('@/api/transferHttp', async () => {
  const actual = await vi.importActual<typeof import('@/api/transferHttp')>('@/api/transferHttp');
  return {
    ...actual,
    transferHttp: {
      listDevices: (...args: unknown[]) => listDevicesMock(...args),
      listTasks: (...args: unknown[]) => listTasksMock(...args),
      initUpload: (...args: unknown[]) => initUploadMock(...args),
      uploadChunk: (...args: unknown[]) => uploadChunkMock(...args),
      completeUpload: (...args: unknown[]) => completeUploadMock(...args),
      cancel: vi.fn(),
      retry: vi.fn(),
      resume: vi.fn(),
      getOperation: (...args: unknown[]) => getOperationMock(...args),
      download: (...args: unknown[]) => downloadMock(...args),
    },
  };
});

import {
  buildMobileTransferSendIntentKey,
  retainListOnRefreshFailure,
  shouldReconcileTransferError,
  useMobileTransferController,
} from './useMobileTransferController';

/**
 * Business Logic（为什么需要这个函数）:
 *   hook 测试需要 i18n 才能拼错误文案。
 *
 * Code Logic（这个函数做什么）:
 *   用 I18nextProvider 包裹 children。
 */
function wrapper({ children }: { children: ReactNode }) {
  return <I18nextProvider i18n={i18n}>{children}</I18nextProvider>;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   controller 测试需要最小合法设备。
 *
 * Code Logic（这个函数做什么）:
 *   返回可覆盖的 MobileTransferDevice。
 */
function buildDevice(
  overrides: Partial<MobileTransferDevice> & Pick<MobileTransferDevice, 'id' | 'name' | 'isSelf'>,
): MobileTransferDevice {
  return {
    address: '10.0.0.2',
    port: 62116,
    status: 'online',
    capabilities: [],
    protoVersion: 1,
    ...overrides,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   任务刷新保护需要一份已成功列表。
 *
 * Code Logic（这个函数做什么）:
 *   返回最小 TransferTask。
 */
function buildTask(id: string): TransferTask {
  return {
    id,
    fileName: `${id}.txt`,
    filePath: '',
    fileSize: 8,
    direction: 'receive',
    status: 'completed',
    progress: 1,
    startedAt: '2026-08-18T00:00:00.000Z',
  };
}

beforeEach(() => {
  listDevicesMock.mockReset();
  listTasksMock.mockReset();
  initUploadMock.mockReset();
  uploadChunkMock.mockReset();
  completeUploadMock.mockReset();
  getOperationMock.mockReset();
  downloadMock.mockReset();
  listDevicesMock.mockResolvedValue([
    buildDevice({ id: 'self', name: 'Host', isSelf: true }),
  ]);
  listTasksMock.mockResolvedValue([buildTask('t1')]);
});

afterEach(() => {
  vi.clearAllMocks();
});

describe('mobile transfer controller helpers', () => {
  test('retainListOnRefreshFailure keeps previous when failed even if incoming is empty', () => {
    const previous = [buildTask('keep')];
    expect(retainListOnRefreshFailure(previous, [], true)).toEqual(previous);
    expect(retainListOnRefreshFailure(previous, [buildTask('next')], false)[0]?.id).toBe('next');
  });

  test('send intent key never includes a host path', () => {
    const file = { name: 'photo.jpg', size: 12, lastModified: 99 };
    const key = buildMobileTransferSendIntentKey('self', file);
    expect(key).toContain('photo.jpg');
    expect(key).not.toContain('/');
    expect(key).not.toContain('\\');
  });

  test('shouldReconcileTransferError is true for timeout/network', () => {
    expect(shouldReconcileTransferError(new Error('timeout'))).toBe(true);
    expect(shouldReconcileTransferError(new Error('network offline'))).toBe(true);
    expect(shouldReconcileTransferError(new Error('validation failed'))).toBe(false);
  });
});

describe('useMobileTransferController', () => {
  test('refresh failure does not replace tasks or devices with empty arrays', async () => {
    const { result } = renderHook(() => useMobileTransferController(), { wrapper });

    await waitFor(() => {
      expect(result.current.tasks).toHaveLength(1);
      expect(result.current.devices).toHaveLength(1);
    });

    listTasksMock.mockRejectedValue(new Error('network'));
    listDevicesMock.mockRejectedValue(new Error('network'));

    await act(async () => {
      result.current.onRetryTasks();
      result.current.onRetryDevices();
    });

    await waitFor(() => {
      expect(result.current.tasksState).toBe('error');
      expect(result.current.devicesState).toBe('error');
    });

    expect(result.current.tasks).toHaveLength(1);
    expect(result.current.tasks[0]?.id).toBe('t1');
    expect(result.current.devices).toHaveLength(1);
    expect(result.current.devices[0]?.id).toBe('self');
  });

  test('uncertain complete reconciles via get-operation and does not blind retry', async () => {
    initUploadMock.mockResolvedValue({ id: 'staging-1', receivedBytes: 0 });
    uploadChunkMock.mockResolvedValue({ receivedBytes: 5 });
    completeUploadMock.mockRejectedValueOnce(new Error('timeout contacting host'));
    getOperationMock.mockResolvedValue({ status: 'succeeded', taskId: 't-new' });

    const { result } = renderHook(() => useMobileTransferController(), { wrapper });
    await waitFor(() => {
      expect(result.current.devices.length).toBeGreaterThan(0);
    });

    const file = new File(['hello'], 'hello.txt', { type: 'text/plain' });
    act(() => {
      result.current.onDeviceChange('self');
      result.current.onFileChosen(file);
    });

    await act(async () => {
      result.current.onSend();
    });

    await waitFor(() => {
      expect(getOperationMock).toHaveBeenCalledTimes(1);
    });

    expect(completeUploadMock).toHaveBeenCalledTimes(1);
    expect(initUploadMock).toHaveBeenCalledTimes(1);
    expect(result.current.selectedFileName).toBeNull();
  });
});
