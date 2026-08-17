/**
 * Orchestrator runtime / task / outbox 运行时 schema。
 *
 * Business Logic（为什么需要这个模块）:
 *   看板与状态条在写入 state 前必须拒绝错误 workflow/run 枚举与残缺 runtime 快照。
 *
 * Code Logic（这个模块做什么）:
 *   解码 OrchestratorTask、RuntimeSnapshot、RemoteOutbox、TaskView。
 */

import type {
  AgentCompletionContract,
  AgentProviderId,
  OrchestratorAgentAdapterCatalog,
  OrchestratorAgentAdapterCatalogItem,
  OrchestratorAttemptPhase,
  OrchestratorEvidence,
  OrchestratorRemoteOutboxItem,
  OrchestratorRemoteOutboxStatus,
  OrchestratorRemoteRuntimeStatus,
  OrchestratorReviewDiff,
  OrchestratorRunState,
  OrchestratorRuntimeEvent,
  OrchestratorRuntimeSnapshot,
  OrchestratorRuntimeTaskSummary,
  OrchestratorTask,
  OrchestratorTaskStatus,
  OrchestratorTaskView,
  OrchestratorWorkflowState,
  ReviewDiffFile,
  WorkflowDiagnostic,
  WorkflowDocument,
  WorkflowDocumentStatus,
} from '../types/orchestrator';
import {
  arrayDecoder,
  booleanDecoder,
  enumDecoder,
  literalDecoder,
  nullableDecoder,
  numberDecoder,
  objectDecoder,
  optionalDecoder,
  stringDecoder,
  unionDecoder,
  type Decoder,
} from '../runtimeSchema';

const taskStatusDecoder: Decoder<OrchestratorTaskStatus> = enumDecoder('OrchestratorTaskStatus', [
  'draft',
  'queued',
  'preparing',
  'running',
  'verifying',
  'delivering',
  'done',
  'blocked',
  'aborted',
] as const);

const workflowStateDecoder: Decoder<OrchestratorWorkflowState> = enumDecoder(
  'OrchestratorWorkflowState',
  [
    'backlog',
    'todo',
    'inProgress',
    'humanReview',
    'rework',
    'merging',
    'done',
    'canceled',
  ] as const,
);

const runStateDecoder: Decoder<OrchestratorRunState> = enumDecoder('OrchestratorRunState', [
  'idle',
  'queued',
  'preparing',
  'running',
  'verifying',
  'retrying',
  'blocked',
  'delivering',
] as const);

const attemptPhaseDecoder: Decoder<OrchestratorAttemptPhase> = enumDecoder(
  'OrchestratorAttemptPhase',
  [
    'preparingWorkspace',
    'buildingPrompt',
    'launchingRunner',
    'initializingSession',
    'streaming',
    'finishing',
    'succeeded',
    'failed',
    'timedOut',
    'stalled',
    'canceledByReconciliation',
  ] as const,
);

const outboxStatusDecoder: Decoder<OrchestratorRemoteOutboxStatus> = enumDecoder(
  'OrchestratorRemoteOutboxStatus',
  ['pending', 'sending', 'mirrored', 'failed', 'discarded'] as const,
);

const remoteRuntimeStatusDecoder: Decoder<OrchestratorRemoteRuntimeStatus> = enumDecoder(
  'OrchestratorRemoteRuntimeStatus',
  ['live', 'unsupported', 'offline', 'unavailable'] as const,
);

const stringArrayOrNullDecoder: Decoder<string[] | null> = nullableDecoder(
  arrayDecoder(stringDecoder),
);

/**
 * Business Logic（为什么需要这个 decoder）:
 *   任务 DTO 是看板与详情的主数据。
 *
 * Code Logic（这个 decoder 做什么）:
 *   严格校验 status/workflow/run/attempt 与可空 runtime 字段。
 */
