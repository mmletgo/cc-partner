import {
  shouldForwardTerminalInput,
  writeTerminalReplay,
  type TerminalReplayGate,
} from './terminalReplay';

/**
 * Business Logic（为什么需要这个类）:
 *   测试终端历史输出 replay 时，不依赖真实 xterm 和浏览器 DOM。
 *
 * Code Logic（这个类做什么）:
 *   记录写入内容并延迟保存 callback，测试可手动触发 callback 模拟 xterm 写入完成。
 */
class FakeTerminalWriter {
  writes: string[] = [];
  callback: (() => void) | undefined;

  /**
   * Business Logic（为什么需要这个函数）:
   *   Workbench replay helper 只依赖 terminal.write(data, callback) 这一小段 xterm API。
   *
   * Code Logic（这个函数做什么）:
   *   记录 data 并保存 callback，供测试控制完成时机。
   */
  write(data: string, callback?: () => void): void {
    this.writes.push(data);
    this.callback = callback;
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   terminalReplay 测试使用脚本断言，不引入额外测试框架。
 *
 * Code Logic（这个函数做什么）:
 *   condition 为 false 时抛错，让 tsx 进程以非零状态退出。
 */
function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

const gate: TerminalReplayGate = { current: false };
const terminal = new FakeTerminalWriter();
const scheduledReleases: Array<() => void> = [];

writeTerminalReplay(terminal, '\x1b[c', gate, (release) => {
  scheduledReleases.push(release);
});

assert(gate.current, 'replay should suppress terminal-generated input while pending');
assert(!shouldForwardTerminalInput(gate), 'input must not be forwarded during replay');
assert(JSON.stringify(terminal.writes) === JSON.stringify(['\x1b[c']), 'replay should write buffer');

terminal.callback?.();

assert(
  gate.current,
  'replay should keep suppressing terminal-generated input until delayed release runs',
);
assert(
  !shouldForwardTerminalInput(gate),
  'input must not be forwarded between xterm callback and delayed release',
);
assert(scheduledReleases.length === 1, 'replay should schedule one delayed gate release');

scheduledReleases[0]?.();

assert(!gate.current, 'replay should release suppression after delayed gate release');
assert(shouldForwardTerminalInput(gate), 'live input should be forwarded after replay');
assert(
  !shouldForwardTerminalInput(gate, false),
  'inactive mounted terminals must not forward xterm-generated input',
);

console.log('terminalReplay.test.ts passed');
