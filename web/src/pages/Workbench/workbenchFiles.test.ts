import {
  detectWorkbenchFileType,
  fileCapabilitiesForType,
  formatJsonText,
  formatTomlText,
  reduceFileTabs,
  validateJsonText,
  validateTomlText,
} from './workbenchFiles';

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
  assert(detectWorkbenchFileType('src/App.tsx', null) === 'code', 'tsx extension detected as code');
  assert(detectWorkbenchFileType('data.csv', null) === 'csv', 'csv extension detected');
  assert(detectWorkbenchFileType('config.toml', null) === 'toml', 'toml extension detected');
  assert(detectWorkbenchFileType('data.sqlite', null) === 'sqlite', 'sqlite extension detected');
  assert(detectWorkbenchFileType('logo.png', null) === 'image', 'png extension detected');
  assert(detectWorkbenchFileType('settings.jsonc', null) === 'unsupported', 'jsonc is not treated as strict json');

  const jsonCaps = fileCapabilitiesForType('json');
  assert(jsonCaps.canEdit, 'json is editable');
  assert(jsonCaps.canFormat, 'json can format');
  assert(jsonCaps.mustValidateBeforeSave, 'json validates before save');

  const markdownCaps = fileCapabilitiesForType('markdown');
  assert(markdownCaps.availableModes.includes('source'), 'markdown exposes source mode');
  assert(markdownCaps.availableModes.includes('wysiwyg'), 'markdown exposes wysiwyg mode');
  assert(markdownCaps.availableModes.includes('split'), 'markdown exposes split mode');

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

  const formattedJson = formatJsonText('{"ok":true}');
  assert(formattedJson.ok && formattedJson.text === '{\n  "ok": true\n}\n', 'valid json formatted');
  assert(!formatJsonText('{bad').ok, 'invalid json format rejected');

  const formattedToml = formatTomlText('title = "cc-partner"');
  assert(
    formattedToml.ok && formattedToml.text !== null && formattedToml.text.includes('title = "cc-partner"'),
    'valid toml formatted',
  );
  assert(!formatTomlText('title = ').ok, 'invalid toml format rejected');

  const mutableCaps = fileCapabilitiesForType('markdown');
  mutableCaps.availableModes.push('viewer');
  assert(!fileCapabilitiesForType('markdown').availableModes.includes('viewer'), 'capabilities return copied modes');
}

void main();
