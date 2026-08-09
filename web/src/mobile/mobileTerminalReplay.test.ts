import { describe, test } from 'vitest';
import {
  appendHeldLiveAfterReplay,
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
  test('appendHeldLiveAfterReplay keeps only newer exact deltas from the replay owner', () => {
    const result = appendHeldLiveAfterReplay('history', 10, 'owner-1', [
      {
        sessionId: 's1',
        chunk: 'duplicate',
        generation: 0,
        appendId: 1,
        lastSeq: 10,
        authorityId: 'owner-1',
      },
      {
        sessionId: 's1',
        chunk: 'foreign',
        generation: 0,
        appendId: 2,
        lastSeq: 11,
        authorityId: 'owner-2',
      },
      {
        sessionId: 's1',
        chunk: '-live-1',
        generation: 0,
        appendId: 3,
        lastSeq: 11,
        authorityId: 'owner-1',
      },
      {
        sessionId: 's1',
        chunk: '-live-2',
        generation: 0,
        appendId: 4,
        lastSeq: 13,
        authorityId: 'owner-1',
      },
    ]);

    assertEqual(
      result.buffer,
      'history-live-1-live-2',
      'cutover should append only exact newer deltas from the replay owner',
    );
    assertEqual(result.lastSeq, 13, 'cutover baseline should advance to the last applied seq');
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
