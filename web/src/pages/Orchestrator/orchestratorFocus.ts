/**
 * Orchestrator deep-link 焦点解析（Attention task/outbox 分阶段消费）。
 *
 * Business Logic（为什么需要这个模块）:
 *   Inbox 导航到 automation 后必须等任务/outbox 列表加载再聚焦；目标已解决时要
 *   返回类型化 not-found，避免空白详情或打开终端。
 *
 * Code Logic（这个模块做什么）:
 *   纯函数：根据 loading 与 id 集合判定 pending / found / not_found / none。
 */

/**
 * 焦点目标解析结果。
 *
 * Business Logic（为什么需要这个类型）:
 *   Orchestrator 与 Workbench 协调器需要共享同一套结果语义。
 *
 * Code Logic（字段说明）:
 *   status 区分等待/命中/缺失/无目标；kind 标明 task 或 outbox。
 */
export type OrchestratorFocusTargetResult =
  | { status: 'none' }
  | { status: 'pending'; kind: 'task' | 'outbox'; id: string }
  | { status: 'found'; kind: 'task' | 'outbox'; id: string }
  | { status: 'not_found'; kind: 'task' | 'outbox'; id: string };

/**
 * Business Logic（为什么需要这个函数）:
 *   Attention deep link 在列表加载完成前不能判定失败；加载后缺失必须回退 Inbox。
 *
 * Code Logic（这个函数做什么）:
 *   taskId 优先于 outboxId；loading 时返回 pending；列表中无 id 返回 not_found。
 */
export function resolveOrchestratorFocusTarget(input: {
  loading: boolean;
  focusTaskId: string | null | undefined;
  focusOutboxId: string | null | undefined;
  taskIds: readonly string[];
  outboxIds: readonly string[];
}): OrchestratorFocusTargetResult {
  const taskId = input.focusTaskId?.trim() || null;
  const outboxId = input.focusOutboxId?.trim() || null;

  if (taskId) {
    if (input.loading) return { status: 'pending', kind: 'task', id: taskId };
    if (input.taskIds.includes(taskId)) return { status: 'found', kind: 'task', id: taskId };
    return { status: 'not_found', kind: 'task', id: taskId };
  }

  if (outboxId) {
    if (input.loading) return { status: 'pending', kind: 'outbox', id: outboxId };
    if (input.outboxIds.includes(outboxId)) return { status: 'found', kind: 'outbox', id: outboxId };
    return { status: 'not_found', kind: 'outbox', id: outboxId };
  }

  return { status: 'none' };
}
