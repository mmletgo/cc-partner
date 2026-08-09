import { describe, test } from 'vitest';
import {
  applyStickyModifierToInput,
  dismissMobileTerminalSoftKeyboard,
  encodeAltKeyInput,
  encodeCtrlKeyInput,
  enterMobileTerminalTypingMode,
  getMobileTerminalExtraKeys,
  leaveMobileTerminalTypingMode,
  MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS,
  MOBILE_TERMINAL_STICKY_TIMEOUT_MS,
  resolveMobileTerminalExtraKeyPress,
  toggleStickyModifier,
} from './mobileTerminalExtraKeys';

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

function assertTrue(value: boolean, message: string): void {
  if (!value) throw new Error(message);
}

describe('mobileTerminalExtraKeys', () => {
  test('page 1 and page 2 expose fixed Termux-like key ids', () => {
    const page1 = getMobileTerminalExtraKeys(1).map((key) => key.id);
    const page2 = getMobileTerminalExtraKeys(2).map((key) => key.id);
    assertEqual(
      page1.join(','),
      'esc,enter,shift-tab,slash,up,down,left,right,page-2',
      'page 1 keys',
    );
    assertEqual(
      page2.join(','),
      'ctrl,alt,tab,ctrl-c,ctrl-d,ctrl-z,ctrl-l,home,end,pgup,pgdn,cd-up,ls-la,clear-snippet,page-1',
      'page 2 keys',
    );
  });

  test('control and navigation payloads match PTY sequences', () => {
    assertEqual(MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.esc, '\x1b', 'esc');
    assertEqual(MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.tab, '\t', 'tab');
    assertEqual(MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.shiftTab, '\x1b[Z', 'shift+tab');
    assertEqual(MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.enter, '\r', 'enter');
    assertEqual(MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.up, '\x1b[A', 'up');
    assertEqual(MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.down, '\x1b[B', 'down');
    assertEqual(MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.right, '\x1b[C', 'right');
    assertEqual(MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.left, '\x1b[D', 'left');
    assertEqual(MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.home, '\x1b[H', 'home');
    assertEqual(MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.end, '\x1b[F', 'end');
    assertEqual(MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.pageUp, '\x1b[5~', 'pgup');
    assertEqual(MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.pageDown, '\x1b[6~', 'pgdn');
    assertEqual(MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.ctrlC, '\x03', 'ctrl-c');
    assertEqual(MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.ctrlD, '\x04', 'ctrl-d');
    assertEqual(MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.ctrlZ, '\x1a', 'ctrl-z');
    assertEqual(MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.ctrlL, '\x0c', 'ctrl-l');
  });

  test('shell snippets insert text without trailing enter', () => {
    assertEqual(MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.cdUp, 'cd ..', 'cd ..');
    assertEqual(MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.lsLa, 'ls -la', 'ls -la');
    assertEqual(MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.clear, 'clear', 'clear');
    assertTrue(!MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.cdUp.includes('\r'), 'cd .. must not include CR');
    assertTrue(!MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.lsLa.includes('\n'), 'ls -la must not include LF');
    assertTrue(!MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.clear.endsWith('\r'), 'clear must not auto-enter');
  });

  test('encodeCtrlKeyInput maps letters and common chords', () => {
    assertEqual(encodeCtrlKeyInput('c'), '\x03', 'ctrl-c lower');
    assertEqual(encodeCtrlKeyInput('C'), '\x03', 'ctrl-c upper');
    assertEqual(encodeCtrlKeyInput('d'), '\x04', 'ctrl-d');
    assertEqual(encodeCtrlKeyInput('z'), '\x1a', 'ctrl-z');
    assertEqual(encodeCtrlKeyInput('l'), '\x0c', 'ctrl-l');
    assertEqual(encodeCtrlKeyInput(' '), '\x00', 'ctrl-space');
    assertEqual(encodeCtrlKeyInput('ab'), null, 'multi-char rejected');
  });

  test('encodeAltKeyInput prefixes escape for single chars only', () => {
    assertEqual(encodeAltKeyInput('a'), '\x1ba', 'alt-a');
    assertEqual(encodeAltKeyInput('ab'), null, 'multi-char rejected');
  });

  test('applyStickyModifierToInput consumes sticky even when encoding falls back', () => {
    assertEqual(applyStickyModifierToInput(null, 'x').consume, false, 'no sticky');
    assertEqual(applyStickyModifierToInput(null, 'x').data, 'x', 'pass-through');

    const ctrlC = applyStickyModifierToInput('ctrl', 'c');
    assertEqual(ctrlC.data, '\x03', 'ctrl encodes');
    assertEqual(ctrlC.consume, true, 'ctrl consumes');

    const altX = applyStickyModifierToInput('alt', 'x');
    assertEqual(altX.data, '\x1bx', 'alt encodes');
    assertEqual(altX.consume, true, 'alt consumes');

    // multi-char (paste): still consume sticky once, do not rewrite body
    const paste = applyStickyModifierToInput('ctrl', 'ab');
    assertEqual(paste.data, 'ab', 'paste not rewritten');
    assertEqual(paste.consume, true, 'paste still consumes sticky');
  });

  test('toggleStickyModifier arms, replaces, and disarms', () => {
    assertEqual(toggleStickyModifier(null, 'ctrl').type, 'arm', 'arm ctrl');
    assertEqual(toggleStickyModifier('ctrl', 'ctrl').type, 'disarm', 'disarm ctrl');
    const replace = toggleStickyModifier('ctrl', 'alt');
    assertEqual(replace.type, 'arm', 'replace with alt');
    if (replace.type === 'arm') {
      assertEqual(replace.modifier, 'alt', 'armed alt');
    }
  });

  test('resolveMobileTerminalExtraKeyPress maps defs to actions', () => {
    const page1 = getMobileTerminalExtraKeys(1);
    const page2 = getMobileTerminalExtraKeys(2);
    const esc = page1.find((key) => key.id === 'esc');
    const enter = page1.find((key) => key.id === 'enter');
    const ctrl = page2.find((key) => key.id === 'ctrl');
    const page2Key = page1.find((key) => key.id === 'page-2');
    assertTrue(Boolean(esc && enter && ctrl && page2Key), 'required keys present');
    if (!esc || !enter || !ctrl || !page2Key) return;

    const escResult = resolveMobileTerminalExtraKeyPress(esc);
    assertEqual(escResult.type, 'send', 'esc sends');
    if (escResult.type === 'send') {
      assertEqual(escResult.data, '\x1b', 'esc payload');
    }

    const enterResult = resolveMobileTerminalExtraKeyPress(enter);
    assertEqual(enterResult.type, 'send', 'enter sends');
    if (enterResult.type === 'send') {
      assertEqual(enterResult.data, '\r', 'enter payload');
    }

    const ctrlResult = resolveMobileTerminalExtraKeyPress(ctrl);
    assertEqual(ctrlResult.type, 'toggleModifier', 'ctrl toggles');
    if (ctrlResult.type === 'toggleModifier') {
      assertEqual(ctrlResult.modifier, 'ctrl', 'ctrl modifier');
    }

    const pageResult = resolveMobileTerminalExtraKeyPress(page2Key);
    assertEqual(pageResult.type, 'setPage', 'page switch');
    if (pageResult.type === 'setPage') {
      assertEqual(pageResult.page, 2, 'target page 2');
    }
  });

  test('sticky timeout constant is 3 seconds', () => {
    assertEqual(MOBILE_TERMINAL_STICKY_TIMEOUT_MS, 3000, 'sticky timeout');
  });

  test('dismissMobileTerminalSoftKeyboard blurs xterm textarea but ignores non-editable focus', () => {
    let blurred = false;
    const textarea = {
      tagName: 'TEXTAREA',
      classList: { contains: (token: string) => token === 'xterm-helper-textarea' },
      blur: () => {
        blurred = true;
      },
    };
    assertTrue(dismissMobileTerminalSoftKeyboard(textarea), 'textarea dismissed');
    assertTrue(blurred, 'textarea.blur called');

    blurred = false;
    const button = {
      tagName: 'BUTTON',
      blur: () => {
        blurred = true;
      },
    };
    assertEqual(dismissMobileTerminalSoftKeyboard(button), false, 'button ignored');
    assertEqual(blurred, false, 'button not blurred');
    assertEqual(dismissMobileTerminalSoftKeyboard(null), false, 'null ignored');
  });

  test('typing mode enter/leave toggles readonly and inputmode on helper textarea', () => {
    const attrs = new Map<string, string>();
    let blurred = 0;
    const helper = {
      setAttribute: (name: string, value: string) => {
        attrs.set(name, value);
      },
      removeAttribute: (name: string) => {
        attrs.delete(name);
      },
      blur: () => {
        blurred += 1;
      },
    };

    assertTrue(leaveMobileTerminalTypingMode(helper, null), 'leave applies helper attrs');
    assertEqual(attrs.get('readonly'), 'true', 'readonly set');
    assertEqual(attrs.get('inputmode'), 'none', 'inputmode none');
    assertEqual(blurred, 1, 'leave blurs helper');

    assertTrue(enterMobileTerminalTypingMode(helper), 'enter clears attrs');
    assertEqual(attrs.has('readonly'), false, 'readonly removed');
    assertEqual(attrs.has('inputmode'), false, 'inputmode removed');
    assertEqual(enterMobileTerminalTypingMode(null), false, 'enter null');
    assertEqual(leaveMobileTerminalTypingMode(null, null), false, 'leave null');
  });
});