export const orchestratorTaskDecoder: Decoder<OrchestratorTask> = objectDecoder('OrchestratorTask', {
  id: stringDecoder,
  projectId: stringDecoder,
  title: stringDecoder,
  goal: stringDecoder,
  acceptanceCriteria: stringDecoder,
  status: taskStatusDecoder,
  workflowState: workflowStateDecoder,
  runState: runStateDecoder,
  attemptPhase: nullableDecoder(attemptPhaseDecoder),
  source: stringDecoder,
  externalId: nullableDecoder(stringDecoder),
  externalIdentifier: nullableDecoder(stringDecoder),
  externalUrl: nullableDecoder(stringDecoder),
  externalState: nullableDecoder(stringDecoder),
  externalLabels: stringArrayOrNullDecoder,
  runnerProvider: nullableDecoder(stringDecoder),
  claudeSessionId: nullableDecoder(stringDecoder),
  transcriptPath: nullableDecoder(stringDecoder),
  runtimeStartedAt: nullableDecoder(stringDecoder),
  lastActivityAt: nullableDecoder(stringDecoder),
  lastRuntimeEvent: nullableDecoder(stringDecoder),
  lastRuntimeMessage: nullableDecoder(stringDecoder),
  priority: numberDecoder,
  branchName: nullableDecoder(stringDecoder),
  worktreeId: nullableDecoder(stringDecoder),
  sessionId: nullableDecoder(stringDecoder),
  blockedReason: nullableDecoder(stringDecoder),
  attempt: numberDecoder,
  createdAt: stringDecoder,
  updatedAt: stringDecoder,
  startedAt: nullableDecoder(stringDecoder),
  finishedAt: nullableDecoder(stringDecoder),
});

const runtimeTaskSummaryDecoder: Decoder<OrchestratorRuntimeTaskSummary> = objectDecoder(
  'OrchestratorRuntimeTaskSummary',
  {
    taskId: stringDecoder,
    title: stringDecoder,
    workflowState: workflowStateDecoder,
    runState: runStateDecoder,
    attemptPhase: nullableDecoder(attemptPhaseDecoder),
    sessionId: nullableDecoder(stringDecoder),
    worktreeId: nullableDecoder(stringDecoder),
    lastRuntimeMessage: nullableDecoder(stringDecoder),
    lastActivityAt: nullableDecoder(stringDecoder),
  },
);

const runtimeEventDecoder: Decoder<OrchestratorRuntimeEvent> = objectDecoder(
  'OrchestratorRuntimeEvent',
  {
    id: stringDecoder,
    taskId: stringDecoder,
    taskTitle: stringDecoder,
    kind: stringDecoder,
    message: stringDecoder,
    createdAt: stringDecoder,
  },
);

/**
 * Business Logic（为什么需要这个 decoder）:
 *   状态条依赖完整 runtime snapshot，残缺数据不得伪装 live。
 *
 * Code Logic（这个 decoder 做什么）:
 *   解码 remoteStatus 联合与 running/retrying/events 数组。
 */
export const orchestratorRuntimeSnapshotDecoder: Decoder<OrchestratorRuntimeSnapshot> =
  objectDecoder<OrchestratorRuntimeSnapshot>('OrchestratorRuntimeSnapshot', {
    projectId: stringDecoder,
    projectKind: stringDecoder,
    remoteStatus: unionDecoder<'local' | OrchestratorRemoteRuntimeStatus>(
      'OrchestratorRuntimeRemoteStatus',
      [literalDecoder('local'), remoteRuntimeStatusDecoder],
    ),
    generatedAt: stringDecoder,
    latestTickAt: nullableDecoder(stringDecoder),
    lastDispatchAt: nullableDecoder(stringDecoder),
    lastDispatchedCount: numberDecoder,
    schedulerEnabled: booleanDecoder,
    workflowSource: stringDecoder,
    workflowValid: booleanDecoder,
    workflowError: nullableDecoder(stringDecoder),
    maxConcurrentTasks: numberDecoder,
    slotsUsed: numberDecoder,
    slotsAvailable: numberDecoder,
    latestError: nullableDecoder(stringDecoder),
    runningTasks: arrayDecoder(runtimeTaskSummaryDecoder),
    retryingTasks: arrayDecoder(runtimeTaskSummaryDecoder),
    recentEvents: arrayDecoder(runtimeEventDecoder),
  });

/**
 * Business Logic（为什么需要这个 decoder）:
 *   pending remote 区只展示 outbox，错误 status 不得进入动作路径。
 *
 * Code Logic（这个 decoder 做什么）:
 *   解码 outbox 字段与 status 枚举。
 */
export const orchestratorRemoteOutboxItemDecoder: Decoder<OrchestratorRemoteOutboxItem> =
  objectDecoder('OrchestratorRemoteOutboxItem', {
    id: stringDecoder,
    deviceId: stringDecoder,
    deviceName: stringDecoder,
    remoteProjectPath: stringDecoder,
    remoteProjectId: nullableDecoder(stringDecoder),
    requestJson: stringDecoder,
    status: outboxStatusDecoder,
    remoteTaskId: nullableDecoder(stringDecoder),
    lastError: nullableDecoder(stringDecoder),
    createdAt: stringDecoder,
    updatedAt: stringDecoder,
    sentAt: nullableDecoder(stringDecoder),
  });

/**
 * Business Logic（为什么需要这个 decoder）:
 *   task view 是 local/remote/pendingRemote 判别联合。
 *
 * Code Logic（这个 decoder 做什么）:
 *   按 origin 分支解码。
 */
