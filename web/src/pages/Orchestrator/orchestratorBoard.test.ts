import { describe, test } from 'vitest';
import type {
  OrchestratorRunState,
  OrchestratorTask,
  OrchestratorTaskStatus,
  OrchestratorTaskView,
  OrchestratorWorkflowState,
} from '@/lib/types';
import type { OrchestratorRenderableTask } from '@/lib/orchestratorRemote';
import {
  ORCHESTRATOR_BOARD_LANES,
  canMoveRenderableTaskToWorkflowState,
  groupRenderableTasksByWorkflowState,
  isActiveOrchestratorRunState,
} from './orchestratorBoard';

/**
 * Business Logic（为什么需要这个函数）:
 *   Orchestrator 看板 helper 测试需要在泳道顺序或拖拽规则偏离后端契约时立即失败。
 *
 * Code Logic（这个函数做什么）:
 *   condition 为 false 时抛出 Error，让 tsx 测试进程以非零状态退出。
 */
function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   看板规则只关心 workflowState 与 runState，但渲染项必须携带完整任务 DTO。
 *
 * Code Logic（这个函数做什么）:
 *   构造字段完整的 OrchestratorTask，并允许用参数覆盖 legacy status、workflowState 与 runState。
 */
function createTask(
  id: string,
  workflowState: OrchestratorWorkflowState,
  runState: OrchestratorRunState = 'idle',
  status: OrchestratorTaskStatus = 'queued',
): OrchestratorTask {
  return {
    id,
    projectId: 'project-1',
    title: `task-${id}`,
    goal: 'goal',
    acceptanceCriteria: 'acceptance',
    status,
    workflowState,
    runState,
    attemptPhase: null,
    source: 'local',
    externalId: null,
    externalIdentifier: null,
    externalUrl: null,
    externalState: null,
    externalLabels: null,
    runnerProvider: null,
    claudeSessionId: null,
    transcriptPath: null,
    runtimeStartedAt: null,
    lastActivityAt: null,
    lastRuntimeEvent: null,
    lastRuntimeMessage: null,
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
 *   拖拽规则区分 local 与 remote 任务，测试需要快速构造两种渲染项。
 *
 * Code Logic（这个函数做什么）:
 *   将完整 task 包装为 OrchestratorRenderableTask，并同步生成对应 task view。
 */
function createRenderableTask(
  id: string,
  workflowState: OrchestratorWorkflowState,
  runState: OrchestratorRunState = 'idle',
  origin: 'local' | 'remote' = 'local',
): OrchestratorRenderableTask {
  const task = createTask(id, workflowState, runState);
  const view: OrchestratorTaskView =
    origin === 'local'
      ? { origin: 'local', task }
      : {
          origin: 'remote',
          task,
          deviceId: 'device-1',
          deviceName: 'MacBook Pro',
        };

  return {
    origin,
    task,
    deviceId: origin === 'remote' ? 'device-1' : null,
    deviceName: origin === 'remote' ? 'MacBook Pro' : null,
    view,
  };
}

describe('orchestratorBoard', () => {
  test('board lanes, grouping and drag rules follow backend workflow contract', () => {
    const expectedLaneOrder: readonly OrchestratorWorkflowState[] = [
      'backlog',
      'todo',
      'inProgress',
      'humanReview',
      'rework',
      'merging',
      'done',
      'canceled',
    ];

    assert(
      JSON.stringify(ORCHESTRATOR_BOARD_LANES) === JSON.stringify(expectedLaneOrder),
      'ORCHESTRATOR_BOARD_LANES should match backend WORKFLOW_LANE_ORDER',
    );

    const grouped = groupRenderableTasksByWorkflowState([
      createRenderableTask('todo-1', 'todo'),
      createRenderableTask('review-1', 'humanReview'),
    ]);

    assert(grouped.todo[0]?.task.id === 'todo-1', 'group helper should place todo task in todo lane');
    assert(
      grouped.humanReview[0]?.task.id === 'review-1',
      'group helper should place human review task in humanReview lane',
    );
    assert(grouped.backlog.length === 0, 'group helper should preserve empty backlog lane');
    assert(grouped.done.length === 0, 'group helper should preserve empty done lane');

    assert(
      canMoveRenderableTaskToWorkflowState(createRenderableTask('local-next', 'todo'), 'inProgress'),
      'local idle task should move to the next lane',
    );
    assert(
      canMoveRenderableTaskToWorkflowState(createRenderableTask('local-prev', 'todo'), 'backlog'),
      'local idle task should move to the previous lane',
    );
    assert(
      !canMoveRenderableTaskToWorkflowState(createRenderableTask('local-skip', 'todo'), 'humanReview'),
      'local task should not move across two lanes',
    );
    assert(
      canMoveRenderableTaskToWorkflowState(createRenderableTask('remote-next', 'todo', 'idle', 'remote'), 'inProgress'),
      'remote idle task should move to the next lane',
    );
    assert(
      !canMoveRenderableTaskToWorkflowState(
        createRenderableTask('remote-skip', 'todo', 'idle', 'remote'),
        'humanReview',
      ),
      'remote task should not move across two lanes',
    );
    assert(
      !canMoveRenderableTaskToWorkflowState(
        createRenderableTask('remote-running', 'todo', 'running', 'remote'),
        'inProgress',
      ),
      'remote running task should not be draggable',
    );
    assert(
      !canMoveRenderableTaskToWorkflowState(createRenderableTask('active-next', 'todo', 'running'), 'inProgress'),
      'active runtime task should not be draggable',
    );
    assert(
      !canMoveRenderableTaskToWorkflowState(createRenderableTask('same-lane', 'todo'), 'todo'),
      'task should not move to its current lane',
    );
    assert(
      canMoveRenderableTaskToWorkflowState(createRenderableTask('blocked-next', 'todo', 'blocked'), 'inProgress'),
      'blocked run state should follow adjacent lane movement rules',
    );
    assert(
      canMoveRenderableTaskToWorkflowState(createRenderableTask('idle-next', 'todo', 'idle'), 'inProgress'),
      'idle run state should follow adjacent lane movement rules',
    );

    const activeStates: readonly OrchestratorRunState[] = [
      'preparing',
      'running',
      'verifying',
      'delivering',
    ];
    const inactiveStates: readonly OrchestratorRunState[] = [
      'idle',
      'queued',
      'retrying',
      'blocked',
    ];

    for (const state of activeStates) {
      assert(isActiveOrchestratorRunState(state), `${state} should be treated as an active run state`);
    }

    for (const state of inactiveStates) {
      assert(!isActiveOrchestratorRunState(state), `${state} should not be treated as an active run state`);
    }

    assert(
      !canMoveRenderableTaskToWorkflowState(null, 'todo'),
      'null renderable task should not be draggable',
    );
  });
});
