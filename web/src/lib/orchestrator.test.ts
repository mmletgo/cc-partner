import type { OrchestratorTask, OrchestratorTaskStatus } from './types';
import type { PillTone } from '@/components/primitives';
import {
  groupOrchestratorTasks,
  ORCHESTRATOR_STATUSES,
  orchestratorCreateResultMatchesProject,
  orchestratorStatusTone,
  resolveOrchestratorTaskLoad,
} from './orchestrator';

/**
 * Business Logic（为什么需要这个函数）:
 *   Orchestrator helper 测试需要在契约不一致时立即失败，避免页面把任务放到错误队列或使用无效 Pill tone。
 *
 * Code Logic（这个函数做什么）:
 *   condition 为 false 时抛出 Error，让 tsx 测试进程以非零状态退出。
 */
function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   分组测试只关心任务状态，但 DTO 需要完整字段；集中构造样例能让用例只暴露状态差异。
 *
 * Code Logic（这个函数做什么）:
 *   接收 id 与 status，返回一个字段完整且时间戳稳定的 OrchestratorTask。
 */
function createTask(id: string, status: OrchestratorTaskStatus): OrchestratorTask {
  return {
    id,
    projectId: 'project-1',
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

const queued = createTask('queued-1', 'queued');
const blocked = createTask('blocked-1', 'blocked');
const running = createTask('running-1', 'running');
const groups = groupOrchestratorTasks([queued, blocked, running]);

assert(groups.queued.length === 1, 'groupOrchestratorTasks should put queued tasks into queued group');
assert(groups.queued[0]?.id === queued.id, 'queued group should keep original queued task');
assert(groups.blocked.length === 1, 'groupOrchestratorTasks should put blocked tasks into blocked group');
assert(groups.blocked[0]?.id === blocked.id, 'blocked group should keep original blocked task');
assert(groups.running.length === 1, 'groupOrchestratorTasks should keep running task in running group');

for (const status of ORCHESTRATOR_STATUSES) {
  assert(Array.isArray(groups[status]), `groupOrchestratorTasks should create an array for ${status}`);
}

const supportedTones = new Set<PillTone>(['neutral', 'success', 'warn', 'danger', 'accent']);

for (const status of ORCHESTRATOR_STATUSES) {
  assert(
    supportedTones.has(orchestratorStatusTone(status)),
    `orchestratorStatusTone should return a supported PillTone for ${status}`,
  );
}

assert(orchestratorStatusTone('done') === 'success', 'done status should use success tone');
assert(orchestratorStatusTone('blocked') === 'danger', 'blocked status should use danger tone');
assert(orchestratorStatusTone('running') === 'accent', 'running status should use accent tone');
assert(orchestratorStatusTone('queued') === 'neutral', 'queued status should use neutral tone');
assert(orchestratorStatusTone('aborted') === 'danger', 'aborted status should use danger tone');

assert(
  JSON.stringify(resolveOrchestratorTaskLoad(true, 'project-1')) ===
    JSON.stringify({ kind: 'waiting' }),
  'resolveOrchestratorTaskLoad should wait while Workbench projects are loading',
);
assert(
  JSON.stringify(resolveOrchestratorTaskLoad(false, null)) ===
    JSON.stringify({ kind: 'empty' }),
  'resolveOrchestratorTaskLoad should stay empty after project loading completes without active project',
);
assert(
  JSON.stringify(resolveOrchestratorTaskLoad(false, 'project-1')) ===
    JSON.stringify({ kind: 'load', projectId: 'project-1' }),
  'resolveOrchestratorTaskLoad should load only for a concrete active project id',
);

assert(
  orchestratorCreateResultMatchesProject('project-1', 'project-1'),
  'orchestratorCreateResultMatchesProject should accept matching project ids',
);
assert(
  !orchestratorCreateResultMatchesProject('project-2', 'project-1'),
  'orchestratorCreateResultMatchesProject should reject stale create results after project switch',
);
assert(
  !orchestratorCreateResultMatchesProject(null, 'project-1'),
  'orchestratorCreateResultMatchesProject should reject create results when active project was cleared',
);

console.log('orchestrator.test.ts passed');
