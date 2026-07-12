// @vitest-environment jsdom
/**
 * useOrchestratorRuntimeSnapshot 单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   桌面端 runtime 状态条必须按项目 shortcut 缓存最后一次 live 成功快照，
 *   在 project 切换时丢弃旧请求，并在 remote live→offline 时保留缓存与 cachedAt；
 *   unsupported/unavailable 不能跨项目复用缓存，冷启动 offline 不能伪造快照。
 *
 * Code Logic（这个测试做什么）:
 *   用 renderHook + deferred mock getRuntimeSnapshot 覆盖 load/stale/cache/refresh/unmount 场景。
 */
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, renderHook, waitFor } from '@testing-library/react';

import type { OrchestratorRuntimeSnapshot } from '@/lib/types';

const getRuntimeSnapshotMock = vi.fn();

vi.mock('@/api/orchestrator', () => ({
  orchestratorApi: {
    getRuntimeSnapshot: (...args: unknown[]) => getRuntimeSnapshotMock(...args),
  },
}));

import {
  __clearOrchestratorRuntimeSnapshotCacheForTests,
  useOrchestratorRuntimeSnapshot,
} from './useOrchestratorRuntimeSnapshot';

/**
 * Business Logic（为什么需要这个函数）:
 *   测试需要可控制的 snapshot fixture，字段只需足以区分不同项目与状态。
 *
 * Code Logic（这个函数做什么）:
 *   返回带默认字段的 OrchestratorRuntimeSnapshot，并允许覆盖 projectId/remoteStatus 等。
 */
