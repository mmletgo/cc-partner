import { describe, test } from 'vitest';
import type { TFunction } from 'i18next';
import type {
  OrchestratorRemoteOutboxItem,
  OrchestratorTask,
  OrchestratorTaskStatus,
  OrchestratorTaskView,
  OrchestratorWorkflowState,
} from './types';
import type { PillTone } from '@/components/primitives';
import {
  canQueueOrchestratorTask,
  canQueueOrchestratorTaskForProject,
  canStartOrchestratorTaskForProject,
  canRequestReworkForProject,
  canDeliverReviewedTaskForProject,
  canCancelOrchestratorTaskForProject,
  canCompleteAgentRunForProject,
  canControlBlockedTaskForProject,
  groupOrchestratorTasks,
  ORCHESTRATOR_STATUSES,
  orchestratorAttemptLabel,
  orchestratorCreateResultMatchesProject,
  orchestratorEvidenceKindLabel,
  orchestratorEvidenceKindTone,
  resolveOrchestratorActionSelection,
  orchestratorWorkflowStateTone,
  orchestratorStatusTone,
  orchestratorTaskProgressMessage,
  resolveOrchestratorTaskLoad,
} from './orchestrator';

/**
 * Business Logic（为什么需要这个函数）:
 *   Orchestrator helper 测试需要在契约不一致时立即失败，避免页面把任务放到错误队列或使用无效 Pill tone。
 *
 * Code Logic（这个函数做什么）:
 *   condition 为 false 时抛出 Error，让测试用例失败。
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
function createTask(
  id: string,
  status: OrchestratorTaskStatus,
  projectId = 'project-1',
  attempt = 0,
): OrchestratorTask {
  return {
    id,
    projectId,
    title: `task-${id}`,
    goal: 'goal',
    acceptanceCriteria: 'acceptance',
    status,
    workflowState: 'todo',
    runState: 'idle',
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
    attempt,
    createdAt: '2026-07-05T00:00:00Z',
    updatedAt: '2026-07-05T00:00:00Z',
    startedAt: null,
    finishedAt: null,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Orchestrator helper 的文案契约必须验证 i18n key 和插值参数，避免组件绕过 namespace 硬编码文案。
 *
 * Code Logic（这个函数做什么）:
 *   提供最小 TFunction stub，只覆盖本测试关心的 orchestrator namespace key，并支持 {{attempt}}/{{deviceName}} 插值。
 */
function createTranslator(): TFunction<'orchestrator'> {
  const translations: Record<string, string> = {
    'evidence.kind.developmentAttempt': 'Development attempt',
    'evidence.kind.verificationReview': 'Verification review',
    'evidence.kind.repairPrompt': 'Repair prompt',
    'evidence.kind.remoteOutbox': 'Remote outbox',
    'evidence.kind.generic': 'Evidence',
    'detail.attemptLabel': 'Attempt {{attempt}}',
    'detail.noAttempt': 'Not started',
    'progress.running': 'Attempt {{attempt}} is running in the active terminal.',
    'progress.remoteRunning': 'Attempt {{attempt}} is running on {{deviceName}}.',
    'progress.verifying': 'Attempt {{attempt}} is being verified.',
    'progress.repairing': 'Attempt {{attempt}} is preparing a repair run.',
    'progress.remoteOutbox': 'Waiting to send to {{deviceName}}.',
  };

  return ((key: string, options?: Record<string, unknown>) => {
    const template = translations[key] ?? key;
    return template.replace(/\{\{(\w+)\}\}/g, (_, name: string) =>
      String(options?.[name] ?? ''),
    );
  }) as TFunction<'orchestrator'>;
}

