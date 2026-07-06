/**
 * Orchestrator API - 通过 Tauri invoke 调用 Rust 编排任务命令
 *
 * Business Logic（为什么需要这个模块）:
 *   Orchestrator 前端页面需要读取本机/远端任务视图、创建任务、推进队列并展示证据。
 *
 * Code Logic（这个模块做什么）:
 *   封装 remote-aware Tauri invoke，并导出纯参数构造 helper 供契约测试覆盖。
 */

import { invoke } from './client';
import type {
  OrchestratorEvidence,
  OrchestratorRuntimeSnapshot,
  OrchestratorTask,
  OrchestratorTaskView,
  OrchestratorWorkflowState,
} from '@/lib/types';

/**
 * Business Logic（为什么需要这个常量）:
 *   Orchestrator Workbench UI 必须使用 remote-aware 命令，避免远端项目误走旧的本机-only command。
 *
 * Code Logic（这个常量做什么）:
 *   集中声明任务视图、evidence 和 Workbench 本机项目看板命令名，供 API 方法和契约测试共享。
 */
export const ORCHESTRATOR_REMOTE_COMMANDS = {
  listTaskViews: 'list_orchestrator_task_views',
  createTaskView: 'create_orchestrator_task_view',
  queueTaskView: 'queue_orchestrator_task_view',
  retryTaskView: 'retry_orchestrator_task_view',
  abortTaskView: 'abort_orchestrator_task_view',
  listEvidenceForProject: 'list_orchestrator_task_evidence_for_project',
  // 常量名保留 remote 以减少本轮迁移面；下面两个命令服务 Workbench 本机项目看板。
  moveTaskWorkflowState: 'move_orchestrator_task_workflow_state',
  getRuntimeSnapshot: 'get_orchestrator_runtime_snapshot',
} as const;

/**
 * 创建 Orchestrator 任务的前端请求。
 *
 * Business Logic（为什么需要这个类型）:
 *   用户创建任务时只填写项目、标题、目标、验收标准、可选优先级和外部 tracker 预留字段。
 *
 * Code Logic（这个类型做什么）:
 *   字段保持 camelCase，直接对应 Rust CreateOrchestratorTaskRequest 的 serde 入参。
 */
export interface CreateOrchestratorTaskRequest {
  projectId: string;
  title: string;
  goal: string;
  acceptanceCriteria: string;
  priority?: number;
  source?: string;
  externalId?: string;
  externalIdentifier?: string;
  externalUrl?: string;
  externalState?: string;
  externalLabels?: string[];
}

/**
 * Business Logic（为什么需要这个函数）:
 *   任务列表既支持项目筛选，也支持空项目参数查看全局任务，参数需要稳定归一。
 *
 * Code Logic（这个函数做什么）:
 *   projectId 首尾空白会被 trim；空值或空白字符串统一转为 null。
 */
