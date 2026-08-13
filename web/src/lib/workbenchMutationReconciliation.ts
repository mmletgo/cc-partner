/**
 * Workbench Git mutation 纯对账矩阵。
 *
 * Business Logic（为什么需要这个模块）:
 *   commit/push/merge/remove 在 timeout/network 下不能盲重放；前端用 intent + 权威后置条件
 *   确认是否已生效，与 Rust `confirm_mutation` 语义对齐。
 *   ledger 终态（succeeded/failed）优先于稀疏 authority，避免 commit/push 永远卡 unknown。
 *
 * Code Logic（这个模块做什么）:
 *   导出 `reconcileWorkbenchMutation` 与 merge/remove authority 构建 helper。
 */

import type {
  MutationAuthoritySnapshot,
  MutationIntent,
  WorkbenchMutationOperation,
} from '@/lib/types';

/** 纯对账结果：终态成功 / 终态失败 / 仍不确定。 */
export type WorkbenchMutationReconcileResult =
  | 'confirmedSucceeded'
  | 'confirmedFailed'
  | 'unknown';

/**
 * Business Logic（为什么需要这个函数）:
 *   unknown envelope 后，controller 用 ledger intent 与刷新后的权威状态确认是否已成功。
 *
 * Code Logic（这个函数做什么）:
 *   1. ledger.state === 'succeeded' → confirmedSucceeded（不猜 git identity）
 *   2. ledger.state === 'failed' → confirmedFailed（允许新 id）
 *   3. pending/claimed/running/null → 镜像 Rust confirm_mutation 矩阵：
 *      - commit: headTree==expectedTree 且 ((headParent==beforeHead && head!=beforeHead) 或 head==beforeHead)
 *      - push: remoteRefHead == localHead
 *      - merge: mainContainsSourceHead===true && sourceWorktreePresent===false
 *      - collectMerge: mainContainsSourceHead===true（主工作区留下，不要求源消失）
 *      - remove: worktreeIdentityPresent===false
 */
export function reconcileWorkbenchMutation(
  intent: MutationIntent,
  ledger: WorkbenchMutationOperation | null | undefined,
  authorityAfter: MutationAuthoritySnapshot,
): WorkbenchMutationReconcileResult {
  if (ledger?.state === 'succeeded') {
    return 'confirmedSucceeded';
  }
  if (ledger?.state === 'failed') {
    return 'confirmedFailed';
  }

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
    case 'collectMerge': {
      if (authorityAfter.mainContainsSourceHead === true) {
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

/**
 * Business Logic（为什么需要这个函数）:
 *   desktop/mobile 对账路径需要同一套 merge/remove authority 填充，避免分叉漏掉
 *   mainContainsSourceHead。
 *
 * Code Logic（这个函数做什么）:
 *   - merge: sourceWorktreePresent 来自列表；mainContainsSourceHead 在提供 mainCommitHashes 时
 *     判断是否包含 intent.sourceHead（找不到则为 false，不猜 true）。
 *   - collectMerge: 全部 sources[].oid 都出现在 mainCommitHashes 时 mainContainsSourceHead=true；
 *     不填 sourceWorktreePresent（主工作区不会被删）。
 *   - remove: worktreeIdentityPresent 来自列表。
 *   - 其它 intent 返回空快照。
 */
export function buildMergeRemoveAuthority(
  intent: MutationIntent,
  worktrees: ReadonlyArray<{ id: string }>,
  options?: { mainCommitHashes?: ReadonlyArray<string> },
): MutationAuthoritySnapshot {
  if (intent.kind === 'merge') {
    const sourceWorktreePresent = worktrees.some(
      (item) => item.id === intent.sourceWorktreeId,
    );
    const hashes = options?.mainCommitHashes;
    const mainContainsSourceHead =
      hashes === undefined
        ? undefined
        : hashes.some(
            (hash) =>
              hash === intent.sourceHead
              || intent.sourceHead.startsWith(hash)
              || hash.startsWith(intent.sourceHead),
          );
    return {
      sourceWorktreePresent,
      mainContainsSourceHead,
    };
  }
  if (intent.kind === 'collectMerge') {
    const hashes = options?.mainCommitHashes;
    const mainContainsSourceHead =
      hashes === undefined
        ? undefined
        : intent.sources.every((source) =>
            hashes.some(
              (hash) =>
                hash === source.oid
                || source.oid.startsWith(hash)
                || hash.startsWith(source.oid),
            ),
          );
    return { mainContainsSourceHead };
  }
  if (intent.kind === 'remove') {
    return {
      worktreeIdentityPresent: worktrees.some((item) => item.id === intent.worktreeId),
    };
  }
  return {};
}
