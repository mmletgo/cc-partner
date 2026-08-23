import { describe, expect, test } from 'vitest';
import {
  buildMobileWorkbenchSearch,
  parseMobileWorkbenchLocation,
  resolveMobileWorkbenchLocation,
  resolveRestoredMobileWorkspace,
  syncMobileWorkbenchLocationToHistory,
} from './mobileWorkbenchLocation';
import type { WorkbenchSession, WorkbenchWorktree } from '@/lib/types';

describe('parseMobileWorkbenchLocation', () => {
  test('empty search opens the project list', () => {
    expect(parseMobileWorkbenchLocation('')).toEqual({
      projectId: null,
      panel: 'projects',
      worktreeId: null,
      sessionId: null,
    });
    expect(parseMobileWorkbenchLocation('?')).toEqual({
      projectId: null,
      panel: 'projects',
      worktreeId: null,
      sessionId: null,
    });
  });

  test('projectId without panel restores the terminal workbench', () => {
    expect(parseMobileWorkbenchLocation('?projectId=p1')).toEqual({
      projectId: 'p1',
      panel: 'terminal',
      worktreeId: null,
      sessionId: null,
    });
  });

  test('keeps worktree, session and a project-bound panel', () => {
    expect(
      parseMobileWorkbenchLocation(
        '?projectId=remote%3Adev%3Ap1&panel=files&worktreeId=wt-2&sessionId=s-9',
      ),
    ).toEqual({
      projectId: 'remote:dev:p1',
      panel: 'files',
      worktreeId: 'wt-2',
      sessionId: 's-9',
    });
  });

  test('coerces a project-bound panel without projectId back to the list', () => {
    expect(parseMobileWorkbenchLocation('?panel=terminal')).toEqual({
      projectId: null,
      panel: 'projects',
      worktreeId: null,
      sessionId: null,
    });
  });

  test('unknown or empty panel values fall back to the list or terminal', () => {
    expect(parseMobileWorkbenchLocation('?panel=nope').panel).toBe('projects');
    expect(parseMobileWorkbenchLocation('?projectId=p1&panel=').panel).toBe('terminal');
    expect(parseMobileWorkbenchLocation('?panel=attention')).toEqual({
      projectId: null,
      panel: 'attention',
      worktreeId: null,
      sessionId: null,
    });
  });
});

describe('buildMobileWorkbenchSearch', () => {
  test('writes a stable query for an open terminal workbench', () => {
    expect(
      buildMobileWorkbenchSearch({
        projectId: 'p1',
        panel: 'terminal',
        worktreeId: 'p1:main',
        sessionId: 'p1:s1',
      }),
    ).toBe('?projectId=p1&panel=terminal&worktreeId=p1%3Amain&sessionId=p1%3As1');
  });

  test('omits empty workbench context for the project list', () => {
    expect(
      buildMobileWorkbenchSearch({
        projectId: null,
        panel: 'projects',
        worktreeId: null,
        sessionId: null,
      }),
    ).toBe('');
  });

  test('keeps a global panel without a project', () => {
    expect(
      buildMobileWorkbenchSearch({
        projectId: null,
        panel: 'attention',
        worktreeId: null,
        sessionId: null,
      }),
    ).toBe('?panel=attention');
  });
});

describe('resolveMobileWorkbenchLocation', () => {
  test('clears project context when the user is back on the list', () => {
    expect(
      resolveMobileWorkbenchLocation({
        panel: 'projects',
        projectId: 'p1',
        worktreeId: 'wt',
        sessionId: 's1',
      }),
    ).toEqual({
      projectId: null,
      panel: 'projects',
      worktreeId: null,
      sessionId: null,
    });
  });

  test('keeps project context on in-project shortcut panels', () => {
    expect(
      resolveMobileWorkbenchLocation({
        panel: 'attention',
        projectId: 'p1',
        worktreeId: 'wt',
        sessionId: 's1',
      }),
    ).toEqual({
      projectId: 'p1',
      panel: 'attention',
      worktreeId: 'wt',
      sessionId: 's1',
    });
  });
});

