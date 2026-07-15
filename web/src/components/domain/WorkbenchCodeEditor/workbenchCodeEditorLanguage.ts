import type { Extension } from '@codemirror/state';

/**
 * Workbench CodeMirror 语言动态加载器。
 *
 * Business Logic（为什么需要这个模块）:
 *   工作台文件面板会按文件类型请求语法高亮，但全部语言静态打进编辑器首包会使 lazy chunk
 *   膨胀到数百 KiB。按 canonical 语言动态 import 并缓存 Promise，可在保持未知/纯文本体验
 *   的同时把语言包从编辑器 entry 中拆出。
 *
 * Code Logic（这个模块做什么）:
 *   将别名归一到 canonical key；用显式 dynamic import map（禁止拼接任意 import 字符串）
 *   加载 Extension；loader 本身非 async，直接返回按 canonical 缓存的 Promise 身份；
 *   失败时删除 cache 条目以允许重试；未知语言 resolve null。
 */

/**
 * Business Logic（为什么需要这个类型）:
 *   测试与扩展需要可注入的语言加载函数，统一 Promise<Extension> 合同。
 *
 * Code Logic（这个类型做什么）:
 *   无参函数返回加载语言 Extension 的 Promise。
 */
export type WorkbenchLanguageLoader = () => Promise<Extension>;

/** 规范化后的 canonical 语言 id。 */
type CanonicalLanguage =
  | 'typescript'
  | 'tsx'
  | 'javascript'
  | 'jsx'
  | 'json'
  | 'yaml'
  | 'markdown'
  | 'css'
  | 'html'
  | 'python'
  | 'rust'
  | 'toml'
  | 'shell';

/**
 * Business Logic（为什么需要别名表）:
 *   文件类型识别可能给出 `ts`/`tsx`/`yml` 等短名，编辑器需映射到同一加载器与 cache key。
 *
 * Code Logic（这个表做什么）:
 *   小写无点语言 id / 扩展名 → canonical language。
 */
const LANGUAGE_ALIASES: Record<string, CanonicalLanguage> = {
  typescript: 'typescript',
  ts: 'typescript',
  tsx: 'tsx',
  javascript: 'javascript',
  js: 'javascript',
  mjs: 'javascript',
  cjs: 'javascript',
  jsx: 'jsx',
  json: 'json',
  yaml: 'yaml',
  yml: 'yaml',
  markdown: 'markdown',
  md: 'markdown',
  mdx: 'markdown',
  css: 'css',
  html: 'html',
  htm: 'html',
  python: 'python',
  py: 'python',
  rust: 'rust',
  rs: 'rust',
  toml: 'toml',
  shell: 'shell',
  sh: 'shell',
  bash: 'shell',
  zsh: 'shell',
  fish: 'shell',
};

/**
 * Business Logic（为什么需要显式 import map）:
 *   打包器只能静态分析字面量 dynamic import；禁止 `import(variable)` 以免语言包被漏拆
 *   或运行时路径注入。
 *
 * Code Logic（这个 map 做什么）:
 *   每个 canonical 语言返回非 async 的 loader，内部用固定路径 dynamic import 构造 Extension。
 */
const DEFAULT_LANGUAGE_LOADERS: Record<CanonicalLanguage, WorkbenchLanguageLoader> = {
  typescript: () =>
    import('@codemirror/lang-javascript').then((mod) => mod.javascript({ typescript: true })),
  tsx: () =>
    import('@codemirror/lang-javascript').then((mod) =>
      mod.javascript({ typescript: true, jsx: true }),
    ),
  javascript: () => import('@codemirror/lang-javascript').then((mod) => mod.javascript()),
  jsx: () => import('@codemirror/lang-javascript').then((mod) => mod.javascript({ jsx: true })),
  json: () => import('@codemirror/lang-json').then((mod) => mod.json()),
  yaml: () => import('@codemirror/lang-yaml').then((mod) => mod.yaml()),
  markdown: () => import('@codemirror/lang-markdown').then((mod) => mod.markdown()),
  css: () => import('@codemirror/lang-css').then((mod) => mod.css()),
  html: () => import('@codemirror/lang-html').then((mod) => mod.html()),
  python: () => import('@codemirror/lang-python').then((mod) => mod.python()),
  rust: () => import('@codemirror/lang-rust').then((mod) => mod.rust()),
  toml: () =>
    Promise.all([
      import('@codemirror/language'),
      import('@codemirror/legacy-modes/mode/toml'),
    ]).then(([languageMod, tomlMod]) => languageMod.StreamLanguage.define(tomlMod.toml)),
  shell: () =>
    Promise.all([
      import('@codemirror/language'),
      import('@codemirror/legacy-modes/mode/shell'),
    ]).then(([languageMod, shellMod]) => languageMod.StreamLanguage.define(shellMod.shell)),
};

