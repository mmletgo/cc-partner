import { parse as parseToml, stringify as stringifyToml } from 'smol-toml';

export type WorkbenchDetectedFileType =
  | 'image'
  | 'markdown'
  | 'code'
  | 'json'
  | 'toml'
  | 'csv'
  | 'sqlite'
  | 'text'
  | 'binary'
  | 'unsupported';

export type WorkbenchFileMode = 'viewer' | 'editor' | 'wysiwyg' | 'source' | 'split';

export interface WorkbenchFileCapabilities {
  canPreview: boolean;
  canEdit: boolean;
  canFormat: boolean;
  mustValidateBeforeSave: boolean;
  defaultMode: WorkbenchFileMode;
  availableModes: WorkbenchFileMode[];
}

export interface WorkbenchFileTab {
  id: string;
  path: string;
  name: string;
  detectedType: WorkbenchDetectedFileType;
  mode: WorkbenchFileMode;
  dirty: boolean;
  savedVersion: string | null;
}

export type WorkbenchFileWorkspaceView = 'terminal' | 'files';

export interface WorkbenchFileTabsState {
  tabs: WorkbenchFileTab[];
  activeTabId: string | null;
  view: WorkbenchFileWorkspaceView;
}

export type WorkbenchFileTabsAction =
  | { type: 'opened'; tab: WorkbenchFileTab }
  | { type: 'activated'; id: string }
  | { type: 'closed'; id: string }
  | { type: 'modeChanged'; id: string; mode: WorkbenchFileMode }
  | { type: 'dirtyChanged'; id: string; dirty: boolean }
  | { type: 'saved'; id: string; savedVersion: string | null }
  | { type: 'viewChanged'; view: WorkbenchFileWorkspaceView };

export interface ValidationResult {
  ok: boolean;
  message: string | null;
}

export interface FormatResult {
  ok: boolean;
  text: string | null;
  message: string | null;
}

export interface WorkbenchPathTabCandidate {
  path: string;
  name: string;
  dirty: boolean;
}

export interface WorkbenchDirRequestParts {
  projectId: string;
  worktreeId: string | null;
  path: string;
}

const IMAGE_EXTENSIONS = new Set(['avif', 'bmp', 'gif', 'ico', 'jpeg', 'jpg', 'png', 'svg', 'tif', 'tiff', 'webp']);
const MARKDOWN_EXTENSIONS = new Set(['markdown', 'md', 'mdown', 'mdx', 'mkd']);
const JSON_EXTENSIONS = new Set(['json']);
const UNSUPPORTED_JSON_EXTENSIONS = new Set(['jsonc']);
const TOML_EXTENSIONS = new Set(['toml']);
const CSV_EXTENSIONS = new Set(['csv', 'tsv']);
const SQLITE_EXTENSIONS = new Set(['db', 'sqlite', 'sqlite3']);
const TEXT_EXTENSIONS = new Set([
  'authors',
  'changelog',
  'config',
  'conf',
  'contributors',
  'copying',
  'dockerignore',
  'editorconfig',
  'env',
  'gitattributes',
  'gitignore',
  'ini',
  'licence',
  'license',
  'lock',
  'log',
  'notice',
  'npmrc',
  'nvmrc',
  'properties',
  'readme',
  'text',
  'todo',
  'txt',
]);
const CODE_EXTENSIONS = new Set([
  'bash',
  'c',
  'cc',
  'cjs',
  'clj',
  'cljs',
  'containerfile',
  'cpp',
  'cs',
  'css',
  'cxx',
  'dart',
  'dockerfile',
  'erl',
  'ex',
  'exs',
  'fish',
  'fs',
  'fsx',
  'gemfile',
  'gql',
  'gradle',
  'graphql',
  'go',
  'h',
  'hpp',
  'hrl',
  'html',
  'htm',
  'java',
  'justfile',
  'js',
  'jsx',
  'kt',
  'kts',
  'less',
  'lua',
  'makefile',
  'mjs',
  'pl',
  'pm',
  'php',
  'proto',
  'py',
  'ps1',
  'r',
  'rakefile',
  'rb',
  'rs',
  'sass',
  'scala',
  'scss',
  'sh',
  'sql',
  'svelte',
  'swift',
  'ts',
  'tsx',
  'vue',
  'xml',
  'yaml',
  'yml',
  'zsh',
]);

