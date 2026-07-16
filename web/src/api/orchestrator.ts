/**
 * Orchestrator API - 通过 Tauri invoke 调用 Rust 编排任务命令
 *
 * Business Logic（为什么需要这个模块）:
 *   Orchestrator 前端页面需要读取本机/远端任务视图、创建任务、推进队列并展示证据。
 *
 * Code Logic（这个模块做什么）:
 *   封装 remote-aware Tauri invoke，并导出纯参数构造 helper 供契约测试覆盖。
 */

import {
  orchestratorEvidenceListDecoder,
  orchestratorProjectRefreshResultDecoder,
  orchestratorRemoteOutboxItemDecoder,
  orchestratorReviewDiffDecoder,
  orchestratorRuntimeSnapshotDecoder,
  orchestratorTaskDecoder,
  orchestratorTaskViewDecoder,
  orchestratorTaskViewListDecoder,
  workflowDocumentDecoder,
} from '@/lib/schemas/orchestrator';
import type {
  OrchestratorAgentAdapterCatalog,
  OrchestratorRemoteOutboxItem,
  PrepareAgentDowngradeResult,
  OrchestratorReviewDiff,
  OrchestratorRuntimeSnapshot,
  OrchestratorTask,
  OrchestratorTaskView,
  OrchestratorWorkflowState,
  WorkflowDocument,
} from '@/lib/types';
import { ContractDecodeError } from '@/lib/runtimeSchema';
import { invoke, invokeDecoded } from './client';
import { toOrchestratorRuntimeTransportError } from './orchestratorRuntimeTransportError';

/**
 * Business Logic（为什么需要这个常量）:
 *   Orchestrator Workbench UI 必须使用 remote-aware 命令，避免远端项目误走旧的本机-only command。
 *
 * Code Logic（这个常量做什么）:
 *   集中声明任务视图、evidence 和 Workbench 本机项目看板命令名，供 API 方法和契约测试共享。
 */

/**
 * Business Logic（为什么需要这个函数）:
 *   Retry/Discard outbox 动作需要稳定的 projectId/outboxId 参数形状，供契约测试锁定。
 *
 * Code Logic（这个函数做什么）:
 *   返回 camelCase invoke 参数对象。
 */
export function buildOrchestratorRemoteOutboxActionInvokeArgs(
  projectId: string,
  outboxId: string,
): { projectId: string; outboxId: string } {
  return { projectId, outboxId };
}

export const ORCHESTRATOR_REMOTE_COMMANDS = {
  listTaskViews: 'list_orchestrator_task_views',
  createTaskView: 'create_orchestrator_task_view',
  queueTaskView: 'queue_orchestrator_task_view',
  startTaskView: 'start_orchestrator_task_view',
  retryTaskView: 'retry_orchestrator_task_view',
  requestReworkTaskView: 'request_orchestrator_task_rework_view',
  deliverReviewedTaskView: 'deliver_reviewed_orchestrator_task_view',
  abortTaskView: 'abort_orchestrator_task_view',
  cancelTaskView: 'cancel_orchestrator_task_view',
  refreshProject: 'refresh_orchestrator_project',
  listEvidenceForProject: 'list_orchestrator_task_evidence_for_project',
  getReviewDiff: 'get_orchestrator_review_diff',
  getWorkflowDocument: 'get_workflow_document',
  validateWorkflowDocument: 'validate_workflow_document',
  saveWorkflowDocument: 'save_workflow_document',
  // 常量名保留 remote 以减少本轮迁移面；下面两个命令服务 Workbench 本机项目看板。
  moveTaskWorkflowState: 'move_orchestrator_task_workflow_state',
  getRuntimeSnapshot: 'get_orchestrator_runtime_snapshot',
  retryRemoteOutbox: 'retry_orchestrator_remote_outbox',
  discardRemoteOutbox: 'discard_orchestrator_remote_outbox',
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
  createAction?: OrchestratorCreateAction;
  source?: string;
  externalId?: string;
  externalIdentifier?: string;
  externalUrl?: string;
  externalState?: string;
  externalLabels?: string[];
}

export type OrchestratorCreateAction = 'backlog' | 'todo' | 'start';

/**
 * Orchestrator 项目刷新结果。
 *
 * Business Logic（为什么需要这个类型）:
 *   用户点击刷新项目后需要看到后端是否实际领取了任务，remote 项目也返回本机 shortcut projectId。
 *
 * Code Logic（这个类型做什么）:
 *   对齐 Rust OrchestratorProjectRefreshDto 的 camelCase 序列化字段。
 */
