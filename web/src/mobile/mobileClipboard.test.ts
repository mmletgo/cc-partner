import { describe, test } from 'vitest';
import { writeClipboardText, type WriteClipboardTextDeps } from './mobileClipboard';

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

/**
 * Business Logic（为什么需要这个函数）:
 *   clipboard 测试必须注入最小 document，不能依赖真实 DOM 或安全上下文。
 *
 * Code Logic（这个函数做什么）:
 *   用 spy textarea 组装 body.appendChild/removeChild 与 createElement('textarea')。
 */
function createFakeDocument(
  textarea: { value: string; select: () => void },
): NonNullable<WriteClipboardTextDeps['document']> {
  return {
    body: {
      appendChild: (node: HTMLTextAreaElement) => {
        void node;
      },
      removeChild: (node: HTMLTextAreaElement) => {
        void node;
      },
    },
    createElement: (tag: 'textarea') => {
      void tag;
      return textarea as unknown as HTMLTextAreaElement;
    },
  };
}

describe('writeClipboardText', () => {
  test('empty string returns empty without calling either method', async () => {
    let clipboardCalls = 0;
    let execCalls = 0;
    let createCalls = 0;
    const textarea = { value: '', select() {} };
    const result = await writeClipboardText('', {
      clipboardWriteText: async () => {
        clipboardCalls += 1;
      },
      execCommand: () => {
        execCalls += 1;
        return true;
      },
      document: {
        ...createFakeDocument(textarea),
        createElement: (tag: 'textarea') => {
          createCalls += 1;
          void tag;
          return textarea as unknown as HTMLTextAreaElement;
        },
      },
    });

    assertEqual(result.ok, false, 'empty is not ok');
    if (!result.ok) {
      assertEqual(result.reason, 'empty', 'reason empty');
    }
    assertEqual(clipboardCalls, 0, 'clipboardWriteText not called');
    assertEqual(execCalls, 0, 'execCommand not called');
    assertEqual(createCalls, 0, 'textarea not created');
  });

  test('clipboardWriteText resolve uses clipboard and skips execCommand', async () => {
    let execCalls = 0;
    const written: string[] = [];
    const result = await writeClipboardText('  keep\n', {
      clipboardWriteText: async (text) => {
        written.push(text);
      },
      execCommand: () => {
        execCalls += 1;
        return true;
      },
      document: createFakeDocument({ value: '', select() {} }),
    });

    assertTrue(result.ok, 'clipboard success is ok');
    if (result.ok) {
      assertEqual(result.method, 'clipboard', 'method clipboard');
    }
    assertEqual(written.join('|'), '  keep\n', 'copies terminal text as-is without trim');
    assertEqual(execCalls, 0, 'execCommand not called');
  });

  test('clipboardWriteText reject falls back to execCommand copy', async () => {
    const commands: string[] = [];
    const result = await writeClipboardText('selected', {
      clipboardWriteText: async () => {
        throw new Error('not allowed');
      },
      execCommand: (commandId) => {
        commands.push(commandId);
        return true;
      },
      document: createFakeDocument({ value: '', select() {} }),
    });

    assertTrue(result.ok, 'fallback success is ok');
    if (result.ok) {
      assertEqual(result.method, 'execCommand', 'method execCommand');
    }
    assertEqual(commands.join(','), 'copy', 'execCommand copy');
  });

  test('both methods failing returns failed', async () => {
    const result = await writeClipboardText('selected', {
      clipboardWriteText: async () => {
        throw new Error('not allowed');
      },
      execCommand: () => false,
      document: createFakeDocument({ value: '', select() {} }),
    });

    assertEqual(result.ok, false, 'both fail is not ok');
    if (!result.ok) {
      assertEqual(result.reason, 'failed', 'reason failed');
    }
  });

  test('missing clipboard with successful execCommand uses execCommand', async () => {
    const commands: string[] = [];
    const result = await writeClipboardText('hello', {
      execCommand: (commandId) => {
        commands.push(commandId);
        return true;
      },
      document: createFakeDocument({ value: '', select() {} }),
    });

    assertTrue(result.ok, 'execCommand-only success is ok');
    if (result.ok) {
      assertEqual(result.method, 'execCommand', 'method execCommand');
    }
    assertEqual(commands.join(','), 'copy', 'execCommand copy');
  });

  test('execCommand path assigns textarea value and calls select before copy', async () => {
    const events: string[] = [];
    let stored = '';
    const textarea = {
      get value(): string {
        return stored;
      },
      set value(next: string) {
        events.push(`value:${next}`);
        stored = next;
      },
      select() {
        events.push('select');
      },
    };
    const result = await writeClipboardText('line1\nline2', {
      execCommand: (commandId) => {
        events.push(`exec:${commandId}`);
        return true;
      },
      document: {
        body: {
          appendChild: (node: HTMLTextAreaElement) => {
            events.push('append');
            void node;
          },
          removeChild: (node: HTMLTextAreaElement) => {
            events.push('remove');
            void node;
          },
        },
        createElement: (tag: 'textarea') => {
          events.push(`create:${tag}`);
          return textarea as unknown as HTMLTextAreaElement;
        },
      },
    });

    assertTrue(result.ok, 'spy path is ok');
    if (result.ok) {
      assertEqual(result.method, 'execCommand', 'method execCommand');
    }
    const valueIndex = events.indexOf('value:line1\nline2');
    const selectIndex = events.indexOf('select');
    const copyIndex = events.indexOf('exec:copy');
    assertTrue(valueIndex >= 0, 'assigns textarea.value');
    assertTrue(selectIndex >= 0, 'calls select');
    assertTrue(copyIndex >= 0, 'calls execCommand copy');
    assertTrue(valueIndex < selectIndex, 'value assigned before select');
    assertTrue(selectIndex < copyIndex, 'select called before copy');
  });
});
