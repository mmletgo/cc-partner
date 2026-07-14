/**
 * Orchestrator 自动化编排域类型。
 *
 * Business Logic（为什么需要这个模块）:
 *   任务看板、runtime snapshot 与 remote outbox 需要独立类型边界，与 Workbench 项目 DTO 解耦。
 *
 * Code Logic（这个模块做什么）:
 *   导出任务状态机字面量、任务/证据/runtime/outbox DTO 与 remote-aware task view 联合类型。
 */

/**
 * Orchestrator 任务创建 Prompt 完善结果。
 *
 * Business Logic（为什么需要这个类型）:
 *   项目自动化创建任务时，用户可以只写一个简单 Prompt，再由 AI 生成可编辑的标题、目标和验收标准。
 *
 * Code Logic（字段说明）:
 *   对齐 Rust complete_orchestrator_task_prompt 返回的 camelCase DTO；字段会直接填入创建任务表单。
 */
export interface OrchestratorTaskPromptCompletion {
  title: string;
  goal: string;
  acceptanceCriteria: string;
}

/**
 * Orchestrator 任务生命周期状态。
 *
 * Business Logic（为什么需要这个类型）:
 *   前端需要按统一状态展示任务从草稿、排队、执行、验证、交付到终态的生命周期。
 *
 * Code Logic（这个类型做什么）:
 *   以字符串字面量枚举锁定 Rust OrchestratorTaskStatus 序列化后的状态值，供 DTO 和筛选逻辑复用。
 */
export type OrchestratorTaskStatus =
  | 'draft'
  | 'queued'
  | 'preparing'
  | 'running'
  | 'verifying'
  | 'delivering'
  | 'done'
  | 'blocked'
  | 'aborted';

/**
 * Orchestrator 工作流看板状态。
 *
 * Business Logic（为什么需要这个类型）:
 *   自动化看板需要按后端工作流泳道展示任务，并允许用户拖拽调整本机任务所在泳道。
 *
 * Code Logic（这个类型做什么）:
 *   以字符串字面量枚举锁定 Rust OrchestratorWorkflowState 序列化后的 camelCase 状态值。
 */
export type OrchestratorWorkflowState =
  | 'backlog'
  | 'todo'
  | 'inProgress'
  | 'humanReview'
  | 'rework'
  | 'merging'
  | 'done'
  | 'canceled';

/**
 * Orchestrator 运行态状态。
 *
 * Business Logic（为什么需要这个类型）:
 *   前端需要区分任务的工作流泳道和当前 runner 是否正在占用执行资源。
 *
 * Code Logic（这个类型做什么）:
 *   以字符串字面量枚举锁定 Rust OrchestratorRunState 序列化后的 camelCase 状态值。
 */
export type OrchestratorRunState =
  | 'idle'
  | 'queued'
  | 'preparing'
  | 'running'
  | 'verifying'
  | 'retrying'
  | 'blocked'
  | 'delivering';

/**
 * Orchestrator 当前 attempt 阶段。
 *
 * Business Logic（为什么需要这个类型）:
 *   任务详情和运行时提示需要展示 runner 当前卡在准备、启动、流式输出或收尾等哪个阶段。
 *
 * Code Logic（这个类型做什么）:
 *   以字符串字面量枚举锁定 Rust OrchestratorAttemptPhase 序列化后的 camelCase 阶段值。
 */
export type OrchestratorAttemptPhase =
  | 'preparingWorkspace'
  | 'buildingPrompt'
  | 'launchingRunner'
  | 'initializingSession'
  | 'streaming'
  | 'finishing'
  | 'succeeded'
  | 'failed'
  | 'timedOut'
  | 'stalled'
  | 'canceledByReconciliation';

/**
 * Orchestrator 任务 DTO（对齐 Rust OrchestratorTaskDto，camelCase）。
 *
 * Business Logic（为什么需要这个类型）:
 *   前端后续任务队列和自动化看板需要同时展示 legacy 生命周期、工作流泳道和 runner 运行现场。
 *
 * Code Logic（字段说明）:
 *   status 保留旧 UI 兼容；workflow/run/attempt 字段对齐 split-state 后端 DTO；nullable 后端字段使用 string | null。
 */
