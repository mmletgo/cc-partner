// @vitest-environment jsdom
/**
 * useOperationalNotifications 协调器单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   冷启动 baseline、dedupe、gap 重连、前台抑制、权限与隐私文案是运营通知正确性核心；
 *   任一回归会导致通知刷屏或锁屏泄漏。
 *
 * Code Logic（这个测试做什么）:
 *   mock listen/invoke/sendNotification/path/visibility，覆盖 handshake、四种 kind、
 *   dedupe/replay/gap/owner、偏好、权限与 action 字段约束。
 */

import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, renderHook, waitFor } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import type { ReactNode } from 'react';

import type {
  OperationalNotificationEvent,
  OperationalNotificationSnapshot,
} from '@/api/operationalNotifications';

const getSnapshotMock = vi.fn();
const getConfigMock = vi.fn();
const sendOperationalNotificationMock = vi.fn();
const checkNotificationGrantedMock = vi.fn();
const requestAttentionInvalidationMock = vi.fn();
const listenMock = vi.fn();

type EventHandler = (event: { payload: unknown }) => void;

const listenerHandlers = new Map<string, EventHandler>();

vi.mock('@/api/operationalNotifications', async () => {
  const actual = await vi.importActual<
    typeof import('@/api/operationalNotifications')
  >('@/api/operationalNotifications');
  return {
    ...actual,
    operationalNotificationsApi: {
      getSnapshot: (...args: unknown[]) => getSnapshotMock(...args),
    },
  };
});

vi.mock('@/api/orchestratorConfig', () => ({
  orchestratorConfigApi: {
    get: (...args: unknown[]) => getConfigMock(...args),
    getDefaults: vi.fn(),
    update: vi.fn(),
  },
}));

vi.mock('@/lib/notification', () => ({
  checkNotificationGranted: (...args: unknown[]) =>
    checkNotificationGrantedMock(...args),
  sendOperationalNotification: (...args: unknown[]) =>
    sendOperationalNotificationMock(...args),
  requestNotificationPermission: vi.fn(),
}));

vi.mock('./attentionInvalidation', () => ({
  requestAttentionInvalidation: (...args: unknown[]) =>
    requestAttentionInvalidationMock(...args),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: (...args: unknown[]) => listenMock(...args),
}));

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key: string) => key,
    i18n: { language: 'en' },
  }),
}));

import {
  DEFAULT_OPERATIONAL_NOTIFICATION_PREFERENCES,
  operationalNotificationDedupeKey,
  useOperationalNotifications,
} from './useOperationalNotifications';

/**
 * Business Logic（为什么需要这个函数）:
 *   异步 snapshot/listen 测试需要手动 resolve。
 *
 * Code Logic（这个函数做什么）:
 *   返回 promise 与 resolve/reject 控制器。
 */
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   构造最小合法事件。
 *
 * Code Logic（这个函数做什么）:
 *   返回可覆盖字段的 OperationalNotificationEvent。
 */
