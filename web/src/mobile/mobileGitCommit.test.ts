/**
 * 移动端 Git commit 快捷键执行器测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   终端 FAB 必须与桌面 Git 历史 Commit 同口径：message=null、unknown 对账不盲重放、钩子失败 typed。
 *
 * Code Logic（这个测试做什么）:
 *   注入假 git client，断言 envelope 分支与 ledger 对账结果。
 */

import { describe, expect, test, vi } from 'vitest';
import type {
  WorkbenchMutationEnvelope,
  WorkbenchMutationOperation,
  WorkbenchWorktree,
} from '@/lib/types';
import { executeMobileGitCommit, type MobileGitCommitGitClient } from './mobileGitCommit';

function createWorktree(overrides: Partial<WorkbenchWorktree> = {}): WorkbenchWorktree {
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
    ...overrides,
  };
}

function createGitClient(
  overrides: Partial<MobileGitCommitGitClient> = {},
): MobileGitCommitGitClient {
  return {
    commit: vi.fn(),
    getMutationOperation: vi.fn(),
    ...overrides,
  };
}

function createLedger(
  overrides: Partial<WorkbenchMutationOperation> = {},
): WorkbenchMutationOperation {
  return {
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
    state: 'succeeded',
    outcome: null,
    errorMessage: null,
    projectId: 'project-1',
    worktreeId: 'wt-1',
    createdAt: '2026-07-14T00:00:00Z',
    updatedAt: '2026-07-14T00:00:00Z',
    ...overrides,
  };
}

describe('executeMobileGitCommit', () => {
  /**
   * Business Logic（为什么需要这个测试）:
   *   与 PC Git 历史 Commit 一致：不传手写 message，交给后端生成。
   *
   * Code Logic（这个测试做什么）:
   *   断言 commit 被调用时 message 为 null。
   */
  test('posts commit with null message', async () => {
    const worktree = createWorktree();
    const git = createGitClient({
      commit: vi.fn(async () => ({
        kind: 'succeeded' as const,
        value: worktree,
        clientOperationId: 'op-1',
      })),
    });

    const outcome = await executeMobileGitCommit({
      worktreeId: 'wt-1',
      clientOperationId: 'op-1',
      reconcileOnly: false,
      isCurrent: () => true,
      git,
    });

    expect(git.commit).toHaveBeenCalledWith({
      worktreeId: 'wt-1',
      message: null,
      clientOperationId: 'op-1',
    });
    expect(outcome).toEqual({ type: 'succeeded', worktree });
    expect(git.getMutationOperation).not.toHaveBeenCalled();
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   用户切走 worktree 后，旧响应不得写成成功。
   *
   * Code Logic（这个测试做什么）:
   *   isCurrent 在 commit 返回后为 false，断言 stale 且不对账。
   */
  test('returns stale when context changed after commit', async () => {
    const git = createGitClient({
      commit: vi.fn(async () => ({
        kind: 'succeeded' as const,
        value: createWorktree(),
        clientOperationId: 'op-1',
      })),
    });

    const outcome = await executeMobileGitCommit({
      worktreeId: 'wt-1',
      clientOperationId: 'op-1',
      reconcileOnly: false,
      isCurrent: () => false,
      git,
    });

    expect(outcome).toEqual({ type: 'stale' });
    expect(git.getMutationOperation).not.toHaveBeenCalled();
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   unknown 必须查 ledger，禁止再 POST 一次新 commit。
   *
   * Code Logic（这个测试做什么）:
   *   envelope unknown + ledger succeeded → succeededRefresh；commit 只 1 次。
   */
  test('unknown envelope queries ledger without a second commit', async () => {
    const git = createGitClient({
      commit: vi.fn(
        async (): Promise<WorkbenchMutationEnvelope<WorkbenchWorktree>> => ({
          kind: 'unknown',
          clientOperationId: 'op-1',
          transportClass: 'timeout',
        }),
      ),
      getMutationOperation: vi.fn(
        async (): Promise<WorkbenchMutationOperation | null> => createLedger(),
      ),
    });

    const outcome = await executeMobileGitCommit({
      worktreeId: 'wt-1',
      clientOperationId: 'op-1',
      reconcileOnly: false,
      isCurrent: () => true,
      git,
    });

    expect(outcome).toEqual({ type: 'succeededRefresh' });
    expect(git.commit).toHaveBeenCalledTimes(1);
    expect(git.getMutationOperation).toHaveBeenCalledWith('op-1');
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   unknown 锁住后再次点击只能对账，不能 mint 新 mutation。
   *
   * Code Logic（这个测试做什么）:
   *   reconcileOnly=true 时不调用 commit，ledger failed → failed。
   */
  test('reconcileOnly skips commit and maps failed ledger', async () => {
    const git = createGitClient({
      commit: vi.fn(),
      getMutationOperation: vi.fn(async () =>
        createLedger({ state: 'failed', errorMessage: 'boom' }),
      ),
    });

    const outcome = await executeMobileGitCommit({
      worktreeId: 'wt-1',
      clientOperationId: 'op-1',
      reconcileOnly: true,
      isCurrent: () => true,
      git,
    });

    expect(git.commit).not.toHaveBeenCalled();
    expect(outcome).toEqual({ type: 'failed' });
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   pre-commit 钩子失败必须 typed 交给 UI，不能当成普通 unknown。
   *
   * Code Logic（这个测试做什么）:
   *   envelope failedHook → failedHook outcome 携带 hookFailure。
   */
  test('returns failedHook envelope to the caller', async () => {
    const hookFailure = {
      stage: 'preCommit' as const,
      stdout: 'lint failed',
      stderr: 'error: trailing whitespace',
      exitCode: 1,
    };
    const git = createGitClient({
      commit: vi.fn(async () => ({
        kind: 'failedHook' as const,
        clientOperationId: 'op-hook',
        hookFailure,
      })),
    });

    const outcome = await executeMobileGitCommit({
      worktreeId: 'wt-1',
      clientOperationId: 'op-hook',
      reconcileOnly: false,
      isCurrent: () => true,
      git,
    });

    expect(outcome).toEqual({ type: 'failedHook', hookFailure });
  });
});
