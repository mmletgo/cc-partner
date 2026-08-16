// @vitest-environment jsdom
/**
 * useMobileWorktreeBarController 创建/删除编排测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   条上新建必须复用桌面 createWorktreeWithTerminalWindow（窗口失败不回滚 worktree）；
 *   删除必须走 git.remove envelope，取消 dirty guard 不得打后端。
 *
 * Code Logic（这个测试做什么）:
 *   mock HTTP transport；renderHook 断言 applyCreated/applyRemoval 与 refresh 调用。
 */
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, renderHook, waitFor } from '@testing-library/react';
import type { ReactNode } from 'react';
import { I18nextProvider } from 'react-i18next';
import i18n from '@/i18n';
import type { WorkbenchProject, WorkbenchSession, WorkbenchWorktree } from '@/lib/types';

const createWorktreeMock = vi.fn();
const createSessionMock = vi.fn();
const removeMock = vi.fn();
const getMutationOperationMock = vi.fn();

vi.mock('@/api/workbenchHttp', () => ({
  createHttpOrchestratorClientRequestId: () => 'op-bar-1',
  httpWorkbenchTransport: {
    worktrees: {
      create: (...args: unknown[]) => createWorktreeMock(...args),
      list: vi.fn(),
    },
    sessions: {
      create: (...args: unknown[]) => createSessionMock(...args),
    },
  },
  workbenchHttp: {
    git: {
      remove: (...args: unknown[]) => removeMock(...args),
      getMutationOperation: (...args: unknown[]) => getMutationOperationMock(...args),
    },
  },
}));

import { useMobileWorktreeBarController } from './useMobileWorktreeBarController';

function buildWorktree(
  overrides: Partial<WorkbenchWorktree> & Pick<WorkbenchWorktree, 'id' | 'name'>,
): WorkbenchWorktree {
  return {
    id: overrides.id,
    projectId: overrides.projectId ?? 'project-1',
    name: overrides.name,
    branch: overrides.branch ?? overrides.name,
    baseBranch: overrides.baseBranch ?? null,
    path: overrides.path ?? `/tmp/${overrides.name}`,
    isMain: overrides.isMain ?? false,
    canCollectMerge: false,
    homeBranch: null,
    collectibleBranches: [],
    status: {
      branch: overrides.branch ?? overrides.name,
      changed: 0,
      ahead: 0,
      behind: 0,
      conflicts: 0,
      clean: true,
      canPush: false,
    },
    createdAt: '2026-07-05T00:00:00Z',
    updatedAt: '2026-07-05T00:00:00Z',
  };
}

const project: WorkbenchProject = {
  id: 'project-1',
  name: 'demo',
  kind: 'local',
  deviceId: 'dev-1',
  deviceName: 'this',
  path: '/tmp/demo',
  lastOpenedAt: '2026-07-05T00:00:00Z',
  createdAt: '2026-07-05T00:00:00Z',
  updatedAt: '2026-07-05T00:00:00Z',
};

const main = buildWorktree({ id: 'main', name: 'main', isMain: true });
const created = buildWorktree({ id: 'wt-new', name: 'feature/bar', branch: 'feature/bar' });
const createdSession: WorkbenchSession = {
  id: 'sess-new',
  projectId: 'project-1',
  worktreeId: 'wt-new',
  name: 'window',
  command: 'zsh',
  cwd: '/tmp/feature',
  status: 'running',
  cols: 80,
  rows: 24,
  startedAt: '2026-07-05T00:00:00Z',
  exitedAt: null,
  exitCode: null,
  supportsPanes: true,
  paneCount: 1,
};

function wrapper({ children }: { children: ReactNode }) {
  return <I18nextProvider i18n={i18n}>{children}</I18nextProvider>;
}

afterEach(() => {
  vi.clearAllMocks();
});

describe('useMobileWorktreeBarController', () => {
  beforeEach(() => {
    createWorktreeMock.mockResolvedValue(created);
    createSessionMock.mockResolvedValue(createdSession);
    removeMock.mockResolvedValue({ kind: 'succeeded', value: true, clientOperationId: 'op-bar-1' });
  });

  test('create applies worktree and session then refreshes', async () => {
    const applyCreated = vi.fn();
    const refreshWorktrees = vi.fn(async () => undefined);
    const refreshSessions = vi.fn(async () => undefined);
    const { result } = renderHook(
      () =>
        useMobileWorktreeBarController({
          project,
          worktrees: [main],
          activeWorktreeId: 'main',
          controlsBusy: false,
          confirmSwitchToWorktree: () => true,
          confirmActiveWorktreeChange: () => true,
          applyCreated,
          applyRemoval: vi.fn(),
          beginWorktreeOperation: () => () => undefined,
          refreshWorktrees,
          refreshSessions,
        }),
      { wrapper },
    );

    act(() => {
      result.current.setCreateSuffix('bar');
    });
    await act(async () => {
      await result.current.createWorktree();
    });

    await waitFor(() => {
      expect(createWorktreeMock).toHaveBeenCalledWith('project-1', 'feature/bar', null);
    });
    expect(createSessionMock).toHaveBeenCalled();
    expect(applyCreated).toHaveBeenCalledWith(
      expect.arrayContaining([expect.objectContaining({ id: 'wt-new' })]),
      expect.objectContaining({ id: 'wt-new' }),
      createdSession,
    );
    expect(refreshWorktrees).toHaveBeenCalled();
    expect(refreshSessions).toHaveBeenCalled();
    expect(result.current.createOpen).toBe(false);
  });

  test('create keeps worktree when session create fails', async () => {
    createSessionMock.mockRejectedValue(new Error('session down'));
    const applyCreated = vi.fn();
    const { result } = renderHook(
      () =>
        useMobileWorktreeBarController({
          project,
          worktrees: [main],
          activeWorktreeId: 'main',
          controlsBusy: false,
          confirmSwitchToWorktree: () => true,
          confirmActiveWorktreeChange: () => true,
          applyCreated,
          applyRemoval: vi.fn(),
          beginWorktreeOperation: () => () => undefined,
          refreshWorktrees: vi.fn(async () => undefined),
          refreshSessions: vi.fn(async () => undefined),
        }),
      { wrapper },
    );

    act(() => {
      result.current.setCreateSuffix('bar');
    });
    await act(async () => {
      await result.current.createWorktree();
    });

    expect(applyCreated).toHaveBeenCalledWith(
      expect.arrayContaining([expect.objectContaining({ id: 'wt-new' })]),
      expect.objectContaining({ id: 'wt-new' }),
      null,
    );
    expect(result.current.error).toContain('session down');
  });

  test('remove cancel from dirty guard does not call backend', async () => {
    const feature = buildWorktree({ id: 'feature', name: 'feature/x', branch: 'feature/x' });
    const applyRemoval = vi.fn();
    const { result } = renderHook(
      () =>
        useMobileWorktreeBarController({
          project,
          worktrees: [main, feature],
          activeWorktreeId: 'feature',
          controlsBusy: false,
          confirmSwitchToWorktree: () => true,
          confirmActiveWorktreeChange: () => false,
          applyCreated: vi.fn(),
          applyRemoval,
          beginWorktreeOperation: () => () => undefined,
          refreshWorktrees: vi.fn(async () => undefined),
          refreshSessions: vi.fn(async () => undefined),
        }),
      { wrapper },
    );

    act(() => {
      result.current.requestRemove(feature);
    });
    await act(async () => {
      await result.current.confirmRemove();
    });

    expect(removeMock).not.toHaveBeenCalled();
    expect(applyRemoval).not.toHaveBeenCalled();
  });
});