/** 运行时可覆盖的 loader 表（生产 = DEFAULT；测试可注入失败/延迟 loader）。 */
let languageLoaders: Record<CanonicalLanguage, WorkbenchLanguageLoader> = {
  ...DEFAULT_LANGUAGE_LOADERS,
};

/** canonical language → in-flight / resolved Promise（失败时删除以允许重试）。 */
const languagePromiseCache = new Map<CanonicalLanguage, Promise<Extension>>();

/**
 * Business Logic（为什么需要规范化）:
 *   调用方可能传入带点扩展名、大小写混用或空白，需统一后再查别名表。
 *
 * Code Logic（这个函数做什么）:
 *   trim → lower → 去前导 `.`，再映射到 CanonicalLanguage；未知返回 null。
 */
function resolveCanonicalLanguage(language: string): CanonicalLanguage | null {
  const normalized = language.trim().toLowerCase().replace(/^\./, '');
  if (!normalized) {
    return null;
  }
  return LANGUAGE_ALIASES[normalized] ?? null;
}

/**
 * 按语言标识动态加载 CodeMirror 语法扩展。
 *
 * Business Logic（为什么需要这个函数）:
 *   编辑器打开文件时需要对应语言高亮，但不能把全部语言包同步打入首包；未知语言与加载
 *   失败仍须可编辑纯文本，不能阻断文件查看。
 *
 * Code Logic（这个函数做什么）:
 *   非 async：归一 canonical 后若 cache 命中则直接返回同一 Promise；否则调用显式 loader，
 *   将 Promise 写入 cache；loader reject 时删除 cache 并 rethrow，便于重试。未知语言
 *   resolve `null`（不进入 cache）。
 */
export function loadWorkbenchLanguage(language: string): Promise<Extension | null> {
  const canonical = resolveCanonicalLanguage(language);
  if (!canonical) {
    return Promise.resolve(null);
  }

  const cached = languagePromiseCache.get(canonical);
  if (cached) {
    return cached;
  }

  const loader = languageLoaders[canonical];
  const promise = loader().catch((error: unknown) => {
    languagePromiseCache.delete(canonical);
    throw error;
  });
  languagePromiseCache.set(canonical, promise);
  return promise;
}

/**
 * Business Logic（为什么需要测试钩子）:
 *   单元测试要注入失败/延迟 loader 并在用例间清空 cache，不能依赖真实网络/打包失败。
 *
 * Code Logic（这个函数做什么）:
 *   清空 Promise cache。
 */
export function __clearWorkbenchLanguageCacheForTests(): void {
  languagePromiseCache.clear();
}

/**
 * Business Logic（为什么需要测试钩子）:
 *   失败重试与 in-flight 身份测试需要替换单个 canonical 的 loader。
 *
 * Code Logic（这个函数做什么）:
 *   覆盖 languageLoaders 中指定 canonical 的 loader。
 */
export function __setWorkbenchLanguageLoaderForTests(
  canonical: CanonicalLanguage,
  loader: WorkbenchLanguageLoader,
): void {
  languageLoaders[canonical] = loader;
}

/**
 * Business Logic（为什么需要测试钩子）:
 *   每个用例结束后须恢复默认 dynamic import map，避免污染后续真实 import 测试。
 *
 * Code Logic（这个函数做什么）:
 *   把 languageLoaders 重置为 DEFAULT_LANGUAGE_LOADERS 的浅拷贝。
 */
export function __resetWorkbenchLanguageLoadersForTests(): void {
  languageLoaders = { ...DEFAULT_LANGUAGE_LOADERS };
}
