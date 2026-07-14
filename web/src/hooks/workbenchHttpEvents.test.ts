import { afterEach, describe, expect, test, vi } from 'vitest';
import {
  parseWorkbenchNdjsonChunk,
  parseWorkbenchNdjsonFrames,
  WORKBENCH_HTTP_EVENT_WATCHDOG_MS,
  type WorkbenchNdjsonParserState,
} from './useWorkbenchHttpEvents';

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端 Workbench 事件流测试需要在断言失败时让用例失败，方便 TDD 和 CI 捕获回归。
 *
 * Code Logic（这个函数做什么）:
 *   接收布尔条件和错误消息；条件为 false 时抛出 Error，条件为 true 时帮助 TypeScript 缩窄类型。
 */
function assert(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   parser 测试需要断言 pending 和输出 chunk 的字符串值，同时不希望 TypeScript 把可变 state 缩窄成字面量。
 *
 * Code Logic（这个函数做什么）:
 *   比较 actual/expected；不相等时抛出包含上下文的 Error。
 */
function assertStringEqual(actual: string, expected: string, message: string): void {
  if (actual !== expected) throw new Error(`${message}: expected ${expected}, got ${actual}`);
}

describe('workbenchHttpEvents', () => {
  /**
   * Business Logic（为什么需要这个测试）:
   *   手机浏览器通过 NDJSON 长连接接收 Workbench terminal/status 事件，网络 chunk 可能切在任意 JSON 行中间。
   *
   * Code Logic（这个测试做什么）:
   *   构造一个 parser pending state，先喂入不完整中文输出行，再补齐 terminalOutput 和 terminalStatus 两行，
   *   断言 parser 只解析完整换行结尾事件并正确维护 pending。
   */
  test('parses NDJSON across chunk boundaries', () => {
    const state: WorkbenchNdjsonParserState = { pending: '' };
    const firstChunk = '{"type":"terminalOutput","payload":{"sessionId":"session-a","chunk":"你好';

    const firstEvents = parseWorkbenchNdjsonChunk(state, firstChunk);

    assert(firstEvents.length === 0, 'incomplete JSON line should not emit events');
    assertStringEqual(state.pending, firstChunk, 'incomplete JSON line should remain pending');

    const secondChunk =
      '，移动端","seq":7,"ts":100}}\n' +
      '{"type":"terminalStatus","payload":{"sessionId":"session-a","status":"running","exitCode":null,"ts":101}}\n';

    const secondEvents = parseWorkbenchNdjsonChunk(state, secondChunk);

    assert(secondEvents.length === 2, 'completed chunk should emit two events');
    assertStringEqual(state.pending, '', 'complete newline-delimited chunks should clear pending');
    const outputEvent = secondEvents[0];
    assert(outputEvent?.type === 'terminalOutput', 'first event should be terminalOutput');
    assertStringEqual(
      outputEvent.payload.chunk,
      '你好，移动端',
      'terminalOutput chunk should preserve split Chinese text',
    );
    assert(secondEvents[1]?.type === 'terminalStatus', 'second event should be terminalStatus');
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   typed heartbeat 必须在业务解码前识别，否则 client watchdog 永不重置或抛不支持事件。
   *
   * Code Logic（这个测试做什么）:
   *   解析 heartbeat 与 terminalOutput 混合帧，断言 frames 含 heartbeat 且业务 chunk 仍过滤掉 heartbeat。
   */
  test('parses typed heartbeat frames before business events', () => {
    const state: WorkbenchNdjsonParserState = { pending: '' };
    const chunk =
      '{"type":"heartbeat","sentAt":"2026-07-15T00:00:00Z"}\n' +
      '{"type":"terminalOutput","payload":{"sessionId":"s1","chunk":"x","seq":1,"ts":1}}\n';

    const frames = parseWorkbenchNdjsonFrames(state, chunk);
    expect(frames).toHaveLength(2);
    expect(frames[0]).toEqual({
      kind: 'heartbeat',
      sentAt: '2026-07-15T00:00:00Z',
    });
    expect(frames[1]?.kind).toBe('event');

    const state2: WorkbenchNdjsonParserState = { pending: '' };
    const eventsOnly = parseWorkbenchNdjsonChunk(state2, chunk);
    expect(eventsOnly).toHaveLength(1);
    expect(eventsOnly[0]?.type).toBe('terminalOutput');
  });
});

describe('workbenchHttpEvents watchdog contract', () => {
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   半开连接 35s 无帧必须只 abort 当前连接并在 lifecycle 仍活跃时重连；
   *   heartbeat 与业务帧都要重置 watchdog。
   *
   * Code Logic（这个测试做什么）:
   *   用可控 ReadableStream + fake timers 模拟 hook 内 connect 语义：
   *   先发 heartbeat，推进 34s 不 abort；再静默 35s 触发 child abort 并二次 fetch。
   */
  test('watchdog aborts child after silence and reconnects while lifecycle active', async () => {
    vi.useFakeTimers();

    type StreamController = ReadableStreamDefaultController<Uint8Array>;
    const controllers: StreamController[] = [];
    let fetchCount = 0;

    const mockFetch = vi.fn((_input: RequestInfo | URL, init?: RequestInit) => {
      fetchCount += 1;
      const stream = new ReadableStream<Uint8Array>({
        start(controller) {
          controllers.push(controller);
          init?.signal?.addEventListener('abort', () => {
            try {
              controller.error(new DOMException('The operation was aborted.', 'AbortError'));
            } catch {
              // already closed
            }
          });
        },
      });
      return Promise.resolve({
        ok: true,
        body: stream,
      } as Response);
    });
    vi.stubGlobal('fetch', mockFetch);

    // 内联复刻 hook 的 lifecycle/child/watchdog 合同，避免把 React hook 绑进 node 环境。
    const lifecycle = new AbortController();
    const reconnectDelayMs = 10;
    const watchdogMs = WORKBENCH_HTTP_EVENT_WATCHDOG_MS;
    let stopped = false;
    let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
    const encoder = new TextEncoder();

    /**
     * Business Logic（为什么需要这个函数）:
     *   测试需要与生产 hook 相同的重连调度语义。
     *
     * Code Logic（这个函数做什么）:
     *   lifecycle 未结束时延迟调用 connect。
     */
    function scheduleReconnect(): void {
      if (stopped || lifecycle.signal.aborted) return;
      reconnectTimer = setTimeout(() => {
        void connect();
      }, reconnectDelayMs);
    }

    /**
     * Business Logic（为什么需要这个函数）:
     *   测试连接体必须与 production：child abort + watchdog 重置一致。
     *
     * Code Logic（这个函数做什么）:
     *   fetch events，读 frames，heartbeat/业务都 resetWatchdog；结束 scheduleReconnect。
     */
    async function connect(): Promise<void> {
      if (stopped || lifecycle.signal.aborted) return;
      const connection = new AbortController();
      const onLife = (): void => connection.abort();
      lifecycle.signal.addEventListener('abort', onLife);
      let watchdog: ReturnType<typeof setTimeout> | null = null;
      /**
       * Business Logic（为什么需要这个函数）:
       *   任何完整 frame 都证明连接存活。
       *
       * Code Logic（这个函数做什么）:
       *   重置 watchdog；触发时仅 abort child。
       */
      const resetWatchdog = (): void => {
        if (watchdog) clearTimeout(watchdog);
        watchdog = setTimeout(() => connection.abort(), watchdogMs);
      };
      resetWatchdog();
      try {
        const response = await fetch('/api/workbench/events', {
          method: 'GET',
          signal: connection.signal,
        });
        if (!response.body) throw new Error('no body');
        const reader = response.body.getReader();
        const decoder = new TextDecoder();
        const state: WorkbenchNdjsonParserState = { pending: '' };
        while (!stopped && !lifecycle.signal.aborted) {
          const { done, value } = await reader.read();
          if (done) break;
          if (!value) continue;
          const chunk = decoder.decode(value, { stream: true });
          parseWorkbenchNdjsonFrames(state, chunk).forEach(() => {
            resetWatchdog();
          });
        }
      } catch {
        if (lifecycle.signal.aborted) return;
      } finally {
        if (watchdog) clearTimeout(watchdog);
        lifecycle.signal.removeEventListener('abort', onLife);
        if (!lifecycle.signal.aborted && !stopped) scheduleReconnect();
      }
    }

    void connect();
    await vi.advanceTimersByTimeAsync(0);
    expect(fetchCount).toBe(1);

    // heartbeat 重置 watchdog
    controllers[0]?.enqueue(
      encoder.encode('{"type":"heartbeat","sentAt":"2026-07-15T00:00:00Z"}\n'),
    );
    await vi.advanceTimersByTimeAsync(watchdogMs - 1_000);
    expect(fetchCount).toBe(1);

    // 业务帧也重置
    controllers[0]?.enqueue(
      encoder.encode(
        '{"type":"terminalOutput","payload":{"sessionId":"s","chunk":"a","seq":1,"ts":1}}\n',
      ),
    );
    await vi.advanceTimersByTimeAsync(watchdogMs - 1_000);
    expect(fetchCount).toBe(1);

    // 静默满 watchdog → child abort → reconnect
    await vi.advanceTimersByTimeAsync(watchdogMs + reconnectDelayMs + 5);
    expect(fetchCount).toBe(2);

    stopped = true;
    lifecycle.abort();
    if (reconnectTimer) clearTimeout(reconnectTimer);
  });
});
