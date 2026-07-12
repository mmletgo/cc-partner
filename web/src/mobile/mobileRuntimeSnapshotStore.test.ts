import { describe, test, beforeEach } from 'vitest';
import type { OrchestratorRuntimeSnapshot } from '@/lib/types';
import {
  applyMobileRuntimeSnapshotFailure,
  applyMobileRuntimeSnapshotSuccess,
  beginMobileRuntimeSnapshotLoad,
  getMobileRuntimeSnapshotCacheSize,
  isCurrentMobileRuntimeSnapshotRequest,
  nextMobileRuntimeSnapshotRequestSeq,
  resetMobileRuntimeSnapshotStore,
} from './mobileRuntimeSnapshotStore';

/**
 * Business Logic（为什么需要这个函数）:
 *   store 单测需要快速失败并带上下文，避免 vitest 默认断言信息过短。
 *
 * Code Logic（这个函数做什么）:
 *   condition 为 false 时抛 Error。
 */
function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   多场景单测需要最小合法 snapshot fixture。
 *
 * Code Logic（这个函数做什么）:
 *   返回可覆盖 projectId/remoteStatus 的空运行时快照。
 */
function makeSnapshot(
  overrides: Partial<OrchestratorRuntimeSnapshot> = {},
): OrchestratorRuntimeSnapshot {
  return {
    projectId: 'project-1',
    projectKind: 'remote',
    remoteStatus: 'live',
    generatedAt: '2026-07-11T10:00:00.000Z',
    latestTickAt: '2026-07-11T10:00:01.000Z',
    lastDispatchAt: '2026-07-11T10:00:01.000Z',
    lastDispatchedCount: 1,
    schedulerEnabled: true,
    workflowSource: 'WORKFLOW.md',
    workflowValid: true,
    workflowError: null,
    maxConcurrentTasks: 1,
    slotsUsed: 0,
    slotsAvailable: 1,
    latestError: null,
    runningTasks: [],
    retryingTasks: [],
    recentEvents: [],
    ...overrides,
  };
}

