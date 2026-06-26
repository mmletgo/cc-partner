import {
  collectTabsForPath,
  detectWorkbenchFileType,
  dirtyTabNames,
  dropExpandedPathTree,
  dropPathTreeEntries,
  fileCapabilitiesForType,
  formatJsonText,
  formatTomlText,
  formatYamlText,
  isLatestRequest,
  isSameOrDescendantPath,
  parseWorkbenchDirRequestKey,
  reduceFileTabs,
  validateJsonText,
  validateTomlText,
  validateYamlText,
  workbenchDirRequestKey,
  workbenchDirRequestKeyMatchesPath,
} from './workbenchFiles';
// @ts-expect-error - 本仓库 tsconfig 未在 compilerOptions.types 纳入 node,node:process 类型缺失,运行时 tsx 正常
import { exit } from 'node:process';

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 文件工作区 helper 采用轻量脚本测试，需要在没有测试框架时也能快速表达失败原因。
 *
 * Code Logic（这个函数做什么）:
 *   接收布尔条件和失败消息；条件为 false 时抛出 Error 让 tsx 进程以失败状态退出。
 */
function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   文件工作区第一版需要统一覆盖文件类型识别、能力映射、tab 状态切换和保存前校验。
 *
 * Code Logic（这个函数做什么）:
 *   顺序调用 Workbench 文件 helper，断言关键行为符合设计规格。
 */
