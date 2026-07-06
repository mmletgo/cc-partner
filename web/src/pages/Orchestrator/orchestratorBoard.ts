import type { OrchestratorRenderableTask } from '@/lib/orchestratorRemote';
import type { OrchestratorRunState, OrchestratorWorkflowState } from '@/lib/types';

/**
 * Orchestrator 工作流看板泳道顺序，必须与后端 WORKFLOW_LANE_ORDER 保持一致。
 */
export const ORCHESTRATOR_BOARD_LANES: readonly OrchestratorWorkflowState[] = [
  'backlog',
  'todo',
  'inProgress',
  'humanReview',
  'rework',
  'merging',
  'done',
  'canceled',
];

/**
 * Orchestrator 看板分组结果。
 *
 * Business Logic（为什么需要这个类型）:
 *   看板 UI 需要稳定渲染全部 workflow 泳道，即使某个泳道当前没有任务。
 *
 * Code Logic（这个类型做什么）:
 *   使用 OrchestratorWorkflowState 作为完整 key 集合，每个 key 对应渲染任务数组。
 */
export type OrchestratorBoardGroups = Record<
  OrchestratorWorkflowState,
  OrchestratorRenderableTask[]
>;

/**
 * Business Logic（为什么需要这个函数）:
 *   自动化看板需要按任务当前 workflowState 展示任务，同时保留空泳道用于拖拽目标。
 *
 * Code Logic（这个函数做什么）:
 *   先按固定泳道顺序创建空数组，再遍历渲染任务并按 task.task.workflowState 追加到对应泳道。
 */
export function groupRenderableTasksByWorkflowState(
  tasks: OrchestratorRenderableTask[],
): OrchestratorBoardGroups {
  const groups = ORCHESTRATOR_BOARD_LANES.reduce<OrchestratorBoardGroups>((acc, lane) => {
    acc[lane] = [];
    return acc;
  }, {} as OrchestratorBoardGroups);

  for (const task of tasks) {
    groups[task.task.workflowState].push(task);
  }

  return groups;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   运行中的任务正在占用 runner 或验证资源，看板不能允许用户拖拽改变其 workflow 泳道。
 *
 * Code Logic（这个函数做什么）:
 *   仅 preparing/running/verifying/delivering 视为活跃运行态，其余 queued/retrying/blocked/idle 均返回 false。
 */
export function isActiveOrchestratorRunState(state: OrchestratorRunState): boolean {
  return (
    state === 'preparing' ||
    state === 'running' ||
    state === 'verifying' ||
    state === 'delivering'
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   看板拖拽只能调整本机、非活跃任务，且每次只能移动到相邻 workflow 泳道，避免远端或运行中任务被本机 UI 误改。
 *
 * Code Logic（这个函数做什么）:
 *   对 null、remote、active run state、相同泳道和非相邻泳道逐项拒绝；只有 local 且目标索引相差 1 时返回 true。
 */
export function canMoveRenderableTaskToWorkflowState(
  item: OrchestratorRenderableTask | null,
  targetState: OrchestratorWorkflowState,
): boolean {
  if (!item) return false;
  if (item.origin !== 'local') return false;
  if (isActiveOrchestratorRunState(item.task.runState)) return false;
  if (item.task.workflowState === targetState) return false;

  const currentIndex = ORCHESTRATOR_BOARD_LANES.indexOf(item.task.workflowState);
  const targetIndex = ORCHESTRATOR_BOARD_LANES.indexOf(targetState);
  if (currentIndex === -1 || targetIndex === -1) return false;

  return Math.abs(currentIndex - targetIndex) === 1;
}