const FILE_CAPABILITIES: Record<WorkbenchDetectedFileType, WorkbenchFileCapabilities> = {
  image: {
    canPreview: true,
    canEdit: false,
    canFormat: false,
    mustValidateBeforeSave: false,
    defaultMode: 'viewer',
    availableModes: ['viewer'],
  },
  markdown: {
    canPreview: true,
    canEdit: true,
    canFormat: false,
    mustValidateBeforeSave: false,
    defaultMode: 'wysiwyg',
    availableModes: ['source', 'wysiwyg', 'split'],
  },
  code: {
    canPreview: true,
    canEdit: true,
    canFormat: false,
    mustValidateBeforeSave: false,
    defaultMode: 'editor',
    availableModes: ['editor'],
  },
  json: {
    canPreview: true,
    canEdit: true,
    canFormat: true,
    mustValidateBeforeSave: true,
    defaultMode: 'editor',
    availableModes: ['editor'],
  },
  toml: {
    canPreview: true,
    canEdit: true,
    canFormat: true,
    mustValidateBeforeSave: true,
    defaultMode: 'editor',
    availableModes: ['editor'],
  },
  csv: {
    canPreview: true,
    canEdit: false,
    canFormat: false,
    mustValidateBeforeSave: false,
    defaultMode: 'viewer',
    availableModes: ['viewer'],
  },
  sqlite: {
    canPreview: true,
    canEdit: false,
    canFormat: false,
    mustValidateBeforeSave: false,
    defaultMode: 'viewer',
    availableModes: ['viewer'],
  },
  text: {
    canPreview: true,
    canEdit: true,
    canFormat: false,
    mustValidateBeforeSave: false,
    defaultMode: 'editor',
    availableModes: ['editor'],
  },
  binary: {
    canPreview: false,
    canEdit: false,
    canFormat: false,
    mustValidateBeforeSave: false,
    defaultMode: 'viewer',
    availableModes: ['viewer'],
  },
  unsupported: {
    canPreview: false,
    canEdit: false,
    canFormat: false,
    mustValidateBeforeSave: false,
    defaultMode: 'viewer',
    availableModes: ['viewer'],
  },
};

/**
 * Business Logic（为什么需要这个函数）:
 *   关闭或删除路径前需要知道哪些已打开文件 tab 会受影响，避免静默丢弃未保存编辑。
 *
 * Code Logic（这个函数做什么）:
 *   文件路径只匹配同路径 tab；目录路径匹配同路径和所有后代路径，返回原 tab 对象列表。
 */