export function buildListOrchestratorTaskViewsInvokeArgs(
  projectId?: string | null,
): Record<string, unknown> {
  return {
    projectId: projectId?.trim() || null,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   创建任务时前端必须把 request 作为单个命名参数传给 Tauri command，避免字段散落。
 *
 * Code Logic（这个函数做什么）:
 *   仅包装 `{ request }`，不重命名、不清理字段，让后端负责业务校验与文本归一。
 */
export function buildCreateOrchestratorTaskViewInvokeArgs(
  request: CreateOrchestratorTaskRequest,
): Record<string, unknown> {
  return { request };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   remote-aware 任务 action 必须同时带 projectId 和 taskId，后端据此决定本机执行或远端代理。
 *
 * Code Logic（这个函数做什么）:
 *   projectId/taskId 首尾空白会被 trim，再包装成 `{ projectId, taskId }` 供 invoke 使用。
 */
export function buildOrchestratorTaskViewActionInvokeArgs(
  projectId: string,
  taskId: string,
): Record<string, unknown> {
  return { projectId: projectId.trim(), taskId: taskId.trim() };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   任务详情 evidence 查询必须绑定 active project，避免远端 taskId 与本机 taskId 作用域混淆。
 *
 * Code Logic（这个函数做什么）:
 *   复用 remote-aware action 参数结构，返回 `{ projectId, taskId }`。
 */
export function buildListOrchestratorTaskEvidenceForProjectInvokeArgs(
  projectId: string,
  taskId: string,
): Record<string, unknown> {
  return buildOrchestratorTaskViewActionInvokeArgs(projectId, taskId);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   看板拖拽需要把本机任务移动到目标 workflow 泳道，后端要求 request 包裹业务参数。
 *
 * Code Logic（这个函数做什么）:
 *   projectId/taskId 首尾空白会被 trim，targetState 原样保留，再包装成 `{ request }`。
 */
export function buildMoveOrchestratorTaskWorkflowStateInvokeArgs(
  projectId: string,
  taskId: string,
  targetState: OrchestratorWorkflowState,
): Record<string, unknown> {
  return {
    request: {
      projectId: projectId.trim(),
      taskId: taskId.trim(),
      targetState,
    },
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 自动化看板需要读取当前项目调度器运行时快照，参数必须绑定明确项目。
 *
 * Code Logic（这个函数做什么）:
 *   projectId 首尾空白会被 trim，再包装成 `{ projectId }` 供 invoke 使用。
 */
export function buildOrchestratorRuntimeSnapshotInvokeArgs(
  projectId: string,
): Record<string, unknown> {
  return { projectId: projectId.trim() };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   completeAgentRun 仍是本机-only 旧命令，参数 helper 需要与 remote-aware action helper 分开。
 *
 * Code Logic（这个函数做什么）:
 *   taskId 首尾空白会被 trim，再包装成 `{ taskId }` 供 invoke 使用。
 */
export function buildOrchestratorTaskIdInvokeArgs(taskId: string): Record<string, unknown> {
  return { taskId: taskId.trim() };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   旧 API 方法仍返回裸任务 DTO，但 remote offline create 只会返回 pending outbox，不能伪造任务。
 *
 * Code Logic（这个函数做什么）:
 *   local/remote 返回 view.task；pendingRemote 抛出错误，提示调用方应改用 task view API。
 */
function unwrapTaskView(view: OrchestratorTaskView): OrchestratorTask {
  if (view.origin === 'pendingRemote') {
    throw new Error('Pending remote task view does not contain an OrchestratorTask');
  }
  return view.task;
}

export const orchestratorApi = {
  /**
   * Business Logic（为什么需要这个函数）:
   *   Orchestrator 任务页需要读取某个项目下的本机任务、远端任务和待发送 outbox。
   *
   * Code Logic（这个函数做什么）:
   *   调用 list_orchestrator_task_views，并通过 helper 归一化 projectId 参数。
   */
  listTaskViews: (projectId?: string | null) =>
    invoke<OrchestratorTaskView[]>(
      ORCHESTRATOR_REMOTE_COMMANDS.listTaskViews,
      buildListOrchestratorTaskViewsInvokeArgs(projectId),
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户提交任务表单后，需要创建本机/远端任务视图，远端离线时可能返回 pendingRemote。
   *
   * Code Logic（这个函数做什么）:
   *   调用 create_orchestrator_task_view，并保持 request 字段原样交给后端校验。
   */
  createTaskView: (request: CreateOrchestratorTaskRequest) =>
    invoke<OrchestratorTaskView>(
      ORCHESTRATOR_REMOTE_COMMANDS.createTaskView,
      buildCreateOrchestratorTaskViewInvokeArgs(request),
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户确认草稿任务后，需要把本机或远端真实任务切换为排队状态。
   *
   * Code Logic（这个函数做什么）:
   *   调用 queue_orchestrator_task_view，并通过 helper 归一化 projectId/taskId 参数。
   */
  queueTaskView: (projectId: string, taskId: string) =>
    invoke<OrchestratorTaskView>(
      ORCHESTRATOR_REMOTE_COMMANDS.queueTaskView,
      buildOrchestratorTaskViewActionInvokeArgs(projectId, taskId),
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   任务详情需要读取当前项目下当前任务的验证输出与交付证据。
   *
   * Code Logic（这个函数做什么）:
   *   调用 list_orchestrator_task_evidence_for_project，并通过 projectId/taskId helper 归一参数。
   */
  listEvidence: (projectId: string, taskId: string) =>
    invoke<OrchestratorEvidence[]>(
      ORCHESTRATOR_REMOTE_COMMANDS.listEvidenceForProject,
      buildListOrchestratorTaskEvidenceForProjectInvokeArgs(projectId, taskId),
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户在 Workbench 自动化看板拖拽本机任务时，需要把任务切换到相邻 workflow 泳道。
   *
   * Code Logic（这个函数做什么）:
   *   调用 move_orchestrator_task_workflow_state，并按后端要求用 `{ request }` 包裹参数。
   */
  moveTaskWorkflowState: (
    projectId: string,
    taskId: string,
    targetState: OrchestratorWorkflowState,
  ) =>
    invoke<OrchestratorTaskView>(
      ORCHESTRATOR_REMOTE_COMMANDS.moveTaskWorkflowState,
      buildMoveOrchestratorTaskWorkflowStateInvokeArgs(projectId, taskId, targetState),
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   Workbench 自动化看板需要展示调度器是否启用、workflow 是否有效和当前并发槽位。
   *
   * Code Logic（这个函数做什么）:
   *   调用 get_orchestrator_runtime_snapshot，并返回后端 camelCase runtime snapshot DTO。
   */
  getRuntimeSnapshot: (projectId: string) =>
    invoke<OrchestratorRuntimeSnapshot>(
      ORCHESTRATOR_REMOTE_COMMANDS.getRuntimeSnapshot,
      buildOrchestratorRuntimeSnapshotInvokeArgs(projectId),
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户确认 Claude Code 已完成后，需要触发后端验证并推进任务状态。
   *
   * Code Logic（这个函数做什么）:
   *   调用 complete_orchestrator_agent_run，并返回更新后的任务 DTO。
   */
  completeAgentRun: (taskId: string) =>
    invoke<OrchestratorTask>(
      'complete_orchestrator_agent_run',
      buildOrchestratorTaskIdInvokeArgs(taskId),
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   Blocked 任务处理完原因后，需要重新进入队列等待后续调度。
   *
   * Code Logic（这个函数做什么）:
   *   调用 retry_orchestrator_task_view，并返回更新后的任务视图。
   */
  retryTaskView: (projectId: string, taskId: string) =>
    invoke<OrchestratorTaskView>(
      ORCHESTRATOR_REMOTE_COMMANDS.retryTaskView,
      buildOrchestratorTaskViewActionInvokeArgs(projectId, taskId),
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户决定停止任务时，需要把任务置为 Aborted 但保留 worktree/session。
   *
   * Code Logic（这个函数做什么）:
   *   调用 abort_orchestrator_task_view，并返回更新后的任务视图。
   */
  abortTaskView: (projectId: string, taskId: string) =>
    invoke<OrchestratorTaskView>(
      ORCHESTRATOR_REMOTE_COMMANDS.abortTaskView,
      buildOrchestratorTaskViewActionInvokeArgs(projectId, taskId),
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   少量旧调用方仍可能只需要真实任务数组，但底层必须走 remote-aware 任务视图命令。
   *
   * Code Logic（这个函数做什么）:
   *   调用 listTaskViews 后过滤 pendingRemote，并展开 local/remote 的 task DTO。
   */
  listTasks: async (projectId?: string | null): Promise<OrchestratorTask[]> => {
    const views = await orchestratorApi.listTaskViews(projectId);
    return views.flatMap((view) => (view.origin === 'pendingRemote' ? [] : [view.task]));
  },

  /**
   * Business Logic（为什么需要这个函数）:
   *   兼容旧 task API 时仍必须使用 remote-aware create；pendingRemote 无法返回裸任务。
   *
   * Code Logic（这个函数做什么）:
   *   调用 createTaskView 并解包 local/remote task；pendingRemote 抛错提醒调用方改用 view API。
   */
  createTask: async (request: CreateOrchestratorTaskRequest): Promise<OrchestratorTask> => {
    return unwrapTaskView(await orchestratorApi.createTaskView(request));
  },

  /**
   * Business Logic（为什么需要这个函数）:
   *   兼容旧 task API 时 action 也必须绑定 projectId，避免误走旧本机-only command。
   *
   * Code Logic（这个函数做什么）:
   *   调用 queueTaskView 并解包返回的真实任务 DTO。
   */
  queueTask: async (projectId: string, taskId: string): Promise<OrchestratorTask> => {
    return unwrapTaskView(await orchestratorApi.queueTaskView(projectId, taskId));
  },

  /**
   * Business Logic（为什么需要这个函数）:
   *   兼容旧 task API 时 blocked 重试也必须支持远端项目代理。
   *
   * Code Logic（这个函数做什么）:
   *   调用 retryTaskView 并解包返回的真实任务 DTO。
   */
  retryTask: async (projectId: string, taskId: string): Promise<OrchestratorTask> => {
    return unwrapTaskView(await orchestratorApi.retryTaskView(projectId, taskId));
  },

  /**
   * Business Logic（为什么需要这个函数）:
   *   兼容旧 task API 时终止操作也必须支持远端项目代理。
   *
   * Code Logic（这个函数做什么）:
   *   调用 abortTaskView 并解包返回的真实任务 DTO。
   */
  abortTask: async (projectId: string, taskId: string): Promise<OrchestratorTask> => {
    return unwrapTaskView(await orchestratorApi.abortTaskView(projectId, taskId));
  },
};

export const buildListOrchestratorTasksInvokeArgs = buildListOrchestratorTaskViewsInvokeArgs;
export const buildCreateOrchestratorTaskInvokeArgs = buildCreateOrchestratorTaskViewInvokeArgs;
export const buildQueueOrchestratorTaskInvokeArgs = buildOrchestratorTaskViewActionInvokeArgs;
