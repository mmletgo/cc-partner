// @vitest-environment jsdom
/**
 * useLanDisclosureStartup 单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   状态机 loading→required→starting→pass / error 可重试是 LAN gate 正确性合同。
 *
 * Code Logic（这个测试做什么）:
 *   mock backendApi；覆盖 required、pass、ack 成功与失败。
 */

import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, renderHook, waitFor } from '@testing-library/react';

const getStatusMock = vi.fn();
const acknowledgeMock = vi.fn();

vi.mock('@/api/backend', () => ({
  backendApi: {
    getLanDisclosureStatus: (...args: unknown[]) => getStatusMock(...args),
    acknowledgeLanDisclosureAndStartBackend: (...args: unknown[]) =>
      acknowledgeMock(...args),
  },
}));

import { useLanDisclosureStartup } from './useLanDisclosureStartup';

function requiredStatus() {
  return {
    required: true,
    version: 1,
    localAddresses: ['192.168.1.10'],
    preferredPort: 62116,
    mdnsPort: 5353,
    alreadyRunning: false,
    actualHttpPort: null,
  };
}

describe('useLanDisclosureStartup', () => {
  beforeEach(() => {
    getStatusMock.mockReset();
    acknowledgeMock.mockReset();
  });

  afterEach(() => {
    cleanup();
  });

  test('loads required status', async () => {
    getStatusMock.mockResolvedValue(requiredStatus());
    const { result } = renderHook(() => useLanDisclosureStartup());
    await waitFor(() => expect(result.current.phase).toBe('required'));
    expect(result.current.status?.preferredPort).toBe(62116);
  });

  test('loads pass when not required', async () => {
    getStatusMock.mockResolvedValue({ ...requiredStatus(), required: false });
    const { result } = renderHook(() => useLanDisclosureStartup());
    await waitFor(() => expect(result.current.phase).toBe('pass'));
  });

  test('acknowledge success moves to pass', async () => {
    getStatusMock.mockResolvedValue(requiredStatus());
    acknowledgeMock.mockResolvedValue({
      actualHttpPort: 62116,
      localAddresses: ['192.168.1.10'],
      reusedExisting: false,
      version: 1,
    });
    const { result } = renderHook(() => useLanDisclosureStartup());
    await waitFor(() => expect(result.current.phase).toBe('required'));
    await act(async () => {
      await result.current.acknowledge();
    });
    expect(result.current.phase).toBe('pass');
    expect(result.current.startResult?.actualHttpPort).toBe(62116);
  });

  test('acknowledge failure is fail-closed and retryable', async () => {
    getStatusMock.mockResolvedValue(requiredStatus());
    acknowledgeMock
      .mockRejectedValueOnce(new Error('start failed'))
      .mockResolvedValueOnce({
        actualHttpPort: 62117,
        localAddresses: ['192.168.1.10'],
        reusedExisting: false,
        version: 1,
      });
    const { result } = renderHook(() => useLanDisclosureStartup());
    await waitFor(() => expect(result.current.phase).toBe('required'));
    await act(async () => {
      await result.current.acknowledge();
    });
    expect(result.current.phase).toBe('error');
    await act(async () => {
      await result.current.retry();
    });
    expect(result.current.phase).toBe('pass');
  });

  test('status load failure stays on error without pass', async () => {
    getStatusMock.mockRejectedValue(new Error('bootstrap read failed'));
    const { result } = renderHook(() => useLanDisclosureStartup());
    await waitFor(() => expect(result.current.phase).toBe('error'));
    expect(result.current.phase).not.toBe('pass');
  });
});
