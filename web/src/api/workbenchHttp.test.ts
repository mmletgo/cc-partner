import { describe, test } from 'vitest';
import { httpOrchestratorTransport, httpWorkbenchTransport } from './workbenchHttp';

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench HTTP adapter 测试需要验证移动端请求体契约，避免可选字段被遗漏后让后端收到不稳定 payload。
 *
 * Code Logic（这个函数做什么）:
 *   condition 为 false 时抛错，让测试用例失败。
 */
function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

describe('workbenchHttp', () => {
  test('mobile workbench and orchestrator HTTP adapters send expected routes and bodies', async () => {
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
      assert(
        capturedUrls[0] === '/api/mobile/workbench/prompt-optimizer/stream-to-session',
        'streamToTerminal should call the mobile-facing prompt optimizer route',
      );

      await httpWorkbenchTransport.sessions.switchPane('session-1');

      assert(
        capturedUrls[1] === '/api/mobile/workbench/sessions/switch-pane',
        'switchPane should call the mobile-facing switch-pane HTTP route',
      );
      assert(
        JSON.stringify(capturedBodies[1]) === JSON.stringify({ sessionId: 'session-1' }),
        'switchPane should send sessionId only',
      );

      await httpWorkbenchTransport.sessions.zoomPane('session-1');

      assert(
        capturedUrls[2] === '/api/mobile/workbench/sessions/zoom-pane',
        'zoomPane should call the mobile-facing zoom-pane HTTP route',
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
        createAction: 'todo',
        source: 'linear',
        externalId: 'lin-123',
        externalIdentifier: 'APP-123',
        externalUrl: 'https://linear.app/team/issue/APP-123',
        externalState: 'In Progress',
        externalLabels: ['mobile', 'p1'],
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
      assert(createBody.createAction === 'todo', 'create should forward explicit createAction');
      assert(createBody.source === 'linear', 'create should include tracker source');
      assert(createBody.externalId === 'lin-123', 'create should include externalId');
      assert(createBody.externalIdentifier === 'APP-123', 'create should include externalIdentifier');
      assert(
        createBody.externalUrl === 'https://linear.app/team/issue/APP-123',
        'create should include externalUrl',
      );
      assert(createBody.externalState === 'In Progress', 'create should include externalState');
      assert(
        JSON.stringify(createBody.externalLabels) === JSON.stringify(['mobile', 'p1']),
        'create should include externalLabels',
      );
      assert(!('queue' in createBody), 'create should not send legacy queue flag');
      assert(
        typeof createBody.clientRequestId === 'string' && createBody.clientRequestId.length > 0,
        'create should include a non-empty clientRequestId',
      );

      await httpOrchestratorTransport.tasks.listViews('remote-project-1');

      assert(
        capturedUrls[5] === '/api/orchestrator/task-views/list',
        'orchestrator task view list should call the mobile task-views list route',
      );
      assert(
        JSON.stringify(capturedBodies[5]) === JSON.stringify({ projectId: 'remote-project-1' }),
        'orchestrator task view list should send projectId only',
      );

      await httpOrchestratorTransport.tasks.createView({
        projectId: 'remote-project-1',
        title: '移动端远端创建任务',
        goal: '从手机端代理到远端设备创建项目级自动化任务',
        acceptanceCriteria: '远端设备返回 task view',
        priority: 2,
        createAction: 'start',
        source: 'github',
        externalId: 'gh-456',
        externalIdentifier: 'GH-456',
        externalUrl: 'https://github.com/org/repo/issues/456',
        externalState: 'triaged',
        externalLabels: ['remote', 'bug'],
      });

      const createViewBody = capturedBodies[6] as Record<string, unknown>;
      assert(
        capturedUrls[6] === '/api/orchestrator/task-views/create',
        'orchestrator task view create should call the mobile task-views create route',
      );
      assert(createViewBody.projectId === 'remote-project-1', 'createView should include projectId');
      assert(createViewBody.title === '移动端远端创建任务', 'createView should include title');
      assert(createViewBody.goal === '从手机端代理到远端设备创建项目级自动化任务', 'createView should include goal');
      assert(createViewBody.acceptanceCriteria === '远端设备返回 task view', 'createView should include acceptanceCriteria');
      assert(createViewBody.priority === 2, 'createView should include priority');
      assert(createViewBody.createAction === 'start', 'createView should forward explicit createAction');
      assert(createViewBody.source === 'github', 'createView should include tracker source');
      assert(createViewBody.externalId === 'gh-456', 'createView should include externalId');
      assert(createViewBody.externalIdentifier === 'GH-456', 'createView should include externalIdentifier');
      assert(
        createViewBody.externalUrl === 'https://github.com/org/repo/issues/456',
        'createView should include externalUrl',
      );
      assert(createViewBody.externalState === 'triaged', 'createView should include externalState');
      assert(
        JSON.stringify(createViewBody.externalLabels) === JSON.stringify(['remote', 'bug']),
        'createView should include externalLabels',
      );
      assert(!('queue' in createViewBody), 'createView should not send legacy queue flag');
      assert(
        typeof createViewBody.clientRequestId === 'string' && createViewBody.clientRequestId.length > 0,
        'createView should include a non-empty clientRequestId',
      );

      await httpOrchestratorTransport.tasks.createView({
        projectId: 'remote-project-1',
        title: '创建到 Backlog',
        goal: '只保存任务',
        acceptanceCriteria: '任务保持 Backlog',
        createAction: 'backlog',
      });

      await httpOrchestratorTransport.tasks.createView({
        projectId: 'remote-project-1',
        title: '创建到 Todo',
        goal: '等待调度器领取',
        acceptanceCriteria: '任务进入 Todo',
        createAction: 'todo',
      });

      assert(
        (capturedBodies[7] as Record<string, unknown>).createAction === 'backlog',
        'createView should pass backlog createAction',
      );
      assert(
        (capturedBodies[8] as Record<string, unknown>).createAction === 'todo',
        'createView should pass todo createAction',
      );

      await httpOrchestratorTransport.tasks.completePrompt({
        projectId: 'remote-project-1',
        prompt: '移动端自动化任务弹窗',
        workingDirectory: ' /Users/hans/web_project/cc-partner ',
      });

      assert(
        capturedUrls[9] === '/api/orchestrator/tasks/complete-prompt',
        'orchestrator task prompt completion should call the complete-prompt route',
      );
      assert(
        JSON.stringify(capturedBodies[9]) ===
          JSON.stringify({
            projectId: 'remote-project-1',
            prompt: '移动端自动化任务弹窗',
            workingDirectory: '/Users/hans/web_project/cc-partner',
          }),
        'orchestrator task prompt completion should normalize prompt and workingDirectory',
      );

      await httpOrchestratorTransport.tasks.listEvidence('remote-project-1', 'remote:device-a:task-1');

      assert(
        capturedUrls[10] === '/api/orchestrator/tasks/evidence',
        'orchestrator evidence should call the task evidence route',
      );
      assert(
        JSON.stringify(capturedBodies[10]) ===
          JSON.stringify({ projectId: 'remote-project-1', taskId: 'remote:device-a:task-1' }),
        'orchestrator evidence should include projectId and taskId',
      );

      await httpOrchestratorTransport.getRuntimeSnapshot('remote-project-1');

      assert(
        capturedUrls[11] === '/api/mobile/orchestrator/runtime-snapshot',
        'runtime snapshot should call the mobile remote-aware route',
      );
      assert(
        JSON.stringify(capturedBodies[11]) === JSON.stringify({ projectId: 'remote-project-1' }),
        'runtime snapshot should send projectId only',
      );
    } finally {
      globalThis.fetch = originalFetch;
    }
  });
});
