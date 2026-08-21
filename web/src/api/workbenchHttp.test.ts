import { afterEach, describe, expect, test, vi } from 'vitest';
import {
  createHttpOrchestratorClientRequestId,
  getJson,
  HTTP_LONG_MUTATION_TIMEOUT_MS,
  httpOrchestratorTransport,
  httpWorkbenchTransport,
  postJson,
  resolveHttpTimeoutMs,
  workbenchHttp,
} from './workbenchHttp';
import { OrchestratorRuntimeTransportError } from './orchestratorRuntimeTransportError';

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

/** 最小合法 OrchestratorTask，供 createView decoder 通过。 */
const validTaskFixture = {
  id: 't1',
  projectId: 'p1',
  title: 'Title',
  goal: 'Goal',
  acceptanceCriteria: 'AC',
  status: 'queued',
  workflowState: 'todo',
  runState: 'idle',
  attemptPhase: null,
  source: 'internal',
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
  createdAt: '2026-07-13T00:00:00.000Z',
  updatedAt: '2026-07-13T00:00:00.000Z',
  startedAt: null,
  finishedAt: null,
};

/** 最小合法 outbox DTO。 */
const validOutboxFixture = {
  id: 'o1',
  deviceId: 'd1',
  deviceName: 'Peer',
  remoteProjectPath: '/tmp/p',
  remoteProjectId: null,
  requestJson: '{}',
  status: 'failed',
  remoteTaskId: null,
  lastError: 'offline',
  createdAt: '2026-07-13T00:00:00.000Z',
  updatedAt: '2026-07-13T00:00:00.000Z',
  sentAt: null,
};

/** 最小合法 runtime snapshot。 */
const validRuntimeFixture = {
  projectId: 'p1',
  projectKind: 'local',
  remoteStatus: 'local',
  generatedAt: '2026-07-13T00:00:00.000Z',
  latestTickAt: null,
  lastDispatchAt: null,
  lastDispatchedCount: 0,
  schedulerEnabled: true,
  workflowSource: 'builtin',
  workflowValid: true,
  workflowError: null,
  maxConcurrentTasks: 1,
  slotsUsed: 0,
  slotsAvailable: 1,
  latestError: null,
  runningTasks: [],
  retryingTasks: [],
  recentEvents: [],
};

/**
 * Business Logic（为什么需要这个函数）:
 *   decoder-aware HTTP 成功体需要形状合法，否则契约测试无法验证请求 body/route。
 *
 * Code Logic（这个函数做什么）:
 *   按 URL 返回最小合法成功 JSON；未匹配路径回退到 {ok:true}。
 */
function mockSuccessBodyForUrl(url: string): unknown {
  if (url.includes('/task-views/list')) {
    return { views: [] };
  }
  if (url.includes('/task-views/create')) {
    return { origin: 'local', task: validTaskFixture };
  }
  if (url.includes('/outbox/retry') || url.includes('/outbox/discard')) {
    return validOutboxFixture;
  }
  if (url.includes('/runtime-snapshot')) {
    return validRuntimeFixture;
  }
  if (url.includes('/tasks/list')) {
    return { tasks: [] };
  }
  if (url.includes('/tasks/create')) {
    return validTaskFixture;
  }
  if (url.includes('/tasks/evidence')) {
    return { evidence: [] };
  }
  if (url.includes('/tasks/complete-prompt')) {
    return { title: 't', goal: 'g', acceptanceCriteria: 'a' };
  }
  return { ok: true, sessionId: 'session-1' };
}