export interface OrchestratorTask {
  id: string;
  projectId: string;
  title: string;
  goal: string;
  acceptanceCriteria: string;
  status: OrchestratorTaskStatus;
  workflowState: OrchestratorWorkflowState;
  runState: OrchestratorRunState;
  attemptPhase: OrchestratorAttemptPhase | null;
  source: string;
  externalId: string | null;
  externalIdentifier: string | null;
  externalUrl: string | null;
  externalState: string | null;
  externalLabels: string[] | null;
  runnerProvider: string | null;
  claudeSessionId: string | null;
  transcriptPath: string | null;
  runtimeStartedAt: string | null;
  lastActivityAt: string | null;
  lastRuntimeEvent: string | null;
  lastRuntimeMessage: string | null;
  priority: number;
  branchName: string | null;
  worktreeId: string | null;
  sessionId: string | null;
  blockedReason: string | null;
  attempt: number;
  createdAt: string;
  updatedAt: string;
  startedAt: string | null;
  finishedAt: string | null;
}

/**
 * Orchestrator runtime snapshot 任务摘要。
 *
 * Business Logic（为什么需要这个类型）:
 *   自动化状态条需要展示运行中和待重试任务的低噪音摘要，用户不用展开任务详情也能看到现场。
 *
 * Code Logic（字段说明）:
 *   对齐 Rust OrchestratorRuntimeTaskSummaryDto；attemptPhase 和 runtime 字段允许为空。
 */
export interface OrchestratorRuntimeTaskSummary {
  taskId: string;
  title: string;
  workflowState: OrchestratorWorkflowState;
  runState: OrchestratorRunState;
  attemptPhase: OrchestratorAttemptPhase | null;
  sessionId: string | null;
  worktreeId: string | null;
  lastRuntimeMessage: string | null;
  lastActivityAt: string | null;
}

/**
 * Orchestrator runtime snapshot 最近事件。
 *
 * Business Logic（为什么需要这个类型）:
 *   状态条需要展示最近 scheduler/runner 事件，帮助用户判断自动化是刚刷新、正在执行还是已阻塞。
 *
 * Code Logic（字段说明）:
 *   对齐 Rust OrchestratorRuntimeEventDto；不暴露 payloadJson，避免 UI 依赖调试结构。
 */
export interface OrchestratorRuntimeEvent {
  id: string;
  taskId: string;
  taskTitle: string;
  kind: string;
  message: string;
  createdAt: string;
}

/**
 * Orchestrator 远端 runtime 展示状态（live / unsupported / offline / unavailable）。
 *
 * Business Logic（为什么需要这个类型）:
 *   桌面/移动端状态条需要把 owning device 的可达性与能力情况区分开，
 *   不能把 offline 与 unsupported 混成同一种空态；本机项目 snapshot 使用 'local' 字面量，不进入该联合。
 *
 * Code Logic（这个类型做什么）:
 *   以字面量联合锁定远端四态；对齐 owning-device 成功/失败映射。snapshot.remoteStatus 为 'local' | 本联合。
 */
export type OrchestratorRemoteRuntimeStatus =
  | 'live'
  | 'unsupported'
  | 'offline'
  | 'unavailable';

/**
 * Orchestrator 项目运行时快照 DTO（对齐 Rust OrchestratorRuntimeSnapshotDto，camelCase）。
 *
 * Business Logic（为什么需要这个类型）:
 *   Workbench 自动化看板需要展示当前项目调度器、workflow 配置有效性、并发槽位占用和运行摘要。
 *
 * Code Logic（字段说明）:
 *   workflowError/latestError 为后端可空错误文本；recentEvents/runningTasks/retryingTasks 为状态条摘要数据。
 *   remoteStatus 在本机为 local，远端在线成功为 live，其余为 unsupported/offline/unavailable。
 */