async function main(): Promise<void> {
  assert(detectWorkbenchFileType('README.md', null) === 'markdown', 'markdown extension detected');
  assert(detectWorkbenchFileType('README.mdx', null) === 'markdown', 'mdx extension detected as markdown');
  assert(detectWorkbenchFileType('Makefile', null) === 'code', 'Makefile detected as code');
  assert(detectWorkbenchFileType('Dockerfile', null) === 'code', 'Dockerfile detected as code');
  assert(detectWorkbenchFileType('README', null) === 'text', 'README without extension detected as text');
  assert(detectWorkbenchFileType('LICENSE', null) === 'text', 'LICENSE without extension detected as text');
  assert(detectWorkbenchFileType('.npmrc', null) === 'text', '.npmrc detected as text');
  assert(detectWorkbenchFileType('src/App.tsx', null) === 'code', 'tsx extension detected as code');
  assert(detectWorkbenchFileType('src/index.mjs', null) === 'code', 'mjs extension detected as code');
  assert(detectWorkbenchFileType('index.html', null) === 'html', 'html extension detected');
  assert(detectWorkbenchFileType('partial.htm', null) === 'html', 'htm extension detected');
  assert(detectWorkbenchFileType('data.csv', null) === 'csv', 'csv extension detected');
  assert(detectWorkbenchFileType('config.toml', null) === 'toml', 'toml extension detected');
  assert(detectWorkbenchFileType('config.yaml', null) === 'yaml', 'yaml extension detected');
  assert(detectWorkbenchFileType('workflow.yml', null) === 'yaml', 'yml extension detected');
  assert(detectWorkbenchFileType('data.sqlite', null) === 'sqlite', 'sqlite extension detected');
  assert(detectWorkbenchFileType('logo.png', null) === 'image', 'png extension detected');
  assert(detectWorkbenchFileType('scan.tiff', null) === 'image', 'tiff extension detected');
  assert(detectWorkbenchFileType('settings.jsonc', null) === 'unsupported', 'jsonc is not treated as strict json');
  assert(
    detectWorkbenchFileType('settings.jsonc', 'application/json') === 'unsupported',
    'jsonc extension overrides json mime fallback',
  );

  const jsonCaps = fileCapabilitiesForType('json');
  assert(jsonCaps.canEdit, 'json is editable');
  assert(jsonCaps.canFormat, 'json can format');
  assert(jsonCaps.mustValidateBeforeSave, 'json validates before save');

  const yamlCaps = fileCapabilitiesForType('yaml');
  assert(yamlCaps.canPreview, 'yaml can preview');
  assert(yamlCaps.canEdit, 'yaml is editable');
  assert(yamlCaps.canFormat, 'yaml can format');
  assert(yamlCaps.mustValidateBeforeSave, 'yaml validates before save');

  const markdownCaps = fileCapabilitiesForType('markdown');
  assert(markdownCaps.availableModes.includes('source'), 'markdown exposes source mode');
  assert(markdownCaps.availableModes.includes('wysiwyg'), 'markdown exposes wysiwyg mode');
  assert(markdownCaps.availableModes.includes('split'), 'markdown exposes split mode');

  const htmlCaps = fileCapabilitiesForType('html');
  assert(htmlCaps.canPreview, 'html can preview');
  assert(htmlCaps.canEdit, 'html is editable');
  assert(!htmlCaps.canFormat, 'html does not format');
  assert(htmlCaps.availableModes.includes('source'), 'html exposes source mode');
  assert(htmlCaps.availableModes.includes('wysiwyg'), 'html exposes preview mode via wysiwyg');
  assert(htmlCaps.availableModes.includes('split'), 'html exposes split mode');

  const codeCaps = fileCapabilitiesForType('code');
  assert(!codeCaps.availableModes.includes('wysiwyg'), 'code does not expose wysiwyg mode');

  const csvCaps = fileCapabilitiesForType('csv');
  assert(!csvCaps.canEdit, 'csv is not editable');
  assert(csvCaps.canPreview, 'csv can preview');
  assert(!csvCaps.availableModes.includes('editor'), 'csv does not expose editor mode');

  const tabs = reduceFileTabs(
    { tabs: [], activeTabId: null, view: 'terminal' },
    {
      type: 'opened',
      tab: {
        id: 'readme',
        path: 'README.md',
        name: 'README.md',
        detectedType: 'markdown',
        mode: 'wysiwyg',
        dirty: false,
        savedVersion: 'v1',
      },
    },
  );
  assert(tabs.activeTabId === 'readme', 'opened tab becomes active');
  assert(tabs.view === 'files', 'opening file switches to files view');

  const dirtyTabs = reduceFileTabs(tabs, { type: 'dirtyChanged', id: 'readme', dirty: true });
  const sourceTabs = reduceFileTabs(dirtyTabs, { type: 'modeChanged', id: 'readme', mode: 'source' });
  assert(sourceTabs.tabs[0]?.dirty === true, 'dirty action marks tab dirty');
  assert(sourceTabs.tabs[0]?.mode === 'source', 'mode action updates tab mode');

  const reopenedTabs = reduceFileTabs(sourceTabs, {
    type: 'opened',
    tab: {
      id: 'readme',
      path: 'docs/README.md',
      name: 'README copy.md',
      detectedType: 'markdown',
      mode: 'wysiwyg',
      dirty: false,
      savedVersion: 'v2',
    },
  });
  assert(reopenedTabs.tabs.length === 1, 'reopening existing tab keeps one tab');
  assert(reopenedTabs.tabs[0]?.dirty === true, 'reopening existing tab preserves dirty state');
  assert(reopenedTabs.tabs[0]?.mode === 'source', 'reopening existing tab preserves mode');
  assert(reopenedTabs.tabs[0]?.path === 'docs/README.md', 'reopening existing tab refreshes path metadata');

  const savedTabs = reduceFileTabs(reopenedTabs, { type: 'saved', id: 'readme', savedVersion: 'v3' });
  assert(savedTabs.tabs[0]?.dirty === false, 'saved action clears dirty state');
  assert(savedTabs.tabs[0]?.savedVersion === 'v3', 'saved action updates saved version');

  const secondTabs = reduceFileTabs(savedTabs, {
    type: 'opened',
    tab: {
      id: 'config',
      path: 'config.toml',
      name: 'config.toml',
      detectedType: 'toml',
      mode: 'editor',
      dirty: false,
      savedVersion: 'cfg1',
    },
  });
  const closedActiveTabs = reduceFileTabs(secondTabs, { type: 'closed', id: 'config' });
  assert(closedActiveTabs.activeTabId === 'readme', 'closing active tab activates previous tab');
  assert(closedActiveTabs.view === 'files', 'closing active tab keeps files view when tabs remain');

  assert(validateJsonText('{"ok":true}').ok, 'valid json accepted');
  assert(!validateJsonText('{bad').ok, 'invalid json rejected');
  assert(validateTomlText('title = "cc-partner"').ok, 'valid toml accepted');
  assert(!validateTomlText('title = ').ok, 'invalid toml rejected');
  assert(validateYamlText('name: cc-partner\nitems:\n  - workbench\n').ok, 'valid yaml accepted');
  assert(!validateYamlText('name: [').ok, 'invalid yaml rejected');

  const formattedJson = formatJsonText('{"ok":true}');
  assert(formattedJson.ok && formattedJson.text === '{\n  "ok": true\n}\n', 'valid json formatted');
  assert(!formatJsonText('{bad').ok, 'invalid json format rejected');

  const formattedToml = formatTomlText('title = "cc-partner"');
  assert(
    formattedToml.ok && formattedToml.text !== null && formattedToml.text.includes('title = "cc-partner"'),
    'valid toml formatted',
  );
  assert(!formatTomlText('title = ').ok, 'invalid toml format rejected');

  const formattedYaml = formatYamlText('name: cc-partner\nitems:\n- yaml\n');
  assert(
    formattedYaml.endsWith('\n') && formattedYaml.includes('items:'),
    'valid yaml formatted with trailing newline',
  );
  let yamlFormatFailed = false;
  try {
    formatYamlText('name: [');
  } catch {
    yamlFormatFailed = true;
  }
  assert(yamlFormatFailed, 'invalid yaml format rejected');

  const mutableCaps = fileCapabilitiesForType('markdown');
  mutableCaps.availableModes.push('viewer');
  assert(!fileCapabilitiesForType('markdown').availableModes.includes('viewer'), 'capabilities return copied modes');

  const openedTabs = [
    { id: 'readme', path: 'README.md', name: 'README.md', dirty: true },
    { id: 'src-main', path: 'src/main.ts', name: 'main.ts', dirty: false },
    { id: 'src-app', path: 'src/App.tsx', name: 'App.tsx', dirty: true },
    { id: 'docs', path: 'docs/guide.md', name: 'guide.md', dirty: false },
  ];
  assert(
    collectTabsForPath(openedTabs, 'README.md', 'file').map((tab) => tab.id).join(',') === 'readme',
    'file path collection only matches the exact file tab',
  );
  assert(
    collectTabsForPath(openedTabs, 'src', 'dir').map((tab) => tab.id).join(',') === 'src-main,src-app',
    'directory path collection includes descendant file tabs',
  );
  assert(
    dirtyTabNames(collectTabsForPath(openedTabs, 'src', 'dir')).join(',') === 'App.tsx',
    'dirty tab names only include dirty affected tabs',
  );

  const mainRootKey = workbenchDirRequestKey('project-1', null, '');
  const worktreeRootKey = workbenchDirRequestKey('project-1', 'wt-1', '');
  const worktreeSrcKey = workbenchDirRequestKey('project-1', 'wt-1', 'src');
  assert(mainRootKey !== worktreeRootKey, 'dir request key separates main workspace from worktree');
  assert(worktreeRootKey !== worktreeSrcKey, 'dir request key separates paths');
  assert(parseWorkbenchDirRequestKey(worktreeSrcKey)?.path === 'src', 'dir request key parses path');
  assert(parseWorkbenchDirRequestKey('not-json') === null, 'invalid dir request key is ignored');
  assert(isSameOrDescendantPath('src/App.tsx', 'src'), 'descendant path matches parent directory');
  assert(!isSameOrDescendantPath('src-old/App.tsx', 'src'), 'sibling prefix does not match parent directory');
  assert(
    workbenchDirRequestKeyMatchesPath(workbenchDirRequestKey('project-1', 'wt-1', 'src/components'), 'project-1', 'wt-1', 'src'),
    'dir request key matcher includes descendant paths',
  );
  assert(
    !workbenchDirRequestKeyMatchesPath(workbenchDirRequestKey('project-1', 'wt-2', 'src/components'), 'project-1', 'wt-1', 'src'),
    'dir request key matcher isolates worktrees',
  );
  const cachedChildren = {
    src: ['main.ts'],
    'src/components': ['Button.tsx'],
    docs: ['guide.md'],
  };
  const droppedChildren = dropPathTreeEntries(cachedChildren, 'src');
  assert(!('src' in droppedChildren), 'dropping path tree removes exact directory cache');
  assert(!('src/components' in droppedChildren), 'dropping path tree removes descendant directory cache');
  assert('docs' in droppedChildren, 'dropping path tree keeps unrelated directory cache');
  const expanded = dropExpandedPathTree(new Set(['src', 'src/components', 'docs']), 'src');
  assert(!expanded.has('src') && !expanded.has('src/components'), 'dropping expanded path tree removes subtree');
  assert(expanded.has('docs'), 'dropping expanded path tree keeps unrelated expanded path');
  assert(!isLatestRequest(2, 1), 'older request seq is not latest');
  assert(isLatestRequest(2, 2), 'matching request seq is latest');
}

void main()
  .then(() => {
    exit(0);
  })
  .catch((error: unknown) => {
    console.error(error);
    exit(1);
  });