function buildSnapshot(
  overrides: Partial<OrchestratorRuntimeSnapshot> = {},
): OrchestratorRuntimeSnapshot {
  return {
    projectId: 'local-project',
    projectKind: 'local',
    remoteStatus: 'local',
    generatedAt: '2026-07-11T10:00:00.000Z',
    latestTickAt: '2026-07-11T10:00:01.000Z',
    lastDispatchAt: null,
    lastDispatchedCount: 0,
    schedulerEnabled: true,
    workflowSource: 'WORKFLOW.md',
    workflowValid: true,
    workflowError: null,
    maxConcurrentTasks: 2,
    slotsUsed: 1,
    slotsAvailable: 1,
    latestError: null,
    runningTasks: [],
    retryingTasks: [],
    recentEvents: [],
    ...overrides,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   异步 API 测试需要手动 resolve/reject，才能验证 stale guard 与 loading 中间态。
 *
 * Code Logic（这个函数做什么）:
 *   返回 promise 与 resolve/reject 控制器，供 mock 返回后在 act 中推进。
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

beforeEach(() => {
  getRuntimeSnapshotMock.mockReset();
  __clearOrchestratorRuntimeSnapshotCacheForTests();
  vi.useFakeTimers({ shouldAdvanceTime: true });
  vi.setSystemTime(new Date('2026-07-11T12:00:00.000Z'));
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe('useOrchestratorRuntimeSnapshot', () => {
  test('loads local live snapshot', async () => {
    const local = buildSnapshot({ projectId: 'local-a', remoteStatus: 'local' });
    getRuntimeSnapshotMock.mockResolvedValueOnce(local);

    const { result } = renderHook(() =>
      useOrchestratorRuntimeSnapshot({ projectId: 'local-a', enabled: true }),
    );

    await waitFor(() => {
      expect(result.current.loading).toBe(false);
    });

    expect(getRuntimeSnapshotMock).toHaveBeenCalledWith('local-a');
    expect(result.current.snapshot).toEqual(local);
    expect(result.current.remoteStatus).toBeNull();
    expect(result.current.cachedAt).toBeNull();
    expect(result.current.error).toBeNull();
  });

  test('loads remote live snapshot', async () => {
    const remoteLive = buildSnapshot({
      projectId: 'remote:dev-1:proj',
      projectKind: 'remote',
      remoteStatus: 'live',
      generatedAt: '2026-07-11T11:00:00.000Z',
      slotsUsed: 2,
    });
    getRuntimeSnapshotMock.mockResolvedValueOnce(remoteLive);

    const { result } = renderHook(() =>
      useOrchestratorRuntimeSnapshot({ projectId: 'remote:dev-1:proj', enabled: true }),
    );

    await waitFor(() => {
      expect(result.current.loading).toBe(false);
    });

    expect(result.current.snapshot).toEqual(remoteLive);
    expect(result.current.remoteStatus).toBe('live');
    expect(result.current.cachedAt).toBeNull();
  });

  test('discards in-flight response after project switch', async () => {
    const first = deferred<OrchestratorRuntimeSnapshot>();
    const second = deferred<OrchestratorRuntimeSnapshot>();
    getRuntimeSnapshotMock
      .mockImplementationOnce(() => first.promise)
      .mockImplementationOnce(() => second.promise);

    const { result, rerender } = renderHook(
      ({ projectId }: { projectId: string }) =>
        useOrchestratorRuntimeSnapshot({ projectId, enabled: true }),
      { initialProps: { projectId: 'project-a' } },
    );

    expect(getRuntimeSnapshotMock).toHaveBeenCalledWith('project-a');

    rerender({ projectId: 'project-b' });
    expect(getRuntimeSnapshotMock).toHaveBeenCalledWith('project-b');

    await act(async () => {
      first.resolve(
        buildSnapshot({
          projectId: 'project-a',
          remoteStatus: 'local',
          generatedAt: '2026-07-11T10:00:00.000Z',
        }),
      );
      await Promise.resolve();
    });

    expect(result.current.snapshot?.projectId).not.toBe('project-a');

    await act(async () => {
      second.resolve(
        buildSnapshot({
          projectId: 'project-b',
          remoteStatus: 'local',
          generatedAt: '2026-07-11T10:05:00.000Z',
        }),
      );
      await Promise.resolve();
    });

    await waitFor(() => {
      expect(result.current.loading).toBe(false);
    });
    expect(result.current.snapshot?.projectId).toBe('project-b');
    expect(result.current.snapshot?.generatedAt).toBe('2026-07-11T10:05:00.000Z');
  });

  test('remote live then offline preserves last success with cachedAt', async () => {
    const live = buildSnapshot({
      projectId: 'remote:dev:proj',
      projectKind: 'remote',
      remoteStatus: 'live',
      generatedAt: '2026-07-11T09:00:00.000Z',
      slotsUsed: 3,
    });
    const offline = buildSnapshot({
      projectId: 'remote:dev:proj',
      projectKind: 'remote',
      remoteStatus: 'offline',
      generatedAt: '2026-07-11T12:00:00.000Z',
      slotsUsed: 0,
      schedulerEnabled: false,
      workflowValid: false,
      latestError: 'device offline',
    });

    getRuntimeSnapshotMock.mockResolvedValueOnce(live).mockResolvedValueOnce(offline);

    const { result } = renderHook(() =>
      useOrchestratorRuntimeSnapshot({ projectId: 'remote:dev:proj', enabled: true }),
    );

    await waitFor(() => {
      expect(result.current.remoteStatus).toBe('live');
    });
    expect(result.current.snapshot?.slotsUsed).toBe(3);
    expect(result.current.cachedAt).toBeNull();

    await act(async () => {
      await result.current.refresh();
    });

    await waitFor(() => {
      expect(result.current.remoteStatus).toBe('offline');
    });
    expect(result.current.snapshot?.slotsUsed).toBe(3);
    expect(result.current.snapshot?.generatedAt).toBe('2026-07-11T09:00:00.000Z');
    expect(result.current.cachedAt).toBe('2026-07-11T12:00:00.000Z');
    expect(result.current.error).toBeNull();
  });

  test('unsupported and unavailable do not reuse another project cache', async () => {
    const projectALive = buildSnapshot({
      projectId: 'remote:a:proj',
      projectKind: 'remote',
      remoteStatus: 'live',
      generatedAt: '2026-07-11T08:00:00.000Z',
      slotsUsed: 7,
    });
    const projectBUnsupported = buildSnapshot({
      projectId: 'remote:b:proj',
      projectKind: 'remote',
      remoteStatus: 'unsupported',
      generatedAt: '2026-07-11T12:00:00.000Z',
      slotsUsed: 0,
      latestError: 'unsupported',
    });
    const projectCUnavailable = buildSnapshot({
      projectId: 'remote:c:proj',
      projectKind: 'remote',
      remoteStatus: 'unavailable',
      generatedAt: '2026-07-11T12:00:00.000Z',
      slotsUsed: 0,
      latestError: 'unavailable',
    });

    getRuntimeSnapshotMock
      .mockResolvedValueOnce(projectALive)
      .mockResolvedValueOnce(projectBUnsupported)
      .mockResolvedValueOnce(projectCUnavailable);

    const { result, rerender } = renderHook(
      ({ projectId }: { projectId: string }) =>
        useOrchestratorRuntimeSnapshot({ projectId, enabled: true }),
      { initialProps: { projectId: 'remote:a:proj' } },
    );

    await waitFor(() => {
      expect(result.current.remoteStatus).toBe('live');
    });
    expect(result.current.snapshot?.slotsUsed).toBe(7);

    rerender({ projectId: 'remote:b:proj' });
    await waitFor(() => {
      expect(result.current.loading).toBe(false);
    });
    expect(result.current.remoteStatus).toBe('unsupported');
    expect(result.current.snapshot).toBeNull();
    expect(result.current.cachedAt).toBeNull();

    rerender({ projectId: 'remote:c:proj' });
    await waitFor(() => {
      expect(result.current.loading).toBe(false);
    });
    expect(result.current.remoteStatus).toBe('unavailable');
    expect(result.current.snapshot).toBeNull();
    expect(result.current.cachedAt).toBeNull();
  });

  test('cold-start offline has null snapshot', async () => {
    getRuntimeSnapshotMock.mockResolvedValueOnce(
      buildSnapshot({
        projectId: 'remote:cold:proj',
        projectKind: 'remote',
        remoteStatus: 'offline',
        generatedAt: '2026-07-11T12:00:00.000Z',
        slotsUsed: 0,
        latestError: 'offline',
      }),
    );

    const { result } = renderHook(() =>
      useOrchestratorRuntimeSnapshot({ projectId: 'remote:cold:proj', enabled: true }),
    );

    await waitFor(() => {
      expect(result.current.loading).toBe(false);
    });

    expect(result.current.remoteStatus).toBe('offline');
    expect(result.current.snapshot).toBeNull();
    expect(result.current.cachedAt).toBeNull();
  });

  test('unmount cleanup discards late response', async () => {
    const pending = deferred<OrchestratorRuntimeSnapshot>();
    getRuntimeSnapshotMock.mockImplementationOnce(() => pending.promise);

    const { result, unmount } = renderHook(() =>
      useOrchestratorRuntimeSnapshot({ projectId: 'project-unmount', enabled: true }),
    );

    expect(result.current.loading).toBe(true);
    unmount();

    await act(async () => {
      pending.resolve(
        buildSnapshot({
          projectId: 'project-unmount',
          remoteStatus: 'local',
          generatedAt: '2026-07-11T13:00:00.000Z',
        }),
      );
      await Promise.resolve();
    });

    // 卸载后不应抛错；缓存也不应被这次 late response 写入（下一次挂载仍需重新请求）。
    getRuntimeSnapshotMock.mockResolvedValueOnce(
      buildSnapshot({
        projectId: 'project-unmount',
        remoteStatus: 'local',
        generatedAt: '2026-07-11T14:00:00.000Z',
      }),
    );
    const remounted = renderHook(() =>
      useOrchestratorRuntimeSnapshot({ projectId: 'project-unmount', enabled: true }),
    );
    await waitFor(() => {
      expect(remounted.result.current.loading).toBe(false);
    });
    expect(remounted.result.current.snapshot?.generatedAt).toBe('2026-07-11T14:00:00.000Z');
    expect(getRuntimeSnapshotMock).toHaveBeenCalledTimes(2);
  });

  test('refresh success replaces cache', async () => {
    const first = buildSnapshot({
      projectId: 'remote:dev:proj',
      projectKind: 'remote',
      remoteStatus: 'live',
      generatedAt: '2026-07-11T09:00:00.000Z',
      slotsUsed: 1,
    });
    const second = buildSnapshot({
      projectId: 'remote:dev:proj',
      projectKind: 'remote',
      remoteStatus: 'live',
      generatedAt: '2026-07-11T10:00:00.000Z',
      slotsUsed: 4,
    });
    getRuntimeSnapshotMock.mockResolvedValueOnce(first).mockResolvedValueOnce(second);

    const { result } = renderHook(() =>
      useOrchestratorRuntimeSnapshot({ projectId: 'remote:dev:proj', enabled: true }),
    );

    await waitFor(() => {
      expect(result.current.snapshot?.slotsUsed).toBe(1);
    });

    await act(async () => {
      await result.current.refresh();
    });

    await waitFor(() => {
      expect(result.current.snapshot?.slotsUsed).toBe(4);
    });
    expect(result.current.snapshot?.generatedAt).toBe('2026-07-11T10:00:00.000Z');
    expect(result.current.remoteStatus).toBe('live');
    expect(result.current.cachedAt).toBeNull();
  });

  test('project switch A to B isolates previous project on first render', async () => {
    const projectA = buildSnapshot({
      projectId: 'project-a',
      remoteStatus: 'local',
      generatedAt: '2026-07-11T10:00:00.000Z',
      slotsUsed: 9,
    });
    const projectBPending = deferred<OrchestratorRuntimeSnapshot>();
    getRuntimeSnapshotMock
      .mockResolvedValueOnce(projectA)
      .mockImplementationOnce(() => projectBPending.promise);

    const { result, rerender } = renderHook(
      ({ projectId }: { projectId: string }) =>
        useOrchestratorRuntimeSnapshot({ projectId, enabled: true }),
      { initialProps: { projectId: 'project-a' } },
    );

    await waitFor(() => {
      expect(result.current.snapshot?.projectId).toBe('project-a');
    });
    expect(result.current.snapshot?.slotsUsed).toBe(9);

    rerender({ projectId: 'project-b' });
    // 首帧不得继续展示 A 的 telemetry。
    expect(result.current.snapshot?.projectId).not.toBe('project-a');
    expect(result.current.loading).toBe(true);

    await act(async () => {
      projectBPending.resolve(
        buildSnapshot({
          projectId: 'project-b',
          remoteStatus: 'local',
          generatedAt: '2026-07-11T10:10:00.000Z',
          slotsUsed: 1,
        }),
      );
      await Promise.resolve();
    });
    await waitFor(() => {
      expect(result.current.loading).toBe(false);
    });
    expect(result.current.snapshot?.projectId).toBe('project-b');
  });

  test('discards response when snapshot.projectId mismatches target', async () => {
    getRuntimeSnapshotMock.mockResolvedValueOnce(
      buildSnapshot({
        projectId: 'wrong-project',
        remoteStatus: 'live',
        projectKind: 'remote',
        generatedAt: '2026-07-11T11:00:00.000Z',
        slotsUsed: 8,
      }),
    );

    const { result } = renderHook(() =>
      useOrchestratorRuntimeSnapshot({ projectId: 'target-project', enabled: true }),
    );

    await waitFor(() => {
      expect(result.current.loading).toBe(false);
    });
    expect(result.current.snapshot).toBeNull();
    expect(result.current.remoteStatus).toBeNull();
    expect(result.current.cachedAt).toBeNull();
  });

  test('local success then reject does not mark offline from local cache', async () => {
    const local = buildSnapshot({
      projectId: 'local-only',
      remoteStatus: 'local',
      generatedAt: '2026-07-11T09:00:00.000Z',
      slotsUsed: 2,
    });
    getRuntimeSnapshotMock
      .mockResolvedValueOnce(local)
      .mockRejectedValueOnce(new Error('repo failed'));

    const { result } = renderHook(() =>
      useOrchestratorRuntimeSnapshot({ projectId: 'local-only', enabled: true }),
    );

    await waitFor(() => {
      expect(result.current.snapshot?.remoteStatus).toBe('local');
    });

    await act(async () => {
      await result.current.refresh();
    });

    await waitFor(() => {
      expect(result.current.loading).toBe(false);
    });
    expect(result.current.remoteStatus).not.toBe('offline');
    expect(result.current.error?.message).toBe('repo failed');
    // local 成功不得写入 remote live cache，因此失败后不能展示旧 local 为 offline 缓存。
    expect(result.current.cachedAt).toBeNull();
  });

  test('cached remote non-network failure maps to unavailable not offline', async () => {
    const live = buildSnapshot({
      projectId: 'remote:dev:cache',
      projectKind: 'remote',
      remoteStatus: 'live',
      generatedAt: '2026-07-11T09:00:00.000Z',
      slotsUsed: 3,
    });
    getRuntimeSnapshotMock
      .mockResolvedValueOnce(live)
      .mockRejectedValueOnce(new Error('protocol invalid payload'));

    const { result } = renderHook(() =>
      useOrchestratorRuntimeSnapshot({ projectId: 'remote:dev:cache', enabled: true }),
    );

    await waitFor(() => {
      expect(result.current.remoteStatus).toBe('live');
    });

    await act(async () => {
      await result.current.refresh();
    });

    await waitFor(() => {
      expect(result.current.loading).toBe(false);
    });
    expect(result.current.remoteStatus).toBe('unavailable');
    expect(result.current.remoteStatus).not.toBe('offline');
    expect(result.current.snapshot).toBeNull();
    expect(result.current.cachedAt).toBeNull();
    expect(result.current.error?.message).toContain('protocol');
  });

  test('never writes localStorage/sessionStorage and cold init has no fabricated cache', async () => {
    const originalLocalGet = globalThis.localStorage?.getItem?.bind(globalThis.localStorage);
    const originalLocalSet = globalThis.localStorage?.setItem?.bind(globalThis.localStorage);
    const originalSessionGet = globalThis.sessionStorage?.getItem?.bind(globalThis.sessionStorage);
    const originalSessionSet = globalThis.sessionStorage?.setItem?.bind(globalThis.sessionStorage);
    let storageTouched = false;
    if (globalThis.localStorage) {
      globalThis.localStorage.getItem = ((...args: Parameters<Storage['getItem']>) => {
        storageTouched = true;
        return originalLocalGet?.(...args) ?? null;
      }) as Storage['getItem'];
      globalThis.localStorage.setItem = ((...args: Parameters<Storage['setItem']>) => {
        storageTouched = true;
        originalLocalSet?.(...args);
      }) as Storage['setItem'];
    }
    if (globalThis.sessionStorage) {
      globalThis.sessionStorage.getItem = ((...args: Parameters<Storage['getItem']>) => {
        storageTouched = true;
        return originalSessionGet?.(...args) ?? null;
      }) as Storage['getItem'];
      globalThis.sessionStorage.setItem = ((...args: Parameters<Storage['setItem']>) => {
        storageTouched = true;
        originalSessionSet?.(...args);
      }) as Storage['setItem'];
    }

    try {
      // 冷启动：模块缓存已 clear，offline 不得伪造 snapshot。
      getRuntimeSnapshotMock.mockResolvedValueOnce(
        buildSnapshot({
          projectId: 'remote:storage:proj',
          projectKind: 'remote',
          remoteStatus: 'offline',
          generatedAt: '2026-07-12T01:00:00.000Z',
          slotsUsed: 0,
        }),
      );
      const cold = renderHook(() =>
        useOrchestratorRuntimeSnapshot({ projectId: 'remote:storage:proj', enabled: true }),
      );
      await waitFor(() => {
        expect(cold.result.current.loading).toBe(false);
      });
      expect(cold.result.current.snapshot).toBeNull();
      expect(cold.result.current.cachedAt).toBeNull();
      expect(cold.result.current.remoteStatus).toBe('offline');

      // live 成功后 offline 仅进程内缓存，仍不得触碰 storage。
      getRuntimeSnapshotMock
        .mockResolvedValueOnce(
          buildSnapshot({
            projectId: 'remote:storage:proj',
            projectKind: 'remote',
            remoteStatus: 'live',
            generatedAt: '2026-07-12T02:00:00.000Z',
            slotsUsed: 5,
          }),
        )
        .mockResolvedValueOnce(
          buildSnapshot({
            projectId: 'remote:storage:proj',
            projectKind: 'remote',
            remoteStatus: 'offline',
            generatedAt: '2026-07-12T03:00:00.000Z',
            slotsUsed: 0,
          }),
        );
      const live = renderHook(() =>
        useOrchestratorRuntimeSnapshot({ projectId: 'remote:storage:proj', enabled: true }),
      );
      await waitFor(() => {
        expect(live.result.current.remoteStatus).toBe('live');
      });
      await act(async () => {
        await live.result.current.refresh();
      });
      await waitFor(() => {
        expect(live.result.current.remoteStatus).toBe('offline');
      });
      expect(live.result.current.snapshot?.generatedAt).toBe('2026-07-12T02:00:00.000Z');
      // receivedAt 来自 new Date().toISOString()；fake timer 可能前进毫秒，只断言存在且为 ISO。
      expect(live.result.current.cachedAt).toEqual(expect.stringMatching(/^2026-07-11T12:00:00\.\d{3}Z$/));
      expect(storageTouched).toBe(false);
    } finally {
      if (globalThis.localStorage && originalLocalGet && originalLocalSet) {
        globalThis.localStorage.getItem = originalLocalGet;
        globalThis.localStorage.setItem = originalLocalSet;
      }
      if (globalThis.sessionStorage && originalSessionGet && originalSessionSet) {
        globalThis.sessionStorage.getItem = originalSessionGet;
        globalThis.sessionStorage.setItem = originalSessionSet;
      }
    }
  });

  test('preserves unique owner runtime fields exactly on remote live load', async () => {
    const owner = buildSnapshot({
      projectId: 'remote:dev-owner:proj',
      projectKind: 'remote',
      remoteStatus: 'live',
      generatedAt: '2026-07-12T15:16:17.018Z',
      latestTickAt: '2026-07-12T15:15:00.001Z',
      lastDispatchAt: '2026-07-12T15:14:30.002Z',
      lastDispatchedCount: 7,
      maxConcurrentTasks: 6,
      slotsUsed: 4,
      slotsAvailable: 2,
      latestError: 'owner-only-latest-error-p4t7',
      runningTasks: [
        {
          taskId: 'remote:dev-owner:task-owner-p4t7',
          title: 'owner running',
          workflowState: 'inProgress',
          runState: 'running',
          attemptPhase: 'streaming',
          sessionId: 'remote:dev-owner:session-owner-p4t7',
          worktreeId: 'remote:dev-owner:worktree-owner-p4t7',
          lastRuntimeMessage: 'owner streaming p4t7',
          lastActivityAt: '2026-07-12T15:13:00.000Z',
        },
      ],
      recentEvents: [
        {
          id: 'event-owner-p4t7',
          taskId: 'remote:dev-owner:task-owner-p4t7',
          taskTitle: 'owner running',
          kind: 'runner',
          message: 'owner-only-event-fingerprint-p4t7',
          createdAt: '2026-07-12T15:12:00.000Z',
        },
      ],
    });
    getRuntimeSnapshotMock.mockResolvedValueOnce(owner);

    const { result } = renderHook(() =>
      useOrchestratorRuntimeSnapshot({ projectId: 'remote:dev-owner:proj', enabled: true }),
    );
    await waitFor(() => {
      expect(result.current.loading).toBe(false);
    });

    expect(result.current.snapshot).toEqual(owner);
    expect(result.current.snapshot?.generatedAt).toBe('2026-07-12T15:16:17.018Z');
    expect(result.current.snapshot?.latestTickAt).toBe('2026-07-12T15:15:00.001Z');
    expect(result.current.snapshot?.slotsUsed).toBe(4);
    expect(result.current.snapshot?.recentEvents[0]?.message).toBe(
      'owner-only-event-fingerprint-p4t7',
    );
    expect(result.current.remoteStatus).toBe('live');
    expect(result.current.cachedAt).toBeNull();
  });
});