function buildEvent(
  overrides: Partial<OperationalNotificationEvent> = {},
): OperationalNotificationEvent {
  return {
    kind: 'humanReview',
    opaqueSourceId: 'src-1',
    stateVersion: 1,
    occurredAt: '2026-07-15T10:00:00.000Z',
    ownerInstanceId: 'owner-1',
    sequence: 1,
    ...overrides,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   handshake baseline 需要 snapshot fixture。
 *
 * Code Logic（这个函数做什么）:
 *   返回 asOfCursor + items 的 snapshot。
 */
function buildSnapshot(
  overrides: Partial<OperationalNotificationSnapshot> = {},
): OperationalNotificationSnapshot {
  return {
    asOfCursor: { ownerInstanceId: 'owner-1', sequence: 10 },
    items: [],
    truncated: false,
    ...overrides,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   hook 依赖 Router location 做前台抑制。
 *
 * Code Logic（这个函数做什么）:
 *   MemoryRouter wrapper，默认路径 /settings。
 */
function createWrapper(initialPath = '/settings') {
  return function Wrapper({ children }: { children: ReactNode }) {
    return (
      <MemoryRouter initialEntries={[initialPath]}>{children}</MemoryRouter>
    );
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   模拟 Tauri emit 到已注册 listener。
 *
 * Code Logic（这个函数做什么）:
 *   调用 listenerHandlers 中对应 event 的 handler。
 */
function emit(eventName: string, payload: unknown): void {
  const handler = listenerHandlers.get(eventName);
  if (!handler) {
    throw new Error(`No listener registered for ${eventName}`);
  }
  handler({ payload });
}

/**
 * Business Logic（为什么需要这个函数）:
 *   测试环境需伪装可注册 Tauri listener。
 *
 * Code Logic（这个函数做什么）:
 *   写入 window.__TAURI_INTERNALS__.transformCallback。
 */
function enableTauriInternals(): void {
  Object.defineProperty(window, '__TAURI_INTERNALS__', {
    configurable: true,
    value: {
      transformCallback: () => undefined,
    },
  });
}

/**
 * Business Logic（为什么需要这个函数）:
 *   清理伪装的 Tauri internals。
 *
 * Code Logic（这个函数做什么）:
 *   delete __TAURI_INTERNALS__。
 */
function disableTauriInternals(): void {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  delete (window as any).__TAURI_INTERNALS__;
}

beforeEach(() => {
  cleanup();
  listenerHandlers.clear();
  getSnapshotMock.mockReset();
  getConfigMock.mockReset();
  sendOperationalNotificationMock.mockReset();
  checkNotificationGrantedMock.mockReset();
  requestAttentionInvalidationMock.mockReset();
  listenMock.mockReset();

  enableTauriInternals();
  Object.defineProperty(document, 'visibilityState', {
    configurable: true,
    get: () => 'visible',
  });

  getConfigMock.mockResolvedValue({
    ...DEFAULT_OPERATIONAL_NOTIFICATION_PREFERENCES,
    notifyHumanReview: true,
    notifyBlocked: true,
    notifyRemoteOutboxFailed: true,
    notifyTaskDone: true,
  });
  checkNotificationGrantedMock.mockResolvedValue(true);
  getSnapshotMock.mockResolvedValue(buildSnapshot());

  listenMock.mockImplementation(async (eventName: string, handler: EventHandler) => {
    listenerHandlers.set(eventName, handler);
    return () => {
      listenerHandlers.delete(eventName);
    };
  });
});

afterEach(() => {
  cleanup();
  disableTauriInternals();
  vi.restoreAllMocks();
});

describe('useOperationalNotifications', () => {
  test('first snapshot establishes baseline without notification spam', async () => {
    const items = [
      buildEvent({ kind: 'humanReview', opaqueSourceId: 'a', stateVersion: 1, sequence: 1 }),
      buildEvent({ kind: 'blocked', opaqueSourceId: 'b', stateVersion: 2, sequence: 2 }),
      buildEvent({
        kind: 'remoteOutboxFailed',
        opaqueSourceId: 'c',
        stateVersion: 3,
        sequence: 3,
      }),
    ];
    getSnapshotMock.mockResolvedValue(
      buildSnapshot({ items, asOfCursor: { ownerInstanceId: 'owner-1', sequence: 10 } }),
    );

    renderHook(() => useOperationalNotifications(), {
      wrapper: createWrapper(),
    });

    await waitFor(() => {
      expect(getSnapshotMock).toHaveBeenCalled();
    });
    expect(sendOperationalNotificationMock).not.toHaveBeenCalled();
    expect(requestAttentionInvalidationMock).not.toHaveBeenCalled();
  });

  test('all four kinds notify when enabled including Done', async () => {
    renderHook(() => useOperationalNotifications(), {
      wrapper: createWrapper(),
    });
    await waitFor(() => expect(listenerHandlers.has('operational:notification')).toBe(true));

    const kinds = [
      'humanReview',
      'blocked',
      'remoteOutboxFailed',
      'taskDone',
    ] as const;

    for (let i = 0; i < kinds.length; i += 1) {
      await act(async () => {
        emit(
          'operational:notification',
          buildEvent({
            kind: kinds[i],
            opaqueSourceId: `src-${kinds[i]}`,
            stateVersion: 10 + i,
            sequence: 20 + i,
          }),
        );
      });
    }

    await waitFor(() => {
      expect(sendOperationalNotificationMock).toHaveBeenCalledTimes(4);
    });
    expect(requestAttentionInvalidationMock).toHaveBeenCalledTimes(4);

    const titles = sendOperationalNotificationMock.mock.calls.map(
      (call) => (call[0] as { title: string }).title,
    );
    expect(titles).toEqual([
      'orchestrator:notifications.humanReview.title',
      'orchestrator:notifications.blocked.title',
      'orchestrator:notifications.remoteOutboxFailed.title',
      'orchestrator:notifications.taskDone.title',
    ]);
  });

  test('Done notifies while on non-project settings path (independent of open project)', async () => {
    renderHook(() => useOperationalNotifications(), {
      wrapper: createWrapper('/settings'),
    });
    await waitFor(() => expect(listenerHandlers.has('operational:notification')).toBe(true));

    await act(async () => {
      emit(
        'operational:notification',
        buildEvent({
          kind: 'taskDone',
          opaqueSourceId: 'done-1',
          stateVersion: 5,
          sequence: 30,
        }),
      );
    });

    await waitFor(() => {
      expect(sendOperationalNotificationMock).toHaveBeenCalledTimes(1);
    });
    expect(sendOperationalNotificationMock.mock.calls[0][0]).toEqual({
      title: 'orchestrator:notifications.taskDone.title',
      body: 'orchestrator:notifications.taskDone.body',
    });
  });

  test('persistent state-revision dedupe: same key never notifies twice', async () => {
    renderHook(() => useOperationalNotifications(), {
      wrapper: createWrapper(),
    });
    await waitFor(() => expect(listenerHandlers.has('operational:notification')).toBe(true));

    const event = buildEvent({
      kind: 'blocked',
      opaqueSourceId: 'dup',
      stateVersion: 7,
      sequence: 40,
    });

    await act(async () => {
      emit('operational:notification', event);
      emit('operational:notification', { ...event, sequence: 41, occurredAt: 'later' });
    });

    await waitFor(() => {
      expect(sendOperationalNotificationMock).toHaveBeenCalledTimes(1);
    });
  });

  test('reconnect replay same key no spam', async () => {
    renderHook(() => useOperationalNotifications(), {
      wrapper: createWrapper(),
    });
    await waitFor(() => expect(listenerHandlers.has('operational:notification')).toBe(true));

    const event = buildEvent({
      kind: 'humanReview',
      opaqueSourceId: 'replay',
      stateVersion: 3,
      sequence: 50,
    });
    await act(async () => {
      emit('operational:notification', event);
    });
    await waitFor(() => expect(sendOperationalNotificationMock).toHaveBeenCalledTimes(1));

    // 模拟断线重放相同 key
    await act(async () => {
      emit('operational:notification', { ...event, sequence: 51 });
    });
    expect(sendOperationalNotificationMock).toHaveBeenCalledTimes(1);
  });

  test('gap snapshot baseline no spam', async () => {
    renderHook(() => useOperationalNotifications(), {
      wrapper: createWrapper(),
    });
    await waitFor(() => expect(listenerHandlers.has('backend:runtime-gap')).toBe(true));

    const gapSnapshotItems = [
      buildEvent({
        kind: 'humanReview',
        opaqueSourceId: 'gap-a',
        stateVersion: 1,
        sequence: 100,
      }),
    ];
    getSnapshotMock.mockResolvedValueOnce(
      buildSnapshot({
        items: gapSnapshotItems,
        asOfCursor: { ownerInstanceId: 'owner-1', sequence: 100 },
      }),
    );

    await act(async () => {
      emit('backend:runtime-gap', { ownerInstanceId: 'owner-1' });
    });

    await waitFor(() => {
      expect(getSnapshotMock.mock.calls.length).toBeGreaterThanOrEqual(2);
    });
    expect(sendOperationalNotificationMock).not.toHaveBeenCalled();
  });

  test('owner restart (ownerInstanceId change) re-handshake', async () => {
    renderHook(() => useOperationalNotifications(), {
      wrapper: createWrapper(),
    });
    await waitFor(() => expect(listenerHandlers.has('operational:notification')).toBe(true));

    const baselineEvent = buildEvent({
      kind: 'blocked',
      opaqueSourceId: 'owner2-baseline',
      stateVersion: 1,
      ownerInstanceId: 'owner-2',
      sequence: 1,
    });

    getSnapshotMock.mockResolvedValueOnce(
      buildSnapshot({
        asOfCursor: { ownerInstanceId: 'owner-2', sequence: 5 },
        items: [baselineEvent],
      }),
    );

    // 不同 owner 的 live 事件触发 re-handshake；该 key 会出现在新 baseline 中
    await act(async () => {
      emit('operational:notification', baselineEvent);
    });

    await waitFor(() => {
      expect(getSnapshotMock.mock.calls.length).toBeGreaterThanOrEqual(2);
    });
    // baseline 建立后同 key 不刷屏
    expect(sendOperationalNotificationMock).not.toHaveBeenCalled();

    // 新 owner 下 future revision 仍须通知
    await act(async () => {
      emit(
        'operational:notification',
        buildEvent({
          kind: 'blocked',
          opaqueSourceId: 'owner2-baseline',
          stateVersion: 2,
          ownerInstanceId: 'owner-2',
          sequence: 6,
        }),
      );
    });

    await waitFor(() => {
      expect(sendOperationalNotificationMock).toHaveBeenCalledTimes(1);
    });
  });

  test('same state no repeat after gap baseline', async () => {
    renderHook(() => useOperationalNotifications(), {
      wrapper: createWrapper(),
    });
    await waitFor(() => expect(listenerHandlers.has('operational:notification')).toBe(true));

    const shared = buildEvent({
      kind: 'remoteOutboxFailed',
      opaqueSourceId: 'outbox-1',
      stateVersion: 4,
      sequence: 70,
    });

    getSnapshotMock.mockResolvedValueOnce(
      buildSnapshot({
        items: [shared],
        asOfCursor: { ownerInstanceId: 'owner-1', sequence: 70 },
      }),
    );

    await act(async () => {
      emit('backend:runtime-gap', { ownerInstanceId: 'owner-1' });
    });
    await waitFor(() => expect(getSnapshotMock.mock.calls.length).toBeGreaterThanOrEqual(2));

    await act(async () => {
      emit('operational:notification', { ...shared, sequence: 71 });
    });

    expect(sendOperationalNotificationMock).not.toHaveBeenCalled();
  });

  test('future revision after gap still notifies', async () => {
    renderHook(() => useOperationalNotifications(), {
      wrapper: createWrapper(),
    });
    await waitFor(() => expect(listenerHandlers.has('operational:notification')).toBe(true));

    getSnapshotMock.mockResolvedValueOnce(
      buildSnapshot({
        items: [
          buildEvent({
            kind: 'humanReview',
            opaqueSourceId: 'task-x',
            stateVersion: 1,
          }),
        ],
        asOfCursor: { ownerInstanceId: 'owner-1', sequence: 80 },
      }),
    );

    await act(async () => {
      emit('backend:runtime-gap', { ownerInstanceId: 'owner-1' });
    });
    await waitFor(() => expect(getSnapshotMock.mock.calls.length).toBeGreaterThanOrEqual(2));

    await act(async () => {
      emit(
        'operational:notification',
        buildEvent({
          kind: 'humanReview',
          opaqueSourceId: 'task-x',
          stateVersion: 2,
          sequence: 81,
        }),
      );
    });

    await waitFor(() => {
      expect(sendOperationalNotificationMock).toHaveBeenCalledTimes(1);
    });
  });

  test('foreground authority suppression on /attention and /workbench', async () => {
    const { unmount } = renderHook(() => useOperationalNotifications(), {
      wrapper: createWrapper('/attention'),
    });
    await waitFor(() => expect(listenerHandlers.has('operational:notification')).toBe(true));

    await act(async () => {
      emit(
        'operational:notification',
        buildEvent({
          kind: 'blocked',
          opaqueSourceId: 'fg-1',
          stateVersion: 1,
          sequence: 90,
        }),
      );
    });

    await waitFor(() => {
      expect(requestAttentionInvalidationMock).toHaveBeenCalledTimes(1);
    });
    expect(sendOperationalNotificationMock).not.toHaveBeenCalled();
    unmount();

    renderHook(() => useOperationalNotifications(), {
      wrapper: createWrapper('/workbench'),
    });
    await waitFor(() => expect(listenerHandlers.has('operational:notification')).toBe(true));

    await act(async () => {
      emit(
        'operational:notification',
        buildEvent({
          kind: 'blocked',
          opaqueSourceId: 'fg-2',
          stateVersion: 1,
          sequence: 91,
        }),
      );
    });

    await waitFor(() => {
      expect(requestAttentionInvalidationMock).toHaveBeenCalledTimes(2);
    });
    expect(sendOperationalNotificationMock).not.toHaveBeenCalled();
  });

  test('permission denied → no sendNotification', async () => {
    checkNotificationGrantedMock.mockResolvedValue(false);
    renderHook(() => useOperationalNotifications(), {
      wrapper: createWrapper(),
    });
    await waitFor(() => expect(listenerHandlers.has('operational:notification')).toBe(true));

    await act(async () => {
      emit(
        'operational:notification',
        buildEvent({
          kind: 'humanReview',
          opaqueSourceId: 'perm',
          stateVersion: 1,
          sequence: 95,
        }),
      );
    });

    await waitFor(() => {
      expect(requestAttentionInvalidationMock).toHaveBeenCalledTimes(1);
    });
    expect(sendOperationalNotificationMock).not.toHaveBeenCalled();
  });

  test('privacy-safe title/body: task title/goal sentinels never appear', async () => {
    const TASK_TITLE_SENTINEL = 'SECRET_TASK_TITLE_SHOULD_NEVER_APPEAR';
    const GOAL_SENTINEL = 'SECRET_GOAL_SHOULD_NEVER_APPEAR';

    renderHook(() => useOperationalNotifications(), {
      wrapper: createWrapper(),
    });
    await waitFor(() => expect(listenerHandlers.has('operational:notification')).toBe(true));

    await act(async () => {
      // 模拟恶意/错误 payload 附带 title/goal 字段；协调器必须忽略并只用固定 i18n 文案。
      const leakyPayload = {
        ...buildEvent({
          kind: 'humanReview',
          opaqueSourceId: `opaque-${TASK_TITLE_SENTINEL}`,
          stateVersion: 1,
          sequence: 100,
        }),
        title: TASK_TITLE_SENTINEL,
        goal: GOAL_SENTINEL,
      };
      emit('operational:notification', leakyPayload);
    });

    await waitFor(() => {
      expect(sendOperationalNotificationMock).toHaveBeenCalledTimes(1);
    });
    const payload = sendOperationalNotificationMock.mock.calls[0][0] as {
      title: string;
      body: string;
    };
    expect(payload.title).not.toContain(TASK_TITLE_SENTINEL);
    expect(payload.body).not.toContain(TASK_TITLE_SENTINEL);
    expect(payload.title).not.toContain(GOAL_SENTINEL);
    expect(payload.body).not.toContain(GOAL_SENTINEL);
    expect(payload).toEqual({
      title: 'orchestrator:notifications.humanReview.title',
      body: 'orchestrator:notifications.humanReview.body',
    });
  });

  test('deferred snapshot: buffer before resolve; <=asOf baseline; later notify once', async () => {
    const snapshotDeferred = deferred<OperationalNotificationSnapshot>();
    getSnapshotMock.mockReturnValueOnce(snapshotDeferred.promise);

    renderHook(() => useOperationalNotifications(), {
      wrapper: createWrapper(),
    });
    await waitFor(() => expect(listenerHandlers.has('operational:notification')).toBe(true));

    // 在 snapshot resolve 前到达的事件
    await act(async () => {
      emit(
        'operational:notification',
        buildEvent({
          kind: 'blocked',
          opaqueSourceId: 'old',
          stateVersion: 1,
          sequence: 5,
        }),
      );
      emit(
        'operational:notification',
        buildEvent({
          kind: 'blocked',
          opaqueSourceId: 'new',
          stateVersion: 2,
          sequence: 15,
        }),
      );
    });
    expect(sendOperationalNotificationMock).not.toHaveBeenCalled();

    await act(async () => {
      snapshotDeferred.resolve(
        buildSnapshot({
          asOfCursor: { ownerInstanceId: 'owner-1', sequence: 10 },
          items: [
            buildEvent({
              kind: 'blocked',
              opaqueSourceId: 'snap',
              stateVersion: 1,
            }),
          ],
        }),
      );
    });

    await waitFor(() => {
      expect(sendOperationalNotificationMock).toHaveBeenCalledTimes(1);
    });
    expect(sendOperationalNotificationMock.mock.calls[0][0]).toEqual({
      title: 'orchestrator:notifications.blocked.title',
      body: 'orchestrator:notifications.blocked.body',
    });
  });

  test('Gap/owner-change while snapshot pending is handled without spam', async () => {
    const first = deferred<OperationalNotificationSnapshot>();
    const second = deferred<OperationalNotificationSnapshot>();
    getSnapshotMock
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise);

    renderHook(() => useOperationalNotifications(), {
      wrapper: createWrapper(),
    });
    await waitFor(() => expect(listenerHandlers.has('backend:runtime-gap')).toBe(true));

    await act(async () => {
      emit(
        'operational:notification',
        buildEvent({
          kind: 'humanReview',
          opaqueSourceId: 'pending-1',
          stateVersion: 1,
          sequence: 3,
        }),
      );
      emit('backend:runtime-gap', { ownerInstanceId: 'owner-2' });
    });

    await act(async () => {
      first.resolve(
        buildSnapshot({
          asOfCursor: { ownerInstanceId: 'owner-1', sequence: 10 },
          items: [],
        }),
      );
    });

    await act(async () => {
      second.resolve(
        buildSnapshot({
          asOfCursor: { ownerInstanceId: 'owner-2', sequence: 0 },
          items: [
            buildEvent({
              kind: 'humanReview',
              opaqueSourceId: 'pending-1',
              stateVersion: 1,
              ownerInstanceId: 'owner-2',
            }),
          ],
        }),
      );
    });

    await waitFor(() => {
      expect(getSnapshotMock.mock.calls.length).toBeGreaterThanOrEqual(2);
    });
    // 第二份 snapshot 把 pending-1 作为 baseline，不应通知
    expect(sendOperationalNotificationMock).not.toHaveBeenCalled();
  });

  test('listener registration failure handled without throw', async () => {
    listenMock.mockRejectedValue(new Error('listen failed'));
    getSnapshotMock.mockResolvedValue(
      buildSnapshot({
        items: [
          buildEvent({ kind: 'blocked', opaqueSourceId: 'x', stateVersion: 1 }),
        ],
      }),
    );

    expect(() => {
      renderHook(() => useOperationalNotifications(), {
        wrapper: createWrapper(),
      });
    }).not.toThrow();

    await waitFor(() => {
      expect(getSnapshotMock).toHaveBeenCalled();
    });
    expect(sendOperationalNotificationMock).not.toHaveBeenCalled();
  });

  test('no actionType/extra/onAction registration on send payload', async () => {
    renderHook(() => useOperationalNotifications(), {
      wrapper: createWrapper(),
    });
    await waitFor(() => expect(listenerHandlers.has('operational:notification')).toBe(true));

    await act(async () => {
      emit(
        'operational:notification',
        buildEvent({
          kind: 'remoteOutboxFailed',
          opaqueSourceId: 'out',
          stateVersion: 2,
          sequence: 110,
        }),
      );
    });

    await waitFor(() => {
      expect(sendOperationalNotificationMock).toHaveBeenCalledTimes(1);
    });
    const payload = sendOperationalNotificationMock.mock.calls[0][0] as Record<
      string,
      unknown
    >;
    expect(Object.keys(payload).sort()).toEqual(['body', 'title']);
    expect(payload).not.toHaveProperty('actionType');
    expect(payload).not.toHaveProperty('extra');
    expect(payload).not.toHaveProperty('onAction');
  });

  test('disabled preference suppresses OS notify and Attention invalidation', async () => {
    getConfigMock.mockResolvedValue({
      notifyHumanReview: false,
      notifyBlocked: false,
      notifyRemoteOutboxFailed: false,
      notifyTaskDone: false,
    });

    renderHook(() => useOperationalNotifications(), {
      wrapper: createWrapper(),
    });
    await waitFor(() => expect(listenerHandlers.has('operational:notification')).toBe(true));

    await act(async () => {
      emit(
        'operational:notification',
        buildEvent({
          kind: 'humanReview',
          opaqueSourceId: 'pref-off',
          stateVersion: 1,
          sequence: 120,
        }),
      );
    });

    // 给 microtask 一点时间
    await act(async () => {
      await Promise.resolve();
    });
    expect(sendOperationalNotificationMock).not.toHaveBeenCalled();
    expect(requestAttentionInvalidationMock).not.toHaveBeenCalled();
  });

  test('dedupe key format is kind:opaqueSourceId:stateVersion', () => {
    expect(
      operationalNotificationDedupeKey({
        kind: 'taskDone',
        opaqueSourceId: 'abc',
        stateVersion: 9,
      }),
    ).toBe('taskDone:abc:9');
  });
});