export interface OrchestratorRuntimeSnapshot {
  projectId: string;
  projectKind: 'local' | 'remote' | string;
  remoteStatus: 'local' | OrchestratorRemoteRuntimeStatus;
  generatedAt: string;
  latestTickAt: string | null;
  lastDispatchAt: string | null;
  lastDispatchedCount: number;
  schedulerEnabled: boolean;
  workflowSource: string;
  workflowValid: boolean;
  workflowError: string | null;
  maxConcurrentTasks: number;
  slotsUsed: number;
  slotsAvailable: number;
  latestError: string | null;
  runningTasks: OrchestratorRuntimeTaskSummary[];
  retryingTasks: OrchestratorRuntimeTaskSummary[];
  recentEvents: OrchestratorRuntimeEvent[];
}

/**
 * Orchestrator runtime 桌面/移动端显示态（进程内缓存 + 远端状态）。
 *
 * Business Logic（为什么需要这个类型）:
 *   远端离线后仍需展示最后一次成功快照与收到时间，但不能把缓存交给 scheduler/动作逻辑；桌面 hook 与移动 store 共用形状、缓存彼此独立。
 *
 * Code Logic（字段说明）:
 *   snapshot 为当前应渲染的快照（live/local 成功或 offline 缓存）；remoteStatus 为远端四态或 null（本机）；
 *   cachedAt 仅在展示缓存时有值；loading/error 描述请求过程。
 */
export interface OrchestratorRuntimeDisplayState {
  snapshot: OrchestratorRuntimeSnapshot | null;
  remoteStatus: OrchestratorRemoteRuntimeStatus | null;
  cachedAt: string | null;
  loading: boolean;
  error: Error | null;
}

/**
 * Orchestrator 远端 outbox 状态。
 *
 * Business Logic（为什么需要这个类型）:
 *   远端设备离线时，用户创建的任务会先进入本机待发送队列，前端需要展示发送状态但不能当作真实任务操作。
 *
 * Code Logic（这个类型做什么）:
 *   以字符串字面量枚举锁定 Rust OrchestratorRemoteOutboxStatus 序列化后的状态值。
 */
export type OrchestratorRemoteOutboxStatus = 'pending' | 'sending' | 'mirrored' | 'failed' | 'discarded';

/**
 * Orchestrator 远端 outbox DTO（对齐 Rust OrchestratorRemoteOutboxDto，camelCase）。
 *
 * Business Logic（为什么需要这个类型）:
 *   远端项目创建任务可能还没有远端 taskId，前端必须展示待发送/失败状态并避免入队、重试、终止或读取证据。
 *
 * Code Logic（字段说明）:
 *   requestJson 保存原始创建请求；remoteTaskId 只有镜像成功后才可能出现；lastError 用于展示发送失败原因。
 */
export interface OrchestratorRemoteOutboxItem {
  id: string;
  deviceId: string;
  deviceName: string;
  remoteProjectPath: string;
  remoteProjectId: string | null;
  requestJson: string;
  status: OrchestratorRemoteOutboxStatus;
  remoteTaskId: string | null;
  lastError: string | null;
  createdAt: string;
  updatedAt: string;
  sentAt: string | null;
}

/**
 * Orchestrator remote-aware 任务视图 DTO（serde tag = origin）。
 *
 * Business Logic（为什么需要这个类型）:
 *   Workbench 既要展示本机任务，也要展示已镜像的远端任务和仍在 outbox 中的远端待发送项。
 *
 * Code Logic（这个类型做什么）:
 *   使用 discriminated union 精确区分 local、remote 和 pendingRemote 三类视图。
 */
export type OrchestratorTaskView =
  | { origin: 'local'; task: OrchestratorTask }
  | {
      origin: 'remote';
      task: OrchestratorTask;
      deviceId: string;
      deviceName: string;
    }
  | { origin: 'pendingRemote'; item: OrchestratorRemoteOutboxItem };

/**
 * Orchestrator 任务证据 DTO（对齐 Rust OrchestratorEvidenceDto，camelCase）。
 *
 * Business Logic（为什么需要这个类型）:
 *   前端任务详情需要展示验证命令输出和后续交付证据，帮助用户判断任务是否可继续交付。
 *
 * Code Logic（字段说明）:
 *   kind/summary 用于区分证据类型和结果，content 保存可展开的原始输出文本。
 */
export interface OrchestratorEvidence {
  id: string;
  taskId: string;
  kind: string;
  title: string;
  summary: string;
  content: string;
  createdAt: string;
}
