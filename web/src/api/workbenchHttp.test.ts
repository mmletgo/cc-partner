import { httpOrchestratorTransport, httpWorkbenchTransport } from './workbenchHttp';

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench HTTP adapter 测试需要验证移动端请求体契约，避免可选字段被遗漏后让后端收到不稳定 payload。
 *
 * Code Logic（这个函数做什么）:
 *   condition 为 false 时抛错，让 tsx 测试进程以非零状态退出。
 */
function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

const capturedBodies: unknown[] = [];
const capturedUrls: string[] = [];
const originalFetch = globalThis.fetch;

globalThis.fetch = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
  capturedUrls.push(String(input));
  capturedBodies.push(JSON.parse(String(init?.body ?? '{}')) as unknown);
  return {
    ok: true,
    json: async () => ({ ok: true, sessionId: 'session-1' }),
  } as Response;
};

try {
  await httpWorkbenchTransport.prompt.streamToTerminal('优化并填入终端', {
    targetLanguage: 'zh',
    sessionId: 'session-1',
  });

  assert(
    JSON.stringify(capturedBodies[0]) ===
      JSON.stringify({
        prompt: '优化并填入终端',
        workingDirectory: null,
        targetLanguage: 'zh',
        sessionId: 'session-1',
      }),
    'streamToTerminal should normalize omitted workingDirectory to null',
  );

  await httpWorkbenchTransport.sessions.switchPane('session-1');

  assert(
    capturedUrls[1] === '/api/workbench/sessions/switch-pane',
    'switchPane should call the switch-pane HTTP route',
  );
  assert(
    JSON.stringify(capturedBodies[1]) === JSON.stringify({ sessionId: 'session-1' }),
    'switchPane should send sessionId only',
  );

  await httpWorkbenchTransport.sessions.zoomPane('session-1');

  assert(
    capturedUrls[2] === '/api/workbench/sessions/zoom-pane',
    'zoomPane should call the zoom-pane HTTP route',
  );
  assert(
    JSON.stringify(capturedBodies[2]) === JSON.stringify({ sessionId: 'session-1' }),
    'zoomPane should send sessionId only',
  );

  await httpOrchestratorTransport.tasks.list('project-1');

  assert(
    capturedUrls[3] === '/api/orchestrator/tasks/list',
    'orchestrator task list should call the project task list route',
  );
  assert(
    JSON.stringify(capturedBodies[3]) === JSON.stringify({ projectId: 'project-1' }),
    'orchestrator task list should send projectId only',
  );

  await httpOrchestratorTransport.tasks.create({
    projectId: 'project-1',
    title: '移动端创建任务',
    goal: '从手机端提交项目级自动化任务',
    acceptanceCriteria: '任务进入队列',
    priority: 3,
  });

  const createBody = capturedBodies[4] as Record<string, unknown>;
  assert(
    capturedUrls[4] === '/api/orchestrator/tasks/create',
    'orchestrator task create should call the create route',
  );
  assert(createBody.projectId === 'project-1', 'create should include projectId');
  assert(createBody.title === '移动端创建任务', 'create should include title');
  assert(createBody.goal === '从手机端提交项目级自动化任务', 'create should include goal');
  assert(createBody.acceptanceCriteria === '任务进入队列', 'create should include acceptanceCriteria');
  assert(createBody.priority === 3, 'create should include priority');
  assert(createBody.queue === true, 'create should default queue to true');
  assert(
    typeof createBody.clientRequestId === 'string' && createBody.clientRequestId.length > 0,
    'create should include a non-empty clientRequestId',
  );
} finally {
  globalThis.fetch = originalFetch;
}

console.log('workbenchHttp.test.ts passed');