export function collectTabsForPath<Tab extends WorkbenchPathTabCandidate>(
  tabs: Tab[],
  path: string,
  kind: 'file' | 'dir' | string,
): Tab[] {
  if (kind === 'dir') {
    return tabs.filter((tab) => isSameOrDescendantPath(tab.path, path));
  }
  return tabs.filter((tab) => tab.path === path);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   删除或重命名目录后，文件树缓存和已展开状态需要同步清掉该目录子树，避免展示不存在的旧节点。
 *
 * Code Logic（这个函数做什么）:
 *   对同一路径直接命中；非空目录路径用 `path/` 前缀判断后代；根路径命中所有非空相对路径。
 */
export function isSameOrDescendantPath(path: string, targetPath: string): boolean {
  if (path === targetPath) return true;
  if (!targetPath) return path.length > 0;
  return path.startsWith(`${targetPath}/`);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   目录删除/重命名后，缓存中的旧子目录列表不能继续驱动文件树 UI。
 *
 * Code Logic（这个函数做什么）:
 *   从 Record 中移除 key 等于目标目录或位于目标目录后代的项；没有变化时返回原对象。
 */
export function dropPathTreeEntries<TValue>(entries: Record<string, TValue>, path: string): Record<string, TValue> {
  let changed = false;
  const nextEntries: Record<string, TValue> = {};
  for (const [entryPath, value] of Object.entries(entries)) {
    if (isSameOrDescendantPath(entryPath, path)) {
      changed = true;
      continue;
    }
    nextEntries[entryPath] = value;
  }
  return changed ? nextEntries : entries;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   目录删除/重命名后，被移除子树不应继续保持展开状态。
 *
 * Code Logic（这个函数做什么）:
 *   复制 Set 并过滤目标目录及其后代路径；没有变化时返回原 Set。
 */
export function dropExpandedPathTree(paths: Set<string>, path: string): Set<string> {
  let changed = false;
  const nextPaths = new Set<string>();
  for (const entryPath of paths) {
    if (isSameOrDescendantPath(entryPath, path)) {
      changed = true;
      continue;
    }
    nextPaths.add(entryPath);
  }
  return changed ? nextPaths : paths;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   确认放弃未保存修改时，提示文案需要列出受影响的 dirty 文件名。
 *
 * Code Logic（这个函数做什么）:
 *   从 tab 列表中过滤 dirty=true 的项并返回文件名数组，保持原顺序。
 */
export function dirtyTabNames<Tab extends WorkbenchPathTabCandidate>(tabs: Tab[]): string[] {
  return tabs.filter((tab) => tab.dirty).map((tab) => tab.name);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   文件树目录加载请求需要按项目、worktree 和路径隔离顺序号，避免旧响应覆盖新目录内容。
 *
 * Code Logic（这个函数做什么）:
 *   将 projectId、worktreeId 和 path 序列化为稳定 key；main worktree 用固定标记区分。
 */
export function workbenchDirRequestKey(projectId: string, worktreeId: string | null, path: string): string {
  return JSON.stringify([projectId, worktreeId ?? 'main', path]);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   路径变更后需要让对应目录子树的旧请求失效，必须先从请求 key 中还原项目/worktree/path。
 *
 * Code Logic（这个函数做什么）:
 *   解析 `workbenchDirRequestKey` 生成的 JSON tuple；不符合格式时返回 null。
 */
export function parseWorkbenchDirRequestKey(key: string): WorkbenchDirRequestParts | null {
  try {
    const parts: unknown = JSON.parse(key);
    if (!Array.isArray(parts) || parts.length !== 3) return null;
    const [projectId, worktreeId, path] = parts;
    if (typeof projectId !== 'string' || typeof worktreeId !== 'string' || typeof path !== 'string') {
      return null;
    }
    return {
      projectId,
      worktreeId: worktreeId === 'main' ? null : worktreeId,
      path,
    };
  } catch {
    return null;
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   删除/重命名目录后，只有同项目、同 worktree、同子树范围内的目录请求需要作废。
 *
 * Code Logic（这个函数做什么）:
 *   解析请求 key 后比较 projectId/worktreeId，并用路径子树判断目标请求是否受影响。
 */
export function workbenchDirRequestKeyMatchesPath(
  key: string,
  projectId: string,
  worktreeId: string | null,
  path: string,
): boolean {
  const parts = parseWorkbenchDirRequestKey(key);
  return Boolean(
    parts &&
      parts.projectId === projectId &&
      parts.worktreeId === worktreeId &&
      isSameOrDescendantPath(parts.path, path),
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   异步请求返回时需要统一判断当前序号是否仍是最新，旧响应不应写入 UI 状态。
 *
 * Code Logic（这个函数做什么）:
 *   比较当前保存的最新序号与请求发起时捕获的序号，完全相等才允许应用结果。
 */
export function isLatestRequest(currentSeq: number | undefined, requestSeq: number): boolean {
  return currentSeq === requestSeq;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 文件树会传入项目相对路径，文件查看器需要用稳定扩展名判断打开方式。
 *
 * Code Logic（这个函数做什么）:
 *   从路径最后一段提取小写扩展名；无扩展名时兼容 `.env` / `.gitignore` 这类点文件名。
 */
function extensionFromFilename(filename: string): string | null {
  const normalizedName = filename.trim().split(/[\\/]/).pop()?.toLowerCase() ?? '';
  if (!normalizedName) return null;
  const lastDotIndex = normalizedName.lastIndexOf('.');
  if (lastDotIndex === -1) return normalizedName;
  if (lastDotIndex === 0 && normalizedName.indexOf('.', 1) === -1) {
    return normalizedName.slice(1);
  }
  const extension = normalizedName.slice(lastDotIndex + 1);
  return extension.length > 0 ? extension : null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   后端和系统文件信息返回的 mime 可能带 charset，前端需要先归一化再参与能力判断。
 *
 * Code Logic（这个函数做什么）:
 *   去掉 mime 参数并转小写；空字符串返回 null。
 */
function normalizeMime(mime: string | null): string | null {
  if (!mime) return null;
  const normalizedMime = mime.split(';')[0]?.trim().toLowerCase() ?? '';
  return normalizedMime.length > 0 ? normalizedMime : null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   文件查看器打开文件前必须知道应使用图片预览、Markdown 编辑器、代码编辑器还是只读预览。
 *
 * Code Logic（这个函数做什么）:
 *   先按扩展名识别项目常见文件；扩展名缺失时用 mime 兜底判断文本、图片和二进制类型。
 */
export function detectWorkbenchFileType(filename: string, mime: string | null): WorkbenchDetectedFileType {
  const extension = extensionFromFilename(filename);
  if (extension && IMAGE_EXTENSIONS.has(extension)) return 'image';
  if (extension && MARKDOWN_EXTENSIONS.has(extension)) return 'markdown';
  if (extension && UNSUPPORTED_JSON_EXTENSIONS.has(extension)) return 'unsupported';
  if (extension && JSON_EXTENSIONS.has(extension)) return 'json';
  if (extension && TOML_EXTENSIONS.has(extension)) return 'toml';
  if (extension && CSV_EXTENSIONS.has(extension)) return 'csv';
  if (extension && SQLITE_EXTENSIONS.has(extension)) return 'sqlite';
  if (extension && CODE_EXTENSIONS.has(extension)) return 'code';
  if (extension && TEXT_EXTENSIONS.has(extension)) return 'text';

  const normalizedMime = normalizeMime(mime);
  if (!normalizedMime) return 'unsupported';
  if (normalizedMime.startsWith('image/')) return 'image';
  if (normalizedMime === 'application/json' || normalizedMime.endsWith('+json')) return 'json';
  if (normalizedMime === 'application/toml' || normalizedMime === 'text/toml') return 'toml';
  if (normalizedMime === 'text/csv' || normalizedMime === 'text/tab-separated-values') return 'csv';
  if (normalizedMime === 'application/vnd.sqlite3' || normalizedMime === 'application/x-sqlite3') return 'sqlite';
  if (normalizedMime === 'application/octet-stream') return 'binary';
  if (normalizedMime.startsWith('text/')) return 'text';
  return 'unsupported';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 文件 tab 需要基于文件类型统一决定是否允许编辑、预览、格式化和保存前校验。
 *
 * Code Logic（这个函数做什么）:
 *   返回文件类型对应能力对象的浅拷贝，并复制 availableModes，避免调用方污染共享配置。
 */
export function fileCapabilitiesForType(type: WorkbenchDetectedFileType): WorkbenchFileCapabilities {
  const capabilities = FILE_CAPABILITIES[type];
  return { ...capabilities, availableModes: [...capabilities.availableModes] };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   打开文件后 Workbench 需要把文件 tab 激活，并从终端视图切到文件工作区。
 *
 * Code Logic（这个函数做什么）:
 *   按 action 纯函数更新 tabs、activeTabId、view；重复打开同一 tab 时只刷新 metadata 并保留 dirty/mode 等用户态。
 */
export function reduceFileTabs(
  state: WorkbenchFileTabsState,
  action: WorkbenchFileTabsAction,
): WorkbenchFileTabsState {
  switch (action.type) {
    case 'opened': {
      const nextTabs = state.tabs.some((tab) => tab.id === action.tab.id)
        ? state.tabs.map((tab) =>
            tab.id === action.tab.id
              ? {
                  ...tab,
                  path: action.tab.path,
                  name: action.tab.name,
                  detectedType: action.tab.detectedType,
                }
              : tab,
          )
        : [...state.tabs, action.tab];
      return { tabs: nextTabs, activeTabId: action.tab.id, view: 'files' };
    }
    case 'activated': {
      if (!state.tabs.some((tab) => tab.id === action.id)) return state;
      return { ...state, activeTabId: action.id, view: 'files' };
    }
    case 'closed': {
      const nextTabs = state.tabs.filter((tab) => tab.id !== action.id);
      if (state.activeTabId !== action.id) return { ...state, tabs: nextTabs };
      const nextActiveTabId = nextTabs.at(-1)?.id ?? null;
      return { tabs: nextTabs, activeTabId: nextActiveTabId, view: nextActiveTabId ? 'files' : 'terminal' };
    }
    case 'modeChanged': {
      return {
        ...state,
        tabs: state.tabs.map((tab) => (tab.id === action.id ? { ...tab, mode: action.mode } : tab)),
      };
    }
    case 'dirtyChanged': {
      return {
        ...state,
        tabs: state.tabs.map((tab) => (tab.id === action.id ? { ...tab, dirty: action.dirty } : tab)),
      };
    }
    case 'saved': {
      return {
        ...state,
        tabs: state.tabs.map((tab) =>
          tab.id === action.id ? { ...tab, dirty: false, savedVersion: action.savedVersion } : tab,
        ),
      };
    }
    case 'viewChanged':
      return { ...state, view: action.view };
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   JSON 文件保存前必须阻止语法错误写回磁盘，避免破坏配置或数据文件。
 *
 * Code Logic（这个函数做什么）:
 *   使用原生 JSON.parse 做语义校验；成功返回 ok，失败返回错误消息。
 */
export function validateJsonText(text: string): ValidationResult {
  try {
    JSON.parse(text);
    return { ok: true, message: null };
  } catch (error: unknown) {
    return { ok: false, message: error instanceof Error ? error.message : 'Invalid JSON' };
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   TOML 文件保存前必须阻止语法错误写回磁盘，避免破坏项目配置文件。
 *
 * Code Logic（这个函数做什么）:
 *   使用 smol-toml 的 parse 做语义校验；成功返回 ok，失败返回错误消息。
 */
export function validateTomlText(text: string): ValidationResult {
  try {
    parseToml(text);
    return { ok: true, message: null };
  } catch (error: unknown) {
    return { ok: false, message: error instanceof Error ? error.message : 'Invalid TOML' };
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   JSON 文件编辑器提供格式化按钮时，需要用同一套严格 JSON 语义避免格式化 JSONC 等非支持格式。
 *
 * Code Logic（这个函数做什么）:
 *   先用 JSON.parse 校验并解析，再用 JSON.stringify 以 2 空格缩进输出并补末尾换行；失败返回错误消息。
 */
export function formatJsonText(text: string): FormatResult {
  try {
    const parsedValue: unknown = JSON.parse(text);
    return { ok: true, text: `${JSON.stringify(parsedValue, null, 2)}\n`, message: null };
  } catch (error: unknown) {
    return { ok: false, text: null, message: error instanceof Error ? error.message : 'Invalid JSON' };
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   TOML 文件编辑器提供格式化按钮时，需要复用保存前同源语义校验并输出规范 TOML。
 *
 * Code Logic（这个函数做什么）:
 *   使用 smol-toml parse 解析，再用 stringify 重新序列化；失败返回错误消息。
 */
export function formatTomlText(text: string): FormatResult {
  try {
    const parsedValue = parseToml(text);
    return { ok: true, text: stringifyToml(parsedValue), message: null };
  } catch (error: unknown) {
    return { ok: false, text: null, message: error instanceof Error ? error.message : 'Invalid TOML' };
  }
}
