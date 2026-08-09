import { describe, test } from 'vitest';
import {
  prepareInitialReplayBuffer,
  shouldForwardMobileTerminalInput,
} from './mobileTerminalReplay';
import type { TerminalReplayGate } from '@/pages/Workbench/terminalReplay';

/**
 * Business Logic（为什么需要这个函数）:
 *   当前 web tsconfig 会编译 src 下测试文件，但未启用 Node 类型；测试断言需要避免依赖 node:assert。
 *
 * Code Logic（这个函数做什么）:
 *   比较 actual 与 expected，不一致时抛出 Error 让用例失败。
 */
function assertEqual<T>(actual: T, expected: T, message: string): void {
  if (actual !== expected) {
    throw new Error(`${message}: expected ${String(expected)}, received ${String(actual)}`);
  }
}

describe('mobileTerminalReplay', () => {
  test('prepareInitialReplayBuffer selects aligned live tail or keeps replay baseline', () => {
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
      'aligned live buffer baseline must equal bytes written to xterm',
    );

    // live 仅为 replay 后缀（NDJSON 重建后只有近期增量）：必须仍以完整 replay 为 baseline，
    // 否则 store.reset(短 live) 后 buffer effect 可能 clear 掉 xterm scrollback。
    const suffixLive = prepareInitialReplayBuffer(
      'history-line-1\nhistory-line-2\nrecent-tail',
      'recent-tail',
    );
    assertEqual(
      suffixLive.data,
      'history-line-1\nhistory-line-2\nrecent-tail',
      'suffix live must still write full replay history into xterm',
    );
    assertEqual(
      suffixLive.writtenBuffer,
      suffixLive.data,
      'writtenBuffer must equal full written data, not the short live suffix',
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

    // 另一 window 的 live 与当前 replay 仅有短字符串偶然重叠时，旧 KMP 会拼出混合历史。
    // 严格前后缀对齐后必须只保留当前 session 的 HTTP replay。
    const otherWindowLive = prepareInitialReplayBuffer(
      'window-a-session\nprompt$ ls\nfile-a\n',
      'window-b-session\nprompt$ pwd\n/tmp/b\n',
    );
    assertEqual(
      otherWindowLive.data,
      'window-a-session\nprompt$ ls\nfile-a\n',
      'foreign session live must not be mixed into current replay',
    );
    assertEqual(
      otherWindowLive.writtenBuffer.includes('window-b-session'),
      false,
      'written baseline must not contain the other window history',
    );

    // 部分字符重叠（如共同 prompt 前缀）也不得触发拼接。
    const partialOverlap = prepareInitialReplayBuffer(
      'long-history-for-window-one\n$ ',
      'unrelated-output-from-window-two\n$ ',
    );
    assertEqual(
      partialOverlap.data,
      'long-history-for-window-one\n$ ',
      'partial string overlap must not concatenate foreign live into replay',
    );
  });

  test('shouldForwardMobileTerminalInput respects replay readiness and gate', () => {
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
  });
});
