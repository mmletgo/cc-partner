/**
 * Orchestrator API - 通过 Tauri invoke 调用 Rust 编排任务命令
 *
 * Business Logic（为什么需要这个模块）:
 *   Orchestrator 前端页面后续需要读取项目任务队列并创建草稿任务。
 *
 * Code Logic（这个模块做什么）:
 *   封装 list/create 两个 invoke，并导出纯参数构造 helper 供契约测试覆盖。
 */

import { invoke } from './client';
import type { OrchestratorEvidence, OrchestratorProjectConfig, OrchestratorTask } from '@/lib/types';

/**
 * 创建 Orchestrator 任务的前端请求。
 *
 * Business Logic（为什么需要这个类型）:
 *   用户创建任务时只填写项目、标题、目标、验收标准和可选优先级。
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
}

/**
 * Business Logic（为什么需要这个函数）:
 *   任务列表既支持项目筛选，也支持空项目参数查看全局任务，参数需要稳定归一。
 *
 * Code Logic（这个函数做什么）:
 *   projectId 首尾空白会被 trim；空值或空白字符串统一转为 null。
 */
export function buildListOrchestratorTasksInvokeArgs(
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
export function buildCreateOrchestratorTaskInvokeArgs(
  request: CreateOrchestratorTaskRequest,
): Record<string, unknown> {
  return { request };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   入队任务命令只需要 taskId，但参数名必须稳定匹配 Rust queue_orchestrator_task。
 *
 * Code Logic（这个函数做什么）:
 *   taskId 首尾空白会被 trim，再包装成 `{ taskId }` 供 invoke 使用。
 */
export function buildQueueOrchestratorTaskInvokeArgs(taskId: string): Record<string, unknown> {
  return buildOrchestratorTaskIdInvokeArgs(taskId);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   多个 Orchestrator 命令都只接收 taskId，统一参数归一能避免 helper 行为漂移。
 *
 * Code Logic（这个函数做什么）:
 *   taskId 首尾空白会被 trim，再包装成 `{ taskId }` 供 invoke 使用。
 */
export function buildOrchestratorTaskIdInvokeArgs(taskId: string): Record<string, unknown> {
  return { taskId: taskId.trim() };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   项目策略读取必须绑定明确项目，参数名需要稳定匹配 Rust get_orchestrator_project_config。
 *
 * Code Logic（这个函数做什么）:
 *   projectId 首尾空白会被 trim，再包装成 `{ projectId }` 供 invoke 使用。
 */
export function buildGetOrchestratorProjectConfigInvokeArgs(
  projectId: string,
): Record<string, unknown> {
  return { projectId: projectId.trim() };
}

export const orchestratorApi = {
  /**
   * Business Logic（为什么需要这个函数）:
   *   Orchestrator 任务页需要读取某个项目下的任务列表。
   *
   * Code Logic（这个函数做什么）:
   *   调用 list_orchestrator_tasks，并通过 helper 归一化 projectId 参数。
   */
  listTasks: (projectId?: string | null) =>
    invoke<OrchestratorTask[]>(
      'list_orchestrator_tasks',
      buildListOrchestratorTasksInvokeArgs(projectId),
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户提交任务表单后，需要创建后端草稿任务并返回完整 DTO。
   *
   * Code Logic（这个函数做什么）:
   *   调用 create_orchestrator_task，并保持 request 字段原样交给后端校验。
   */
  createTask: (request: CreateOrchestratorTaskRequest) =>
    invoke<OrchestratorTask>(
      'create_orchestrator_task',
      buildCreateOrchestratorTaskInvokeArgs(request),
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户确认草稿任务后，需要把任务切换为排队状态并刷新当前列表中的任务 DTO。
   *
   * Code Logic（这个函数做什么）:
   *   调用 queue_orchestrator_task，并通过 helper 归一化 taskId 参数。
   */
  queueTask: (taskId: string) =>
    invoke<OrchestratorTask>(
      'queue_orchestrator_task',
      buildQueueOrchestratorTaskInvokeArgs(taskId),
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   策略卡需要显示当前项目真实 Orchestrator 配置，缺失配置由后端按默认值创建。
   *
   * Code Logic（这个函数做什么）:
   *   调用 get_orchestrator_project_config，并通过 helper 归一化 projectId 参数。
   */
  getProjectConfig: (projectId: string) =>
    invoke<OrchestratorProjectConfig>(
      'get_orchestrator_project_config',
      buildGetOrchestratorProjectConfigInvokeArgs(projectId),
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   任务详情需要读取当前任务的验证输出与交付证据。
   *
   * Code Logic（这个函数做什么）:
   *   调用 list_orchestrator_task_evidence，并通过统一 taskId helper 归一参数。
   */
  listEvidence: (taskId: string) =>
    invoke<OrchestratorEvidence[]>(
      'list_orchestrator_task_evidence',
      buildOrchestratorTaskIdInvokeArgs(taskId),
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
   *   调用 retry_orchestrator_task，并返回更新后的任务 DTO。
   */
  retryTask: (taskId: string) =>
    invoke<OrchestratorTask>('retry_orchestrator_task', buildOrchestratorTaskIdInvokeArgs(taskId)),

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户决定停止任务时，需要把任务置为 Aborted 但保留 worktree/session。
   *
   * Code Logic（这个函数做什么）:
   *   调用 abort_orchestrator_task，并返回更新后的任务 DTO。
   */
  abortTask: (taskId: string) =>
    invoke<OrchestratorTask>('abort_orchestrator_task', buildOrchestratorTaskIdInvokeArgs(taskId)),
};
