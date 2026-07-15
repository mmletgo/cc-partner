/**
 * Transfer 历史分区与动作矩阵纯函数。
 *
 * Business Logic（为什么需要这个模块）:
 *   页面必须把任务分到 active / needs-attention / recent-completed，
 *   并为 failed 任务决定 resume 还是 retry，避免组件内散落分支。
 *
 * Code Logic（这个模块做什么）:
 *   提供分组分类、resumable/retryable 判定、同 logical recovery 互斥与 clientOperationId mint。
 */

import type { TransferTask } from '@/lib/types';

/** 历史列表分区键。 */
export type TransferHistoryGroup = 'active' | 'needsAttention' | 'completed';

/**
 * Business Logic（为什么需要这个函数）:
 *   列表空组应省略；分区键必须与 status/phase/对账态一致。
 *
 * Code Logic（这个函数做什么）:
 *   reconciling → needsAttention；pending/transferring/活跃 phase → active；
 *   failed/cancelled → needsAttention；其余 completed。
 */
export function classifyTransferGroup(
  task: TransferTask,
  reconciling: boolean,
): TransferHistoryGroup {
  if (reconciling) return 'needsAttention';
  if (task.status === 'pending' || task.status === 'transferring') return 'active';
  if (
    task.phase === 'queued' ||
    task.phase === 'connecting' ||
    task.phase === 'transferring' ||
    task.phase === 'finalizing'
  ) {
    return 'active';
  }
  if (task.status === 'failed' || task.status === 'cancelled') return 'needsAttention';
  return 'completed';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   失败且仍有已确认字节/进度时优先续传，而不是全量重传；
 *   旧 peer 无 transfer.resume.v1 时必须回退「重新传输」，不得显示假续传。
 *
 * Code Logic（这个函数做什么）:
 *   仅 Send + failed + retryable（默认 true）且 transferredBytes>0 或 0<progress<1，
 *   且 `peerSupportsResume === true`（缺省 false，fail-closed）。
 */
export function isTransferResumable(
  task: TransferTask,
  peerSupportsResume: boolean = false,
): boolean {
  if (!peerSupportsResume) return false;
  if (task.direction !== 'send') return false;
  if (task.status !== 'failed') return false;
  if (task.failure && !task.failure.retryable) return false;
  const bytes = task.transferredBytes ?? 0;
  if (bytes > 0) return true;
  return task.progress > 0 && task.progress < 1;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   无 resume metadata 的失败、旧 peer 无 resume 能力、或 cancelled，允许用户显式重新传输。
 *
 * Code Logic（这个函数做什么）:
 *   Send 方向：failed 且 retryable 且非 resumable → true；cancelled → true。
 *   `peerSupportsResume` 透传给 isTransferResumable。
 */
export function isTransferRetryable(
  task: TransferTask,
  peerSupportsResume: boolean = false,
): boolean {
  if (task.direction !== 'send') return false;
  if (task.status === 'cancelled') return true;
  if (task.status !== 'failed') return false;
  if (task.failure && !task.failure.retryable) return false;
  return !isTransferResumable(task, peerSupportsResume);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Open/Reveal 仅 same-device 本机已接收 completed 任务。
 *
 * Code Logic（这个函数做什么）:
 *   direction=receive 且 status=completed。
 */
export function canOpenRevealTransfer(task: TransferTask): boolean {
  return task.direction === 'receive' && task.status === 'completed';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   同一 logical transfer 下若已有 child attempt 在 pending/transferring/对账中，
 *   旧 failed 行不得再点 resume/retry 另 mint clientOperationId 并发发送。
 *
 * Code Logic（这个函数做什么）:
 *   解析 logicalId（logicalTransferId 缺省回落 task.id）；扫描 tasks：
 *   同 logical 且 status=pending|transferring、活跃 phase、或 reconciling → true。
 *   自身 failed/cancelled 行若未 reconciling 不锁自己，但 sibling 活跃会锁。
 */
export function isLogicalTransferRecoveryLocked(
  task: TransferTask,
  tasks: readonly TransferTask[],
  reconcilingIds: ReadonlySet<string> = new Set(),
): boolean {
  const logicalId = resolveLogicalTransferId(task);
  for (const candidate of tasks) {
    if (resolveLogicalTransferId(candidate) !== logicalId) continue;
    if (reconcilingIds.has(candidate.id)) return true;
    if (isTransferAttemptActive(candidate)) return true;
  }
  return false;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   recovery 互斥与列表扫描共用同一 logical 身份解析。
 *
 * Code Logic（这个函数做什么）:
 *   非空 logicalTransferId 优先，否则回落 task.id。
 */
export function resolveLogicalTransferId(task: TransferTask): string {
  const logical = task.logicalTransferId?.trim();
  return logical && logical.length > 0 ? logical : task.id;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   判定某 attempt 是否仍占用 logical transfer 的发送槽。
 *
 * Code Logic（这个函数做什么）:
 *   pending/transferring 或 phase 为 queued/connecting/transferring/finalizing。
 */
export function isTransferAttemptActive(task: TransferTask): boolean {
  if (task.status === 'pending' || task.status === 'transferring') return true;
  return (
    task.phase === 'queued' ||
    task.phase === 'connecting' ||
    task.phase === 'transferring' ||
    task.phase === 'finalizing'
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   retry/resume 需要稳定 clientOperationId；同一用户意图在 pending/unknown 期间复用。
 *
 * Code Logic（这个函数做什么）:
 *   优先 crypto.randomUUID；不可用时时间戳+随机串。
 */
export function mintTransferClientOperationId(): string {
  const randomUUID = globalThis.crypto?.randomUUID;
  if (typeof randomUUID === 'function') {
    return randomUUID.call(globalThis.crypto);
  }
  const timePart = Date.now().toString(36);
  const randomPart = Math.random().toString(36).slice(2);
  return `xfer-op-${timePart}-${randomPart}`;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   transport timeout/network 后不得盲重试，必须进入对账。
 *
 * Code Logic（这个函数做什么）:
 *   匹配 TIMEOUT / NETWORK_OFFLINE / timeout / network 类文案或 code。
 */
export function isTransferOutcomeUncertain(err: unknown): boolean {
  if (err == null) return false;
  const code =
    typeof err === 'object' && err !== null && 'code' in err
      ? String((err as { code?: unknown }).code ?? '')
      : '';
  const message = err instanceof Error ? err.message : String(err);
  const hay = `${code} ${message}`.toLowerCase();
  return (
    hay.includes('timeout') ||
    hay.includes('network') ||
    hay.includes('offline') ||
    hay.includes('unavailable') ||
    code === 'TIMEOUT' ||
    code === 'NETWORK_OFFLINE'
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   按分区键拆分任务，保持原列表相对顺序。
 *
 * Code Logic（这个函数做什么）:
 *   返回三个数组；调用方对空数组省略渲染。
 */
export function groupTransferTasks(
  tasks: readonly TransferTask[],
  reconcilingIds: ReadonlySet<string>,
): {
  active: TransferTask[];
  needsAttention: TransferTask[];
  completed: TransferTask[];
} {
  const active: TransferTask[] = [];
  const needsAttention: TransferTask[] = [];
  const completed: TransferTask[] = [];
  for (const task of tasks) {
    const group = classifyTransferGroup(task, reconcilingIds.has(task.id));
    if (group === 'active') active.push(task);
    else if (group === 'needsAttention') needsAttention.push(task);
    else completed.push(task);
  }
  return { active, needsAttention, completed };
}