export interface OrchestratorProjectRefreshResult {
  projectId: string;
  dispatched: number;
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
 *   requestRework 是显式业务动作，除 projectId/taskId 外还必须传递用户填写的返工原因供后端写 evidence。
 *
 * Code Logic（这个函数做什么）:
 *   projectId/taskId/reason 首尾空白会被 trim，再包装成 `{ projectId, taskId, reason }`。
 */
export function buildOrchestratorTaskReworkInvokeArgs(
  projectId: string,
  taskId: string,
  reason: string,
): Record<string, unknown> {
  return {
    projectId: projectId.trim(),
    taskId: taskId.trim(),
    reason: reason.trim(),
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Deliver 前必须把审阅 digest 一并交给后端做漂移比对；无 digest 时保持旧参数形状。
 *
 * Code Logic（这个函数做什么）:
 *   projectId/taskId trim；expectedReviewDigest 非空时写入 camelCase 字段，否则省略。
 */
export function buildDeliverReviewedOrchestratorTaskViewInvokeArgs(
  projectId: string,
  taskId: string,
  expectedReviewDigest?: string | null,
): Record<string, unknown> {
  const args: Record<string, unknown> = {
    projectId: projectId.trim(),
    taskId: taskId.trim(),
  };
  const digest = expectedReviewDigest?.trim();
  if (digest) {
    args.expectedReviewDigest = digest;
  }
  return args;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Changes tab / Deliver 前需要按 project+task 拉取有界 review diff，参数形状需契约锁定。
 *
 * Code Logic（这个函数做什么）:
 *   复用 remote-aware action 参数结构，返回 `{ projectId, taskId }`。
 */
export function buildGetOrchestratorReviewDiffInvokeArgs(
  projectId: string,
  taskId: string,
): Record<string, unknown> {
  return buildOrchestratorTaskViewActionInvokeArgs(projectId, taskId);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   向导 get/validate 需要稳定 projectId 参数形状。
 *
 * Code Logic（这个函数做什么）:
 *   trim 后包装 `{ projectId }`。
 */
export function buildGetWorkflowDocumentInvokeArgs(projectId: string): Record<string, unknown> {
  return { projectId: projectId.trim() };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   保存前权威校验必须带 projectId 与当前草稿 content。
 *
 * Code Logic（这个函数做什么）:
 *   trim projectId，content 原样传递。
 */
export function buildValidateWorkflowDocumentInvokeArgs(
  projectId: string,
  content: string,
): Record<string, unknown> {
  return { projectId: projectId.trim(), content };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   CAS 保存必须同时带 expectedHash 与 content；冲突时保留草稿。
 *
 * Code Logic（这个函数做什么）:
 *   trim projectId/expectedHash，content 原样传递。
 */
export function buildSaveWorkflowDocumentInvokeArgs(
  projectId: string,
  expectedHash: string,
  content: string,
): Record<string, unknown> {
  return {
    projectId: projectId.trim(),
    expectedHash: expectedHash.trim(),
    content,
  };
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
   *   invokeDecoded list_orchestrator_task_views；参数经 helper 归一化 projectId。
   */
  listTaskViews: (projectId?: string | null) =>
    invokeDecoded(
      ORCHESTRATOR_REMOTE_COMMANDS.listTaskViews,
      buildListOrchestratorTaskViewsInvokeArgs(projectId),
      orchestratorTaskViewListDecoder,
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户提交任务表单后，需要创建本机/远端任务视图，远端离线时可能返回 pendingRemote。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded create_orchestrator_task_view；request 字段原样交给后端校验。
   */
  createTaskView: (request: CreateOrchestratorTaskRequest) =>
    invokeDecoded(
      ORCHESTRATOR_REMOTE_COMMANDS.createTaskView,
      buildCreateOrchestratorTaskViewInvokeArgs(request),
      orchestratorTaskViewDecoder,
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户确认草稿任务后，需要把本机或远端真实任务切换为排队状态。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded queue_orchestrator_task_view → OrchestratorTaskView；参数经 helper 归一。
   */
  queueTaskView: (projectId: string, taskId: string) =>
    invokeDecoded(
      ORCHESTRATOR_REMOTE_COMMANDS.queueTaskView,
      buildOrchestratorTaskViewActionInvokeArgs(projectId, taskId),
      orchestratorTaskViewDecoder,
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户点击 Start 时，需要把本机或远端真实任务放入 scheduler 可领取路径。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded start_orchestrator_task_view → OrchestratorTaskView。
   */
  startTaskView: (projectId: string, taskId: string) =>
    invokeDecoded(
      ORCHESTRATOR_REMOTE_COMMANDS.startTaskView,
      buildOrchestratorTaskViewActionInvokeArgs(projectId, taskId),
      orchestratorTaskViewDecoder,
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   任务详情需要读取当前项目下当前任务的验证输出与交付证据。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded list_orchestrator_task_evidence_for_project → OrchestratorEvidence[]。
   */
  listEvidence: (projectId: string, taskId: string) =>
    invokeDecoded(
      ORCHESTRATOR_REMOTE_COMMANDS.listEvidenceForProject,
      buildListOrchestratorTaskEvidenceForProjectInvokeArgs(projectId, taskId),
      orchestratorEvidenceListDecoder,
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户在 Workbench 自动化看板拖拽本机任务时，需要把任务切换到相邻 workflow 泳道。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded move_orchestrator_task_workflow_state → OrchestratorTaskView。
   */
  moveTaskWorkflowState: (
    projectId: string,
    taskId: string,
    targetState: OrchestratorWorkflowState,
  ) =>
    invokeDecoded(
      ORCHESTRATOR_REMOTE_COMMANDS.moveTaskWorkflowState,
      buildMoveOrchestratorTaskWorkflowStateInvokeArgs(projectId, taskId, targetState),
      orchestratorTaskViewDecoder,
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   Workbench 自动化看板需要展示调度器是否启用、workflow 是否有效和当前并发槽位。
   *
   * Code Logic（这个函数做什么）:
   *   调用 get_orchestrator_runtime_snapshot，并返回后端 camelCase runtime snapshot DTO。
   */
  /**
   * Business Logic（为什么需要这个函数）:
   *   Workbench 自动化看板需要展示调度器是否启用、workflow 是否有效和当前并发槽位。
   *   传输失败必须抛结构化 kind，禁止 hook 再靠 Error.message 关键词猜 offline。
   *
   * Code Logic（这个函数做什么）:
   *   调用 get_orchestrator_runtime_snapshot；reject 包装为 OrchestratorRuntimeTransportError(kind=unknown)，
   *   因为后端成功路径已把远端四态收敛为 DTO，真正抛错通常是本机命令/协议层，不能推断 network。
   */
  getRuntimeSnapshot: async (projectId: string): Promise<OrchestratorRuntimeSnapshot> => {
    try {
      return await invokeDecoded(
        ORCHESTRATOR_REMOTE_COMMANDS.getRuntimeSnapshot,
        buildOrchestratorRuntimeSnapshotInvokeArgs(projectId),
        orchestratorRuntimeSnapshotDecoder,
      );
    } catch (reason) {
      // 契约失败原样抛出；其它 invoke reject 收敛为 unknown transport（不可关键词猜 offline）。
      if (reason instanceof ContractDecodeError) {
        throw reason;
      }
      throw toOrchestratorRuntimeTransportError(reason, 'unknown');
    }
  },

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户确认 Claude Code 已完成后，需要触发后端验证并推进任务状态。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded complete_orchestrator_agent_run → OrchestratorTask（local-only）。
   */
  completeAgentRun: (taskId: string) =>
    invokeDecoded(
      'complete_orchestrator_agent_run',
      buildOrchestratorTaskIdInvokeArgs(taskId),
      orchestratorTaskDecoder,
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   Blocked 任务处理完原因后，需要重新进入队列等待后续调度。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded retry_orchestrator_task_view → OrchestratorTaskView。
   */
  retryTaskView: (projectId: string, taskId: string) =>
    invokeDecoded(
      ORCHESTRATOR_REMOTE_COMMANDS.retryTaskView,
      buildOrchestratorTaskViewActionInvokeArgs(projectId, taskId),
      orchestratorTaskViewDecoder,
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   人工复核未通过时，用户需要把本机或远端真实任务送回 Rework，并记录返工原因。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded request_orchestrator_task_rework_view → OrchestratorTaskView。
   */
  requestReworkTaskView: (projectId: string, taskId: string, reason: string) =>
    invokeDecoded(
      ORCHESTRATOR_REMOTE_COMMANDS.requestReworkTaskView,
      buildOrchestratorTaskReworkInvokeArgs(projectId, taskId, reason),
      orchestratorTaskViewDecoder,
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   Human Review Changes tab 与 Deliver 前需要拉取当前 attempt 有界 review diff 快照。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded get_orchestrator_review_diff → OrchestratorReviewDiff。
   */
  getReviewDiff: (projectId: string, taskId: string): Promise<OrchestratorReviewDiff> =>
    invokeDecoded(
      ORCHESTRATOR_REMOTE_COMMANDS.getReviewDiff,
      buildGetOrchestratorReviewDiffInvokeArgs(projectId, taskId),
      orchestratorReviewDiffDecoder,
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   WORKFLOW 向导打开时需要检测 missing/valid/invalid/readError 与 contentHash。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded get_workflow_document → WorkflowDocument。
   */
  getWorkflowDocument: (projectId: string): Promise<WorkflowDocument> =>
    invokeDecoded(
      ORCHESTRATOR_REMOTE_COMMANDS.getWorkflowDocument,
      buildGetWorkflowDocumentInvokeArgs(projectId),
      workflowDocumentDecoder,
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   保存前必须调用后端权威 validator，返回 diagnostics 与规范化 preview。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded validate_workflow_document → WorkflowDocument。
   */
  validateWorkflowDocument: (projectId: string, content: string): Promise<WorkflowDocument> =>
    invokeDecoded(
      ORCHESTRATOR_REMOTE_COMMANDS.validateWorkflowDocument,
      buildValidateWorkflowDocumentInvokeArgs(projectId, content),
      workflowDocumentDecoder,
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   向导 CAS 保存 WORKFLOW.md；冲突返回 workflow_document_changed，成功不 dispatch。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded save_workflow_document(expectedHash, content) → WorkflowDocument。
   */
  saveWorkflowDocument: (
    projectId: string,
    expectedHash: string,
    content: string,
  ): Promise<WorkflowDocument> =>
    invokeDecoded(
      ORCHESTRATOR_REMOTE_COMMANDS.saveWorkflowDocument,
      buildSaveWorkflowDocumentInvokeArgs(projectId, expectedHash, content),
      workflowDocumentDecoder,
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   人工复核通过且 Settings 允许 full-auto delivery 时，用户可以显式触发交付；
   *   对 diff-capable owner 必须携带审阅 digest 做漂移校验。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded deliver_reviewed_orchestrator_task_view，args 在提供时含 expectedReviewDigest。
   */
  deliverReviewedTaskView: (
    projectId: string,
    taskId: string,
    expectedReviewDigest?: string | null,
  ) =>
    invokeDecoded(
      ORCHESTRATOR_REMOTE_COMMANDS.deliverReviewedTaskView,
      buildDeliverReviewedOrchestratorTaskViewInvokeArgs(
        projectId,
        taskId,
        expectedReviewDigest,
      ),
      orchestratorTaskViewDecoder,
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户决定停止任务时，需要把任务置为 Aborted 但保留 worktree/session。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded abort_orchestrator_task_view → OrchestratorTaskView。
   */
  abortTaskView: (projectId: string, taskId: string) =>
    invokeDecoded(
      ORCHESTRATOR_REMOTE_COMMANDS.abortTaskView,
      buildOrchestratorTaskViewActionInvokeArgs(projectId, taskId),
      orchestratorTaskViewDecoder,
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户取消任务时，需要进入 Canceled/Idle 并保留现场与 evidence。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded cancel_orchestrator_task_view → OrchestratorTaskView。
   */
  cancelTaskView: (projectId: string, taskId: string) =>
    invokeDecoded(
      ORCHESTRATOR_REMOTE_COMMANDS.cancelTaskView,
      buildOrchestratorTaskViewActionInvokeArgs(projectId, taskId),
      orchestratorTaskViewDecoder,
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户需要显式刷新当前项目，触发一次后端 best-effort dispatch/reconcile。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded refresh_orchestrator_project → OrchestratorProjectRefreshResult。
   */
  refreshProject: (projectId: string) =>
    invokeDecoded(
      ORCHESTRATOR_REMOTE_COMMANDS.refreshProject,
      buildOrchestratorRuntimeSnapshotInvokeArgs(projectId),
      orchestratorProjectRefreshResultDecoder,
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   桌面 Automation UI 对 failed outbox 点 Retry 时，应在本机把状态回到 pending，不代理远端。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded retry_orchestrator_remote_outbox → OrchestratorRemoteOutboxItem。
   */
  retryRemoteOutbox: (projectId: string, outboxId: string): Promise<OrchestratorRemoteOutboxItem> =>
    invokeDecoded(
      ORCHESTRATOR_REMOTE_COMMANDS.retryRemoteOutbox,
      { projectId, outboxId },
      orchestratorRemoteOutboxItemDecoder,
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   桌面 Automation UI 对 failed outbox 点 Discard 时，应在本机进入 discarded 审计终态。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded discard_orchestrator_remote_outbox → OrchestratorRemoteOutboxItem。
   */
  discardRemoteOutbox: (projectId: string, outboxId: string): Promise<OrchestratorRemoteOutboxItem> =>
    invokeDecoded(
      ORCHESTRATOR_REMOTE_COMMANDS.discardRemoteOutbox,
      { projectId, outboxId },
      orchestratorRemoteOutboxItemDecoder,
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
   *   兼容旧 task API 时 start 也必须支持远端项目代理。
   *
   * Code Logic（这个函数做什么）:
   *   调用 startTaskView 并解包返回的真实任务 DTO。
   */
  startTask: async (projectId: string, taskId: string): Promise<OrchestratorTask> => {
    return unwrapTaskView(await orchestratorApi.startTaskView(projectId, taskId));
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
   *   兼容旧 task API 时 requestRework 也必须支持远端项目代理。
   *
   * Code Logic（这个函数做什么）:
   *   调用 requestReworkTaskView 并解包返回的真实任务 DTO。
   */
  requestReworkTask: async (
    projectId: string,
    taskId: string,
    reason: string,
  ): Promise<OrchestratorTask> => {
    return unwrapTaskView(await orchestratorApi.requestReworkTaskView(projectId, taskId, reason));
  },

  /**
   * Business Logic（为什么需要这个函数）:
   *   兼容旧 task API 时 deliverReviewed 也必须支持远端项目代理。
   *
   * Code Logic（这个函数做什么）:
   *   调用 deliverReviewedTaskView 并解包返回的真实任务 DTO。
   */
  deliverReviewedTask: async (projectId: string, taskId: string): Promise<OrchestratorTask> => {
    return unwrapTaskView(await orchestratorApi.deliverReviewedTaskView(projectId, taskId));
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

  /**
   * Business Logic（为什么需要这个函数）:
   *   兼容旧 task API 时 cancel 也必须支持远端项目代理。
   *
   * Code Logic（这个函数做什么）:
   *   调用 cancelTaskView 并解包返回的真实任务 DTO。
   */
  cancelTask: async (projectId: string, taskId: string): Promise<OrchestratorTask> => {
    return unwrapTaskView(await orchestratorApi.cancelTaskView(projectId, taskId));
  },
};

export const buildListOrchestratorTasksInvokeArgs = buildListOrchestratorTaskViewsInvokeArgs;
export const buildCreateOrchestratorTaskInvokeArgs = buildCreateOrchestratorTaskViewInvokeArgs;
export const buildQueueOrchestratorTaskInvokeArgs = buildOrchestratorTaskViewActionInvokeArgs;


/**
 * Business Logic（为什么需要这个函数）:
 *   Settings 需要拉取 owner adapter 可用性（redacted）。
 *
 * Code Logic（这个函数做什么）:
 *   invoke list_orchestrator_agent_adapters。
 */
export async function listOrchestratorAgentAdapters(
  projectId?: string | null,
): Promise<OrchestratorAgentAdapterCatalog> {
  return invoke<OrchestratorAgentAdapterCatalog>('list_orchestrator_agent_adapters', {
    projectId: projectId ?? null,
  });
}

/**
 * Business Logic（为什么需要这个函数）:
 *   旧 peer 降级前必须 quiesce 非 Claude Runner（local-only）。
 *
 * Code Logic（这个函数做什么）:
 *   invoke prepare_orchestrator_agent_downgrade。
 */
export async function prepareOrchestratorAgentDowngrade(): Promise<PrepareAgentDowngradeResult> {
  return invoke<PrepareAgentDowngradeResult>('prepare_orchestrator_agent_downgrade', {});
}
