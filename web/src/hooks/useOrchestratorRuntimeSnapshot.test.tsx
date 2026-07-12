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
});
