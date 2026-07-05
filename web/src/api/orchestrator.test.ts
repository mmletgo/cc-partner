import { readFileSync } from 'node:fs';
import {
  ORCHESTRATOR_REMOTE_COMMANDS,
  buildCreateOrchestratorTaskViewInvokeArgs,
  buildListOrchestratorTaskViewsInvokeArgs,
  buildListOrchestratorTaskEvidenceForProjectInvokeArgs,
  buildOrchestratorTaskViewActionInvokeArgs,
  orchestratorApi,
} from './orchestrator';

/**
 * Business Logic（为什么需要这个函数）:
 *   Orchestrator API helper 测试需要直接失败并暴露参数契约差异，避免组件层传错命令参数。
 *
 * Code Logic（这个函数做什么）:
 *   condition 为 false 时抛出 Error，让 tsx 测试进程以非零状态退出。
 */
function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

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
  ORCHESTRATOR_REMOTE_COMMANDS.retryTaskView === 'retry_orchestrator_task_view',
  'retryTaskView should use the remote-aware backend command',
);
assert(
  ORCHESTRATOR_REMOTE_COMMANDS.abortTaskView === 'abort_orchestrator_task_view',
  'abortTaskView should use the remote-aware backend command',
);
assert(
  ORCHESTRATOR_REMOTE_COMMANDS.listEvidenceForProject ===
    'list_orchestrator_task_evidence_for_project',
  'listEvidence should use the project-scoped evidence backend command',
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

assert(
  !orchestratorApiSource.includes('buildGetOrchestratorConfigForProjectInvokeArgs'),
  'orchestrator task API should not export a project config invoke helper',
);
assert(
  !orchestratorApiSource.includes('get_orchestrator_config_for_project'),
  'orchestrator task API should not call the legacy project config backend command',
);

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
};
const createArgs = buildCreateOrchestratorTaskViewInvokeArgs(request);

assert(
  JSON.stringify(createArgs) === JSON.stringify({ request }),
  'createTask should wrap request without renaming fields',
);

const queueArgs = buildOrchestratorTaskViewActionInvokeArgs(' project-1 ', ' task-1 ');

assert(
  JSON.stringify(queueArgs) === JSON.stringify({ projectId: 'project-1', taskId: 'task-1' }),
  'task view actions should trim projectId and taskId before invoking backend',
);

const evidenceArgs = buildListOrchestratorTaskEvidenceForProjectInvokeArgs(
  ' project-1 ',
  ' task-1 ',
);

assert(
  JSON.stringify(evidenceArgs) === JSON.stringify({ projectId: 'project-1', taskId: 'task-1' }),
  'listEvidence should include projectId and taskId before invoking backend',
);

console.log('orchestrator.test.ts passed');