describe('orchestrator', () => {
  test('groups every legacy status and maps status tones', () => {
    const allStatusTasks = ORCHESTRATOR_STATUSES.map((status) => createTask(`${status}-1`, status));
    const groups = groupOrchestratorTasks(allStatusTasks);

    for (const status of ORCHESTRATOR_STATUSES) {
      const task = groups[status][0];
      assert(groups[status].length === 1, `groupOrchestratorTasks should group exactly one ${status} task`);
      assert(task?.status === status, `groupOrchestratorTasks should keep ${status} task status`);
      assert(task?.id === `${status}-1`, `groupOrchestratorTasks should keep original ${status} task id`);
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
  });

  test('maps workflow state tones and evidence kind labels/tones', () => {
    const t = createTranslator();
    const supportedTones = new Set<PillTone>(['neutral', 'success', 'warn', 'danger', 'accent']);
    const workflowStates: readonly OrchestratorWorkflowState[] = [
      'backlog',
      'todo',
      'inProgress',
      'humanReview',
      'rework',
      'merging',
      'done',
      'canceled',
    ];

    for (const state of workflowStates) {
      assert(
        supportedTones.has(orchestratorWorkflowStateTone(state)),
        `orchestratorWorkflowStateTone should return a supported PillTone for ${state}`,
      );
    }

    assert(orchestratorWorkflowStateTone('done') === 'success', 'done workflow state should use success tone');
    assert(orchestratorWorkflowStateTone('rework') === 'warn', 'rework workflow state should use warn tone');
    assert(
      orchestratorWorkflowStateTone('inProgress') === 'accent',
      'inProgress workflow state should use accent tone',
    );
    assert(
      orchestratorWorkflowStateTone('canceled') === 'danger',
      'canceled workflow state should use danger tone',
    );

    assert(
      orchestratorEvidenceKindLabel('developmentAttempt', t) === 'Development attempt',
      'developmentAttempt evidence kind should use localized label',
    );
    assert(
      orchestratorEvidenceKindLabel('verificationReview', t) === 'Verification review',
      'verificationReview evidence kind should use localized label',
    );
    assert(
      orchestratorEvidenceKindLabel('repairPrompt', t) === 'Repair prompt',
      'repairPrompt evidence kind should use localized label',
    );
    assert(
      orchestratorEvidenceKindLabel('remoteOutbox', t) === 'Remote outbox',
      'remoteOutbox evidence kind should use localized label',
    );
    assert(
      orchestratorEvidenceKindTone('developmentAttempt') === 'accent',
      'developmentAttempt evidence kind should use accent tone',
    );
    assert(
      orchestratorEvidenceKindTone('verificationReview') === 'success',
      'verificationReview evidence kind should use success tone',
    );
    assert(
      orchestratorEvidenceKindTone('repairPrompt') === 'warn',
      'repairPrompt evidence kind should use warn tone',
    );
    assert(
      orchestratorEvidenceKindTone('remoteOutbox') === 'neutral',
      'remoteOutbox evidence kind should use neutral tone',
    );
  });

  test('builds attempt/progress copy and load/selection match helpers', () => {
    const t = createTranslator();
    const runningTask = createTask('running-progress', 'running', 'project-1', 1);
    const verifyingTask = createTask('verifying-progress', 'verifying', 'project-1', 2);
    const repairingTask = createTask('repairing-progress', 'preparing', 'project-1', 3);
    const remoteRunningView: OrchestratorTaskView = {
      origin: 'remote',
      task: runningTask,
      deviceId: 'device-a',
      deviceName: 'Studio Mac',
    };
    const pendingRemoteItem: OrchestratorRemoteOutboxItem = {
      id: 'outbox-1',
      deviceId: 'device-a',
      deviceName: 'Studio Mac',
      remoteProjectPath: '/Users/hans/project',
      remoteProjectId: null,
      requestJson: '{}',
      status: 'pending',
      remoteTaskId: null,
      lastError: null,
      createdAt: '2026-07-05T00:00:00Z',
      updatedAt: '2026-07-05T00:00:00Z',
      sentAt: null,
    };

    assert(
      orchestratorAttemptLabel(runningTask, t) === 'Attempt 1',
      'running task should expose current attempt label',
    );
    assert(
      orchestratorTaskProgressMessage({ origin: 'local', task: runningTask }, t) ===
        'Attempt 1 is running in the active terminal.',
      'running task should expose running progress copy',
    );
    assert(
      orchestratorTaskProgressMessage(remoteRunningView, t) === 'Attempt 1 is running on Studio Mac.',
      'remote running task should expose remote device progress copy',
    );
    assert(
      orchestratorTaskProgressMessage({ origin: 'local', task: verifyingTask }, t) ===
        'Attempt 2 is being verified.',
      'verifying task should expose verifier progress copy',
    );
    assert(
      orchestratorTaskProgressMessage({ origin: 'local', task: repairingTask }, t) ===
        'Attempt 3 is preparing a repair run.',
      'preparing task after the first attempt should expose repair progress copy',
    );
    assert(
      orchestratorTaskProgressMessage({ origin: 'pendingRemote', item: pendingRemoteItem }, t) ===
        'Waiting to send to Studio Mac.',
      'pending remote task should expose outbox progress copy',
    );

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
    assert(
      resolveOrchestratorActionSelection('task-b', 'task-a') === 'task-b',
      'resolveOrchestratorActionSelection should not steal selection back to an old action response',
    );
    assert(
      resolveOrchestratorActionSelection(null, 'task-a') === null,
      'resolveOrchestratorActionSelection should keep the detail drawer closed when selection is empty',
    );
  });

  test('gates queue/start/rework/deliver/cancel/complete/blocked actions by project and state', () => {
    assert(
      canQueueOrchestratorTask(createTask('draft-queue', 'draft')),
      'canQueueOrchestratorTask should allow draft tasks to enter the queue',
    );
    assert(
      !canQueueOrchestratorTask(createTask('running-queue', 'running')),
      'canQueueOrchestratorTask should reject running tasks',
    );
    assert(!canQueueOrchestratorTask(null), 'canQueueOrchestratorTask should reject null tasks');

    assert(
      canQueueOrchestratorTaskForProject(createTask('draft-same-project', 'draft'), 'project-1'),
      'canQueueOrchestratorTaskForProject should allow draft tasks from the active project',
    );
    assert(
      !canQueueOrchestratorTaskForProject(
        createTask('draft-other-project', 'draft', 'project-2'),
        'project-1',
      ),
      'canQueueOrchestratorTaskForProject should reject draft tasks from another project',
    );
    assert(
      !canQueueOrchestratorTaskForProject(createTask('running-same-project', 'running'), 'project-1'),
      'canQueueOrchestratorTaskForProject should reject running tasks from the active project',
    );
    assert(
      !canQueueOrchestratorTaskForProject(null, 'project-1'),
      'canQueueOrchestratorTaskForProject should reject null tasks',
    );

    const backlogDraftTask = createTask('backlog-start', 'draft');
    backlogDraftTask.workflowState = 'backlog';
    backlogDraftTask.runState = 'idle';
    const todoIdleTask = createTask('todo-start', 'queued');
    todoIdleTask.workflowState = 'todo';
    todoIdleTask.runState = 'idle';
    const todoRunningTask = createTask('todo-running-start', 'running');
    todoRunningTask.workflowState = 'todo';
    todoRunningTask.runState = 'running';

    assert(
      canStartOrchestratorTaskForProject(backlogDraftTask, 'project-1'),
      'canStartOrchestratorTaskForProject should allow Backlog/Draft tasks',
    );
    assert(
      canStartOrchestratorTaskForProject(todoIdleTask, 'project-1'),
      'canStartOrchestratorTaskForProject should allow Todo/Idle tasks',
    );
    assert(
      !canStartOrchestratorTaskForProject(todoRunningTask, 'project-1'),
      'canStartOrchestratorTaskForProject should reject tasks with active run state',
    );
    assert(
      !canStartOrchestratorTaskForProject(backlogDraftTask, 'project-2'),
      'canStartOrchestratorTaskForProject should reject tasks from another project',
    );

    const humanReviewTask = createTask('reviewed', 'done');
    humanReviewTask.workflowState = 'humanReview';
    humanReviewTask.runState = 'idle';
    const deliveredDoneTask = createTask('delivered', 'done');
    deliveredDoneTask.workflowState = 'done';
    deliveredDoneTask.runState = 'idle';

    assert(
      canRequestReworkForProject(humanReviewTask, 'project-1'),
      'canRequestReworkForProject should allow HumanReview tasks',
    );
    assert(
      !canRequestReworkForProject(deliveredDoneTask, 'project-1'),
      'canRequestReworkForProject should reject already delivered Done tasks',
    );
    assert(
      canDeliverReviewedTaskForProject(humanReviewTask, 'project-1'),
      'canDeliverReviewedTaskForProject should allow HumanReview tasks',
    );
    assert(
      !canDeliverReviewedTaskForProject(deliveredDoneTask, 'project-1'),
      'canDeliverReviewedTaskForProject should reject Done tasks outside HumanReview',
    );

    assert(
      canCancelOrchestratorTaskForProject(createTask('running-cancel', 'running'), 'project-1'),
      'canCancelOrchestratorTaskForProject should allow active project running tasks',
    );
    assert(
      canCancelOrchestratorTaskForProject(createTask('blocked-cancel', 'blocked'), 'project-1'),
      'canCancelOrchestratorTaskForProject should allow blocked tasks',
    );
    assert(
      canCancelOrchestratorTaskForProject(humanReviewTask, 'project-1'),
      'canCancelOrchestratorTaskForProject should allow HumanReview tasks despite legacy done status',
    );
    assert(
      !canCancelOrchestratorTaskForProject(deliveredDoneTask, 'project-1'),
      'canCancelOrchestratorTaskForProject should reject completed tasks',
    );
    assert(
      !canCancelOrchestratorTaskForProject(createTask('aborted-cancel', 'aborted'), 'project-1'),
      'canCancelOrchestratorTaskForProject should reject already canceled tasks',
    );

    assert(
      canCompleteAgentRunForProject(createTask('running-same-project', 'running'), 'project-1'),
      'canCompleteAgentRunForProject should allow running tasks from the active project',
    );
    assert(
      !canCompleteAgentRunForProject(createTask('running-other-project', 'running', 'project-2'), 'project-1'),
      'canCompleteAgentRunForProject should reject running tasks from another project',
    );
    assert(
      !canCompleteAgentRunForProject(createTask('queued-complete', 'queued'), 'project-1'),
      'canCompleteAgentRunForProject should reject non-running tasks',
    );
    assert(
      canControlBlockedTaskForProject(createTask('blocked-same-project', 'blocked'), 'project-1'),
      'canControlBlockedTaskForProject should allow blocked tasks from the active project',
    );
    assert(
      !canControlBlockedTaskForProject(createTask('blocked-other-project', 'blocked', 'project-2'), 'project-1'),
      'canControlBlockedTaskForProject should reject blocked tasks from another project',
    );
    assert(
      !canControlBlockedTaskForProject(createTask('running-blocked-control', 'running'), 'project-1'),
      'canControlBlockedTaskForProject should reject non-blocked tasks',
    );
  });
});
