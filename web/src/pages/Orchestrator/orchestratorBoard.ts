import type { OrchestratorRenderableTask } from '@/lib/orchestratorRemote';
import type { OrchestratorRunState, OrchestratorWorkflowState } from '@/lib/types';

/** 任务块最少成员数。 */
export const MIN_ORCHESTRATOR_BLOCK_MEMBERS = 2;
/** 任务块最多成员数。 */
export const MAX_ORCHESTRATOR_BLOCK_MEMBERS = 8;

/** 允许打开创建弹窗的泳道。 */
export const ORCHESTRATOR_LANE_CREATE_STATES: readonly OrchestratorWorkflowState[] = [
  'backlog',
  'todo',
];

/** 允许块末尾追加的 head 泳道。 */
export const ORCHESTRATOR_BLOCK_APPEND_STATES: readonly OrchestratorWorkflowState[] = [
  'backlog',
  'todo',
  'inProgress',
];

/**
 * Orchestrator 看板卡片：独立任务或串行任务块。
 *
 * Business Logic（为什么需要这个类型）:
 *   块成员不得再以独立卡片出现在其它泳道，必须聚合成一块卡片落在 head 泳道。
 *
 * Code Logic（这个类型做什么）:
 *   task 卡片包装单个渲染项；block 卡片携带按 blockIndex 排序的成员。
 */
export type OrchestratorBoardItem =
  | { kind: 'task'; item: OrchestratorRenderableTask }
  | {
      kind: 'block';
      blockId: string;
      title: string;
      members: OrchestratorRenderableTask[];
    };

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
  OrchestratorBoardItem[]
>;

/**
 * Business Logic（为什么需要这个函数）:
 *   空泳道必须始终存在，拖拽目标与 Lane + 才能稳定渲染。
 *
 * Code Logic（这个函数做什么）:
 *   按固定泳道顺序创建空数组。
 */
