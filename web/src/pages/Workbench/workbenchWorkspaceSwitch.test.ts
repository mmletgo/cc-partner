import { describe, test } from 'vitest';
import { readFileSync } from 'node:fs';

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 终端和文件预览应提供对称入口，避免用户从终端回到已打开文件时只能重新点击右侧文件树。
 *
 * Code Logic（这个函数做什么）:
 *   读取源码或 locale 文本并断言包含指定片段；缺失时抛出带上下文的错误。
 */
function assertContains(source: string, expected: string, message: string): void {
  if (!source.includes(expected)) {
    throw new Error(message);
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 文件预览头部需要避免重复信息占用高度，测试要能明确阻止第二行 toolbar 回归。
 *
 * Code Logic（这个函数做什么）:
 *   检查源码不包含指定片段；如果仍包含则抛出带上下文的错误。
 */
function assertNotContains(source: string, unexpected: string, message: string): void {
  if (source.includes(unexpected)) {
    throw new Error(message);
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   终端和文件预览导航栏必须复用同一个布局组件，测试需要明确锁住复用次数，避免后续又分叉成多套 UI。
 *
 * Code Logic（这个函数做什么）:
 *   统计源码里指定片段出现次数；次数不符合预期时抛出带上下文的错误。
 */
function assertOccurrenceCount(source: string, expected: string, count: number, message: string): void {
  const actual = source.split(expected).length - 1;

  if (actual !== count) {
    throw new Error(`${message}: expected ${count}, got ${actual}`);
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   终端 action 组的按钮顺序会影响用户肌肉记忆，文件预览入口需要固定在最右侧。
 *
 * Code Logic（这个函数做什么）:
 *   在源码中查找两个片段的首次位置，并断言前者出现在后者之前；缺失或顺序错误时抛出错误。
 */
function assertSubstringOrder(source: string, before: string, after: string, message: string): void {
  const beforeIndex = source.indexOf(before);
  const afterIndex = source.indexOf(after);

  if (beforeIndex < 0 || afterIndex < 0 || beforeIndex >= afterIndex) {
    throw new Error(message);
  }
}

/**
 * Business Logic（为什么需要这个测试）:
 *   终端和文件预览导航栏需要复用同一个布局组件；工具栏按作用域重新划分后：
 *   浏览器预览/文件工作区/Agent Ledger 入口位于工作区标题行（终端全屏时隐藏），
 *   终端工具栏保留会话搜索/Prompt 工具/适应尺寸/窗格菜单/全屏，
 *   窗格四操作（分屏右/下、切换、关闭）收敛进 WorkbenchPaneTools 菜单。
 *
 * Code Logic（这个测试做什么）:
 *   静态读取 Workbench 页面、窗格菜单、文件工作区、共享导航组件和 workbench i18n 资源，
 *   检查切换回调、按钮绑定、禁用条件、文案 key、作用域分层顺序、
 *   共享导航样式以及无第二行 toolbar 的布局契约。
 */
describe('workbenchWorkspaceSwitch', () => {
  test('locks terminal/files nav reuse, ordering, file path tab labels and shared nav styles', async () => {
  const workbenchSource = readFileSync(new URL('./Workbench.tsx', import.meta.url), 'utf8');
  const promptToolsSource = readFileSync(new URL('./WorkbenchPromptTools.tsx', import.meta.url), 'utf8');
  const paneToolsSource = readFileSync(
    new URL('../../components/domain/WorkbenchPaneTools/WorkbenchPaneTools.tsx', import.meta.url),
    'utf8',
  );
  const fileWorkspaceSource = readFileSync(
    new URL('../../components/domain/WorkbenchFileWorkspace/WorkbenchFileWorkspace.tsx', import.meta.url),
    'utf8',
  );
  const htmlPreviewSource = readFileSync(
    new URL('../../components/domain/WorkbenchHtmlPreview/WorkbenchHtmlPreview.tsx', import.meta.url),
    'utf8',
  );
  const markdownEditorSource = readFileSync(
    new URL('../../components/domain/WorkbenchMarkdownEditor/WorkbenchMarkdownEditor.tsx', import.meta.url),
    'utf8',
  );
  const workspaceNavSource = readFileSync(
    new URL('../../components/layout/WorkbenchWorkspaceNav/WorkbenchWorkspaceNav.tsx', import.meta.url),
    'utf8',
  );
  const workspaceNavStyles = readFileSync(
    new URL('../../components/layout/WorkbenchWorkspaceNav/WorkbenchWorkspaceNav.module.css', import.meta.url),
    'utf8',
  );
  const zhLocale = readFileSync(new URL('../../i18n/locales/zh/workbench.json', import.meta.url), 'utf8');
  const enLocale = readFileSync(new URL('../../i18n/locales/en/workbench.json', import.meta.url), 'utf8');

  assertContains(workbenchSource, 'const handleReturnToFiles = useCallback', 'terminal -> files callback exists');
  assertContains(workbenchSource, "setWorkspaceView('files');", 'callback opens file workspace layer');
  assertContains(workbenchSource, 'disabled={fileTabs.length === 0}', 'file preview button is disabled with no opened tabs');
  assertContains(workbenchSource, 'className={styles.terminalActionButton}', 'terminal action buttons use text style class');
  assertContains(workbenchSource, "t('workbench:fileWorkspace.openFiles')", 'button uses localized file preview label');
  // 工作区级入口上移到标题行：浏览器预览 → 文件工作区 → Agent Ledger → 快照。
  assertContains(
    workbenchSource,
    "t('workbench:browserPreview.openWorkspace')",
    'workspace header exposes browser preview entry',
  );
  assertSubstringOrder(
    workbenchSource,
    "title={t('workbench:browserPreview.openWorkspace')}",
    "title={t('workbench:fileWorkspace.openFiles')}",
    'browser preview entry precedes file workspace entry in workspace header actions',
  );
  assertSubstringOrder(
    workbenchSource,
    "title={t('workbench:fileWorkspace.openFiles')}",
    '<AgentLedgerWorkbenchChrome',
    'file workspace entry precedes agent ledger in workspace header actions',
  );
  assertSubstringOrder(
    workbenchSource,
    '<AgentLedgerWorkbenchChrome',
    "t('workbench:workspaceSnapshot.openButton')",
    'agent ledger entry precedes workspace snapshot button',
  );
  assertContains(
    workbenchSource,
    "title={terminalFullscreenLabel}",
    'terminal fullscreen action renders in pane navigation actions',
  );
  assertNotContains(
    workbenchSource,
    'terminalFullscreen ? null :',
    'terminal fullscreen must keep terminal window tabs available for switching',
  );
  // 终端工具栏只剩：会话搜索 → Prompt 工具 → 适应尺寸 → 窗格菜单 → 全屏。
  assertSubstringOrder(
    workbenchSource,
    "title={t('workbench:sessionSearch.open')}",
    '<WorkbenchPromptTools',
    'session search precedes prompt tools in terminal actions',
  );
  assertSubstringOrder(
    workbenchSource,
    '<WorkbenchPromptTools',
    "title={t('workbench:fitTerminalSize')}",
    'prompt tools precede fit-size action in terminal actions',
  );
  assertSubstringOrder(
    workbenchSource,
    "title={t('workbench:fitTerminalSize')}",
    '<WorkbenchPaneTools',
    'fit-size action precedes pane tools menu in terminal actions',
  );
  assertSubstringOrder(
    workbenchSource,
    '<WorkbenchPaneTools',
    "title={terminalFullscreenLabel}",
    'pane tools menu precedes terminal fullscreen action',
  );
  assertNotContains(
    workbenchSource,
    "title={t('workbench:splitPaneRight')}",
    'pane split actions live in WorkbenchPaneTools, not the terminal toolbar',
  );
  assertContains(workbenchSource, "actionsAriaLabel={t('workbench:paneActions')}", 'terminal action group keeps aria label');
  assertContains(
    workbenchSource,
    '<WorkbenchSessionTabs',
    'terminal session tabs are delegated to WorkbenchSessionTabs',
  );
  assertNotContains(
    workbenchSource,
    '<button\n                      key={session.id}\n                      type="button"\n                      className={styles.sessionTab}',
    'terminal session tab shell must not be an inline button with nested close control',
  );
  assertOccurrenceCount(workbenchSource, '<WorkbenchWorkspaceNav', 1, 'Workbench terminal workspace uses shared nav once');
  assertNotContains(
    workbenchSource,
    "t('workbench:automationWorkspace.open')",
    'terminal toolbar no longer includes automation entry',
  );
  assertOccurrenceCount(fileWorkspaceSource, '<WorkbenchWorkspaceNav', 1, 'file workspace uses shared nav once');
  assertContains(
    fileWorkspaceSource,
    "actionsAriaLabel={t('workbench:fileWorkspace.actions')}",
    'file action group keeps aria label',
  );
  assertContains(
    workspaceNavSource,
    'export function WorkbenchWorkspaceNav',
    'shared workspace nav component exists',
  );
  assertContains(
    workspaceNavSource,
    '<section className={styles.nav} aria-label={ariaLabel}>',
    'shared workspace nav owns the outer navigation row',
  );
  assertContains(
    workspaceNavSource,
    "role={actionsAriaLabel ? 'group' : undefined}",
    'shared nav labels actions group',
  );
  assertContains(workspaceNavStyles, 'min-height: 64px;', 'shared nav matches terminal nav height');
  assertContains(
    workspaceNavStyles,
    'padding: var(--space-4) var(--space-6);',
    'shared nav matches terminal nav padding',
  );
  assertContains(
    workspaceNavStyles,
    'container: workbench-workspace-nav / inline-size;',
    'shared nav measures its own available width for responsive actions',
  );
  assertContains(
    workspaceNavStyles,
    '@container workbench-workspace-nav (max-width: 1280px)',
    'shared nav collapses action labels when its own width is constrained',
  );
  assertContains(
    workspaceNavStyles,
    "[data-workbench-responsive-label='true']",
    'shared nav hides only responsive labels while preserving button icons',
  );
  assertOccurrenceCount(
    workbenchSource,
    'data-workbench-responsive-action="true"',
    5,
    'terminal toolbar + workspace header actions stay responsive; pane actions moved to WorkbenchPaneTools',
  );
  assertOccurrenceCount(
    promptToolsSource,
    'data-workbench-responsive-action="true"',
    2,
    'WorkbenchPromptTools renders Prompt optimizer + favorite quick input actions side by side',
  );
  assertOccurrenceCount(
    paneToolsSource,
    'data-workbench-responsive-action="true"',
    1,
    'WorkbenchPaneTools renders a single responsive trigger for the pane menu',
  );
  assertContains(
    paneToolsSource,
    "t('workbench:switchPane')",
    'pane tools menu exposes switch-pane action',
  );
  assertSubstringOrder(
    paneToolsSource,
    "t('workbench:splitPaneRight')",
    "t('workbench:splitPaneDown')",
    'split right precedes split down in pane menu',
  );
  assertSubstringOrder(
    paneToolsSource,
    "t('workbench:splitPaneDown')",
    "t('workbench:switchPane')",
    'switch pane follows split-down in pane menu',
  );
  assertSubstringOrder(
    paneToolsSource,
    "t('workbench:switchPane')",
    "t('workbench:closePane')",
    'switch pane precedes close pane in pane menu',
  );
  assertOccurrenceCount(
    fileWorkspaceSource,
    'data-workbench-responsive-action="true"',
    3,
    'file workspace marks every toolbar action as responsive',
  );
  assertContains(
    fileWorkspaceSource,
    '<span className={styles.tabName}>{tab.path}</span>',
    'file tab label renders full relative path',
  );
  assertContains(
    fileWorkspaceSource,
    '<div className={styles.toolbarActions}>',
    'file actions render in the tab header row',
  );
  assertContains(
    fileWorkspaceSource,
    "from '../WorkbenchHtmlPreview';",
    'HTML preview component is imported by file workspace',
  );
  assertContains(
    fileWorkspaceSource,
    "case 'html':",
    'file workspace dispatches HTML files to the HTML preview component',
  );
  assertContains(
    fileWorkspaceSource,
    '<WorkbenchHtmlPreview',
    'file workspace renders the HTML preview component',
  );
  assertContains(
    htmlPreviewSource,
    'role="group"',
    'HTML preview mode switch uses segmented control group semantics',
  );
  assertContains(
    htmlPreviewSource,
    'aria-pressed={option.mode === mode}',
    'HTML preview mode buttons expose pressed state',
  );
  assertContains(
    htmlPreviewSource,
    'sandbox=""',
    'HTML preview iframe stays fully sandboxed',
  );
  assertContains(
    htmlPreviewSource,
    'srcDoc={iframeSrcDoc}',
    'HTML preview iframe renders rewritten HTML source',
  );
  assertContains(
    htmlPreviewSource,
    'previewResult?.source === value',
    'HTML preview ignores stale async asset rewrite results',
  );
  assertContains(
    fileWorkspaceSource,
    'loadAsset={loadHtmlAsset}',
    'file workspace passes HTML asset loader into preview component',
  );
  assertContains(
    fileWorkspaceSource,
    'documentPath={activeTab.path}\n              mode={coerceMarkdownMode(activeTab.mode)}',
    'file workspace passes shared asset loader into Markdown editor',
  );
  assertContains(
    markdownEditorSource,
    "from '@tiptap/extension-image'",
    'Markdown editor registers Tiptap image extension',
  );
  assertContains(
    markdownEditorSource,
    "src: ''",
    'Markdown editor prevents raw image src loading before asset rewrite',
  );
  assertNotContains(
    fileWorkspaceSource,
    '<div className={styles.fileToolbar}>',
    'file preview does not render a second toolbar row',
  );
  assertNotContains(
    fileWorkspaceSource,
    'className={styles.fileTitleBlock}',
    'file preview no longer renders a separate title block below tabs',
  );
  assertNotContains(
    fileWorkspaceSource,
    'className={styles.filePath}',
    'file path is no longer duplicated below tabs',
  );
  assertNotContains(
    fileWorkspaceSource,
    '<dl className={styles.fileMeta}>',
    'file preview toolbar does not render a separate metadata row',
  );
  assertNotContains(
    fileWorkspaceSource,
    "t('workbench:fileWorkspace.type')",
    'file preview toolbar no longer shows detected type',
  );
  assertContains(zhLocale, '"actions": "文件操作"', 'zh file actions label exists');
  assertContains(zhLocale, '"openFiles": "文件预览"', 'zh file preview label exists');
  assertContains(zhLocale, '"htmlPreview"', 'zh HTML preview copy exists');
  assertContains(enLocale, '"actions": "File actions"', 'en file actions label exists');
  assertContains(enLocale, '"openFiles": "File preview"', 'en file preview label exists');
  assertContains(enLocale, '"htmlPreview"', 'en HTML preview copy exists');
  });
});
