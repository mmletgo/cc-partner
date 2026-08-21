import { describe, test } from 'vitest';
import { readFileSync } from 'node:fs';

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 三工作区切换、共享 nav 复用、文件/浏览器/HTML/Markdown 预览控件都需要在静态层面
 *   锁住 contract；任何回归必须立刻在 CI 暴露。
 *
 * Code Logic（这个函数做什么）:
 *   读取源码或 locale 文本并断言包含 / 不包含指定片段；缺失时抛出带上下文的错误。
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
 *   Workbench 工作台三工作区（终端 / 网页浏览 / 文件浏览）由标题行的 WorkbenchWorkspaceSwitch 统一
 *   承担切换；终端/文件/浏览器工作区不再自带"返回终端"按钮。终端工具栏保留会话搜索 / Prompt 工具
 *   / 适应尺寸 / 窗格菜单 / 全屏，窗格四操作收敛进 WorkbenchPaneTools 菜单。文件工作区、浏览器
 *   工作区、HTML/Markdown 编辑器仍共享 WorkbenchWorkspaceNav 与既有 toolbar 布局。
 *
 * Code Logic（这个测试做什么）:
 *   静态读取 Workbench 页面、窗格菜单、文件工作区、共享导航组件、WorkbenchWorkspaceSwitch 组件
 *   和 workbench i18n 资源，检查切换组件渲染、按钮绑定、禁用条件、文案 key、作用域分层顺序、
 *   共享导航样式以及无第二行 toolbar 的布局契约。
 */
