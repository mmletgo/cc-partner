import { describe, test } from 'vitest';
import {
  formatShortcutForDisplay,
  getDefaultShortcutValue,
  resolveShortcutRecording,
  shortcutHasConflictingCommandOrControl,
} from './shortcutRecorder';

/**
 * Business Logic（为什么需要）:
 *   前端项目 tsconfig 不启用 Node 类型，行为测试不能依赖 node:assert。
 *
 * Code Logic（做什么）:
 *   将实际值和期望值序列化后比较，不一致时抛出 Error 让 node 进程非零退出。
 */
function assertDeepEqual(actual: unknown, expected: unknown): void {
  const actualJson = JSON.stringify(actual);
  const expectedJson = JSON.stringify(expected);
  if (actualJson !== expectedJson) {
    throw new Error(`Expected ${expectedJson}, got ${actualJson}`);
  }
}

/**
 * Business Logic（为什么需要）:
 *   快捷键展示文本测试只需要基础相等断言，避免引入测试框架。
 *
 * Code Logic（做什么）:
 *   使用 Object.is 比较两个字符串，不一致时抛出可读错误。
 */
function assertEqual(actual: string, expected: string): void {
  if (!Object.is(actual, expected)) {
    throw new Error(`Expected ${expected}, got ${actual}`);
  }
}

/**
 * Business Logic（为什么需要）:
 *   快捷键录制逻辑只依赖 KeyboardEvent 的少数字段，测试需要构造最小事件对象
 *   来覆盖用户真实按键组合。
 *
 * Code Logic（做什么）:
 *   接收 key 与修饰键布尔值，补齐未传修饰键为 false，返回
 *   resolveShortcutRecording 可消费的事件形状。
 */
function keyboardEvent(init: {
  key: string;
  metaKey?: boolean;
  ctrlKey?: boolean;
  altKey?: boolean;
  shiftKey?: boolean;
}) {
  return {
    key: init.key,
    metaKey: init.metaKey ?? false,
    ctrlKey: init.ctrlKey ?? false,
    altKey: init.altKey ?? false,
    shiftKey: init.shiftKey ?? false,
  };
}

describe('shortcutRecorder', () => {
  test('resolves recordings, formats display labels and defaults per platform', () => {
    assertDeepEqual(
      resolveShortcutRecording(keyboardEvent({ key: 's', metaKey: true, shiftKey: true })),
      { type: 'record', value: '<cmd>+<shift>+s' },
    );

    assertDeepEqual(
      resolveShortcutRecording(keyboardEvent({ key: 'S', ctrlKey: true, shiftKey: true })),
      { type: 'record', value: '<ctrl>+<shift>+s' },
    );

    assertDeepEqual(resolveShortcutRecording(keyboardEvent({ key: 'Shift', shiftKey: true })), {
      type: 'pending',
    });

    assertDeepEqual(
      resolveShortcutRecording(keyboardEvent({ key: 'Control', ctrlKey: true }), {
        allowModifierOnly: true,
      }),
      { type: 'record', value: '<ctrl>' },
    );

    assertDeepEqual(
      resolveShortcutRecording(keyboardEvent({ key: 'Control', ctrlKey: true })),
      { type: 'pending' },
    );

    assertDeepEqual(resolveShortcutRecording(keyboardEvent({ key: 'Escape' })), {
      type: 'cancel',
    });

    assertDeepEqual(resolveShortcutRecording(keyboardEvent({ key: 'Backspace' })), {
      type: 'clear',
      value: '',
    });

    assertEqual(formatShortcutForDisplay('<cmd>+<shift>+s'), 'Cmd+Shift+S');
    assertEqual(formatShortcutForDisplay('<ctrl>+<alt>+f5'), 'Ctrl+Alt+F5');
    assertEqual(formatShortcutForDisplay('<ctrl>'), 'Control');
    assertEqual(getDefaultShortcutValue('MacIntel'), '<cmd>+<shift>+s');
    assertEqual(getDefaultShortcutValue('Win32'), '<ctrl>+<shift>+s');

    assertDeepEqual(
      resolveShortcutRecording(
        keyboardEvent({ key: 's', metaKey: true, ctrlKey: true }),
      ),
      { type: 'reject', reason: 'cmdCtrlConflict' },
    );
    assertDeepEqual(
      resolveShortcutRecording(
        keyboardEvent({ key: 's', metaKey: true, ctrlKey: true, shiftKey: true }),
      ),
      { type: 'reject', reason: 'cmdCtrlConflict' },
    );
    assertDeepEqual(
      resolveShortcutRecording(keyboardEvent({ key: 'Control', metaKey: true, ctrlKey: true }), {
        allowModifierOnly: true,
      }),
      { type: 'reject', reason: 'cmdCtrlConflict' },
    );
    if (shortcutHasConflictingCommandOrControl('<cmd>+<ctrl>+s') !== true) {
      throw new Error('expected cmd+ctrl to conflict');
    }
    if (shortcutHasConflictingCommandOrControl('<cmd>+<cmd>+s') !== true) {
      throw new Error('expected duplicate cmd to conflict');
    }
    if (shortcutHasConflictingCommandOrControl('<cmd>+<shift>+s') !== false) {
      throw new Error('expected cmd+shift to be allowed');
    }
    if (shortcutHasConflictingCommandOrControl('<ctrl>') !== false) {
      throw new Error('expected lone ctrl to be allowed');
    }
  });
});