export function createEmptyOrchestratorBoardGroups(): OrchestratorBoardGroups {
  return ORCHESTRATOR_BOARD_LANES.reduce<OrchestratorBoardGroups>((acc, lane) => {
    acc[lane] = [];
    return acc;
  }, {} as OrchestratorBoardGroups);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   块内展示顺序必须跟 blockIndex 一致，不能按列表返回顺序。
 *
 * Code Logic（这个函数做什么）:
 *   按 blockIndex 升序，缺失 index 视为 0，再用 createdAt/id 稳定打破平局。
 */
export function sortBlockMembers(
  members: OrchestratorRenderableTask[],
): OrchestratorRenderableTask[] {
  return [...members].sort((left, right) => {
    const leftIndex = left.task.blockIndex ?? 0;
    const rightIndex = right.task.blockIndex ?? 0;
    if (leftIndex !== rightIndex) return leftIndex - rightIndex;
    const created = left.task.createdAt.localeCompare(right.task.createdAt);
    if (created !== 0) return created;
    return left.task.id.localeCompare(right.task.id);
  });
}

/**
 * Business Logic（为什么需要这个函数）:
 *   块卡片必须落在第一个未完成成员所在泳道；全完成则进 Done。
 *
 * Code Logic（这个函数做什么）:
 *   跳过 done/canceled，取第一个开放成员的 workflowState；否则返回 done。
 */
export function blockHeadLane(
  members: OrchestratorRenderableTask[],
): OrchestratorWorkflowState {
  const open = sortBlockMembers(members).find((member) => {
    const state = member.task.workflowState;
    return state !== 'done' && state !== 'canceled';
  });
  return open?.task.workflowState ?? 'done';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   看板需要按 blockId 聚合成块卡片，避免同一块拆到多列。
 *
 * Code Logic（这个函数做什么）:
 *   无 blockId 的任务按自身 workflowState 入列；有 blockId 的成员排序后整块放入 head 泳道。
 */
export function groupBoardItems(tasks: OrchestratorRenderableTask[]): OrchestratorBoardGroups {
  const groups = createEmptyOrchestratorBoardGroups();
  const blocks = new Map<string, OrchestratorRenderableTask[]>();

  for (const task of tasks) {
    const blockId = task.task.blockId?.trim();
    if (!blockId) {
      groups[task.task.workflowState].push({ kind: 'task', item: task });
      continue;
    }
    const members = blocks.get(blockId) ?? [];
    members.push(task);
    blocks.set(blockId, members);
  }

  for (const [blockId, members] of blocks) {
    const sorted = sortBlockMembers(members);
    const title =
      sorted.find((member) => member.task.blockTitle?.trim())?.task.blockTitle?.trim() ||
      sorted[0]?.task.title ||
      blockId;
    groups[blockHeadLane(sorted)].push({
      kind: 'block',
      blockId,
      title,
      members: sorted,
    });
  }

  return groups;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   自动化看板需要按任务当前 workflowState 展示任务，同时保留空泳道用于拖拽目标。
 *
 * Code Logic（这个函数做什么）:
 *   先按固定泳道顺序创建空数组，再遍历渲染任务并按 task.task.workflowState 追加到对应泳道。
 */
export function groupRenderableTasksByWorkflowState(
  tasks: OrchestratorRenderableTask[],
): Record<OrchestratorWorkflowState, OrchestratorRenderableTask[]> {
  const groups = ORCHESTRATOR_BOARD_LANES.reduce<
    Record<OrchestratorWorkflowState, OrchestratorRenderableTask[]>
  >((acc, lane) => {
    acc[lane] = [];
    return acc;
  }, {} as Record<OrchestratorWorkflowState, OrchestratorRenderableTask[]>);

  for (const task of tasks) {
    groups[task.task.workflowState].push(task);
  }

  return groups;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Lane + 只出现在 Backlog/Todo，其它泳道不得提供创建入口。
 *
 * Code Logic（这个函数做什么）:
 *   判断泳道是否属于 ORCHESTRATOR_LANE_CREATE_STATES。
 */
export function canCreateInLane(lane: OrchestratorWorkflowState): boolean {
  return ORCHESTRATOR_LANE_CREATE_STATES.includes(lane);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   追加只允许 head 仍在 backlog/todo/inProgress，且无人进入复核/返工/合入/交付。
 *
 * Code Logic（这个函数做什么）:
 *   检查人数、head 泳道、禁止泳道与 delivering runState。
 */
export function canAppendToBlock(members: OrchestratorRenderableTask[]): boolean {
  if (members.length >= MAX_ORCHESTRATOR_BLOCK_MEMBERS) return false;
  const head = blockHeadLane(members);
  if (!ORCHESTRATOR_BLOCK_APPEND_STATES.includes(head)) return false;
  return members.every((member) => {
    const workflow = member.task.workflowState;
    if (
      workflow === 'humanReview' ||
      workflow === 'rework' ||
      workflow === 'merging'
    ) {
      return false;
    }
    return member.task.runState !== 'delivering';
  });
}

/**
 * Business Logic（为什么需要这个函数）:
 *   重排只能在整块尚未开工时进行，避免打乱已跑过的共享 worktree 历史。
 *
 * Code Logic（这个函数做什么）:
 *   全部成员必须是 backlog 或 todo，且 runState=idle。
 */
export function canReorderBlock(members: OrchestratorRenderableTask[]): boolean {
  if (members.length < MIN_ORCHESTRATOR_BLOCK_MEMBERS) return false;
  return members.every((member) => {
    const workflow = member.task.workflowState;
    return (workflow === 'backlog' || workflow === 'todo') && member.task.runState === 'idle';
  });
}

/**
 * Business Logic（为什么需要这个函数）:
 *   已开工的块不能整块拖泳道；只有全部仍 backlog 或全部仍 todo 才允许在这两泳道间相邻拖。
 *
 * Code Logic（这个函数做什么）:
 *   校验全员同泳道、非活跃运行态，再套用相邻泳道规则。
 */
export function canMoveBoardBlockToWorkflowState(
  members: OrchestratorRenderableTask[],
  targetState: OrchestratorWorkflowState,
): boolean {
  if (members.length === 0) return false;
  if (members.some((member) => isActiveOrchestratorRunState(member.task.runState))) {
    return false;
  }
  const lanes = new Set(members.map((member) => member.task.workflowState));
  if (lanes.size !== 1) return false;
  const head = members[0];
  if (!head) return false;
  const current = head.task.workflowState;
  if (current !== 'backlog' && current !== 'todo') return false;
  if (current === targetState) return false;
  return canMoveRenderableTaskToWorkflowState(
    {
      ...head,
      task: { ...head.task, workflowState: current, runState: 'idle' },
    },
    targetState,
  );
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
 *   看板拖拽可调整本机或远端非活跃任务，且每次只能移动到相邻 workflow 泳道；运行中任务不得改泳道。
 *   Backlog→Todo 与空闲非堵塞→Rework 的入队副作用由后端 move-workflow-state 完成，前端只判断相邻/非运行中。
 *
 * Code Logic（这个函数做什么）:
 *   对 null、active run state、相同泳道和非相邻泳道逐项拒绝；local/remote 只要目标索引相差 1 即返回 true。
 */
export function canMoveRenderableTaskToWorkflowState(
  item: OrchestratorRenderableTask | null,
  targetState: OrchestratorWorkflowState,
): boolean {
  if (!item) return false;
  if (isActiveOrchestratorRunState(item.task.runState)) return false;
  if (item.task.workflowState === targetState) return false;

  const currentIndex = ORCHESTRATOR_BOARD_LANES.indexOf(item.task.workflowState);
  const targetIndex = ORCHESTRATOR_BOARD_LANES.indexOf(targetState);
  if (currentIndex === -1 || targetIndex === -1) return false;

  return Math.abs(currentIndex - targetIndex) === 1;
}
