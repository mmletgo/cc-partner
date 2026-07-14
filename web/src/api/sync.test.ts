/**
 * syncApi 契约单元测试
 *
 * Business Logic（为什么需要这个测试）:
 *   trigger_sync 必须返回 SyncRunResult；partial/unreachable 不得被 helper 判为成功。
 *
 * Code Logic（这个测试做什么）:
 *   mock invoke，断言命令名与 isDeviceSucceeded/isDomainSucceeded 语义。
 */

import { beforeEach, describe, expect, test, vi } from 'vitest';
import type { SyncRunResult } from './sync';

const mockInvoke = vi.fn();

vi.mock('./client', () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
}));

import {
  isDeviceSucceeded,
  isDomainSucceeded,
  succeededCounts,
  syncApi,
} from './sync';

describe('syncApi', () => {
  beforeEach(() => {
    mockInvoke.mockReset();
  });

  test('trigger invokes trigger_sync and returns SyncRunResult', async () => {
    const payload: SyncRunResult = {
      accepted: true,
      succeeded_devices: 0,
      synced: 0,
      note: '已与 0/1 个设备完全同步',
      devices: [
        {
          device_id: 'd1',
          device_name: 'Peer',
          status: 'partial',
          domains: [
            {
              domain: 'prompt',
              outcome: { kind: 'succeeded', pulled: 1, pushed: 0, unchanged: 2 },
            },
            {
              domain: 'ssh_target',
              outcome: { kind: 'succeeded', pulled: 0, pushed: 0, unchanged: 0 },
            },
            {
              domain: 'scratchpad',
              outcome: { kind: 'unreachable', class: 'network' },
            },
          ],
        },
      ],
    };
    mockInvoke.mockResolvedValueOnce(payload);

    const result = await syncApi.trigger();

    expect(mockInvoke).toHaveBeenCalledWith('trigger_sync');
    expect(result.succeeded_devices).toBe(0);
    expect(result.devices[0].status).toBe('partial');
  });

  test('partial and unreachable never count as success', () => {
    expect(isDeviceSucceeded('partial')).toBe(false);
    expect(isDeviceSucceeded('unreachable')).toBe(false);
    expect(isDeviceSucceeded('protocol_error')).toBe(false);
    expect(isDeviceSucceeded('resource_limit')).toBe(false);
    expect(isDeviceSucceeded('succeeded')).toBe(true);

    expect(isDomainSucceeded({ kind: 'unreachable', class: 'timeout' })).toBe(false);
    expect(
      isDomainSucceeded({ kind: 'succeeded', pulled: 0, pushed: 0, unchanged: 1 }),
    ).toBe(true);
    expect(
      succeededCounts({ kind: 'protocol_error', code: 'x' }),
    ).toBeNull();
    expect(
      succeededCounts({ kind: 'succeeded', pulled: 2, pushed: 1, unchanged: 3 }),
    ).toEqual({ pulled: 2, pushed: 1, unchanged: 3 });
  });
});
