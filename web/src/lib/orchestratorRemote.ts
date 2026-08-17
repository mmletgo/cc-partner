import { ORCHESTRATOR_STATUSES } from './orchestrator';
import type {
  OrchestratorRemoteOutboxItem,
  OrchestratorRemoteRuntimeStatus,
  OrchestratorTask,
  OrchestratorTaskStatus,
  OrchestratorTaskView,
} from './types';

/**
 * Business Logic（为什么需要这个类型）:
 *   Orchestrator 队列需要展示本机和远端真实任务，同时保留远端设备信息用于提示任务来源。
 *
 * Code Logic（这个类型做什么）:
 *   把 local/remote task view 展平成渲染项；pendingRemote 不会进入该结构。
 */
export interface OrchestratorRenderableTask {
  origin: 'local' | 'remote';
  task: OrchestratorTask;
  deviceId: string | null;
  deviceName: string | null;
  view: OrchestratorTaskView;
}

/**
 * Business Logic（为什么需要这个类型）:
 *   队列页需要同时渲染真实任务列表和远端待发送列表，两者操作能力不同。
 *
 * Code Logic（这个类型做什么）:
 *   tasks 保存 local/remote 可分组任务；pendingRemoteItems 保存不可操作的 outbox 条目。
 */
export interface OrchestratorTaskViewSplit {
  tasks: OrchestratorRenderableTask[];
  pendingRemoteItems: OrchestratorRemoteOutboxItem[];
}

/**
 * Business Logic（为什么需要这个类型）:
 *   真实任务按生命周期分组时仍要保留 local/remote 元信息，不能退化成裸 OrchestratorTask。
 *
 * Code Logic（这个类型做什么）:
 *   使用 OrchestratorTaskStatus 作为完整 key 集合，每个 key 对应渲染任务数组。
 */
export type OrchestratorRenderableTaskGroups = Record<
  OrchestratorTaskStatus,
  OrchestratorRenderableTask[]
>;

/**
 * Business Logic（为什么需要这个函数）:
 *   页面需要把后端 tagged union 拆成真实任务和远端待发送项，避免 pending outbox 混入状态分组。
 *
 * Code Logic（这个函数做什么）:
 *   单次遍历 task views；local/remote 转为 OrchestratorRenderableTask，pendingRemote 收集 item。
 */
export function splitOrchestratorTaskViews(
  views: OrchestratorTaskView[],
): OrchestratorTaskViewSplit {
  const tasks: OrchestratorRenderableTask[] = [];
  const pendingRemoteItems: OrchestratorRemoteOutboxItem[] = [];

  for (const view of views) {
    if (view.origin === 'pendingRemote') {
      pendingRemoteItems.push(view.item);
      continue;
    }
    tasks.push({
      origin: view.origin,
      task: view.task,
      deviceId: view.origin === 'remote' ? view.deviceId : null,
      deviceName: view.origin === 'remote' ? view.deviceName : null,
      view,
    });
  }

  return { tasks, pendingRemoteItems };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   队列视图需要按生命周期状态展示真实任务，同时保留空状态列用于稳定渲染。
 *
 * Code Logic（这个函数做什么）:
 *   先按完整状态集合创建空数组，再按 task.status 把渲染任务追加到对应数组。
 */
export function groupOrchestratorRenderableTasks(
  tasks: OrchestratorRenderableTask[],
): OrchestratorRenderableTaskGroups {
  const groups = ORCHESTRATOR_STATUSES.reduce<OrchestratorRenderableTaskGroups>((acc, status) => {
    acc[status] = [];
    return acc;
  }, {} as OrchestratorRenderableTaskGroups);

  for (const task of tasks) {
    groups[task.task.status].push(task);
  }

  return groups;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   pendingRemote 只是待发送 outbox 项，不能执行入队、重试、终止或 evidence 查询。
 *
 * Code Logic（这个函数做什么）:
 *   local/remote 真实任务返回 true；pendingRemote 返回 false。
 */
export function isOrchestratorTaskViewActionable(view: OrchestratorTaskView | null): boolean {
  return Boolean(view && view.origin !== 'pendingRemote');
}

/**
 * Business Logic（为什么需要这个函数）:
 *   完成运行等写操作只能在本机或远端 live 时发出；offline/unsupported 不得假装可写。
 *
 * Code Logic（这个函数做什么）:
 *   本机 snapshot 归一化为 null，视为在线；远端仅 live 为在线。
 */
export function isOrchestratorRemoteActionOnline(
  remoteStatus: OrchestratorRemoteRuntimeStatus | null,
): boolean {
  return remoteStatus === null || remoteStatus === 'live';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   少数本机-only 路径仍需区分 origin，但不能再当作完成按钮门闩。
 *
 * Code Logic（这个函数做什么）:
 *   仅当 view origin 为 local 时返回 true。
 */
export function isLocalOrchestratorTaskView(view: OrchestratorTaskView | null): boolean {
  return view?.origin === 'local';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   action 和 selection 逻辑需要从 local/remote view 中读取 taskId，pending outbox 没有可操作 taskId。
 *
 * Code Logic（这个函数做什么）:
 *   local/remote 返回 task.id；pendingRemote 返回 null。
 */
export function getOrchestratorTaskViewTaskId(view: OrchestratorTaskView | null): string | null {
  if (!view || view.origin === 'pendingRemote') return null;
  return view.task.id;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   action 和 evidence 命令必须同时携带 projectId 和 taskId，pending outbox 不能提供该组合。
 *
 * Code Logic（这个函数做什么）:
 *   local/remote 返回 task.projectId；pendingRemote 返回 null。
 */
export function getOrchestratorTaskViewProjectId(view: OrchestratorTaskView | null): string | null {
  if (!view || view.origin === 'pendingRemote') return null;
  return view.task.projectId;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   创建或 action 返回单个 task view 时，页面需要把它稳定合并进当前列表，而不是整页刷新。
 *
 * Code Logic（这个函数做什么）:
 *   local/remote 按 task.id 替换；pendingRemote 按 outbox item.id 替换；找不到时插入列表头部。
 */
export function upsertOrchestratorTaskView(
  current: OrchestratorTaskView[],
  next: OrchestratorTaskView,
): OrchestratorTaskView[] {
  const nextKey = orchestratorTaskViewStableKey(next);
  const existingIndex = current.findIndex((view) => orchestratorTaskViewStableKey(view) === nextKey);
  if (existingIndex === -1) return [next, ...current];
  return current.map((view, index) => (index === existingIndex ? next : view));
}

/**
 * Business Logic（为什么需要这个函数）:
 *   upsert 需要一个不会混淆真实任务和 outbox 项的稳定 key。
 *
 * Code Logic（这个函数做什么）:
 *   local/remote 使用 task 前缀加 task.id；pendingRemote 使用 pending 前缀加 item.id。
 */
function orchestratorTaskViewStableKey(view: OrchestratorTaskView): string {
  if (view.origin === 'pendingRemote') return `pending:${view.item.id}`;
  return `task:${view.task.id}`;
}