describe('mobileRuntimeSnapshotStore', () => {
  beforeEach(() => {
    resetMobileRuntimeSnapshotStore();
  });

  test('caches successful live snapshot per project and reuses it on offline', () => {
    const seqA = nextMobileRuntimeSnapshotRequestSeq('project-a');
    const live = makeSnapshot({
      projectId: 'project-a',
      remoteStatus: 'live',
      generatedAt: '2026-07-11T12:00:00.000Z',
    });
    const success = applyMobileRuntimeSnapshotSuccess(
      'project-a',
      seqA,
      live,
      '2026-07-11T12:00:05.000Z',
    );
    assert(success !== null, 'live success should apply');
    assert(success?.snapshot?.generatedAt === '2026-07-11T12:00:00.000Z', 'live snapshot preserved');
    assert(success?.remoteStatus === 'live', 'live remoteStatus');
    assert(success?.cachedAt === '2026-07-11T12:00:05.000Z', 'cachedAt is client receipt time');

    const seqOffline = nextMobileRuntimeSnapshotRequestSeq('project-a');
    const offline = applyMobileRuntimeSnapshotSuccess(
      'project-a',
      seqOffline,
      makeSnapshot({ projectId: 'project-a', remoteStatus: 'offline' }),
    );
    assert(offline !== null, 'offline success should apply');
    assert(
      offline?.snapshot?.generatedAt === '2026-07-11T12:00:00.000Z',
      'offline should keep last live snapshot for display',
    );
    assert(offline?.remoteStatus === 'offline', 'offline status');
    assert(offline?.cachedAt === '2026-07-11T12:00:05.000Z', 'offline keeps prior cachedAt');
  });

  test('discards stale responses when a newer request exists', () => {
    const staleSeq = nextMobileRuntimeSnapshotRequestSeq('project-1');
    const freshSeq = nextMobileRuntimeSnapshotRequestSeq('project-1');
    assert(
      !isCurrentMobileRuntimeSnapshotRequest('project-1', staleSeq),
      'stale seq should not be current',
    );
    assert(
      isCurrentMobileRuntimeSnapshotRequest('project-1', freshSeq),
      'fresh seq should be current',
    );

    const staleResult = applyMobileRuntimeSnapshotSuccess(
      'project-1',
      staleSeq,
      makeSnapshot({ generatedAt: 'stale' }),
    );
    assert(staleResult === null, 'stale success must be discarded');
    assert(getMobileRuntimeSnapshotCacheSize() === 0, 'stale success must not write cache');

    const freshResult = applyMobileRuntimeSnapshotSuccess(
      'project-1',
      freshSeq,
      makeSnapshot({ generatedAt: 'fresh' }),
      '2026-07-11T13:00:00.000Z',
    );
    assert(freshResult?.snapshot?.generatedAt === 'fresh', 'fresh success applies');
  });

  test('cold offline has empty snapshot while unsupported/unavailable clear cache', () => {
    const coldSeq = nextMobileRuntimeSnapshotRequestSeq('cold');
    const coldOffline = applyMobileRuntimeSnapshotSuccess(
      'cold',
      coldSeq,
      makeSnapshot({ projectId: 'cold', remoteStatus: 'offline' }),
    );
    assert(coldOffline?.snapshot === null, 'cold offline snapshot is null');
    assert(coldOffline?.cachedAt === null, 'cold offline cachedAt is null');
    assert(coldOffline?.remoteStatus === 'offline', 'cold offline status');

    const liveSeq = nextMobileRuntimeSnapshotRequestSeq('project-x');
    applyMobileRuntimeSnapshotSuccess(
      'project-x',
      liveSeq,
      makeSnapshot({ projectId: 'project-x', remoteStatus: 'live' }),
      '2026-07-11T14:00:00.000Z',
    );
    assert(getMobileRuntimeSnapshotCacheSize() === 1, 'live writes cache');

    const unsupportedSeq = nextMobileRuntimeSnapshotRequestSeq('project-x');
    const unsupported = applyMobileRuntimeSnapshotSuccess(
      'project-x',
      unsupportedSeq,
      makeSnapshot({ projectId: 'project-x', remoteStatus: 'unsupported' }),
    );
    assert(unsupported?.snapshot === null, 'unsupported clears display snapshot');
    assert(unsupported?.remoteStatus === 'unsupported', 'unsupported status');
    assert(getMobileRuntimeSnapshotCacheSize() === 0, 'unsupported clears cache');

    const liveSeq2 = nextMobileRuntimeSnapshotRequestSeq('project-y');
    applyMobileRuntimeSnapshotSuccess(
      'project-y',
      liveSeq2,
      makeSnapshot({ projectId: 'project-y', remoteStatus: 'live' }),
    );
    const unavailableSeq = nextMobileRuntimeSnapshotRequestSeq('project-y');
    const unavailable = applyMobileRuntimeSnapshotSuccess(
      'project-y',
      unavailableSeq,
      makeSnapshot({ projectId: 'project-y', remoteStatus: 'unavailable' }),
    );
    assert(unavailable?.remoteStatus === 'unavailable', 'unavailable status');
    assert(unavailable?.snapshot === null, 'unavailable clears snapshot');
    assert(getMobileRuntimeSnapshotCacheSize() === 0, 'unavailable clears cache');
  });

  test('does not share cache across projects and never uses localStorage', () => {
    const originalGetItem = globalThis.localStorage?.getItem?.bind(globalThis.localStorage);
    const originalSetItem = globalThis.localStorage?.setItem?.bind(globalThis.localStorage);
    let storageTouched = false;
    if (globalThis.localStorage) {
      globalThis.localStorage.getItem = ((...args: Parameters<Storage['getItem']>) => {
        storageTouched = true;
        return originalGetItem?.(...args) ?? null;
      }) as Storage['getItem'];
      globalThis.localStorage.setItem = ((...args: Parameters<Storage['setItem']>) => {
        storageTouched = true;
        originalSetItem?.(...args);
      }) as Storage['setItem'];
    }

    try {
      const seqA = nextMobileRuntimeSnapshotRequestSeq('a');
      applyMobileRuntimeSnapshotSuccess(
        'a',
        seqA,
        makeSnapshot({ projectId: 'a', generatedAt: 'A' }),
        't-a',
      );
      const seqB = nextMobileRuntimeSnapshotRequestSeq('b');
      applyMobileRuntimeSnapshotSuccess(
        'b',
        seqB,
        makeSnapshot({ projectId: 'b', generatedAt: 'B' }),
        't-b',
      );

      const offlineA = applyMobileRuntimeSnapshotSuccess(
        'a',
        nextMobileRuntimeSnapshotRequestSeq('a'),
        makeSnapshot({ projectId: 'a', remoteStatus: 'offline' }),
      );
      assert(offlineA?.snapshot?.generatedAt === 'A', 'project a keeps its own cache');
      assert(offlineA?.snapshot?.projectId === 'a', 'project a cache is isolated');

      const begin = beginMobileRuntimeSnapshotLoad('b');
      assert(begin.loading === true, 'begin load marks loading');
      assert(begin.snapshot?.generatedAt === 'B', 'begin load can surface project b cache');

      const fail = applyMobileRuntimeSnapshotFailure(
        'b',
        nextMobileRuntimeSnapshotRequestSeq('b'),
        new Error('network down'),
      );
      assert(fail?.error?.message === 'network down', 'failure keeps error');
      assert(fail?.snapshot?.generatedAt === 'B', 'failure keeps display cache');
      assert(fail?.remoteStatus === 'offline', 'transport failure maps to offline display');
      assert(!storageTouched, 'store must not touch localStorage');
    } finally {
      if (globalThis.localStorage && originalGetItem && originalSetItem) {
        globalThis.localStorage.getItem = originalGetItem;
        globalThis.localStorage.setItem = originalSetItem;
      }
    }
  });

  test('reset leaves cold empty state and preserves unique owner live fields', () => {
    assert(getMobileRuntimeSnapshotCacheSize() === 0, 'beforeEach reset must cold-start empty');
    const beginCold = beginMobileRuntimeSnapshotLoad('cold-owner');
    assert(beginCold.snapshot === null, 'cold begin has no snapshot');
    assert(beginCold.cachedAt === null, 'cold begin has no cachedAt');
    assert(beginCold.remoteStatus === null, 'cold begin has no remoteStatus');

    const ownerGeneratedAt = '2026-07-12T15:16:17.018Z';
    const ownerEvent = 'owner-only-event-fingerprint-p4t7';
    const live = makeSnapshot({
      projectId: 'owner-mobile',
      remoteStatus: 'live',
      generatedAt: ownerGeneratedAt,
      latestTickAt: '2026-07-12T15:15:00.001Z',
      lastDispatchedCount: 7,
      slotsUsed: 4,
      slotsAvailable: 2,
      latestError: 'owner-only-latest-error-p4t7',
      recentEvents: [
        {
          id: 'event-owner-p4t7',
          taskId: 'task-owner-p4t7',
          taskTitle: 'owner running',
          kind: 'runner',
          message: ownerEvent,
          createdAt: '2026-07-12T15:12:00.000Z',
        },
      ],
    });
    const applied = applyMobileRuntimeSnapshotSuccess(
      'owner-mobile',
      nextMobileRuntimeSnapshotRequestSeq('owner-mobile'),
      live,
      '2026-07-12T15:16:20.000Z',
    );
    assert(applied?.snapshot?.generatedAt === ownerGeneratedAt, 'owner generatedAt preserved');
    assert(applied?.snapshot?.slotsUsed === 4, 'owner slots preserved');
    assert(
      applied?.snapshot?.recentEvents[0]?.message === ownerEvent,
      'owner event message preserved',
    );
    assert(applied?.remoteStatus === 'live', 'live status');
    assert(applied?.cachedAt === '2026-07-12T15:16:20.000Z', 'client receipt cachedAt');
  });
});
