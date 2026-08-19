/**
 * 移动端终端 Commit 快捷键与 Git 面板共用的 commit mutation 执行器。
 *
 * Business Logic（为什么需要这个模块）:
 *   终端右侧 FAB 与 Git 历史 Commit 必须走同一条 API：不弹手写 message，后端 `git add -A` 后用
 *   Claude Code 生成提交信息；timeout/network 只能 unknown 对账，禁止盲重放。
 *
 * Code Logic（这个模块做什么）:
 *   注入 git client 与 isCurrent 守卫，执行 commit 或仅对账，返回 typed outcome。
 */

import {
  isMutationFailedHook,
  isMutationSucceeded,
  isMutationUnknown,
} from '@/lib/asyncState/mutationOutcome';
import type {
  WorkbenchHookFailure,
  WorkbenchMutationEnvelope,
  WorkbenchMutationOperation,
  WorkbenchWorktree,
} from '@/lib/types';
import { reconcileWorkbenchMutation } from '@/lib/workbenchMutationReconciliation';

export interface MobileGitCommitRequest {
  worktreeId: string;
  message?: string | null;
  clientOperationId: string;
}

export interface MobileGitCommitGitClient {
  commit(
    request: MobileGitCommitRequest,
  ): Promise<WorkbenchMutationEnvelope<WorkbenchWorktree>>;
  getMutationOperation(clientOperationId: string): Promise<WorkbenchMutationOperation | null>;
}

export type MobileGitCommitOutcome =
  | { type: 'stale' }
  | { type: 'succeeded'; worktree: WorkbenchWorktree }
  | { type: 'succeededRefresh' }
  | { type: 'unknown' }
  | { type: 'failedHook'; hookFailure: WorkbenchHookFailure }
  | { type: 'failed' };

export interface ExecuteMobileGitCommitParams {
  worktreeId: string;
  clientOperationId: string;
  /** unknown 相位只查询 ledger，不再次 POST commit。 */
  reconcileOnly: boolean;
  isCurrent: () => boolean;
  git: MobileGitCommitGitClient;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   unknown 后必须用同一 clientOperationId 对账，不能猜成败。
 *
 * Code Logic（这个函数做什么）:
 *   查询 ledger；succeeded/failed 走 reconcileWorkbenchMutation；缺 intent 时按 ledger.state 判定。
 */
async function reconcileMobileGitCommitUnknown(
  clientOperationId: string,
  isCurrent: () => boolean,
  git: MobileGitCommitGitClient,
): Promise<MobileGitCommitOutcome> {
  const ledger = await git.getMutationOperation(clientOperationId);
  if (!isCurrent()) return { type: 'stale' };
  if (!ledger) return { type: 'unknown' };
  const confirmed = reconcileWorkbenchMutation(ledger.intent, ledger, {});
  if (confirmed === 'confirmedSucceeded') return { type: 'succeededRefresh' };
  if (confirmed === 'confirmedFailed') return { type: 'failed' };
  return { type: 'unknown' };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   终端 Commit FAB 与桌面 Git 历史按钮必须发出同一 mutation：message=null、稳定 operation id。
 *
 * Code Logic（这个函数做什么）:
 *   reconcileOnly 时只对账；否则 POST commit(message=null)；每步后检查 isCurrent。
 */
export async function executeMobileGitCommit(
  params: ExecuteMobileGitCommitParams,
): Promise<MobileGitCommitOutcome> {
  const { worktreeId, clientOperationId, reconcileOnly, isCurrent, git } = params;
  if (reconcileOnly) {
    return reconcileMobileGitCommitUnknown(clientOperationId, isCurrent, git);
  }

  const envelope = await git.commit({
    worktreeId,
    message: null,
    clientOperationId,
  });
  if (!isCurrent()) return { type: 'stale' };

  if (isMutationSucceeded(envelope)) {
    return { type: 'succeeded', worktree: envelope.value };
  }
  if (isMutationFailedHook(envelope)) {
    return { type: 'failedHook', hookFailure: envelope.hookFailure };
  }
  if (isMutationUnknown(envelope)) {
    return reconcileMobileGitCommitUnknown(envelope.clientOperationId, isCurrent, git);
  }
  return { type: 'unknown' };
}
