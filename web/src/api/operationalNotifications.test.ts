/**
 * operationalNotifications API 单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   snapshot 命令名与 API 入口是前端协调器与后端的固定契约，不得漂移。
 *
 * Code Logic（这个测试做什么）:
 *   mock invoke，断言命令名、无参调用与返回透传。
 */

import { beforeEach, describe, expect, test, vi } from 'vitest';

const invokeMock = vi.fn();

vi.mock('./client', () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

import {
  OPERATIONAL_NOTIFICATION_SNAPSHOT_COMMAND,
  operationalNotificationsApi,
  type OperationalNotificationSnapshot,
} from './operationalNotifications';

/**
 * Business Logic（为什么需要这个函数）:
 *   测试需要最小合法 snapshot。
 *
 * Code Logic（这个函数做什么）:
 *   返回空 items 的 OperationalNotificationSnapshot。
 */
function emptySnapshot(): OperationalNotificationSnapshot {
  return {
    asOfCursor: { ownerInstanceId: 'owner-1', sequence: 0 },
    items: [],
    truncated: false,
  };
}

describe('operationalNotificationsApi', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  test('getSnapshot invokes get_operational_notification_snapshot without args', async () => {
    const snapshot = emptySnapshot();
    invokeMock.mockResolvedValueOnce(snapshot);

    await expect(operationalNotificationsApi.getSnapshot()).resolves.toEqual(snapshot);
    expect(OPERATIONAL_NOTIFICATION_SNAPSHOT_COMMAND).toBe(
      'get_operational_notification_snapshot',
    );
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith(OPERATIONAL_NOTIFICATION_SNAPSHOT_COMMAND);
  });
});
