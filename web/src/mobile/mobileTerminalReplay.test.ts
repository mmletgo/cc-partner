import {
  planMobileReplayReadyBufferWrite,
  prepareInitialReplayBuffer,
  shouldForwardMobileTerminalInput,
} from './mobileTerminalReplay';
import type { TerminalReplayGate } from '@/pages/Workbench/terminalReplay';

/**
 * Business Logic（为什么需要这个函数）:
 *   当前 web tsconfig 会编译 src 下测试文件，但未启用 Node 类型；测试断言需要避免依赖 node:assert。
 *
 * Code Logic（这个函数做什么）:
 *   比较 actual 与 expected，不一致时抛出 Error 让 tsx 进程以失败状态退出。
 */
function assertEqual<T>(actual: T, expected: T, message: string): void {
  if (actual !== expected) {
    throw new Error(`${message}: expected ${String(expected)}, received ${String(actual)}`);
  }
}

const replayOnly = prepareInitialReplayBuffer('replay-screen', '');
assertEqual(replayOnly.data, 'replay-screen', 'replay-only should write replay buffer');
assertEqual(
  replayOnly.writtenBuffer,
  'replay-screen',
  'replay-only should use replay buffer as written baseline',
);

const liveOnly = prepareInitialReplayBuffer('', 'live-screen');
assertEqual(liveOnly.data, 'live-screen', 'live-only should write live buffer');
assertEqual(
  liveOnly.writtenBuffer,
  'live-screen',
  'live-only should use live buffer as written baseline',
);

const appendedLive = prepareInitialReplayBuffer('screen-a', 'screen-a-tail');
assertEqual(
  appendedLive.data,
  'screen-a-tail',
  'aligned live buffer should write replay plus append tail',
);
assertEqual(
  appendedLive.writtenBuffer,
  'screen-a-tail',
  'aligned live buffer should become written baseline',
);

const unalignedLive = prepareInitialReplayBuffer('replay-history', 'fresh-live-window');
assertEqual(
  unalignedLive.data,
  'replay-history',
  'unaligned live buffer must not be concatenated into initial replay',
);
assertEqual(
  unalignedLive.writtenBuffer,
  'replay-history',
  'unaligned live buffer should keep replay as safe baseline',
);

const gate: TerminalReplayGate = { current: false };
assertEqual(
  shouldForwardMobileTerminalInput(gate, false, true),
  false,
  'mobile terminal input should stay blocked before replay is ready',
);
assertEqual(
  shouldForwardMobileTerminalInput(gate, true, true),
  true,
  'mobile terminal input should forward after replay is ready',
);
gate.current = true;
assertEqual(
  shouldForwardMobileTerminalInput(gate, true, true),
  false,
  'mobile terminal input should stay blocked while replay gate is active',
);

const readyPlan = planMobileReplayReadyBufferWrite('screen-a', 'screen-a-tail');
assertEqual(
  readyPlan.mode,
  'append',
  'replay-ready live buffer should be reconciled immediately',
);
assertEqual(readyPlan.data, '-tail', 'replay-ready live buffer should append only new tail');

console.log('mobileTerminalReplay.test.ts passed');
