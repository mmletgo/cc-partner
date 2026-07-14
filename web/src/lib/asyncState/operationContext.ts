/**
 * 操作上下文键与序列：防止旧 project/worktree 异步结果污染新上下文。
 *
 * Business Logic（为什么需要这个模块）:
 *   Workbench Git 长操作的 success/catch/finally 若无 context 校验，
 *   会把旧 project/worktree 的 busy/error 写入用户已切换到的新上下文。
 *
 * Code Logic（这个模块做什么）:
 *   定义 WorkbenchOperationKey 与 isCurrentOperation / nextOperationSequence 纯函数。
 */

/**
 * Workbench 异步 mutation 的上下文键。
 *
 * Business Logic（为什么需要这个类型）:
 *   commit/push/merge/remove 等操作必须绑定 project + worktree + 单调 sequence。
 *
 * Code Logic（字段说明）:
 *   projectId 为项目 shortcut id；worktreeId 可为 null（项目级操作）；
 *   sequence 为该上下文内的操作序号。
 */
export type WorkbenchOperationKey = {
  projectId: string;
  worktreeId: string | null;
  sequence: number;
};

/**
 * Business Logic（为什么需要这个函数）:
 *   success/catch/finally 每个写入点都必须确认仍处于发起操作时的上下文。
 *
 * Code Logic（这个函数做什么）:
 *   projectId、worktreeId、sequence 三者严格相等时返回 true。
 */
export function isCurrentOperation(
  current: WorkbenchOperationKey,
  settled: WorkbenchOperationKey,
): boolean {
  return (
    current.projectId === settled.projectId
    && current.worktreeId === settled.worktreeId
    && current.sequence === settled.sequence
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   发起新操作时需要单调递增 sequence，使旧结算自然过期。
 *
 * Code Logic（这个函数做什么）:
 *   返回 current + 1。
 */
export function nextOperationSequence(current: number): number {
  return current + 1;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   切换 project/worktree 时构造新 key，并重置或延续 sequence。
 *
 * Code Logic（这个函数做什么）:
 *   用给定 projectId/worktreeId 与 sequence 组装 WorkbenchOperationKey。
 */
export function createOperationKey(
  projectId: string,
  worktreeId: string | null,
  sequence: number,
): WorkbenchOperationKey {
  return { projectId, worktreeId, sequence };
}
