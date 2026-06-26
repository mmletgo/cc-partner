// node:fs 类型由 @types/node 提供,但本仓库 tsconfig 未在 compilerOptions.types 显式纳入 node,
// tsx 测试上下文下类型缺失,故局部抑制(运行时 tsx 正常解析;node:fs 是 node 内置,无需安装)。
// @ts-expect-error - 本仓库 tsconfig 未在 compilerOptions.types 纳入 node,node:fs 类型缺失,运行时 tsx 正常
import { readFileSync } from 'node:fs';
// @ts-expect-error - 本仓库 tsconfig 未在 compilerOptions.types 纳入 node,node:process 类型缺失,运行时 tsx 正常
import { exit } from 'node:process';
import { getWorkbenchCodeEditorLanguageExtensions } from './workbenchCodeEditorLanguage';

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 代码编辑器需要稳定使用 One Dark Pro 语法色，同时背景、行号和当前行必须跟随应用主题。
 *
 * Code Logic（这个函数做什么）:
 *   检查源码是否包含 One Dark Pro 语法核心色值、CodeMirror theme/highlight 扩展和 design token 绑定。
 */
function assertContains(source: string, expected: string, message: string): void {
  if (!source.includes(expected)) {
    throw new Error(message);
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   CSS Modules 只负责编辑器尺寸和外框，CodeMirror 内部主题由显式 theme prop 接管，避免 @uiw 默认 light theme 把暗色界面刷成白底。
 *
 * Code Logic（这个函数做什么）:
 *   读取组件、theme 和 CSS 源码，断言主题通过 theme prop 注入，背景/gutter/active line 读取设计 token，
 *   同时 CSS 不再声明 `.cm-gutters`/`.cm-activeLine` 颜色覆盖。
 */
async function main(): Promise<void> {
  const editorSource = readFileSync(new URL('./WorkbenchCodeEditor.tsx', import.meta.url), 'utf8');
  const themeSource = readFileSync(new URL('./workbenchCodeEditorTheme.ts', import.meta.url), 'utf8');
  const cssSource = readFileSync(new URL('./WorkbenchCodeEditor.module.css', import.meta.url), 'utf8');

  assertContains(themeSource, "foreground: '#abb2bf'", 'One Dark Pro foreground is configured');
  assertContains(themeSource, "keyword: '#c678dd'", 'One Dark Pro keyword color is configured');
  assertContains(themeSource, "string: '#98c379'", 'One Dark Pro string color is configured');
  assertContains(themeSource, "number: '#d19a66'", 'One Dark Pro number color is configured');
  assertContains(themeSource, "property: '#e06c75'", 'One Dark Pro property color is configured');
  assertContains(themeSource, "function: '#61afef'", 'One Dark Pro function color is configured');
  assertContains(themeSource, "backgroundColor: 'var(--surface)'", 'CodeMirror editor background follows the app surface token');
  assertContains(themeSource, "backgroundColor: 'var(--bg)'", 'CodeMirror gutter background follows the app bg token');
  assertContains(themeSource, "color: 'var(--meta)'", 'CodeMirror gutter text follows the app meta token');
  assertContains(themeSource, "backgroundColor: 'var(--accent-soft)'", 'CodeMirror active line follows the app accent-soft token');
  assertContains(themeSource, 'syntaxHighlighting(WORKBENCH_ONE_DARK_PRO_HIGHLIGHT)', 'CodeMirror syntax highlighting extension is exported');
  assertContains(editorSource, 'theme={WORKBENCH_CODE_EDITOR_THEME}', 'Custom CodeMirror theme is passed through @uiw theme prop');
  assertContains(editorSource, 'WORKBENCH_ONE_DARK_PRO_SYNTAX_EXTENSION', 'One Dark Pro syntax extension is injected into the editor');

  if (getWorkbenchCodeEditorLanguageExtensions('yaml').length === 0) {
    throw new Error('YAML language extension should be registered');
  }

  if (cssSource.includes('.cm-gutters') || cssSource.includes('.cm-activeLine')) {
    throw new Error('CodeMirror internal color selectors should be owned by the One Dark Pro theme extension');
  }
}

void main()
  .then(() => {
    exit(0);
  })
  .catch((error: unknown) => {
    console.error(error);
    exit(1);
  });