export const orchestratorTaskViewDecoder: Decoder<OrchestratorTaskView> =
  unionDecoder<OrchestratorTaskView>('OrchestratorTaskView', [
    objectDecoder('OrchestratorTaskViewLocal', {
      origin: literalDecoder('local'),
      task: orchestratorTaskDecoder,
    }),
    objectDecoder('OrchestratorTaskViewRemote', {
      origin: literalDecoder('remote'),
      task: orchestratorTaskDecoder,
      deviceId: stringDecoder,
      deviceName: stringDecoder,
    }),
    objectDecoder('OrchestratorTaskViewPendingRemote', {
      origin: literalDecoder('pendingRemote'),
      item: orchestratorRemoteOutboxItemDecoder,
    }),
  ]);

/** task view 列表 decoder。 */
export const orchestratorTaskViewListDecoder: Decoder<OrchestratorTaskView[]> = arrayDecoder(
  orchestratorTaskViewDecoder,
);

/**
 * Business Logic（为什么需要这个 decoder）:
 *   HTTP task-views/list 响应包装为 `{ views: [...] }`。
 *
 * Code Logic（这个 decoder 做什么）:
 *   解码 views 数组。
 */
export const orchestratorTaskViewListResponseDecoder: Decoder<{ views: OrchestratorTaskView[] }> =
  objectDecoder('OrchestratorTaskViewListResponse', {
    views: orchestratorTaskViewListDecoder,
  });

/**
 * Business Logic（为什么需要这个 decoder）:
 *   任务详情 evidence 时间线依赖完整 id/kind/content，残缺项不得进入详情态。
 *
 * Code Logic（这个 decoder 做什么）:
 *   解码 OrchestratorEvidence 全部必填 string 字段（kind 允许前向 string）。
 */
export const orchestratorEvidenceDecoder: Decoder<OrchestratorEvidence> = objectDecoder(
  'OrchestratorEvidence',
  {
    id: stringDecoder,
    taskId: stringDecoder,
    kind: stringDecoder,
    title: stringDecoder,
    summary: stringDecoder,
    content: stringDecoder,
    createdAt: stringDecoder,
  },
);

/** evidence 列表 decoder。 */
export const orchestratorEvidenceListDecoder: Decoder<OrchestratorEvidence[]> = arrayDecoder(
  orchestratorEvidenceDecoder,
);

/**
 * Business Logic（为什么需要这个 decoder）:
 *   项目刷新结果驱动 dispatched 提示，损坏字段不得伪造领取成功。
 *
 * Code Logic（这个 decoder 做什么）:
 *   解码 projectId 与 dispatched 计数。
 */
export const orchestratorProjectRefreshResultDecoder: Decoder<{
  projectId: string;
  dispatched: number;
}> = objectDecoder('OrchestratorProjectRefreshResult', {
  projectId: stringDecoder,
  dispatched: numberDecoder,
});

/**
 * Business Logic（为什么需要这个 decoder）:
 *   Changes tab 单文件条目必须带 path/status 与 binary/truncated 标记，残缺 patch 结构不得进入 UI。
 *
 * Code Logic（这个 decoder 做什么）:
 *   解码 ReviewDiffFile 的 path/status/additions/deletions/patch/binary/truncated。
 */
export const reviewDiffFileDecoder: Decoder<ReviewDiffFile> = objectDecoder('ReviewDiffFile', {
  path: stringDecoder,
  status: stringDecoder,
  additions: numberDecoder,
  deletions: numberDecoder,
  patch: nullableDecoder(stringDecoder),
  binary: booleanDecoder,
  truncated: booleanDecoder,
});

/**
 * Business Logic（为什么需要这个 decoder）:
 *   Deliver 前 digest 与 Changes 展示依赖完整 review diff 快照；损坏 DTO 不得进入审阅态。
 *
 * Code Logic（这个 decoder 做什么）:
 *   解码 OrchestratorReviewDiff 身份字段、files 列表、totalFiles/truncated 与 reviewDigest。
 */
export const orchestratorReviewDiffDecoder: Decoder<OrchestratorReviewDiff> = objectDecoder(
  'OrchestratorReviewDiff',
  {
    taskId: stringDecoder,
    baseRef: stringDecoder,
    headRef: stringDecoder,
    files: arrayDecoder(reviewDiffFileDecoder),
    totalFiles: numberDecoder,
    truncated: booleanDecoder,
    reviewDigest: stringDecoder,
  },
);

