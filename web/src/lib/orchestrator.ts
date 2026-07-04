import type { PillTone } from '@/components/primitives';
import type { OrchestratorTask, OrchestratorTaskStatus } from './types';

/**
 * Business Logic（为什么需要这个类型）:
 *   Orchestrator 页面需要把任务生命周期状态映射为现有 Pill 视觉 tone，避免页面使用不存在的状态颜色。
 *
 * Code Logic（这个类型做什么）:
 *   直接复用 primitives/Pill 的 tone 联合类型，保证 helper 返回值与组件 prop 类型一致。
 */
export type OrchestratorStatusTone = PillTone;

/**
 * Business Logic（为什么需要这个类型）:
 *   队列页需要按生命周期状态展示任务列表，同时保留空状态列用于稳定渲染。
 *
 * Code Logic（这个类型做什么）:
 *   使用 OrchestratorTaskStatus 作为完整 key 集合，每个 key 对应同状态任务数组。
 */
export type OrchestratorTaskGroups = Record<OrchestratorTaskStatus, OrchestratorTask[]>;

/**
 * Business Logic（为什么需要这个类型）:
 *   Orchestrator 页面必须等 Workbench 项目状态稳定后，才能决定是否加载任务，避免无项目时误查全局任务。
 *
 * Code Logic（这个类型做什么）:
 *   waiting 表示项目 Provider 仍在加载；empty 表示没有 active project；load 携带明确 projectId。
 */
export type OrchestratorTaskLoadDecision =
  | { kind: 'waiting' }
  | { kind: 'empty' }
  | { kind: 'load'; projectId: string };

/**
 * Business Logic（为什么需要这个常量）:
 *   Orchestrator 页面需要稳定的状态展示顺序，避免后端返回顺序影响前端队列布局。
 *
 * Code Logic（这个常量做什么）:
 *   按草稿、排队、执行中间态、完成与终止态定义完整生命周期状态序列。
 */
export const ORCHESTRATOR_STATUSES: readonly OrchestratorTaskStatus[] = [
  'draft',
  'queued',
  'preparing',
  'running',
  'verifying',
  'delivering',
  'done',
  'blocked',
  'aborted',
];

/**
 * Business Logic（为什么需要这个函数）:
 *   Orchestrator 队列视图需要把后端返回的扁平任务列表拆分到各状态栏目中。
 *
 * Code Logic（这个函数做什么）:
 *   先按完整状态集合创建空数组，再单次遍历 tasks，把每个任务追加到对应状态数组并返回。
 */
export function groupOrchestratorTasks(tasks: OrchestratorTask[]): OrchestratorTaskGroups {
  const groups = ORCHESTRATOR_STATUSES.reduce<OrchestratorTaskGroups>((acc, status) => {
    acc[status] = [];
    return acc;
  }, {} as OrchestratorTaskGroups);

  for (const task of tasks) {
    groups[task.status].push(task);
  }

  return groups;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   页面需要明确区分“项目列表还在加载”和“加载完成但没有项目”，避免把空项目当作全局任务查询。
 *
 * Code Logic（这个函数做什么）:
 *   projectsLoading 为 true 时返回 waiting；activeProjectId 为空时返回 empty；否则返回携带 projectId 的 load。
 */
export function resolveOrchestratorTaskLoad(
  projectsLoading: boolean,
  activeProjectId: string | null | undefined,
): OrchestratorTaskLoadDecision {
  if (projectsLoading) return { kind: 'waiting' };
  if (!activeProjectId) return { kind: 'empty' };
  return { kind: 'load', projectId: activeProjectId };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用户创建任务期间可能切换项目，旧项目的返回结果不能插入当前项目队列。
 *
 * Code Logic（这个函数做什么）:
 *   比较当前 activeProjectId 与提交时捕获的 projectId，只有完全一致才允许应用创建结果。
 */
export function orchestratorCreateResultMatchesProject(
  currentProjectId: string | null | undefined,
  submittedProjectId: string,
): boolean {
  return currentProjectId === submittedProjectId;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   只有草稿任务应允许用户手动加入队列，避免运行中或已完成任务被重复排队。
 *
 * Code Logic（这个函数做什么）:
 *   对 null 做安全短路；仅当 task.status 为 draft 时返回 true。
 */
export function canQueueOrchestratorTask(task: OrchestratorTask | null): boolean {
  return task?.status === 'draft';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   项目切换期间旧项目任务可能仍在本地状态中，入队按钮必须只允许当前项目的草稿任务操作。
 *
 * Code Logic（这个函数做什么）:
 *   先复用草稿状态校验，再要求任务 projectId 与当前 activeProjectId 完全一致。
 */
export function canQueueOrchestratorTaskForProject(
  task: OrchestratorTask | null,
  currentProjectId: string | null | undefined,
): boolean {
  if (!task || task.status !== 'draft') return false;
  return task.projectId === currentProjectId;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Running 任务详情才应显示“Claude Code 已完成，开始验证”，且项目切换后不能操作旧项目任务。
 *
 * Code Logic（这个函数做什么）:
 *   对 null 做安全短路；仅当 task.status 为 running 且 projectId 匹配当前项目时返回 true。
 */
export function canCompleteAgentRunForProject(
  task: OrchestratorTask | null,
  currentProjectId: string | null | undefined,
): boolean {
  if (!task || task.status !== 'running') return false;
  return task.projectId === currentProjectId;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Blocked 任务详情才应显示打开 Workbench、重试和终止控制，避免误操作其它状态任务。
 *
 * Code Logic（这个函数做什么）:
 *   对 null 做安全短路；仅当 task.status 为 blocked 且 projectId 匹配当前项目时返回 true。
 */
export function canControlBlockedTaskForProject(
  task: OrchestratorTask | null,
  currentProjectId: string | null | undefined,
): boolean {
  if (!task || task.status !== 'blocked') return false;
  return task.projectId === currentProjectId;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   队列和详情区域需要用一致颜色表达任务状态，并且只能使用 Pill 已支持的 tone。
 *
 * Code Logic（这个函数做什么）:
 *   将成功态映射为 success，阻塞/终止态映射为 danger，执行中间态映射为 accent，草稿/排队映射为 neutral。
 */
export function orchestratorStatusTone(status: OrchestratorTaskStatus): OrchestratorStatusTone {
  switch (status) {
    case 'done':
      return 'success';
    case 'blocked':
    case 'aborted':
      return 'danger';
    case 'preparing':
    case 'running':
    case 'verifying':
    case 'delivering':
      return 'accent';
    case 'draft':
    case 'queued':
      return 'neutral';
  }
}
