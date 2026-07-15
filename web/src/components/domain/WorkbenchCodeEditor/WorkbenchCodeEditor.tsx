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
 *   - 通过 loadWorkbenchLanguage 异步加载语言扩展；加载中/失败按纯文本可编辑渲染；request sequence 丢弃过期 Promise
 *   - 将 CodeMirror 的 onChange value 透传给调用方，由上层负责保存、脏状态和文件生命周期
 */

import CodeMirror from '@uiw/react-codemirror';
import type { BasicSetupOptions } from '@uiw/react-codemirror';
import type { Extension } from '@codemirror/state';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ReactElement } from 'react';
import { loadWorkbenchLanguage } from './workbenchCodeEditorLanguage';
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
 *   用 request sequence + loadWorkbenchLanguage 异步装入语言扩展；加载中/失败时 languageExtension=null
 *   （纯文本）但仍保持 theme 与 One Dark Pro syntax 扩展；渲染带自定义 theme prop 的 100% 高度
 *   CodeMirror，并启用行号、折叠 gutter、当前行高亮、括号匹配和搜索快捷键。
 */
/**
 * 已装载语言扩展快照：仅当 language 与当前请求语言一致时才应用，切换语言时派生为纯文本。
 */
interface LoadedLanguageExtension {
  language: string;
  extension: Extension | null;
}

export function WorkbenchCodeEditor({
  value,
  language,
  readOnly = false,
  onChange,
}: WorkbenchCodeEditorProps): ReactElement {
  const [loadedLanguage, setLoadedLanguage] = useState<LoadedLanguageExtension | null>(null);
  const languageRequestSeqRef = useRef(0);

  useEffect(() => {
    /**
     * Business Logic（为什么需要这个 effect）:
     *   文件切换会改变 language；语言包按需动态 import，完成后才能启用语法高亮。
     *
     * Code Logic（这个 effect 做什么）:
     *   request sequence 丢弃过期 Promise；成功/失败都写入 loadedLanguage。
     *   不在 effect 同步 setState(null)——language 与 loadedLanguage.language 不一致时
     *   下方派生 languageExtension=null，避免 set-state-in-effect 级联渲染。
     */
    const requestSeq = languageRequestSeqRef.current + 1;
    languageRequestSeqRef.current = requestSeq;

    void loadWorkbenchLanguage(language).then(
      (extension) => {
        if (languageRequestSeqRef.current !== requestSeq) {
          return;
        }
        setLoadedLanguage({ language, extension });
      },
      () => {
        // import 失败：保持纯文本可编辑，不阻塞文件查看
        if (languageRequestSeqRef.current !== requestSeq) {
          return;
        }
        setLoadedLanguage({ language, extension: null });
      },
    );
  }, [language]);

  // 切换语言瞬间（异步 loader 完成前）强制纯文本，避免错误高亮闪烁
  const languageExtension =
    loadedLanguage && loadedLanguage.language === language ? loadedLanguage.extension : null;

  const extensions = useMemo(
    () => [
      ...(languageExtension ? [languageExtension] : []),
      WORKBENCH_ONE_DARK_PRO_SYNTAX_EXTENSION,
    ],
    [languageExtension],
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
