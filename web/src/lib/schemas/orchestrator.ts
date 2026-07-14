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
  OrchestratorAttemptPhase,
  OrchestratorRemoteOutboxItem,
  OrchestratorRemoteOutboxStatus,
  OrchestratorRemoteRuntimeStatus,
  OrchestratorRunState,
  OrchestratorRuntimeEvent,
  OrchestratorRuntimeSnapshot,
  OrchestratorRuntimeTaskSummary,
  OrchestratorTask,
  OrchestratorTaskStatus,
  OrchestratorTaskView,
  OrchestratorWorkflowState,
} from '../types/orchestrator';
import {
  arrayDecoder,
  booleanDecoder,
  enumDecoder,
  literalDecoder,
  nullableDecoder,
  numberDecoder,
  objectDecoder,
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