describe('workbenchWorkspaceSwitch', () => {
  test('locks workspace switch reuse, file path tab labels and shared nav styles', async () => {
  const workbenchSource = readFileSync(new URL('./Workbench.tsx', import.meta.url), 'utf8');
  const workspaceSwitchSlotSource = readFileSync(
    new URL('./views/WorkbenchWorkspaceSwitchSlot.tsx', import.meta.url),
    'utf8',
  );
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
  const workspaceSwitchSource = readFileSync(
    new URL('../../components/domain/WorkbenchWorkspaceSwitch/WorkbenchWorkspaceSwitch.tsx', import.meta.url),
    'utf8',
  );
  const zhLocale = readFileSync(new URL('../../i18n/locales/zh/workbench.json', import.meta.url), 'utf8');
  const enLocale = readFileSync(new URL('../../i18n/locales/en/workbench.json', import.meta.url), 'utf8');

  // === 三元切换组件 ===
  assertContains(
    workbenchSource,
    '<WorkbenchWorkspaceSwitchSlot',
    'workspace header renders WorkbenchWorkspaceSwitchSlot',
  );
  assertContains(
    workbenchSource,
    "value={workspaceView}",
    'switch is bound to workspaceView',
  );
  assertContains(
    workbenchSource,
    'onChange={handleWorkspaceViewChange}',
    'switch uses handleWorkspaceViewChange so files can open empty and flip inspector',
  );
  assertContains(
    workbenchSource,
    "setInspectorTab('files')",
    'switching to files also selects the project-folder inspector tab',
  );
  assertContains(
    workspaceSwitchSlotSource,
    "id: 'terminal'",
    'switch exposes terminal option',
  );
  assertContains(
    workspaceSwitchSlotSource,
    "id: 'browser'",
    'switch exposes browser option',
  );
  assertContains(
    workspaceSwitchSlotSource,
    "id: 'files'",
    'switch exposes files option',
  );
  assertContains(
    workspaceSwitchSlotSource,
    "label: t('workbench:workspaceSwitch.terminal')",
    'switch terminal option uses workspaceSwitch.terminal i18n',
  );
  assertContains(
    workspaceSwitchSlotSource,
    "label: t('workbench:browserPreview.openWorkspace')",
    'switch browser option uses browserPreview.openWorkspace i18n',
  );
  assertContains(
    workspaceSwitchSlotSource,
    "label: t('workbench:fileWorkspace.openFiles')",
    'switch files option uses fileWorkspace.openFiles i18n',
  );
  assertContains(
    workspaceSwitchSlotSource,
    'disabled: !canOpenBrowser',
    'browser option is disabled when no project/worktree',
  );
  assertNotContains(
    workbenchSource,
    'disabled: fileTabs.length === 0',
    'files option stays enabled even when no file tabs are open',
  );
  // 标题行不再用两个独立 Button 触发工作区切换
  assertNotContains(
    workbenchSource,
    "title={t('workbench:browserPreview.openWorkspace')}",
    'browser preview button is no longer a standalone Button',
  );
  assertNotContains(
    workbenchSource,
    "title={t('workbench:fileWorkspace.openFiles')}",
    'file workspace button is no longer a standalone Button',
  );
  assertNotContains(
    workbenchSource,
    "const handleReturnToFiles = useCallback",
    'handleReturnToFiles is replaced by the switch',
  );
  assertNotContains(
    workbenchSource,
    "const handleReturnToTerminal = useCallback",
    'handleReturnToTerminal is removed (no return-to-terminal button left)',
  );
  // === Switch 组件 a11y + 结构 ===
  assertContains(
    workspaceSwitchSource,
    'role="radiogroup"',
    'switch renders a radiogroup',
  );
  assertContains(
    workspaceSwitchSource,
    "role=\"radio\"",
    'switch options render as radio buttons',
  );
  assertContains(
    workspaceSwitchSource,
    'aria-checked',
    'switch exposes checked state to assistive tech',
  );
  assertContains(
    workspaceSwitchSource,
    'aria-disabled',
    'switch exposes disabled state to assistive tech',
  );
  assertContains(
    workspaceSwitchSource,
    "data-workbench-responsive-label=\"true\"",
    'switch options honor the responsive label contract',
  );
  assertContains(
    workspaceSwitchSource,
    'ArrowLeft',
    'switch supports keyboard navigation',
  );
  // === Locale ===
  assertContains(zhLocale, '"workspaceSwitch"', 'zh workspaceSwitch section exists');
  assertContains(zhLocale, '"terminal": "终端"', 'zh workspaceSwitch.terminal = 终端');
  assertContains(zhLocale, '"ariaLabel": "工作区切换"', 'zh workspaceSwitch.ariaLabel = 工作区切换');
  assertContains(zhLocale, '"openFiles": "文件浏览"', 'zh fileWorkspace.openFiles = 文件浏览');
  assertContains(zhLocale, '"empty": "还没有打开文件"', 'zh fileWorkspace.empty guides the blank page');
  assertContains(
    zhLocale,
    '"emptyHint": "点击右侧「项目文件夹」里的文件即可在此显示。"',
    'zh fileWorkspace.emptyHint points to the project folder',
  );
  assertContains(zhLocale, '"openWorkspace": "网页浏览"', 'zh browserPreview.openWorkspace = 网页浏览');
  assertNotContains(zhLocale, '"returnTerminal"', 'zh fileWorkspace.returnTerminal removed');
  assertContains(enLocale, '"workspaceSwitch"', 'en workspaceSwitch section exists');
  assertContains(enLocale, '"terminal": "Terminal"', 'en workspaceSwitch.terminal = Terminal');
  assertContains(enLocale, '"ariaLabel": "Workspace switch"', 'en workspaceSwitch.ariaLabel = Workspace switch');
  assertContains(enLocale, '"openFiles": "File browsing"', 'en fileWorkspace.openFiles = File browsing');
  assertContains(enLocale, '"empty": "No file is open"', 'en fileWorkspace.empty guides the blank page');
  assertContains(
    enLocale,
    '"emptyHint": "Click a file in the Project folder on the right to show it here."',
    'en fileWorkspace.emptyHint points to the project folder',
  );
  assertContains(enLocale, '"openWorkspace": "Web browsing"', 'en browserPreview.openWorkspace = Web browsing');
  assertNotContains(enLocale, '"returnTerminal"', 'en fileWorkspace.returnTerminal removed');

  // === 文件/浏览器/HTML/Markdown 既有 contract（保持不变）===
  assertContains(workbenchSource, "title={terminalFullscreenLabel}", 'terminal fullscreen action renders in pane navigation actions');
  assertNotContains(workbenchSource, 'terminalFullscreen ? null :', 'terminal fullscreen must keep terminal window tabs available for switching');
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
  assertContains(workbenchSource, '<WorkbenchSessionTabs', 'terminal session tabs are delegated to WorkbenchSessionTabs');
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
  // workspace header actions: switch + agent ledger + 快照 + 项目自动化；终端全屏时 agent ledger 触发按钮与快照按钮仍隐藏
  assertContains(
    workspaceSwitchSource,
    'data-workbench-responsive-action="true"',
    'switch honors responsive action contract',
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
  assertContains(paneToolsSource, "t('workbench:switchPane')", 'pane tools menu exposes switch-pane action');
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
  // file workspace: format + save + (return terminal 已移除)
  assertOccurrenceCount(
    fileWorkspaceSource,
    'data-workbench-responsive-action="true"',
    2,
    'file workspace marks remaining toolbar actions as responsive',
  );
  assertContains(
    fileWorkspaceSource,
    'data-testid="workbench-file-workspace-empty"',
    'file workspace empty page is addressable',
  );
  assertContains(
    fileWorkspaceSource,
    "t('workbench:fileWorkspace.emptyHint')",
    'file workspace empty page tells users to open files from the inspector',
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
  assertNotContains(
    fileWorkspaceSource,
    "t('workbench:fileWorkspace.returnTerminal')",
    'file workspace no longer renders a return-to-terminal button',
  );
  assertContains(fileWorkspaceSource, "from '../WorkbenchHtmlPreview';", 'HTML preview component is imported by file workspace');
  assertContains(fileWorkspaceSource, "case 'html':", 'file workspace dispatches HTML files to the HTML preview component');
  assertContains(fileWorkspaceSource, '<WorkbenchHtmlPreview', 'file workspace renders the HTML preview component');
  assertContains(htmlPreviewSource, 'role="group"', 'HTML preview mode switch uses segmented control group semantics');
  assertContains(htmlPreviewSource, 'aria-pressed={option.mode === mode}', 'HTML preview mode buttons expose pressed state');
  assertContains(htmlPreviewSource, 'sandbox=""', 'HTML preview iframe stays fully sandboxed');
  assertContains(htmlPreviewSource, 'srcDoc={iframeSrcDoc}', 'HTML preview iframe renders rewritten HTML source');
  assertContains(htmlPreviewSource, 'previewResult?.source === value', 'HTML preview ignores stale async asset rewrite results');
  assertContains(fileWorkspaceSource, 'loadAsset={loadHtmlAsset}', 'file workspace passes HTML asset loader into preview component');
  assertContains(
    fileWorkspaceSource,
    "documentPath={activeTab.path}\n              mode={coerceMarkdownMode(activeTab.mode)}",
    'file workspace passes shared asset loader into Markdown editor',
  );
  assertContains(markdownEditorSource, "from '@tiptap/extension-image'", 'Markdown editor registers Tiptap image extension');
  assertContains(markdownEditorSource, "src: ''", 'Markdown editor prevents raw image src loading before asset rewrite');
  assertNotContains(fileWorkspaceSource, '<div className={styles.fileToolbar}>', 'file preview does not render a second toolbar row');
  assertNotContains(fileWorkspaceSource, 'className={styles.fileTitleBlock}', 'file preview no longer renders a separate title block below tabs');
  assertNotContains(fileWorkspaceSource, 'className={styles.filePath}', 'file path is no longer duplicated below tabs');
  assertNotContains(fileWorkspaceSource, '<dl className={styles.fileMeta}>', 'file preview toolbar does not render a separate metadata row');
  assertNotContains(fileWorkspaceSource, "t('workbench:fileWorkspace.type')", 'file preview toolbar no longer shows detected type');
  assertContains(zhLocale, '"actions": "文件操作"', 'zh file actions label exists');
  assertContains(zhLocale, '"htmlPreview"', 'zh HTML preview copy exists');
  assertContains(enLocale, '"actions": "File actions"', 'en file actions label exists');
  assertContains(enLocale, '"htmlPreview"', 'en HTML preview copy exists');
  });
});