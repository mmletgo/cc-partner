// @vitest-environment jsdom
/**
 * MobileWorktreePanel remove envelope 对账测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   remove unknown 后查询 ledger 并用 identity 矩阵对账，禁止盲重放。
 *
 * Code Logic（这个测试做什么）:
 *   mock workbenchHttp.git.remove/getMutationOperation 与 confirm，断言调用次数。
 */

import { afterEach, beforeAll, beforeEach, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';

import i18n from '@/i18n';
import type { WorkbenchProject, WorkbenchWorktree } from '@/lib/types';

const removeMock = vi.fn();
const getMutationOperationMock = vi.fn();
const listWorktreesMock = vi.fn();
const refreshMock = vi.fn(async () => undefined);

vi.mock('@/api/workbenchHttp', () => ({
  createHttpOrchestratorClientRequestId: () => 'op-remove-1',
  workbenchHttp: {
    git: {
      remove: (...args: unknown[]) => removeMock(...args),
      getMutationOperation: (...args: unknown[]) => getMutationOperationMock(...args),
      commit: vi.fn(),
      push: vi.fn(),
      merge: vi.fn(),
    },
  },
  httpWorkbenchTransport: {
    worktrees: {
      list: (...args: unknown[]) => listWorktreesMock(...args),
      create: vi.fn(),
    },
  },
}));

import { MobileWorktreePanel } from './MobileWorktreePanel';

function createProject(): WorkbenchProject {
  return {
    id: 'project-1',
    name: 'demo',
    kind: 'local',
    deviceId: 'd1',
    deviceName: 'Mac',
    path: '/tmp/demo',
    lastOpenedAt: '2026-07-14T00:00:00Z',
    createdAt: '2026-07-14T00:00:00Z',
    updatedAt: '2026-07-14T00:00:00Z',
  };
}

function createWorktree(
  overrides: Partial<WorkbenchWorktree> & Pick<WorkbenchWorktree, 'id' | 'name'>,
): WorkbenchWorktree {
  return {
    id: overrides.id,
    projectId: overrides.projectId ?? 'project-1',
    name: overrides.name,
    branch: overrides.branch ?? overrides.name,
    baseBranch: overrides.baseBranch ?? 'main',
    path: overrides.path ?? `/tmp/${overrides.name}`,
    isMain: overrides.isMain ?? false,
    status: overrides.status ?? {
      branch: overrides.branch ?? overrides.name,
      changed: 0,
      ahead: 0,
      behind: 0,
      conflicts: 0,
      clean: true,
      canPush: false,
    },
    createdAt: overrides.createdAt ?? '2026-07-14T00:00:00Z',
    updatedAt: overrides.updatedAt ?? '2026-07-14T00:00:00Z',
  };
}

beforeAll(async () => {
  await i18n.changeLanguage('zh');
  vi.spyOn(window, 'confirm').mockReturnValue(true);
});

beforeEach(() => {
  removeMock.mockReset();
  getMutationOperationMock.mockReset();
  listWorktreesMock.mockReset();
  refreshMock.mockClear();
  listWorktreesMock.mockResolvedValue([
    createWorktree({ id: 'main', name: 'main', isMain: true }),
  ]);
});

afterEach(() => {
  cleanup();
});

describe('MobileWorktreePanel remove reconciliation', () => {
  /**
   * Business Logic（为什么需要这个测试）:
   *   remove unknown 且源 worktree 已不在列表 → confirmedSucceeded 可推进；
   *   对账期间不得二次 remove。
   *
   * Code Logic（这个测试做什么）:
   *   envelope unknown；ledger remove intent；list 不含源 id → 成功刷新。
   */
  test('unknown remove confirms when identity absent and does not replay', async () => {
    const feature = createWorktree({ id: 'feature-1', name: 'feature/x' });
    const main = createWorktree({ id: 'main', name: 'main', isMain: true });
    removeMock.mockResolvedValue({
      kind: 'unknown',
      clientOperationId: 'op-remove-1',
      transportClass: 'timeout',
    });
    getMutationOperationMock.mockResolvedValue({
      clientOperationId: 'op-remove-1',
      kind: 'remove',
      payloadHash: 'hash',
      intent: {
        kind: 'remove',
        projectId: 'project-1',
        worktreeId: 'feature-1',
        path: feature.path,
        branch: feature.branch,
      },
      state: 'succeeded',
      outcome: null,
      errorMessage: null,
      projectId: 'project-1',
      worktreeId: 'feature-1',
      createdAt: '2026-07-14T00:00:00Z',
      updatedAt: '2026-07-14T00:00:00Z',
    });
    listWorktreesMock.mockResolvedValue([main]);

    render(
      <I18nextProvider i18n={i18n}>
        <MobileWorktreePanel
          project={createProject()}
          worktrees={[main, feature]}
          activeWorktreeId="main"
          onSelect={() => true}
          onRefreshWorktrees={refreshMock}
        />
      </I18nextProvider>,
    );

    const removeButtons = screen.getAllByRole('button', { name: '移除 worktree' });
    // feature worktree is second row
    fireEvent.click(removeButtons[removeButtons.length - 1]!);

    await waitFor(() => {
      expect(removeMock).toHaveBeenCalledTimes(1);
      expect(getMutationOperationMock).toHaveBeenCalledWith('op-remove-1');
      expect(refreshMock).toHaveBeenCalled();
    });
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   identity 仍在时保持 unknown，禁用盲重放。
   *
   * Code Logic（这个测试做什么）:
   *   list 仍含源 worktree → 显示未知结果，remove 仅 1 次。
   */
  test('unknown remove stays unknown when identity still present', async () => {
    const feature = createWorktree({ id: 'feature-1', name: 'feature/x' });
    const main = createWorktree({ id: 'main', name: 'main', isMain: true });
    removeMock.mockResolvedValue({
      kind: 'unknown',
      clientOperationId: 'op-remove-1',
      transportClass: 'network',
    });
    getMutationOperationMock.mockResolvedValue({
      clientOperationId: 'op-remove-1',
      kind: 'remove',
      payloadHash: 'hash',
      intent: {
        kind: 'remove',
        projectId: 'project-1',
        worktreeId: 'feature-1',
        path: feature.path,
        branch: feature.branch,
      },
      state: 'running',
      outcome: null,
      errorMessage: null,
      projectId: 'project-1',
      worktreeId: 'feature-1',
      createdAt: '2026-07-14T00:00:00Z',
      updatedAt: '2026-07-14T00:00:00Z',
    });
    listWorktreesMock.mockResolvedValue([main, feature]);

    render(
      <I18nextProvider i18n={i18n}>
        <MobileWorktreePanel
          project={createProject()}
          worktrees={[main, feature]}
          activeWorktreeId="main"
          onSelect={() => true}
          onRefreshWorktrees={refreshMock}
        />
      </I18nextProvider>,
    );

    const removeButtons = screen.getAllByRole('button', { name: '移除 worktree' });
    fireEvent.click(removeButtons[removeButtons.length - 1]!);

    await screen.findByText(/操作结果未知/);
    expect(removeMock).toHaveBeenCalledTimes(1);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   merge/remove 目标常不是 active worktree；切换 active 不得清空 unknown 锁，避免盲重放。
   *
   * Code Logic（这个测试做什么）:
   *   feature unknown 后 rerender 切换 active；仍显示未知；再次 remove 只 reconcile，不二次 remove。
   */
  test('switching active worktree during unknown does not unlock blind re-remove', async () => {
    const feature = createWorktree({ id: 'feature-1', name: 'feature/x' });
    const other = createWorktree({ id: 'feature-2', name: 'feature/y' });
    const main = createWorktree({ id: 'main', name: 'main', isMain: true });
    removeMock.mockResolvedValue({
      kind: 'unknown',
      clientOperationId: 'op-remove-1',
      transportClass: 'timeout',
    });
    getMutationOperationMock.mockResolvedValue({
      clientOperationId: 'op-remove-1',
      kind: 'remove',
      payloadHash: 'hash',
      intent: {
        kind: 'remove',
        projectId: 'project-1',
        worktreeId: 'feature-1',
        path: feature.path,
        branch: feature.branch,
      },
      state: 'running',
      outcome: null,
      errorMessage: null,
      projectId: 'project-1',
      worktreeId: 'feature-1',
      createdAt: '2026-07-14T00:00:00Z',
      updatedAt: '2026-07-14T00:00:00Z',
    });
    listWorktreesMock.mockResolvedValue([main, feature, other]);

    const view = render(
      <I18nextProvider i18n={i18n}>
        <MobileWorktreePanel
          project={createProject()}
          worktrees={[main, feature, other]}
          activeWorktreeId="main"
          onSelect={() => true}
          onRefreshWorktrees={refreshMock}
        />
      </I18nextProvider>,
    );

    // 列表含 main（disabled）+ feature-1 + feature-2；点 feature-1（index 1）
    const removeButtons = screen.getAllByRole('button', { name: '移除 worktree' });
    fireEvent.click(removeButtons[1]!);

    await screen.findByText(/操作结果未知/);
    expect(removeMock).toHaveBeenCalledTimes(1);

    view.rerender(
      <I18nextProvider i18n={i18n}>
        <MobileWorktreePanel
          project={createProject()}
          worktrees={[main, feature, other]}
          activeWorktreeId="feature-2"
          onSelect={() => true}
          onRefreshWorktrees={refreshMock}
        />
      </I18nextProvider>,
    );

    // active 切换后 unknown 锁仍在，动作仍锁，不得盲重放。
    expect(screen.getByText(/操作结果未知/)).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: /重新核对/ }));

    await waitFor(() => {
      expect(getMutationOperationMock).toHaveBeenCalledTimes(2);
    });
    expect(removeMock).toHaveBeenCalledTimes(1);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   在途 reconcile 完成后若 project 已切换，不得把 unknown phase 写回新上下文。
   *
   * Code Logic（这个测试做什么）:
   *   hang getMutationOperation；切 project；再 resolve；新 project 不应显示旧 unknown。
   */
  test('reconcile after project switch does not dirty-write phase into new context', async () => {
    const feature = createWorktree({ id: 'feature-1', name: 'feature/x' });
    const main = createWorktree({ id: 'main', name: 'main', isMain: true });
    removeMock.mockResolvedValue({
      kind: 'unknown',
      clientOperationId: 'op-remove-1',
      transportClass: 'network',
    });

    const deferredLedger: {
      resolve: ((value: unknown) => void) | null;
    } = { resolve: null };
    getMutationOperationMock
      .mockResolvedValueOnce({
        clientOperationId: 'op-remove-1',
        kind: 'remove',
        payloadHash: 'hash',
        intent: {
          kind: 'remove',
          projectId: 'project-1',
          worktreeId: 'feature-1',
          path: feature.path,
          branch: feature.branch,
        },
        state: 'running',
        outcome: null,
        errorMessage: null,
        projectId: 'project-1',
        worktreeId: 'feature-1',
        createdAt: '2026-07-14T00:00:00Z',
        updatedAt: '2026-07-14T00:00:00Z',
      })
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            deferredLedger.resolve = resolve;
          }),
      );
    listWorktreesMock.mockResolvedValue([main, feature]);

    const projectA = createProject();
    const projectB = {
      ...createProject(),
      id: 'project-2',
      name: 'other',
    };
    const view = render(
      <I18nextProvider i18n={i18n}>
        <MobileWorktreePanel
          project={projectA}
          worktrees={[main, feature]}
          activeWorktreeId="main"
          onSelect={() => true}
          onRefreshWorktrees={refreshMock}
        />
      </I18nextProvider>,
    );

    const removeButtons = screen.getAllByRole('button', { name: '移除 worktree' });
    fireEvent.click(removeButtons[removeButtons.length - 1]!);
    await screen.findByText(/操作结果未知/);

    fireEvent.click(screen.getByRole('button', { name: /重新核对/ }));
    await waitFor(() => {
      expect(getMutationOperationMock).toHaveBeenCalledTimes(2);
    });

    view.rerender(
      <I18nextProvider i18n={i18n}>
        <MobileWorktreePanel
          project={projectB}
          worktrees={[
            createWorktree({ id: 'main-b', name: 'main', isMain: true, projectId: 'project-2' }),
          ]}
          activeWorktreeId="main-b"
          onSelect={() => true}
          onRefreshWorktrees={refreshMock}
        />
      </I18nextProvider>,
    );

    deferredLedger.resolve?.({
      clientOperationId: 'op-remove-1',
      kind: 'remove',
      payloadHash: 'hash',
      intent: {
        kind: 'remove',
        projectId: 'project-1',
        worktreeId: 'feature-1',
        path: feature.path,
        branch: feature.branch,
      },
      state: 'running',
      outcome: null,
      errorMessage: null,
      projectId: 'project-1',
      worktreeId: 'feature-1',
      createdAt: '2026-07-14T00:00:00Z',
      updatedAt: '2026-07-14T00:00:00Z',
    });

    await waitFor(() => {
      expect(screen.queryByText(/操作结果未知/)).toBeNull();
    });
  });
});