describe('workbenchHttp', () => {
  test('mobile workbench and orchestrator HTTP adapters send expected routes and bodies', async () => {
    const capturedBodies: unknown[] = [];
    const capturedUrls: string[] = [];
    const originalFetch = globalThis.fetch;

    globalThis.fetch = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
      const url = String(input);
      capturedUrls.push(url);
      capturedBodies.push(JSON.parse(String(init?.body ?? '{}')) as unknown);
      return {
        ok: true,
        json: async () => mockSuccessBodyForUrl(url),
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

      await httpOrchestratorTransport.outbox.retry('remote-project-1', 'outbox-1');
      const retryIdx = capturedUrls.length - 1;
      assert(
        capturedUrls[retryIdx] === '/api/orchestrator/outbox/retry',
        'outbox retry should call mobile local outbox retry route',
      );
      assert(
        JSON.stringify(capturedBodies[retryIdx]) ===
          JSON.stringify({ projectId: 'remote-project-1', outboxId: 'outbox-1' }),
        'outbox retry body should be camelCase projectId/outboxId',
      );

      await httpOrchestratorTransport.outbox.discard('remote-project-1', 'outbox-2');
      const discardIdx = capturedUrls.length - 1;
      assert(
        capturedUrls[discardIdx] === '/api/orchestrator/outbox/discard',
        'outbox discard should call mobile local outbox discard route',
      );
      assert(
        JSON.stringify(capturedBodies[discardIdx]) ===
          JSON.stringify({ projectId: 'remote-project-1', outboxId: 'outbox-2' }),
        'outbox discard body should be camelCase projectId/outboxId',
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

      const createViewIdx = capturedUrls.length - 1;
      const createViewBody = capturedBodies[createViewIdx] as Record<string, unknown>;
      assert(
        capturedUrls[createViewIdx] === '/api/orchestrator/task-views/create',
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
      const backlogIdx = capturedUrls.length - 1;

      await httpOrchestratorTransport.tasks.createView({
        projectId: 'remote-project-1',
        title: '创建到 Todo',
        goal: '等待调度器领取',
        acceptanceCriteria: '任务进入 Todo',
        createAction: 'todo',
      });
      const todoIdx = capturedUrls.length - 1;

      assert(
        (capturedBodies[backlogIdx] as Record<string, unknown>).createAction === 'backlog',
        'createView should pass backlog createAction',
      );
      assert(
        (capturedBodies[todoIdx] as Record<string, unknown>).createAction === 'todo',
        'createView should pass todo createAction',
      );

      await httpOrchestratorTransport.tasks.completePrompt({
        projectId: 'remote-project-1',
        prompt: '移动端自动化任务弹窗',
        workingDirectory: ' /Users/hans/web_project/cc-partner ',
      });
      const completePromptIdx = capturedUrls.length - 1;

      assert(
        capturedUrls[completePromptIdx] === '/api/orchestrator/tasks/complete-prompt',
        'orchestrator task prompt completion should call the complete-prompt route',
      );
      assert(
        JSON.stringify(capturedBodies[completePromptIdx]) ===
          JSON.stringify({
            projectId: 'remote-project-1',
            prompt: '移动端自动化任务弹窗',
            workingDirectory: '/Users/hans/web_project/cc-partner',
          }),
        'orchestrator task prompt completion should normalize prompt and workingDirectory',
      );

      await httpOrchestratorTransport.tasks.listEvidence('remote-project-1', 'remote:device-a:task-1');
      const evidenceIdx = capturedUrls.length - 1;

      assert(
        capturedUrls[evidenceIdx] === '/api/orchestrator/tasks/evidence',
        'orchestrator evidence should call the task evidence route',
      );
      assert(
        JSON.stringify(capturedBodies[evidenceIdx]) ===
          JSON.stringify({ projectId: 'remote-project-1', taskId: 'remote:device-a:task-1' }),
        'orchestrator evidence should include projectId and taskId',
      );

      await httpOrchestratorTransport.getRuntimeSnapshot('remote-project-1');
      const runtimeIdx = capturedUrls.length - 1;

      assert(
        capturedUrls[runtimeIdx] === '/api/mobile/orchestrator/runtime-snapshot',
        'runtime snapshot should call the mobile remote-aware route',
      );
      assert(
        JSON.stringify(capturedBodies[runtimeIdx]) === JSON.stringify({ projectId: 'remote-project-1' }),
        'runtime snapshot should send projectId only',
      );

      await httpWorkbenchTransport.sessions.replay('session-1');
      const replayIdx = capturedUrls.length - 1;
      assert(
        capturedUrls[replayIdx] === '/api/mobile/workbench/sessions/replay',
        'ordinary mobile replay should use the shared sessions replay route',
      );
      assert(
        JSON.stringify(capturedBodies[replayIdx]) ===
          JSON.stringify({ sessionId: 'session-1', refreshHistory: false }),
        'ordinary mobile replay must not capture tmux history',
      );

      await httpWorkbenchTransport.sessions.hydrateScrollback('session-1');
      const hydrationIdx = capturedUrls.length - 1;
      assert(
        capturedUrls[hydrationIdx] === '/api/mobile/workbench/sessions/replay',
        'mobile scrollback hydration should reuse the sessions replay route',
      );
      assert(
        JSON.stringify(capturedBodies[hydrationIdx]) ===
          JSON.stringify({ sessionId: 'session-1', refreshHistory: true }),
        'mobile scrollback hydration must explicitly capture owner tmux history',
      );
    } finally {
      globalThis.fetch = originalFetch;
    }
  });
});


describe('workbenchHttp createView idempotency', () => {
  /**
   * Business Logic（为什么需要这个测试）:
   *   响应丢失后用户重试必须复用同一 clientRequestId，否则 owner 会创建重复任务。
   *
   * Code Logic（这个测试做什么）:
   *   调用方固定一个 id，连续两次 createView；断言两笔 body.clientRequestId 完全相同。
   */
  test('createView reuses caller-provided clientRequestId across retries', async () => {
    const capturedBodies: unknown[] = [];
    const originalFetch = globalThis.fetch;
    globalThis.fetch = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
      const url = String(input);
      capturedBodies.push(JSON.parse(String(init?.body ?? '{}')) as unknown);
      return {
        ok: true,
        json: async () => mockSuccessBodyForUrl(url),
      } as Response;
    };

    try {
      const clientRequestId = createHttpOrchestratorClientRequestId();
      const request = {
        projectId: 'project-retry',
        title: '重试幂等',
        goal: '同一逻辑提交',
        acceptanceCriteria: '仅一条任务',
        createAction: 'todo' as const,
        clientRequestId,
      };

      await httpOrchestratorTransport.tasks.createView(request);
      await httpOrchestratorTransport.tasks.createView(request);

      assert(capturedBodies.length === 2, 'should capture two createView bodies');
      const first = capturedBodies[0] as Record<string, unknown>;
      const second = capturedBodies[1] as Record<string, unknown>;
      assert(
        first.clientRequestId === clientRequestId,
        'first createView should use caller clientRequestId',
      );
      assert(
        second.clientRequestId === clientRequestId,
        'retry createView should reuse the same clientRequestId',
      );
      assert(
        first.clientRequestId === second.clientRequestId,
        'clientRequestId must be stable across retries',
      );
    } finally {
      globalThis.fetch = originalFetch;
    }
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   未传 clientRequestId 时 transport 仍应自动 mint，保证后端契约不破。
   *
   * Code Logic（这个测试做什么）:
   *   两次不带 id 的 createView，断言各自非空且两次不同（新逻辑提交默认新键）。
   */
  test('createView mints non-empty clientRequestId when omitted', async () => {
    const capturedBodies: unknown[] = [];
    const originalFetch = globalThis.fetch;
    globalThis.fetch = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
      const url = String(input);
      capturedBodies.push(JSON.parse(String(init?.body ?? '{}')) as unknown);
      return {
        ok: true,
        json: async () => mockSuccessBodyForUrl(url),
      } as Response;
    };

    try {
      await httpOrchestratorTransport.tasks.createView({
        projectId: 'p',
        title: 'a',
        goal: 'b',
        acceptanceCriteria: 'c',
      });
      await httpOrchestratorTransport.tasks.createView({
        projectId: 'p',
        title: 'a',
        goal: 'b',
        acceptanceCriteria: 'c',
      });
      const first = (capturedBodies[0] as Record<string, unknown>).clientRequestId;
      const second = (capturedBodies[1] as Record<string, unknown>).clientRequestId;
      assert(typeof first === 'string' && first.length > 0, 'first id non-empty');
      assert(typeof second === 'string' && second.length > 0, 'second id non-empty');
      assert(first !== second, 'omitted ids should be freshly minted per call');
    } finally {
      globalThis.fetch = originalFetch;
    }
  });
});

describe('workbenchHttp request policy transport', () => {
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   merge 只要对端 Claude 仍在输出就不能被 mobile overall 180s 墙钟掐断。
   *
   * Code Logic（这个测试做什么）:
   *   untilComplete 无 overall timeout；默认 merge policy 在 longMutation 预算后仍不 settle。
   */
  test('merge waits until peer returns without overall timeout', async () => {
    expect(resolveHttpTimeoutMs({ kind: 'untilComplete' })).toBeNull();
    expect(resolveHttpTimeoutMs({ kind: 'longMutation' })).toBe(HTTP_LONG_MUTATION_TIMEOUT_MS);

    vi.useFakeTimers();
    const mockFetch = vi.fn(() => new Promise<Response>(() => undefined));
    vi.stubGlobal('fetch', mockFetch);
    let settled = false;
    void workbenchHttp.git
      .merge({
        worktreeId: 'wt-1',
        clientOperationId: 'op-merge',
      })
      .then(
        () => {
          settled = true;
        },
        () => {
          settled = true;
        },
      );
    await vi.advanceTimersByTimeAsync(HTTP_LONG_MUTATION_TIMEOUT_MS);
    expect(settled).toBe(false);
    expect(mockFetch).toHaveBeenCalledTimes(1);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   mutation 超时只能 unknown 且禁止自动重放，否则会重复 commit/push。
   *
   * Code Logic（这个测试做什么）:
   *   hang 的 fetch + 短 timeoutMs；断言 unknown envelope 且 fetch 只调用 1 次。
   */
  test('mutation wrapper maps timeout to typed unknown and does not replay', async () => {
    const mockFetch = vi.fn(() => new Promise<Response>(() => undefined));
    vi.stubGlobal('fetch', mockFetch);

    await expect(
      workbenchHttp.git.commit({
        worktreeId: 'wt-1',
        message: null,
        clientOperationId: 'op-1',
        policy: { kind: 'mutation', timeoutMs: 40 },
      }),
    ).resolves.toMatchObject({
      kind: 'unknown',
      clientOperationId: 'op-1',
      transportClass: 'timeout',
    });
    expect(mockFetch).toHaveBeenCalledTimes(1);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   query overall 预算必须覆盖 body decode，否则慢 JSON 会永久挂起。
   *
   * Code Logic（这个测试做什么）:
   *   fetch 立即返回 ok，但 json() 永不 resolve；短 timeout 断言 timeout kind。
   */
  test('query overall timeout covers body decode', async () => {
    const mockFetch = vi.fn(async () => {
      return {
        ok: true,
        json: () => new Promise(() => undefined),
      } as Response;
    });
    vi.stubGlobal('fetch', mockFetch);

    await expect(
      getJson('/api/health', { policy: { kind: 'query', timeoutMs: 40 } }),
    ).rejects.toMatchObject({
      kind: 'timeout',
    });
    // query 对 timeout 可有限重试（最多 1+2），但每次 attempt 的 overall 预算都覆盖 decode。
    expect(mockFetch.mock.calls.length).toBeGreaterThanOrEqual(1);
    expect(mockFetch.mock.calls.length).toBeLessThanOrEqual(3);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   调用方 abort 必须立刻停止且不得进入 query 重试。
   *
   * Code Logic（这个测试做什么）:
   *   外部 AbortController abort 后 hang fetch；断言 callerAbort 且只 1 次 fetch。
   */
  test('caller abort does not retry query', async () => {
    const controller = new AbortController();
    const mockFetch = vi.fn((_input: RequestInfo | URL, init?: RequestInit) => {
      return new Promise<Response>((_resolve, reject) => {
        init?.signal?.addEventListener('abort', () => {
          reject(new DOMException('The operation was aborted.', 'AbortError'));
        });
      });
    });
    vi.stubGlobal('fetch', mockFetch);

    const pending = postJson(
      '/api/orchestrator/tasks/list',
      { projectId: 'p1' },
      { policy: { kind: 'query', signal: controller.signal } },
    );
    controller.abort();
    await expect(pending).rejects.toMatchObject({ kind: 'callerAbort' });
    expect(mockFetch).toHaveBeenCalledTimes(1);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   只读 query 在可见页面可有限重试；mutation 不得自动重试。
   *
   * Code Logic（这个测试做什么）:
   *   network 失败两次后成功；fake timer 推进退避；mutation 一次 network 失败即 unknown。
   */
  test('query retries network failures while visible; mutation does not', async () => {
    vi.useFakeTimers();
    Object.defineProperty(globalThis, 'document', {
      configurable: true,
      value: { visibilityState: 'visible' },
    });

    let queryAttempts = 0;
    const mockFetch = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes('/tasks/list')) {
        queryAttempts += 1;
        if (queryAttempts < 3) {
          throw new TypeError('Failed to fetch');
        }
        return {
          ok: true,
          json: async () => ({ tasks: [] }),
        } as Response;
      }
      // mutation hang path not used here
      throw new TypeError('Failed to fetch');
    });
    vi.stubGlobal('fetch', mockFetch);

    const listPromise = postJson(
      '/api/orchestrator/tasks/list',
      { projectId: 'p1' },
      { policy: { kind: 'query' } },
    );
    await vi.runAllTimersAsync();
    await expect(listPromise).resolves.toEqual({ tasks: [] });
    expect(queryAttempts).toBe(3);

    const mut = workbenchHttp.git.push({
      worktreeId: 'wt-1',
      clientOperationId: 'op-push',
    });
    await expect(mut).resolves.toMatchObject({
      kind: 'unknown',
      clientOperationId: 'op-push',
      transportClass: 'network',
    });
    // list 3 + push 1
    expect(mockFetch.mock.calls.filter((c) => String(c[0]).includes('/push')).length).toBe(1);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   protocol/decode 是确定性失败，不得伪装 unknown envelope。
   *
   * Code Logic（这个测试做什么）:
   *   非 2xx 与非法 envelope 分别抛 protocol/decode。
   */
  test('protocol and decode errors remain errors for mutations', async () => {
    const mockFetch = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes('/commit')) {
        return {
          ok: false,
          status: 409,
          statusText: 'Conflict',
          text: async () => JSON.stringify({ error: '冲突' }),
        } as Response;
      }
      return {
        ok: true,
        json: async () => ({ kind: 'nope' }),
      } as Response;
    });
    vi.stubGlobal('fetch', mockFetch);

    await expect(
      workbenchHttp.git.commit({
        worktreeId: 'wt',
        clientOperationId: 'op-c',
      }),
    ).rejects.toBeInstanceOf(OrchestratorRuntimeTransportError);

    await expect(
      workbenchHttp.git.remove({
        worktreeId: 'wt',
        clientOperationId: 'op-r',
      }),
    ).rejects.toMatchObject({ kind: 'decode' });
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   移动端 merge 关闭会话后 resize 会打到 404；必须透传信封 code，UI 才能按 not_found 吞掉，而不是解析中文。
   *
   * Code Logic（这个测试做什么）:
   *   mock 404 `{ error, code: not_found }`，断言 sessions.resize reject 带 kind=protocol 与 code=not_found。
   */
  test('protocol errors attach stable code from the JSON envelope', async () => {
    const mockFetch = vi.fn(async () => ({
      ok: false,
      status: 404,
      statusText: 'Not Found',
      text: async () => JSON.stringify({ error: '工作台会话不存在', code: 'not_found' }),
    } as Response));
    vi.stubGlobal('fetch', mockFetch);

    await expect(
      httpWorkbenchTransport.sessions.resize('gone-session', 80, 24),
    ).rejects.toMatchObject({
      kind: 'protocol',
      code: 'not_found',
      message: '工作台会话不存在',
    });
  });
});
