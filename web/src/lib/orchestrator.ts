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
