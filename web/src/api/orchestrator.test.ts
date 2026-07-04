import {
  buildCreateOrchestratorTaskInvokeArgs,
  buildListOrchestratorTasksInvokeArgs,
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

const listArgs = buildListOrchestratorTasksInvokeArgs(' project-1 ');

assert(
  JSON.stringify(listArgs) === JSON.stringify({ projectId: 'project-1' }),
  'listTasks should trim projectId before invoking backend',
);

const listAllArgs = buildListOrchestratorTasksInvokeArgs('   ');

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
const createArgs = buildCreateOrchestratorTaskInvokeArgs(request);

assert(
  JSON.stringify(createArgs) === JSON.stringify({ request }),
  'createTask should wrap request without renaming fields',
);

console.log('orchestrator.test.ts passed');
