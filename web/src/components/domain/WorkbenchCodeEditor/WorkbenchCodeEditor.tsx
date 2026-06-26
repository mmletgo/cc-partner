/**
 * WorkbenchCodeEditor 业务组件
 *
 * Business Logic（为什么需要这个组件）:
 *   工作台文件查看/编辑能力需要一个可复用的代码编辑器外壳，后续页面可以按文件语言
 *   展示语法高亮、行号、折叠与搜索等基础编辑能力，同时在只读预览和可编辑文件之间复用同一套交互体验。
 *
 * Code Logic（这个组件做什么）:
 *   - 封装 @uiw/react-codemirror，统一 CodeMirror 的基础 setup、容器样式和 100% 高度布局
 *   - 根据 language prop 通过 useMemo 计算语言扩展，未知语言返回空数组并按纯文本渲染
 *   - 将 CodeMirror 的 onChange value 透传给调用方，由上层负责保存、脏状态和文件生命周期
 */

import CodeMirror from '@uiw/react-codemirror';
import type { Extension } from '@codemirror/state';
import { css } from '@codemirror/lang-css';
import { html } from '@codemirror/lang-html';
import { javascript } from '@codemirror/lang-javascript';
import { json } from '@codemirror/lang-json';
import { markdown } from '@codemirror/lang-markdown';
import { python } from '@codemirror/lang-python';
import { rust } from '@codemirror/lang-rust';
import { StreamLanguage } from '@codemirror/language';
import { shell } from '@codemirror/legacy-modes/mode/shell';
import { toml } from '@codemirror/legacy-modes/mode/toml';
import { useMemo } from 'react';
import type { ReactElement } from 'react';
import styles from './WorkbenchCodeEditor.module.css';

export interface WorkbenchCodeEditorProps {
  value: string;
  language: string;
  readOnly?: boolean;
  onChange: (value: string) => void;
}

/**
 * 计算 CodeMirror 语言扩展
 *
 * Business Logic（为什么需要这个函数）:
 *   工作台文件面板会接收来自文件类型识别逻辑的语言名称或扩展名，编辑器需要把这些业务语言标识
 *   转换成 CodeMirror 可理解的语法扩展；未识别语言仍应可打开为纯文本，不能阻断文件查看流程。
 *
 * Code Logic（这个函数做什么）:
 *   标准化传入的 language 字符串，按常见语言 ID / 文件扩展名返回对应的 CodeMirror Extension 数组；
 *   TS/TSX/JS/JSX 复用 javascript 扩展配置，TOML 和 shell 通过 legacy stream mode 包装，未知语言返回 []。
 */
function languageExtensions(language: string): Extension[] {
  const normalizedLanguage = language.trim().toLowerCase().replace(/^\./, '');

  switch (normalizedLanguage) {
    case 'typescript':
    case 'ts':
      return [javascript({ typescript: true })];
    case 'tsx':
      return [javascript({ typescript: true, jsx: true })];
    case 'javascript':
    case 'js':
    case 'mjs':
    case 'cjs':
      return [javascript()];
    case 'jsx':
      return [javascript({ jsx: true })];
    case 'json':
      return [json()];
    case 'markdown':
    case 'md':
    case 'mdx':
      return [markdown()];
    case 'css':
      return [css()];
    case 'html':
    case 'htm':
      return [html()];
    case 'python':
    case 'py':
      return [python()];
    case 'rust':
    case 'rs':
      return [rust()];
    case 'toml':
      return [StreamLanguage.define(toml)];
    case 'shell':
    case 'sh':
    case 'bash':
    case 'zsh':
    case 'fish':
      return [StreamLanguage.define(shell)];
    default:
      return [];
  }
}

/**
 * 渲染工作台代码编辑器
 *
 * Business Logic（为什么需要这个组件）:
 *   工作台文件查看器后续需要在不同文件 tab 中复用同一个代码编辑体验，并根据当前文件是否可编辑
 *   切换只读和编辑模式，避免页面层重复配置 CodeMirror。
 *
 * Code Logic（这个组件做什么）:
 *   使用 useMemo 按 language 缓存语言扩展，渲染 100% 高度的 CodeMirror，并启用行号、折叠 gutter、
 *   当前行高亮、括号匹配和搜索快捷键；内容变化时只把最新字符串回传给 onChange。
 */
export function WorkbenchCodeEditor({
  value,
  language,
  readOnly = false,
  onChange,
}: WorkbenchCodeEditorProps): ReactElement {
  const extensions = useMemo(() => languageExtensions(language), [language]);

  return (
    <div className={styles.editorShell}>
      <CodeMirror
        value={value}
        height="100%"
        editable={!readOnly}
        readOnly={readOnly}
        extensions={extensions}
        onChange={(next) => onChange(next)}
        basicSetup={{
          lineNumbers: true,
          foldGutter: true,
          highlightActiveLine: true,
          bracketMatching: true,
          searchKeymap: true,
        }}
      />
    </div>
  );
}
