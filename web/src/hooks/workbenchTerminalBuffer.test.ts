import {
  appendWorkbenchTerminalOutput,
  createWorkbenchTerminalBufferStore,
  removeWorkbenchTerminalBuffer,
  resetWorkbenchTerminalBuffer,
} from './workbenchTerminalBuffer';

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 页面切出后仍要由常驻 Provider 保留终端输出，切回页面时 xterm 能 replay 完整屏幕态。
 *
 * Code Logic（这个函数做什么）:
 *   condition 为 false 时抛错，让 tsx 测试进程以非零状态退出。
 */
function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

let buffers = appendWorkbenchTerminalOutput({}, 'session-a', 'abc', 10);
buffers = appendWorkbenchTerminalOutput(buffers, 'session-a', 'defghijk', 10);

assert(buffers['session-a'] === 'bcdefghijk', 'buffer should keep latest max chars');

const resetBuffers = resetWorkbenchTerminalBuffer(buffers, 'session-a');
assert(resetBuffers['session-a'] === '', 'reset should keep session with empty buffer');

const removedBuffers = removeWorkbenchTerminalBuffer(resetBuffers, 'session-a');
assert(!('session-a' in removedBuffers), 'remove should delete session buffer');

const store = createWorkbenchTerminalBufferStore({}, 20);
let sessionANotifications = 0;
let sessionBNotifications = 0;

const unsubscribeA = store.subscribe('session-a', () => {
  sessionANotifications += 1;
});
const unsubscribeB = store.subscribe('session-b', () => {
  sessionBNotifications += 1;
});

store.append('session-a', 'hello');

assert(store.getBuffer('session-a') === 'hello', 'store should cache appended session output');
assert(store.getRevision('session-a') === 1, 'store should bump changed session revision');
assert(store.getRevision('session-b') === 0, 'store should not bump unrelated session revision');
assert(sessionANotifications === 1, 'store should notify changed session subscribers');
assert(sessionBNotifications === 0, 'store should not notify unrelated session subscribers');

unsubscribeA();
unsubscribeB();

console.log('workbenchTerminalBuffer.test.ts passed');
