import {
  parseWorkbenchNdjsonChunk,
  type WorkbenchNdjsonParserState,
} from './useWorkbenchHttpEvents';

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端 Workbench 事件流测试需要在断言失败时让脚本以非零状态退出，方便 TDD 和 CI 捕获回归。
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

/**
 * Business Logic（为什么需要这个函数）:
 *   手机浏览器通过 NDJSON 长连接接收 Workbench terminal/status 事件，网络 chunk 可能切在任意 JSON 行中间。
 *
 * Code Logic（这个函数做什么）:
 *   构造一个 parser pending state，先喂入不完整中文输出行，再补齐 terminalOutput 和 terminalStatus 两行，
 *   断言 parser 只解析完整换行结尾事件并正确维护 pending。
 */
function runParserChunkBoundaryTest(): void {
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
}

runParserChunkBoundaryTest();

console.log('workbenchHttpEvents.test.ts passed');
