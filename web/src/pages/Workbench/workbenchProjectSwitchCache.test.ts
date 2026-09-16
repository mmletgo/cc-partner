import { describe, expect, test } from 'vitest';
import type { WorkbenchSession } from '../../lib/types';
import {
  disappearedSessionIds,
  gitHistoryCacheKey,
  mountedSessionsFromProjects,
  pickCachedWorktreeId,
  rememberLastWorktree,
  resolveWorktreeIdAfterProjectSwitch,
  suggestWorktreeIdFromSessions,
} from './workbenchProjectSwitchCache';

/**
 * Business Logic（为什么需要这个函数）:
 *   切项目缓存测试只关心 id / worktree / 创建顺序，不需要完整 session 运行时字段。
 *
 * Code Logic（这个函数做什么）:
 *   返回满足 WorkbenchSession 结构的最小对象。
 */
function session(overrides: Partial<WorkbenchSession> & { id: string }): WorkbenchSession {
  return {
    projectId: 'p1',
    worktreeId: 'wt-main',
    name: overrides.id,
    command: 'claude',
    cwd: '/repo',
    status: 'running',
    cols: 120,
    rows: 30,
    startedAt: '2026-01-01T00:00:00.000Z',
    exitedAt: null,
    exitCode: null,
    supportsPanes: true,
    paneCount: 1,
    ...overrides,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   pickCachedWorktreeId 只读取 id，测试不必构造完整 worktree DTO。
 *
 * Code Logic（这个函数做什么）:
 *   返回带 id 的最小对象。
 */
function worktree(id: string): { id: string } {
  return { id };
}

describe('pickCachedWorktreeId', () => {
  test('returns null when cache is empty', () => {
    expect(pickCachedWorktreeId({ cachedWorktrees: [], lastWorktreeId: 'wt-a' })).toBeNull();
    expect(
      pickCachedWorktreeId({ cachedWorktrees: undefined, lastWorktreeId: 'wt-a' }),
    ).toBeNull();
  });

  test('keeps last worktree when it still exists', () => {
    expect(
      pickCachedWorktreeId({
        cachedWorktrees: [worktree('wt-main'), worktree('wt-feat')],
        lastWorktreeId: 'wt-feat',
      }),
    ).toBe('wt-feat');
  });

  test('falls back to first worktree when last is missing', () => {
    expect(
      pickCachedWorktreeId({
        cachedWorktrees: [worktree('wt-main'), worktree('wt-feat')],
        lastWorktreeId: 'wt-gone',
      }),
    ).toBe('wt-main');
  });
});

describe('suggestWorktreeIdFromSessions', () => {
  test('prefers a running session worktree over exited ones', () => {
    expect(
      suggestWorktreeIdFromSessions([
        session({ id: 'exited', worktreeId: 'wt-old', status: 'exited' }),
        session({ id: 'run', worktreeId: 'wt-live', status: 'running' }),
      ]),
    ).toBe('wt-live');
  });

  test('returns any worktree when no running session exists', () => {
    expect(
      suggestWorktreeIdFromSessions([
        session({ id: 'exited', worktreeId: 'wt-old', status: 'exited' }),
      ]),
    ).toBe('wt-old');
  });

  test('returns null when sessions have no worktree', () => {
    expect(suggestWorktreeIdFromSessions([session({ id: 'legacy', worktreeId: null })])).toBeNull();
    expect(suggestWorktreeIdFromSessions([])).toBeNull();
  });
});

describe('resolveWorktreeIdAfterProjectSwitch', () => {
  test('returns null when leaving all projects', () => {
    expect(
      resolveWorktreeIdAfterProjectSwitch({
        nextProjectId: null,
        cachedWorktrees: [worktree('wt-main')],
        lastWorktreeId: 'wt-main',
        cachedSessions: [session({ id: 's1' })],
      }),
    ).toBeNull();
  });

  test('prefers cached last worktree over session suggestion', () => {
    expect(
      resolveWorktreeIdAfterProjectSwitch({
        nextProjectId: 'p1',
        cachedWorktrees: [worktree('wt-main'), worktree('wt-feat')],
        lastWorktreeId: 'wt-feat',
        cachedSessions: [session({ id: 's1', worktreeId: 'wt-main' })],
      }),
    ).toBe('wt-feat');
  });

  test('uses session worktree on first visit before worktrees.list returns', () => {
    expect(
      resolveWorktreeIdAfterProjectSwitch({
        nextProjectId: 'p2',
        cachedWorktrees: [],
        lastWorktreeId: null,
        cachedSessions: [session({ id: 's1', projectId: 'p2', worktreeId: 'wt-from-session' })],
      }),
    ).toBe('wt-from-session');
  });
});

describe('gitHistoryCacheKey', () => {
  test('uses a stable none token when worktree is missing', () => {
    expect(gitHistoryCacheKey('p1', null)).toBe('p1::__none__');
    expect(gitHistoryCacheKey('p1', 'wt-main')).toBe('p1::wt-main');
  });
});

describe('disappearedSessionIds', () => {
  test('only reports ids that left the same project slice', () => {
    expect(
      disappearedSessionIds(
        [session({ id: 'keep' }), session({ id: 'gone' })],
        [session({ id: 'keep' }), session({ id: 'new' })],
      ),
    ).toEqual(['gone']);
  });

  test('treats missing previous slice as empty', () => {
    expect(disappearedSessionIds(undefined, [session({ id: 's1' })])).toEqual([]);
  });
});

describe('mountedSessionsFromProjects', () => {
  test('keeps sessions from every visited project so xterm can stay mounted', () => {
    const mounted = mountedSessionsFromProjects({
      pB: [session({ id: 'b1', projectId: 'pB', startedAt: '2026-01-02T00:00:00.000Z' })],
      pA: [session({ id: 'a1', projectId: 'pA', startedAt: '2026-01-01T00:00:00.000Z' })],
    });
    expect(mounted.map((item) => item.id)).toEqual(['a1', 'b1']);
  });
});

describe('rememberLastWorktree', () => {
  test('ignores empty project or worktree ids', () => {
    expect(rememberLastWorktree({}, null, 'wt-a')).toEqual({});
    expect(rememberLastWorktree({}, 'p1', null)).toEqual({});
  });

  test('records the latest worktree per project without cloning when unchanged', () => {
    const current = { p1: 'wt-a' };
    expect(rememberLastWorktree(current, 'p1', 'wt-a')).toBe(current);
    expect(rememberLastWorktree(current, 'p1', 'wt-b')).toEqual({ p1: 'wt-b' });
  });
});
