import { describe, test } from 'vitest';
import { readFileSync } from 'node:fs';
import {
  ORCHESTRATOR_REMOTE_COMMANDS,
  buildCreateOrchestratorTaskViewInvokeArgs,
  buildListOrchestratorTaskViewsInvokeArgs,
  buildListOrchestratorTaskEvidenceForProjectInvokeArgs,
  buildMoveOrchestratorTaskWorkflowStateInvokeArgs,
  buildOrchestratorRuntimeSnapshotInvokeArgs,
  buildOrchestratorTaskReworkInvokeArgs,
  buildOrchestratorTaskViewActionInvokeArgs,
  orchestratorApi,
} from './orchestrator';

/**
 * Business Logic（为什么需要这个函数）:
 *   Orchestrator API helper 测试需要直接失败并暴露参数契约差异，避免组件层传错命令参数。
 *
 * Code Logic（这个函数做什么）:
 *   condition 为 false 时抛出 Error，让测试用例失败。
 */
function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

describe('orchestrator', () => {
  test('remote command map and public surface match backend contracts', () => {
    assert(
      ORCHESTRATOR_REMOTE_COMMANDS.listTaskViews === 'list_orchestrator_task_views',
      'listTaskViews should use the remote-aware backend command',
    );
    assert(
      ORCHESTRATOR_REMOTE_COMMANDS.createTaskView === 'create_orchestrator_task_view',
      'createTaskView should use the remote-aware backend command',
    );
    assert(
      ORCHESTRATOR_REMOTE_COMMANDS.queueTaskView === 'queue_orchestrator_task_view',
      'queueTaskView should use the remote-aware backend command',
    );
    assert(
      ORCHESTRATOR_REMOTE_COMMANDS.startTaskView === 'start_orchestrator_task_view',
      'startTaskView should use the explicit remote-aware start backend command',
    );
    assert(
      ORCHESTRATOR_REMOTE_COMMANDS.retryTaskView === 'retry_orchestrator_task_view',
      'retryTaskView should use the remote-aware backend command',
    );
    assert(
      ORCHESTRATOR_REMOTE_COMMANDS.requestReworkTaskView ===
        'request_orchestrator_task_rework_view',
      'requestReworkTaskView should use the explicit remote-aware rework backend command',
    );
    assert(
      ORCHESTRATOR_REMOTE_COMMANDS.deliverReviewedTaskView ===
        'deliver_reviewed_orchestrator_task_view',
      'deliverReviewedTaskView should use the explicit remote-aware delivery backend command',
    );
    assert(
      ORCHESTRATOR_REMOTE_COMMANDS.abortTaskView === 'abort_orchestrator_task_view',
      'abortTaskView should use the remote-aware backend command',
    );
    assert(
      ORCHESTRATOR_REMOTE_COMMANDS.cancelTaskView === 'cancel_orchestrator_task_view',
      'cancelTaskView should use the explicit remote-aware cancel backend command',
    );
    assert(
      ORCHESTRATOR_REMOTE_COMMANDS.refreshProject === 'refresh_orchestrator_project',
      'refreshProject should use the explicit project refresh backend command',
    );
    assert(
      ORCHESTRATOR_REMOTE_COMMANDS.listEvidenceForProject ===
        'list_orchestrator_task_evidence_for_project',
      'listEvidence should use the project-scoped evidence backend command',
    );
    assert(
      ORCHESTRATOR_REMOTE_COMMANDS.moveTaskWorkflowState ===
        'move_orchestrator_task_workflow_state',
      'moveTaskWorkflowState should use the split-state backend command',
    );
    assert(
      ORCHESTRATOR_REMOTE_COMMANDS.getRuntimeSnapshot === 'get_orchestrator_runtime_snapshot',
      'getRuntimeSnapshot should use the runtime snapshot backend command',
    );
    assert(
      !('getProjectConfig' in ORCHESTRATOR_REMOTE_COMMANDS),
      'orchestrator task API should not expose project-scoped automation config',
    );
    assert(
      !('getProjectConfig' in orchestratorApi),
      'orchestrator task API public surface should not expose project-scoped automation config',
    );

    const orchestratorApiSource = readFileSync(new URL('./orchestrator.ts', import.meta.url), 'utf8');
    const typesSource = readFileSync(new URL('../lib/types.ts', import.meta.url), 'utf8');

    assert(
      !orchestratorApiSource.includes('buildGetOrchestratorConfigForProjectInvokeArgs'),
      'orchestrator task API should not export a project config invoke helper',
    );
    assert(
      !orchestratorApiSource.includes('get_orchestrator_config_for_project'),
      'orchestrator task API should not call the legacy project config backend command',
    );
    assert(
      orchestratorApiSource.includes('createAction'),
      'CreateOrchestratorTaskRequest should expose createAction for the three create buttons',
    );
    assert(
      typesSource.includes('latestTickAt: string | null;'),
      'runtime snapshot type should include latest scheduler tick',
    );
    assert(
      typesSource.includes('runningTasks: OrchestratorRuntimeTaskSummary[];'),
      'runtime snapshot type should include running task summaries',
    );
    assert(
      typesSource.includes('retryingTasks: OrchestratorRuntimeTaskSummary[];'),
      'runtime snapshot type should include retrying task summaries',
    );
    assert(
      typesSource.includes('recentEvents: OrchestratorRuntimeEvent[];'),
      'runtime snapshot type should include recent scheduler/runner events',
    );
    assert(
      typesSource.includes("remoteStatus: 'local' | 'unsupported' | 'unavailable' | 'offline';"),
      'runtime snapshot type should include explicit remote snapshot status',
    );

    assert(
      'moveTaskWorkflowState' in orchestratorApi,
      'orchestrator task API should expose moveTaskWorkflowState',
    );
    assert('startTaskView' in orchestratorApi, 'orchestrator task API should expose startTaskView');
    assert(
      'requestReworkTaskView' in orchestratorApi,
      'orchestrator task API should expose requestReworkTaskView',
    );
    assert(
      'deliverReviewedTaskView' in orchestratorApi,
      'orchestrator task API should expose deliverReviewedTaskView',
    );
    assert('cancelTaskView' in orchestratorApi, 'orchestrator task API should expose cancelTaskView');
    assert('refreshProject' in orchestratorApi, 'orchestrator task API should expose refreshProject');
    assert(
      'getRuntimeSnapshot' in orchestratorApi,
      'orchestrator task API should expose getRuntimeSnapshot',
    );
  });

  test('builds trimmed invoke args for list/create/actions/evidence/runtime', () => {
    const listArgs = buildListOrchestratorTaskViewsInvokeArgs(' project-1 ');

    assert(
      JSON.stringify(listArgs) === JSON.stringify({ projectId: 'project-1' }),
      'listTasks should trim projectId before invoking backend',
    );

    const listAllArgs = buildListOrchestratorTaskViewsInvokeArgs('   ');

    assert(
      JSON.stringify(listAllArgs) === JSON.stringify({ projectId: null }),
      'listTasks should send null for blank projectId',
    );

    const request = {
      projectId: 'project-1',
      title: '实现 API',
      goal: '暴露任务命令',
      acceptanceCriteria: '测试通过',
      priority: 3,
      createAction: 'todo' as const,
      source: 'linear',
      externalId: 'lin-123',
      externalIdentifier: 'APP-123',
      externalUrl: 'https://linear.app/team/issue/APP-123',
      externalState: 'In Progress',
      externalLabels: ['frontend', 'p1'],
    };
    const createArgs = buildCreateOrchestratorTaskViewInvokeArgs(request);

    assert(
      JSON.stringify(createArgs) === JSON.stringify({ request }),
      'createTask should wrap request with createAction without renaming fields',
    );
    assert(
      (createArgs.request as typeof request).createAction === 'todo',
      'createTask should include createAction in the invoke request',
    );

    const queueArgs = buildOrchestratorTaskViewActionInvokeArgs(' project-1 ', ' task-1 ');

    assert(
      JSON.stringify(queueArgs) === JSON.stringify({ projectId: 'project-1', taskId: 'task-1' }),
      'task view actions should trim projectId and taskId before invoking backend',
    );

    const reworkArgs = buildOrchestratorTaskReworkInvokeArgs(
      ' project-1 ',
      ' task-1 ',
      '  需要补充验证证据  ',
    );

    assert(
      JSON.stringify(reworkArgs) ===
        JSON.stringify({
          projectId: 'project-1',
          taskId: 'task-1',
          reason: '需要补充验证证据',
        }),
      'requestRework should trim projectId, taskId and reason before invoking backend',
    );

    const evidenceArgs = buildListOrchestratorTaskEvidenceForProjectInvokeArgs(
      ' project-1 ',
      ' task-1 ',
    );

    assert(
      JSON.stringify(evidenceArgs) === JSON.stringify({ projectId: 'project-1', taskId: 'task-1' }),
      'listEvidence should include projectId and taskId before invoking backend',
    );

    const moveArgs = buildMoveOrchestratorTaskWorkflowStateInvokeArgs(
      ' project-1 ',
      ' task-1 ',
      'humanReview',
    );

    assert(
      JSON.stringify(moveArgs) ===
        JSON.stringify({
          request: {
            projectId: 'project-1',
            taskId: 'task-1',
            targetState: 'humanReview',
          },
        }),
      'moveTaskWorkflowState should wrap trimmed ids and target state in request',
    );

    const runtimeSnapshotArgs = buildOrchestratorRuntimeSnapshotInvokeArgs(' project-1 ');

    assert(
      JSON.stringify(runtimeSnapshotArgs) === JSON.stringify({ projectId: 'project-1' }),
      'getRuntimeSnapshot should trim projectId before invoking backend',
    );
  });
});
