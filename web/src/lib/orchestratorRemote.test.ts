import type {
  OrchestratorRemoteOutboxItem,
  OrchestratorTask,
  OrchestratorTaskStatus,
  OrchestratorTaskView,
} from './types';
import {
  getOrchestratorTaskViewProjectId,
  getOrchestratorTaskViewTaskId,
  groupOrchestratorRenderableTasks,
  isOrchestratorTaskViewActionable,
  splitOrchestratorTaskViews,
  upsertOrchestratorTaskView,
} from './orchestratorRemote';

/**
 * Business Logic（为什么需要这个函数）:
 *   Orchestrator remote helper 测试需要在契约不一致时立即失败，避免 pending outbox 被当作真实任务操作。
 *
 * Code Logic（这个函数做什么）:
 *   condition 为 false 时抛出 Error，让 tsx 测试进程以非零状态退出。
 */
function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   remote-aware helper 测试需要构造 local/remote 任务，但每个用例只关心少数字段。
 *
 * Code Logic（这个函数做什么）:
 *   接收 id、status 和 projectId，返回字段完整且时间戳稳定的 OrchestratorTask。
 */
function createTask(
  id: string,
  status: OrchestratorTaskStatus,
  projectId = 'project-1',
): OrchestratorTask {
  return {
    id,
    projectId,
    title: `task-${id}`,
    goal: 'goal',
    acceptanceCriteria: 'acceptance',
    status,
    priority: 0,
    branchName: null,
    worktreeId: null,
    sessionId: null,
    blockedReason: null,
    attempt: 0,
    createdAt: '2026-07-05T00:00:00Z',
    updatedAt: '2026-07-05T00:00:00Z',
    startedAt: null,
    finishedAt: null,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   pending remote outbox 测试需要稳定样例，验证 UI 不把离线远端草稿误当作任务。
 *
 * Code Logic（这个函数做什么）:
 *   返回字段完整的 OrchestratorRemoteOutboxItem，可通过 partial 覆盖状态和错误字段。
 */
function createPendingItem(
  partial: Partial<OrchestratorRemoteOutboxItem> = {},
): OrchestratorRemoteOutboxItem {
  return {
    id: 'outbox-1',
    deviceId: 'device-1',
    deviceName: 'MacBook Pro',
    remoteProjectPath: '/remote/project',
    remoteProjectId: 'remote-project-1',
    requestJson: '{"title":"remote task"}',
    status: 'pending',
    remoteTaskId: null,
    lastError: null,
    createdAt: '2026-07-05T00:00:00Z',
    updatedAt: '2026-07-05T00:00:00Z',
    sentAt: null,
    ...partial,
  };
}

const localView: OrchestratorTaskView = {
  origin: 'local',
  task: createTask('local-1', 'draft'),
};
const remoteView: OrchestratorTaskView = {
  origin: 'remote',
  task: createTask('remote-1', 'blocked'),
  deviceId: 'device-1',
  deviceName: 'MacBook Pro',
};
const pendingView: OrchestratorTaskView = {
  origin: 'pendingRemote',
  item: createPendingItem({ status: 'failed', lastError: 'device offline' }),
};

const split = splitOrchestratorTaskViews([localView, pendingView, remoteView]);

assert(
  split.tasks.length === 2,
  'splitOrchestratorTaskViews should return real local/remote tasks',
);
assert(
  split.tasks[0]?.task.id === 'local-1',
  'splitOrchestratorTaskViews should keep local task order',
);
assert(
  split.tasks[1]?.origin === 'remote' && split.tasks[1]?.deviceName === 'MacBook Pro',
  'splitOrchestratorTaskViews should preserve remote device metadata',
);
assert(
  split.pendingRemoteItems.length === 1 && split.pendingRemoteItems[0]?.status === 'failed',
  'splitOrchestratorTaskViews should keep pending remote outbox items separately',
);

const groups = groupOrchestratorRenderableTasks(split.tasks);

assert(
  groups.draft[0]?.task.id === 'local-1',
  'groupOrchestratorRenderableTasks should group local tasks by status',
);
assert(
  groups.blocked[0]?.task.id === 'remote-1',
  'groupOrchestratorRenderableTasks should group remote tasks by status',
);
assert(groups.queued.length === 0, 'groupOrchestratorRenderableTasks should keep empty status buckets');

assert(isOrchestratorTaskViewActionable(localView), 'local task views should be actionable');
assert(isOrchestratorTaskViewActionable(remoteView), 'remote task views should be actionable');
assert(!isOrchestratorTaskViewActionable(pendingView), 'pending remote views should not be actionable');

assert(
  getOrchestratorTaskViewTaskId(remoteView) === 'remote-1',
  'getOrchestratorTaskViewTaskId should read task id from remote views',
);
assert(
  getOrchestratorTaskViewProjectId(remoteView) === 'project-1',
  'getOrchestratorTaskViewProjectId should read project id from remote views',
);
assert(
  getOrchestratorTaskViewTaskId(pendingView) === null &&
    getOrchestratorTaskViewProjectId(pendingView) === null,
  'pending remote views should not expose task id or project id',
);

const replacedRemote = upsertOrchestratorTaskView([localView, remoteView], {
  ...remoteView,
  task: createTask('remote-1', 'queued'),
});

assert(
  replacedRemote.length === 2 &&
    replacedRemote[1]?.origin === 'remote' &&
    replacedRemote[1].task.status === 'queued',
  'upsertOrchestratorTaskView should replace a real task view by task id',
);

const insertedPending = upsertOrchestratorTaskView([localView], pendingView);

assert(
  insertedPending.length === 2 && insertedPending[0]?.origin === 'pendingRemote',
  'upsertOrchestratorTaskView should insert a new pending remote view at the front',
);

const replacedPending = upsertOrchestratorTaskView([pendingView, localView], {
  origin: 'pendingRemote',
  item: createPendingItem({ status: 'sending' }),
});

assert(
  replacedPending[0]?.origin === 'pendingRemote' && replacedPending[0].item.status === 'sending',
  'upsertOrchestratorTaskView should replace pending remote views by outbox id',
);

console.log('orchestratorRemote.test.ts passed');
