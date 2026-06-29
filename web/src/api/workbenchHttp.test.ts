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
const originalFetch = globalThis.fetch;

globalThis.fetch = async (_input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
  capturedBodies.push(JSON.parse(String(init?.body ?? '{}')) as unknown);
  return new Response(JSON.stringify({ ok: true, sessionId: 'session-1' }), {
    status: 200,
    headers: { 'Content-Type': 'application/json' },
  });
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
} finally {
  globalThis.fetch = originalFetch;
}

console.log('workbenchHttp.test.ts passed');
