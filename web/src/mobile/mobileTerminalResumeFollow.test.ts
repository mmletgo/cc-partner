import { describe, test } from 'vitest';
import {
  beginMobileTerminalResumePin,
  isMobileTerminalResumePinned,
  isMobileTerminalResumeVisible,
  MOBILE_TERMINAL_RESUME_FOLLOW_MS,
  scrollMobileTerminalToLatest,
  shouldFollowMobileTerminalToLatest,
} from './mobileTerminalResumeFollow';

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

describe('mobileTerminalResumeFollow', () => {
  test('only follows when the document is visible', () => {
    assertEqual(isMobileTerminalResumeVisible('visible'), true, 'visible should follow');
    assertEqual(isMobileTerminalResumeVisible('hidden'), false, 'hidden should not follow');
    assertEqual(
      isMobileTerminalResumeVisible('prerender'),
      false,
      'prerender should not follow',
    );
  });

  test('skips follow while the user is selecting text', () => {
    assertEqual(
      shouldFollowMobileTerminalToLatest({
        visible: true,
        selecting: true,
        hasSelection: false,
      }),
      false,
      'selecting blocks follow',
    );
    assertEqual(
      shouldFollowMobileTerminalToLatest({
        visible: true,
        selecting: false,
        hasSelection: true,
      }),
      false,
      'existing selection blocks follow',
    );
    assertEqual(
      shouldFollowMobileTerminalToLatest({
        visible: true,
        selecting: false,
        hasSelection: false,
      }),
      true,
      'visible idle terminal should follow',
    );
    assertEqual(
      shouldFollowMobileTerminalToLatest({
        visible: false,
        selecting: false,
        hasSelection: false,
      }),
      false,
      'hidden idle terminal should not follow',
    );
  });

  test('pins follow for the reconnect catch-up window then expires', () => {
    const pin = beginMobileTerminalResumePin(1_000);
    assertEqual(pin.untilMs, 1_000 + MOBILE_TERMINAL_RESUME_FOLLOW_MS, 'pin duration');
    assertEqual(isMobileTerminalResumePinned(pin, 1_000), true, 'start of window is pinned');
    assertEqual(
      isMobileTerminalResumePinned(pin, 1_000 + MOBILE_TERMINAL_RESUME_FOLLOW_MS - 1),
      true,
      'just before expiry is pinned',
    );
    assertEqual(
      isMobileTerminalResumePinned(pin, 1_000 + MOBILE_TERMINAL_RESUME_FOLLOW_MS),
      false,
      'expiry is not pinned',
    );
    assertEqual(isMobileTerminalResumePinned(null, 1_000), false, 'null pin is not pinned');
  });

  test('scrolls to the absolute latest buffer line rather than a relative offset', () => {
    const calls: number[] = [];
    const terminal = {
      buffer: { active: { baseY: 80, viewportY: 12 } },
      scrollToLine(line: number): void {
        calls.push(line);
        this.buffer.active.viewportY = line;
      },
    };

    scrollMobileTerminalToLatest(terminal);

    assertEqual(calls.length, 1, 'scroll once');
    assertEqual(calls[0], 80, 'target is baseY');
    assertEqual(terminal.buffer.active.viewportY, 80, 'viewport moved to latest');
  });
});
