/**
 * WorkbenchCodeEditor 业务组件
 *
 * Business Logic（为什么需要这个组件）:
 *   工作台文件查看/编辑能力需要一个可复用的代码编辑器外壳，后续页面可以按文件语言
 *   展示语法高亮、行号、折叠与搜索等基础编辑能力，同时在只读预览和可编辑文件之间复用同一套交互体验。
 *
 * Code Logic（这个组件做什么）:
 *   - 封装 @uiw/react-codemirror，统一 CodeMirror 的基础 setup、随应用主题变化的编辑器背景和 100% 高度布局
 *   - 单独注入 One Dark Pro 语法高亮扩展，避免 @uiw 默认 light theme 覆盖工作台视觉背景
 *   - 根据 language prop 通过 useMemo 计算语言扩展，未知语言返回空数组并按纯文本渲染
 *   - 将 CodeMirror 的 onChange value 透传给调用方，由上层负责保存、脏状态和文件生命周期
 */

import CodeMirror from '@uiw/react-codemirror';
import type { BasicSetupOptions } from '@uiw/react-codemirror';
import { useCallback, useMemo } from 'react';
import type { ReactElement } from 'react';
import { getWorkbenchCodeEditorLanguageExtensions } from './workbenchCodeEditorLanguage';
import {
  WORKBENCH_CODE_EDITOR_THEME,
  WORKBENCH_ONE_DARK_PRO_SYNTAX_EXTENSION,
} from './workbenchCodeEditorTheme';
import styles from './WorkbenchCodeEditor.module.css';

export interface WorkbenchCodeEditorProps {
  value: string;
  language: string;
  readOnly?: boolean;
  onChange: (value: string) => void;
}

const WORKBENCH_CODE_EDITOR_BASIC_SETUP: BasicSetupOptions = {
  lineNumbers: true,
  foldGutter: true,
  highlightActiveLine: true,
  bracketMatching: true,
  searchKeymap: true,
};

/**
 * 渲染工作台代码编辑器
 *
 * Business Logic（为什么需要这个组件）:
 *   工作台文件查看器后续需要在不同文件 tab 中复用同一个代码编辑体验，并根据当前文件是否可编辑
 *   切换只读和编辑模式，避免页面层重复配置 CodeMirror。
 *
 * Code Logic（这个组件做什么）:
 *   使用 useMemo 按 language 缓存语言扩展并追加 One Dark Pro syntax highlight，渲染带自定义 theme prop 的 100% 高度 CodeMirror，
 *   并启用行号、折叠 gutter、当前行高亮、括号匹配和搜索快捷键；内容变化时只把最新字符串回传给 onChange。
 */
export function WorkbenchCodeEditor({
  value,
  language,
  readOnly = false,
  onChange,
}: WorkbenchCodeEditorProps): ReactElement {
  const extensions = useMemo(
    () => [...getWorkbenchCodeEditorLanguageExtensions(language), WORKBENCH_ONE_DARK_PRO_SYNTAX_EXTENSION],
    [language],
  );
  const handleChange = useCallback(
    (next: string) => {
      onChange(next);
    },
    [onChange],
  );

  return (
    <div className={styles.editorShell}>
      <CodeMirror
        value={value}
        height="100%"
        editable={!readOnly}
        readOnly={readOnly}
        extensions={extensions}
        theme={WORKBENCH_CODE_EDITOR_THEME}
        onChange={handleChange}
        basicSetup={WORKBENCH_CODE_EDITOR_BASIC_SETUP}
      />
    </div>
  );
}
