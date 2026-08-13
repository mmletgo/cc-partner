/**
 * workbenchLaunchState 纯函数合同。
 *
 * Business Logic（为什么需要这个测试）:
 *   启动摘要必须 section 独立失败、空数组真实空态、刷新失败保留 stale 缓存；
 *   这些规则是 UI 与 controller 的共同基础，必须先于 UI 锁定。
 *
 * Code Logic（这个测试做什么）:
 *   覆盖 reduce / map / markStale 与 empty array 语义，不触碰 React。
 */

import { describe, expect, test } from 'vitest';
import {
  createInitialLaunchSummaryState,
  markLaunchSummaryStaleOnFailure,
  reduceWorkbenchLaunchResults,
  type WorkbenchLaunchProject,
  type WorkbenchLaunchSummaryWire,
  type WorkbenchLaunchTransfer,
} from './workbenchLaunchState';

const project: WorkbenchLaunchProject = {
  id: 'p1',
  name: 'demo',
  kind: 'local',
  deviceId: 'self',
  deviceName: 'Mac',
  path: '/tmp/demo',
  lastOpenedAt: '2026-07-14T00:00:00.000Z',
};

const transfer: WorkbenchLaunchTransfer = {
  id: 't1',
  filename: 'a.zip',
  status: 'failed',
  direction: 'send',
};

function buildWire(
  overrides: Partial<WorkbenchLaunchSummaryWire> = {},
): WorkbenchLaunchSummaryWire {
  return {
    projects: { kind: 'ready', value: [project] },
    sessions: { kind: 'ready', value: [] },
    tasks: { kind: 'ready', value: [] },
    transfers: { kind: 'ready', value: [] },
    generatedAt: '2026-07-14T12:00:00.000Z',
    ...overrides,
  };
}

describe('workbenchLaunchState', () => {
  test('one failed launch resource preserves the others', () => {
    const previous = createInitialLaunchSummaryState();
    const state = reduceWorkbenchLaunchResults(
      previous,
      buildWire({
        projects: { kind: 'ready', value: [project] },
        transfers: { kind: 'error', message: 'offline' },
      }),
    );

    expect(state.projects.kind).toBe('ready');
    if (state.projects.kind === 'ready') {
      expect(state.projects.value).toEqual([project]);
      expect(state.projects.stale).toBe(false);
    }
    expect(state.transfers.kind).toBe('error');
    if (state.transfers.kind === 'error') {
      expect(state.transfers.message).toBe('offline');
      expect(state.transfers.cached).toBeUndefined();
    }
    expect(state.sessions.kind).toBe('ready');
    expect(state.tasks.kind).toBe('ready');
  });

  test('loading reduces to ready and error independently', () => {
    const previous = createInitialLaunchSummaryState();
    expect(previous.projects.kind).toBe('loading');

    const state = reduceWorkbenchLaunchResults(
      previous,
      buildWire({
        projects: { kind: 'ready', value: [project] },
        sessions: { kind: 'error', message: 'sessions down' },
      }),
    );

    expect(state.projects).toEqual({ kind: 'ready', value: [project], stale: false });
    expect(state.sessions).toEqual({ kind: 'error', message: 'sessions down' });
    expect(state.generatedAt).toBe('2026-07-14T12:00:00.000Z');
  });

  test('stale retention on refresh failure keeps cached ready values', () => {
    const ready = reduceWorkbenchLaunchResults(
      createInitialLaunchSummaryState(),
      buildWire({
        projects: { kind: 'ready', value: [project] },
        transfers: { kind: 'ready', value: [transfer] },
      }),
    );

    const stale = markLaunchSummaryStaleOnFailure(ready, 'network offline');

    expect(stale.projects).toEqual({ kind: 'ready', value: [project], stale: true });
    expect(stale.transfers).toEqual({ kind: 'ready', value: [transfer], stale: true });
    expect(stale.sessions).toEqual({ kind: 'ready', value: [], stale: true });
    expect(stale.generatedAt).toBe('2026-07-14T12:00:00.000Z');
  });

  test('refresh failure converts loading sections to error without inventing values', () => {
    const loading = createInitialLaunchSummaryState();
    const failed = markLaunchSummaryStaleOnFailure(loading, 'timeout');

    expect(failed.projects).toEqual({ kind: 'error', message: 'timeout' });
    expect(failed.sessions).toEqual({ kind: 'error', message: 'timeout' });
    expect(failed.generatedAt).toBeNull();
  });

  test('section error after ready retains cached value', () => {
    const ready = reduceWorkbenchLaunchResults(
      createInitialLaunchSummaryState(),
      buildWire({ transfers: { kind: 'ready', value: [transfer] } }),
    );

    const next = reduceWorkbenchLaunchResults(
      ready,
      buildWire({ transfers: { kind: 'error', message: 'repo failed' } }),
    );

    expect(next.transfers).toEqual({
      kind: 'error',
      message: 'repo failed',
      cached: [transfer],
    });
  });

  test('empty arrays are real empty (not fabricated metrics)', () => {
    const state = reduceWorkbenchLaunchResults(
      createInitialLaunchSummaryState(),
      buildWire({
        projects: { kind: 'ready', value: [] },
        sessions: { kind: 'ready', value: [] },
        tasks: { kind: 'ready', value: [] },
        transfers: { kind: 'ready', value: [] },
      }),
    );

    for (const key of ['projects', 'sessions', 'tasks', 'transfers'] as const) {
      const section = state[key];
      expect(section.kind).toBe('ready');
      if (section.kind === 'ready') {
        expect(Array.isArray(section.value)).toBe(true);
        expect(section.value).toHaveLength(0);
      }
    }
  });
});
