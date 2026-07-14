/**
 * Workbench Git mutation 纯对账矩阵。
 *
 * Business Logic（为什么需要这个模块）:
 *   commit/push/merge/remove 在 timeout/network 下不能盲重放；前端用 intent + 权威后置条件
 *   确认是否已生效，与 Rust `confirm_mutation` 语义对齐。
 *
 * Code Logic（这个模块做什么）:
 *   导出 `reconcileWorkbenchMutation`；ledger 仅作可选上下文（pending 不影响矩阵），
 *   判定只依赖 intent + authorityAfter。
 */

import type {
  MutationAuthoritySnapshot,
  MutationIntent,
  WorkbenchMutationOperation,
} from '@/lib/types';

/** 纯对账结果：仅 confirmedSucceeded | unknown。 */
export type WorkbenchMutationReconcileResult = 'confirmedSucceeded' | 'unknown';

/**
 * Business Logic（为什么需要这个函数）:
 *   unknown envelope 后，controller 用 ledger intent 与刷新后的权威状态确认是否已成功。
 *
 * Code Logic（这个函数做什么）:
 *   镜像 Rust confirm_mutation：
 *   - commit: headTree==expectedTree 且 ((headParent==beforeHead && head!=beforeHead) 或 head==beforeHead)
 *   - push: remoteRefHead == localHead
 *   - merge: mainContainsSourceHead===true && sourceWorktreePresent===false
 *   - remove: worktreeIdentityPresent===false
 *   ledger 为 null/pending 时仍只按 intent+authority 判定；intent 与 ledger.intent 不一致时
 *   不阻断矩阵（调用方负责选择 intent 源）。
 */
export function reconcileWorkbenchMutation(
  intent: MutationIntent,
  _ledger: WorkbenchMutationOperation | null | undefined,
  authorityAfter: MutationAuthoritySnapshot,
): WorkbenchMutationReconcileResult {
  switch (intent.kind) {
    case 'commit': {
      const headTree = authorityAfter.headTree;
      if (headTree == null || headTree !== intent.expectedTree) {
        return 'unknown';
      }
      const head = authorityAfter.head ?? null;
      const headParent = authorityAfter.headParent ?? null;
      const beforeHead = intent.beforeHead;
      // 有新 commit：parent == beforeHead 且 HEAD 前进
      if (
        headParent === beforeHead
        && head != null
        && head !== beforeHead
      ) {
        return 'confirmedSucceeded';
      }
      // no-op：HEAD 未变且 tree 匹配
      if (head === beforeHead) {
        return 'confirmedSucceeded';
      }
      return 'unknown';
    }
    case 'push': {
      if (authorityAfter.remoteRefHead === intent.localHead) {
        return 'confirmedSucceeded';
      }
      return 'unknown';
    }
    case 'merge': {
      if (
        authorityAfter.mainContainsSourceHead === true
        && authorityAfter.sourceWorktreePresent === false
      ) {
        return 'confirmedSucceeded';
      }
      return 'unknown';
    }
    case 'remove': {
      if (authorityAfter.worktreeIdentityPresent === false) {
        return 'confirmedSucceeded';
      }
      return 'unknown';
    }
    default: {
      // 前向兼容：未知 kind 不得猜成功。
      return 'unknown';
    }
  }
}
