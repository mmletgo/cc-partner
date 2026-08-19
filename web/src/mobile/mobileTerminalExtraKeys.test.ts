import { describe, test } from 'vitest';
import {
  applyStickyModifierToInput,
  armExtraKeyPopup,
  beginExtraKeyPopupPress,
  cancelExtraKeyPopupPress,
  clearMobileTerminalHelperTextareaAfterCommit,
  dismissMobileTerminalSoftKeyboard,
  encodeAltKeyInput,
  encodeCtrlKeyInput,
  enterMobileTerminalTypingMode,
  EXTRA_KEY_POPUP_TRIGGER_HIT_ID,
  getMobileTerminalExtraKeys,
  hitTestExtraKeyPopup,
  hoverExtraKeyPopup,
  leaveMobileTerminalTypingMode,
  MOBILE_TERMINAL_EXTRA_KEY_LONG_PRESS_MS,
  MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS,
  MOBILE_TERMINAL_STICKY_TIMEOUT_MS,
  resolveExtraKeyPopupPointerUp,
  resolveMobileTerminalExtraKeyPress,
  selectExtraKeyPopupItem,
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
  test('exposes a stable, dedup, flat Termux-like key sequence', () => {
    const ids = getMobileTerminalExtraKeys().map((key) => key.id);
    // 历史 PAGE 1 / PAGE 2 已合并为单一扁平序列；翻页键（page-1 / page-2）移除；
    // 用户通过键条容器横向滑动浏览全部键，详见 MobileTerminalExtraKeys.tsx。
    assertEqual(
      ids.join(','),
      [
        'esc',
        'shift-tab',
        'slash',
        'up',
        'down',
        'left',
        'right',
        'enter',
        'tab',
        'ctrl',
        'alt',
        'ctrl-c',
        'ctrl-d',
        'ctrl-z',
        'ctrl-l',
        'home',
        'end',
        'pgup',
        'pgdn',
        'cd-up',
        'ls-la',
        'clear-snippet',
      ].join(','),
      'flat extra keys order',
    );
    const unique = new Set(ids);
    assertEqual(unique.size, ids.length, 'extra key ids are unique');
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
    const keys = getMobileTerminalExtraKeys();
    const esc = keys.find((key) => key.id === 'esc');
    const enter = keys.find((key) => key.id === 'enter');
    const ctrl = keys.find((key) => key.id === 'ctrl');
    assertTrue(Boolean(esc && enter && ctrl), 'required keys present');
    if (!esc || !enter || !ctrl) return;

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

    // setPage 分支已随翻页键移除；剩余分支 payload / modifier 仍正确映射。
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

  test('clearMobileTerminalHelperTextareaAfterCommit drops residual IME text after commit', () => {
    let selection: [number, number] | null = null;
    const helper = {
      value: '你好（',
      setSelectionRange: (start: number, end: number) => {
        selection = [start, end];
      },
    };

    assertTrue(
      clearMobileTerminalHelperTextareaAfterCommit(helper),
      'clears non-empty residual after Chinese parentheses commit',
    );
    assertEqual(helper.value, '', 'value emptied');
    assertEqual(selection?.[0], 0, 'selection start zeroed');
    assertEqual(selection?.[1], 0, 'selection end zeroed');

    assertEqual(
      clearMobileTerminalHelperTextareaAfterCommit(helper),
      false,
      'already-empty returns false',
    );
    assertEqual(
      clearMobileTerminalHelperTextareaAfterCommit(null),
      false,
      'null helper returns false',
    );

    // setSelectionRange 抛错时仍必须清空 value（部分宿主未聚焦时会 throw）。
    const flakyHelper = {
      value: '旧内容）',
      setSelectionRange: () => {
        throw new Error('not focused');
      },
    };
    assertTrue(
      clearMobileTerminalHelperTextareaAfterCommit(flakyHelper),
      'still clears when setSelectionRange throws',
    );
    assertEqual(flakyHelper.value, '', 'value cleared despite selection error');
  });

  test('slash key exposes rewind/resume/compact popup snippets without auto-enter', () => {
    const slash = getMobileTerminalExtraKeys().find((key) => key.id === 'slash');
    assertTrue(slash != null, 'slash key exists');
    const popupIds = (slash?.popup ?? []).map((item) => item.id);
    assertEqual(popupIds.join(','), 'slash-rewind,slash-resume,slash-compact', 'popup nearest-first');
    assertEqual(MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.slashRewind, '/rewind', 'rewind payload');
    assertEqual(MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.slashResume, '/resume', 'resume payload');
    assertEqual(MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.slashCompact, '/compact', 'compact payload');
    assertTrue(!MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.slashRewind.includes('\r'), 'rewind no CR');
    assertTrue(!MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.slashResume.includes('\n'), 'resume no LF');
    assertTrue(!MOBILE_TERMINAL_EXTRA_KEY_PAYLOADS.slashCompact.endsWith('\r'), 'compact no auto-enter');
    const rewind = slash?.popup?.find((item) => item.id === 'slash-rewind');
    assertEqual(rewind?.payload, '/rewind', 'rewind item payload');
    assertEqual(MOBILE_TERMINAL_EXTRA_KEY_LONG_PRESS_MS, 400, 'long-press delay');
  });

  test('popup press session: short tap sends trigger; slide sends item; miss cancels', () => {
    const pending = beginExtraKeyPopupPress('slash');
    assertEqual(pending.phase, 'pending', 'down starts pending');
    const shortTap = resolveExtraKeyPopupPointerUp(pending);
    assertEqual(shortTap.type, 'send', 'short tap sends');
    if (shortTap.type === 'send') {
      assertEqual(shortTap.keyId, 'slash', 'short tap key');
      assertEqual(shortTap.hitId, EXTRA_KEY_POPUP_TRIGGER_HIT_ID, 'short tap trigger');
    }

    const opened = armExtraKeyPopup(pending);
    assertEqual(opened.phase, 'open', 'timeout opens popup');
    if (opened.phase === 'open') {
      assertEqual(opened.hoverId, EXTRA_KEY_POPUP_TRIGGER_HIT_ID, 'hover starts on trigger');
    }

    const hovered = hoverExtraKeyPopup(opened, 'slash-rewind');
    const rewindUp = resolveExtraKeyPopupPointerUp(hovered);
    assertEqual(rewindUp.type, 'send', 'slide send');
    if (rewindUp.type === 'send') {
      assertEqual(rewindUp.hitId, 'slash-rewind', 'slide hit rewind');
    }

    const missed = hoverExtraKeyPopup(opened, null);
    const missUp = resolveExtraKeyPopupPointerUp(missed);
    assertEqual(missUp.type, 'cancel', 'outside popup cancels');

    const cancelled = cancelExtraKeyPopupPress();
    assertEqual(cancelled.phase, 'idle', 'cancel returns idle');
    assertEqual(resolveExtraKeyPopupPointerUp(cancelled).type, 'cancel', 'idle up is cancel');
  });

  test('hit-test prefers popup items over the trigger rect', () => {
    const hit = hitTestExtraKeyPopup(12, 20, [
      { id: EXTRA_KEY_POPUP_TRIGGER_HIT_ID, left: 0, top: 40, right: 40, bottom: 80 },
      { id: 'slash-rewind', left: 0, top: 0, right: 40, bottom: 40 },
    ]);
    assertEqual(hit, 'slash-rewind', 'item wins');
    assertEqual(hitTestExtraKeyPopup(12, 50, [
      { id: 'slash-rewind', left: 0, top: 0, right: 40, bottom: 40 },
      { id: EXTRA_KEY_POPUP_TRIGGER_HIT_ID, left: 0, top: 40, right: 40, bottom: 80 },
    ]), EXTRA_KEY_POPUP_TRIGGER_HIT_ID, 'trigger when over key');
    assertEqual(hitTestExtraKeyPopup(100, 100, [
      { id: 'slash-rewind', left: 0, top: 0, right: 40, bottom: 40 },
    ]), null, 'outside is miss');
  });

  test('selectExtraKeyPopupItem maps trigger to the key and item id to popup def', () => {
    const slash = getMobileTerminalExtraKeys().find((key) => key.id === 'slash');
    assertTrue(slash != null, 'slash exists');
    if (!slash) return;
    const trigger = selectExtraKeyPopupItem(slash, EXTRA_KEY_POPUP_TRIGGER_HIT_ID);
    assertEqual(trigger?.id, 'slash', 'trigger is slash');
    const rewind = selectExtraKeyPopupItem(slash, 'slash-rewind');
    assertEqual(rewind?.payload, '/rewind', 'rewind selected');
    assertEqual(selectExtraKeyPopupItem(slash, 'missing'), null, 'unknown id');
  });
});
