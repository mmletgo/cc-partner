import { httpWorkbenchTransport } from './workbenchHttp';

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
} finally {
  globalThis.fetch = originalFetch;
}

console.log('workbenchHttp.test.ts passed');
