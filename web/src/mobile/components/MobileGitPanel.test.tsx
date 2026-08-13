// @vitest-environment jsdom
/**
 * MobileGitPanel mutation envelope 对账测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   typed unknown 后必须查询 ledger 并对账，禁止盲重放新 operation id；
 *   same-message/different-tree 保持 unknown。
 *
 * Code Logic（这个测试做什么）:
 *   mock workbenchHttp.git envelope/ledger，断言 getMutationOperation 与 commit 调用次数。
 */

import { afterEach, beforeAll, beforeEach, describe, expect, test, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';
import type { ReactElement } from 'react';

import i18n from '@/i18n';
import type { WorkbenchProject, WorkbenchWorktree } from '@/lib/types';

const commitMock = vi.fn();
const pushMock = vi.fn();
const getMutationOperationMock = vi.fn();
const listCommitsMock = vi.fn();
const listWorktreesMock = vi.fn();
const refreshWorktreesMock = vi.fn(async () => undefined);
const onWorktreeChangeMock = vi.fn();

vi.mock('@/api/workbenchHttp', () => ({
  createHttpOrchestratorClientRequestId: vi
    .fn()
    .mockReturnValueOnce('op-1')
    .mockReturnValueOnce('op-2')
    .mockReturnValue('op-next'),
  workbenchHttp: {
    git: {
      commit: (...args: unknown[]) => commitMock(...args),
      push: (...args: unknown[]) => pushMock(...args),
      merge: vi.fn(),
      remove: vi.fn(),
      getMutationOperation: (...args: unknown[]) => getMutationOperationMock(...args),
    },
  },
  httpWorkbenchTransport: {
    git: {
      listCommits: (...args: unknown[]) => listCommitsMock(...args),
    },
    worktrees: {
      list: (...args: unknown[]) => listWorktreesMock(...args),
    },
  },
}));

import { MobileGitPanel } from './MobileGitPanel';

/**
 * Business Logic（为什么需要这个函数）:
 *   测试需要最小合法项目/worktree DTO。
 *
 * Code Logic（这个函数做什么）:
 *   补齐必填字段并允许 dirty status 覆盖。
 */
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

function createWorktree(): WorkbenchWorktree {
  return {
    id: 'wt-1',
    projectId: 'project-1',
    name: 'feature/x',
    branch: 'feature/x',
    baseBranch: 'main',
    path: '/tmp/demo-feature',
    isMain: false,
    canCollectMerge: false,
    homeBranch: null,
    collectibleBranches: [],
    status: {
      branch: 'feature/x',
      changed: 1,
      ahead: 0,
      behind: 0,
      conflicts: 0,
      clean: false,
      canPush: true,
    },
    createdAt: '2026-07-14T00:00:00Z',
    updatedAt: '2026-07-14T00:00:00Z',
  };
}

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

beforeEach(() => {
  commitMock.mockReset();
  pushMock.mockReset();
  getMutationOperationMock.mockReset();
  listCommitsMock.mockReset();
  listWorktreesMock.mockReset();
  refreshWorktreesMock.mockClear();
  onWorktreeChangeMock.mockClear();
  listCommitsMock.mockResolvedValue([]);
  listWorktreesMock.mockResolvedValue([createWorktree()]);
});

afterEach(() => {
  cleanup();
});

/**
 * Business Logic（为什么需要这个函数）:
 *   组件测试必须挂 i18n。
 *
 * Code Logic（这个函数做什么）:
 *   I18nextProvider 包裹面板。
 */
function renderPanel(ui: ReactElement = (
  <MobileGitPanel
    project={createProject()}
    worktree={createWorktree()}
    onMergeWorktree={async () => true}
    onRefreshWorktrees={refreshWorktreesMock}
    onWorktreeChange={onWorktreeChangeMock}
  />
)): ReturnType<typeof render> {
  return render(<I18nextProvider i18n={i18n}>{ui}</I18nextProvider>);
}

describe('MobileGitPanel mutation reconciliation', () => {
  /**
   * Business Logic（为什么需要这个测试）:
   *   unknown envelope 后查询 ledger 并对账；不得再次 commit。
   *
   * Code Logic（这个测试做什么）:
   *   commit 返回 unknown；ledger 无 head 权威字段 → 保持 unknown；commit 仅 1 次。
   */
  test('unknown commit queries ledger without blind replay', async () => {
    commitMock.mockResolvedValue({
      kind: 'unknown',
      clientOperationId: 'op-1',
      transportClass: 'timeout',
    });
    getMutationOperationMock.mockResolvedValue({
      clientOperationId: 'op-1',
      kind: 'commit',
      payloadHash: 'hash',
      intent: {
        kind: 'commit',
        projectId: 'project-1',
        worktreeId: 'wt-1',
        beforeHead: 'abc',
        expectedTree: 'tree-expected',
      },
      state: 'running',
      outcome: null,
      errorMessage: null,
      projectId: 'project-1',
      worktreeId: 'wt-1',
      createdAt: '2026-07-14T00:00:00Z',
      updatedAt: '2026-07-14T00:00:00Z',
    });

    renderPanel();
    await waitFor(() => expect(listCommitsMock).toHaveBeenCalled());

    fireEvent.click(screen.getByRole('button', { name: 'Commit' }));

    await waitFor(() => {
      expect(commitMock).toHaveBeenCalledTimes(1);
      expect(getMutationOperationMock).toHaveBeenCalledWith('op-1');
    });

    await screen.findByText(/操作结果未知/);
    // 阻断失败：StatusMessage role=alert 恰好一次；busy 按钮 accessible name 仍为 Commit
    expect(screen.getAllByRole('alert')).toHaveLength(1);
    expect(screen.getByRole('button', { name: 'Commit' })).toBeTruthy();

    // 动作锁定：再次点击不会盲重放新 commit
    fireEvent.click(screen.getByRole('button', { name: 'Commit' }));
    expect(commitMock).toHaveBeenCalledTimes(1);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   same-message/different-tree 不能用 message 猜成功；authority 无 headTree 且 ledger 非终态时 unknown。
   *
   * Code Logic（这个测试做什么）:
   *   ledger state=running + intent expectedTree 存在但 authority 无 headTree → unknown 文案。
   */
  test('same-message different-tree stays unknown without head authority', async () => {
    commitMock.mockResolvedValue({
      kind: 'unknown',
      clientOperationId: 'op-1',
      transportClass: 'network',
    });
    getMutationOperationMock.mockResolvedValue({
      clientOperationId: 'op-1',
      kind: 'commit',
      payloadHash: 'hash',
      intent: {
        kind: 'commit',
        projectId: 'project-1',
        worktreeId: 'wt-1',
        beforeHead: 'parent',
        expectedTree: 'tree-A',
      },
      state: 'running',
      outcome: null,
      errorMessage: null,
      projectId: 'project-1',
      worktreeId: 'wt-1',
      createdAt: '2026-07-14T00:00:00Z',
      updatedAt: '2026-07-14T00:00:00Z',
    });

    renderPanel();
    await waitFor(() => expect(listCommitsMock).toHaveBeenCalled());
    fireEvent.click(screen.getByRole('button', { name: 'Commit' }));

    await screen.findByText(/操作结果未知/);
    expect(commitMock).toHaveBeenCalledTimes(1);
    expect(onWorktreeChangeMock).not.toHaveBeenCalled();
    // same-id recovery 入口可见
    expect(screen.getByRole('button', { name: /重新核对|Retry verify/i })).toBeTruthy();
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   ledger 终态 succeeded 应确认成功，即使无 head 权威字段。
   *
   * Code Logic（这个测试做什么）:
   *   unknown envelope + ledger state=succeeded → 不显示 unknown 锁。
   */
  test('ledger terminal succeeded confirms without head authority', async () => {
    commitMock.mockResolvedValue({
      kind: 'unknown',
      clientOperationId: 'op-ledger',
      transportClass: 'timeout',
    });
    getMutationOperationMock.mockResolvedValue({
      clientOperationId: 'op-ledger',
      kind: 'commit',
      payloadHash: 'hash',
      intent: {
        kind: 'commit',
        projectId: 'project-1',
        worktreeId: 'wt-1',
        beforeHead: 'parent',
        expectedTree: 'tree-A',
      },
      state: 'succeeded',
      outcome: null,
      errorMessage: null,
      projectId: 'project-1',
      worktreeId: 'wt-1',
      createdAt: '2026-07-14T00:00:00Z',
      updatedAt: '2026-07-14T00:00:00Z',
    });

    renderPanel();
    await waitFor(() => expect(listCommitsMock).toHaveBeenCalled());
    fireEvent.click(screen.getByRole('button', { name: 'Commit' }));

    await waitFor(() => {
      expect(getMutationOperationMock).toHaveBeenCalledWith('op-ledger');
      expect(refreshWorktreesMock).toHaveBeenCalled();
    });
    expect(screen.queryByText(/操作结果未知/)).toBeNull();
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   succeeded envelope 应立即推进 worktree 并刷新。
   *
   * Code Logic（这个测试做什么）:
   *   commit 返回 succeeded，断言 onWorktreeChange 与 refresh。
   */
  test('succeeded commit advances UI without ledger query', async () => {
    const next = createWorktree();
    next.status = { ...next.status, clean: true, changed: 0 };
    commitMock.mockResolvedValue({
      kind: 'succeeded',
      value: next,
      clientOperationId: 'op-1',
    });

    renderPanel();
    await waitFor(() => expect(listCommitsMock).toHaveBeenCalled());
    fireEvent.click(screen.getByRole('button', { name: 'Commit' }));

    await waitFor(() => {
      expect(onWorktreeChangeMock).toHaveBeenCalledWith(next);
      expect(refreshWorktreesMock).toHaveBeenCalled();
    });
    expect(getMutationOperationMock).not.toHaveBeenCalled();
  });
});