describe('resolveRestoredMobileWorkspace', () => {
  const worktrees: WorkbenchWorktree[] = [
    {
      id: 'wt-main',
      projectId: 'p1',
      name: 'main',
      branch: 'main',
      baseBranch: null,
      path: '/tmp/p1',
      isMain: true,
      canCollectMerge: false,
      homeBranch: null,
      collectibleBranches: [],
      status: {
        branch: 'main',
        changed: 0,
        ahead: 0,
        behind: 0,
        conflicts: 0,
        clean: true,
        canPush: false,
      },
      createdAt: '2026-08-24T00:00:00Z',
      updatedAt: '2026-08-24T00:00:00Z',
    },
    {
      id: 'wt-feat',
      projectId: 'p1',
      name: 'feat',
      branch: 'feat',
      baseBranch: 'main',
      path: '/tmp/p1-feat',
      isMain: false,
      canCollectMerge: true,
      homeBranch: 'main',
      collectibleBranches: ['feat'],
      status: {
        branch: 'feat',
        changed: 0,
        ahead: 0,
        behind: 0,
        conflicts: 0,
        clean: true,
        canPush: false,
      },
      createdAt: '2026-08-24T00:00:00Z',
      updatedAt: '2026-08-24T00:00:00Z',
    },
  ];
  const sessions: WorkbenchSession[] = [
    {
      id: 's-main',
      projectId: 'p1',
      worktreeId: 'wt-main',
      name: 'main-win',
      command: 'zsh',
      cwd: '/tmp/p1',
      status: 'running',
      cols: 80,
      rows: 24,
      startedAt: '2026-08-24T00:00:00Z',
      exitedAt: null,
      exitCode: null,
      supportsPanes: true,
      paneCount: 1,
    },
    {
      id: 's-feat',
      projectId: 'p1',
      worktreeId: 'wt-feat',
      name: 'feat-win',
      command: 'zsh',
      cwd: '/tmp/p1-feat',
      status: 'running',
      cols: 80,
      rows: 24,
      startedAt: '2026-08-24T00:00:00Z',
      exitedAt: null,
      exitCode: null,
      supportsPanes: true,
      paneCount: 1,
    },
  ];

  test('restores the requested worktree and session when they still exist', () => {
    expect(
      resolveRestoredMobileWorkspace(worktrees, sessions, 'wt-feat', 's-feat'),
    ).toEqual({
      worktree: worktrees[1],
      session: sessions[1],
    });
  });

  test('falls back to preferred worktree and session when ids are gone', () => {
    expect(
      resolveRestoredMobileWorkspace(worktrees, sessions, 'missing-wt', 'missing-s'),
    ).toEqual({
      worktree: worktrees[0],
      session: sessions[0],
    });
  });

  test('moves worktree to the restored session owner', () => {
    expect(
      resolveRestoredMobileWorkspace(worktrees, sessions, 'wt-main', 's-feat'),
    ).toEqual({
      worktree: worktrees[1],
      session: sessions[1],
    });
  });
});

describe('syncMobileWorkbenchLocationToHistory', () => {
  test('replaceState only when the query actually changes', () => {
    const replaced: string[] = [];
    syncMobileWorkbenchLocationToHistory(
      {
        projectId: 'p1',
        panel: 'terminal',
        worktreeId: null,
        sessionId: null,
      },
      'http://host/mobile',
      (url) => {
        replaced.push(url);
      },
    );
    expect(replaced).toEqual(['/mobile?projectId=p1&panel=terminal']);

    syncMobileWorkbenchLocationToHistory(
      {
        projectId: 'p1',
        panel: 'terminal',
        worktreeId: null,
        sessionId: null,
      },
      'http://host/mobile?projectId=p1&panel=terminal',
      (url) => {
        replaced.push(url);
      },
    );
    expect(replaced).toEqual(['/mobile?projectId=p1&panel=terminal']);
  });
});