/**
 * Business Logic（为什么需要这个 decoder）:
 *   Mobile/P2P review-diff HTTP 响应包装为 `{diff}`，边界必须 fail-closed 解码。
 *
 * Code Logic（这个 decoder 做什么）:
 *   解码 `{diff: OrchestratorReviewDiff}`。
 */
export const orchestratorReviewDiffResponseDecoder: Decoder<{ diff: OrchestratorReviewDiff }> =
  objectDecoder('OrchestratorReviewDiffResponse', {
    diff: orchestratorReviewDiffDecoder,
  });

const workflowDocumentStatusDecoder: Decoder<WorkflowDocumentStatus> = enumDecoder(
  'WorkflowDocumentStatus',
  ['missing', 'valid', 'invalid', 'readError'] as const,
);

/**
 * Business Logic（为什么需要这个 decoder）:
 *   向导诊断列表必须 fail-closed，残缺 path/code/message 不得进入 UI。
 *
 * Code Logic（这个 decoder 做什么）:
 *   解码 WorkflowDiagnostic 的 path/line/column/code/message。
 */
export const workflowDiagnosticDecoder: Decoder<WorkflowDiagnostic> = objectDecoder(
  'WorkflowDiagnostic',
  {
    path: stringDecoder,
    line: nullableDecoder(numberDecoder),
    column: nullableDecoder(numberDecoder),
    code: stringDecoder,
    message: stringDecoder,
  },
);

/**
 * Business Logic（为什么需要这个 decoder）:
 *   get/validate/save WORKFLOW 响应共享 DTO，损坏字段不得进入向导草稿或 CAS hash。
 *
 * Code Logic（这个 decoder 做什么）:
 *   解码 status/content/contentHash/diagnostics 与可选 preview。
 */
export const workflowDocumentDecoder: Decoder<WorkflowDocument> = objectDecoder(
  'WorkflowDocument',
  {
    status: workflowDocumentStatusDecoder,
    content: nullableDecoder(stringDecoder),
    contentHash: nullableDecoder(stringDecoder),
    diagnostics: arrayDecoder(workflowDiagnosticDecoder),
    preview: optionalDecoder(nullableDecoder(stringDecoder)),
  },
);

/**
 * Business Logic: provider wire 四内置严格解码；未知值 fail-closed（不映射 Claude）。
 * Code Logic: enumDecoder AgentProviderId。
 */
export const agentProviderIdDecoder: Decoder<AgentProviderId> = enumDecoder('AgentProviderId', [
  'claudeCodeVisible',
  'codexVisible',
  'genericTerminal',
  'openCodeVisible',
  'grokBuildVisible',
  'geminiCliVisible',
  'cursorCliVisible',
] as const);

/**
 * Business Logic: completion 合同严格解码。
 * Code Logic: sentinelLine | hookEvent | manual。
 */
export const agentCompletionContractDecoder: Decoder<AgentCompletionContract> = enumDecoder(
  'AgentCompletionContract',
  ['sentinelLine', 'hookEvent', 'manual'] as const,
);

const openCodeBridgeStatusWireDecoder: Decoder<
  'ready' | 'previewRequired' | 'conflict' | 'unsupported'
> = enumDecoder('OpenCodeBridgeStatusWire', [
  'ready',
  'previewRequired',
  'conflict',
  'unsupported',
] as const);

/**
 * Business Logic: catalog 条目严格 provider/contract；未知 provider 拒绝整包。
 * Code Logic: objectDecoder adapter item。
 */
export const orchestratorAgentAdapterCatalogItemDecoder: Decoder<OrchestratorAgentAdapterCatalogItem> =
  objectDecoder('OrchestratorAgentAdapterCatalogItem', {
    provider: agentProviderIdDecoder,
    available: booleanDecoder,
    completionContract: agentCompletionContractDecoder,
    supportsResume: booleanDecoder,
    supportsUsage: booleanDecoder,
    reasonCode: optionalDecoder(nullableDecoder(stringDecoder)),
    executable: optionalDecoder(nullableDecoder(stringDecoder)),
    version: optionalDecoder(nullableDecoder(stringDecoder)),
    supportEvidence: optionalDecoder(nullableDecoder(stringDecoder)),
    bridgeStatus: optionalDecoder(nullableDecoder(openCodeBridgeStatusWireDecoder)),
    blockedReason: optionalDecoder(nullableDecoder(stringDecoder)),
  });

/**
 * Business Logic: catalog 列表 fail-closed。
 * Code Logic: objectDecoder adapters array。
 */
export const orchestratorAgentAdapterCatalogDecoder: Decoder<OrchestratorAgentAdapterCatalog> =
  objectDecoder('OrchestratorAgentAdapterCatalog', {
    adapters: arrayDecoder(orchestratorAgentAdapterCatalogItemDecoder),
  });
