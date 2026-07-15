// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { cleanup, render, waitFor } from '@testing-library/react';
import type { Extension } from '@codemirror/state';
import type { ReactElement } from 'react';

const loadWorkbenchLanguageMock = vi.hoisted(() => vi.fn());

vi.mock('./workbenchCodeEditorLanguage', () => ({
  loadWorkbenchLanguage: loadWorkbenchLanguageMock,
}));

vi.mock('@uiw/react-codemirror', () => ({
  default: function MockCodeMirror(props: {
    extensions?: Extension[];
    value?: string;
    editable?: boolean;
    readOnly?: boolean;
  }): ReactElement {
    const languageCount = Array.isArray(props.extensions)
      ? props.extensions.filter((item) => item && (item as { __lang?: boolean }).__lang).length
      : 0;
    return (
      <div
        data-testid="mock-codemirror"
        data-language-extensions={String(languageCount)}
        data-editable={String(props.editable !== false && props.readOnly !== true)}
        data-value={props.value ?? ''}
      />
    );
  },
}));

vi.mock('./workbenchCodeEditorTheme', () => ({
  WORKBENCH_CODE_EDITOR_THEME: { __theme: true },
  WORKBENCH_ONE_DARK_PRO_SYNTAX_EXTENSION: { __syntax: true },
}));

import { WorkbenchCodeEditor } from './WorkbenchCodeEditor';

/**
 * Business Logic（为什么需要这些测试）:
 *   语言包异步加载期间用户仍须可编辑纯文本；快速切换文件时旧语言 Promise 不得回写高亮，
 *   加载失败须退回纯文本而不是卡死编辑器。
 *
 * Code Logic（这个套件做什么）:
 *   mock loadWorkbenchLanguage 与 CodeMirror，断言 loading 时仍 editable、成功后注入语言扩展、
 *   快速 A→B 时丢弃旧 Promise、失败时 language extension 为 0。
 */

describe('WorkbenchCodeEditor language loading', () => {
  beforeEach(() => {
    loadWorkbenchLanguageMock.mockReset();
  });

  afterEach(() => {
    cleanup();
  });

  test('renders editable plain text while language is loading', async () => {
    let resolveLang!: (value: Extension | null) => void;
    loadWorkbenchLanguageMock.mockImplementation(
      () =>
        new Promise<Extension | null>((resolve) => {
          resolveLang = resolve;
        }),
    );

    const { getByTestId } = render(
      <WorkbenchCodeEditor value="const x = 1" language="typescript" onChange={() => undefined} />,
    );

    const editor = getByTestId('mock-codemirror');
    expect(editor.getAttribute('data-editable')).toBe('true');
    expect(editor.getAttribute('data-language-extensions')).toBe('0');

    resolveLang(Object.assign({} as Extension, { __lang: true }));
    await waitFor(() => {
      expect(getByTestId('mock-codemirror').getAttribute('data-language-extensions')).toBe('1');
    });
  });

  test('rapid A→B ignores the old language promise', async () => {
    const resolvers: Array<(value: Extension | null) => void> = [];
    loadWorkbenchLanguageMock.mockImplementation(
      () =>
        new Promise<Extension | null>((resolve) => {
          resolvers.push(resolve);
        }),
    );

    const { getByTestId, rerender } = render(
      <WorkbenchCodeEditor value="a" language="typescript" onChange={() => undefined} />,
    );
    expect(resolvers).toHaveLength(1);

    rerender(<WorkbenchCodeEditor value="b" language="python" onChange={() => undefined} />);
    expect(resolvers).toHaveLength(2);

    // 先完成旧的 typescript 请求，不得写入
    resolvers[0](Object.assign({} as Extension, { __lang: true, name: 'ts' }));
    await Promise.resolve();
    expect(getByTestId('mock-codemirror').getAttribute('data-language-extensions')).toBe('0');

    // 完成 python 请求，才注入
    resolvers[1](Object.assign({} as Extension, { __lang: true, name: 'py' }));
    await waitFor(() => {
      expect(getByTestId('mock-codemirror').getAttribute('data-language-extensions')).toBe('1');
    });
  });

  test('import failure falls back to plain text', async () => {
    loadWorkbenchLanguageMock.mockImplementation(() => Promise.reject(new Error('chunk missing')));

    const { getByTestId } = render(
      <WorkbenchCodeEditor value="print(1)" language="python" onChange={() => undefined} />,
    );

    await waitFor(() => {
      expect(loadWorkbenchLanguageMock).toHaveBeenCalled();
    });
    await Promise.resolve();
    expect(getByTestId('mock-codemirror').getAttribute('data-language-extensions')).toBe('0');
    expect(getByTestId('mock-codemirror').getAttribute('data-editable')).toBe('true');
  });
});
