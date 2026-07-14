/**
 * check-css-tokens.mjs — 前端 design token 合同守卫。
 *
 * Business Logic（为什么需要）:
 *   组件曾引用未定义的语义 token（如 --bg-2 / --fg-muted），在浏览器中静默失效。
 *   需要 CI 可执行的合同：语义 token 必须在 tokens.css 定义；运行时结构变量只允许
 *   明确 allowlist；主题相关颜色/阴影必须浅/深双份。
 *
 * Code Logic（做什么）:
 *   剥离 CSS 注释后用正则收集 token 定义与 var(--name) 用法及行号；
 *   校验用法是否在 :root 定义或 allowlist 中；校验主题相关 token 是否同时
 *   出现在 :root 与 [data-theme="dark"]。导出 analyzeCssTokenContract 供测试；
 *   CLI 扫描 web/src 下全部 .css，exit 0/1。
 *
 * Usage:
 *   node scripts/check-css-tokens.mjs
 *   node scripts/check-css-tokens.mjs --self-test
 *   npm run check:css-tokens
 *   npm run check:tokens
 */

import { spawnSync } from 'node:child_process';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

/** 仅允许 TSX inline style 注入的运行时结构变量（非语义 token）。 */
const RUNTIME_STRUCTURAL_ALLOWLIST = new Set([
  'prompt-panel-left',
  'prompt-panel-top',
  'git-graph-color',
]);

/**
 * 主题相关 token 前缀/精确名：必须在 :root 与 [data-theme="dark"] 同时定义。
 * 字体/间距/圆角/动效/z-index 等结构 token 只在 :root 定义（现有规则）。
 */
const THEME_DEPENDENT_EXACT = new Set([
  'bg',
  'surface',
  'surface-warm',
  'fg',
  'fg-2',
  'muted',
  'meta',
  'border',
  'border-soft',
  'accent',
  'accent-on',
  'accent-soft',
  'accent-hover',
  'screenshot-selection',
  'success',
  'warn',
  'danger',
  'danger-soft',
]);

const THEME_DEPENDENT_PREFIXES = [
  'bg-',
  'surface-',
  'fg-',
  'border-',
  'accent-',
  'git-',
  'terminal-',
  'shadow-',
  'danger-',
];

/**
 * 判断 token 是否需要浅/深双主题定义。
 *
 * Business Logic:
 *   颜色与阴影随主题切换；结构尺寸不需要重复声明。
 *
 * Code Logic:
 *   精确名集合 + 前缀匹配。
 *
 * @param {string} name 不带 -- 前缀的 token 名
 * @returns {boolean}
 */
function isThemeDependentToken(name) {
  if (THEME_DEPENDENT_EXACT.has(name)) return true;
  return THEME_DEPENDENT_PREFIXES.some((prefix) => name.startsWith(prefix));
}

/**
 * 去除 CSS 块注释，保留换行以维持行号。
 *
 * Business Logic:
 *   注释中的 var(--xxx) 示例不应触发合同失败。
 *
 * Code Logic:
 *   将 /* ... *\/ 替换为等长换行空白。
 *
 * @param {string} source
 * @returns {string}
 */
function stripCssComments(source) {
  return source.replace(/\/\*[\s\S]*?\*\//g, (block) =>
    block.replace(/[^\n]/g, ' '),
  );
}

/**
 * 从 tokens.css 解析 :root 与 dark 主题中的定义集合。
 *
 * Business Logic:
 *   语义 token 唯一权威源是 tokens.css。
 *
 * Code Logic:
 *   用花括号平衡扫描 :root / [data-theme="dark"] 块，提取 `--name:` 定义。
 *
 * @param {string} tokensSource
 * @returns {{ root: Set<string>, dark: Set<string> }}
 */
function parseTokenDefinitions(tokensSource) {
  const stripped = stripCssComments(tokensSource);
  /** @type {Set<string>} */
  const root = new Set();
  /** @type {Set<string>} */
  const dark = new Set();

  /**
   * 提取选择器块内的 `--name:` 定义。
   *
   * @param {string} block
   * @param {Set<string>} into
   */
  function collectDefs(block, into) {
    const defRe = /--([a-zA-Z0-9_-]+)\s*:/g;
    let m;
    while ((m = defRe.exec(block)) !== null) {
      into.add(m[1]);
    }
  }

  /**
   * 查找 selector 后的 `{...}` 块内容。
   *
   * @param {string} source
   * @param {RegExp} selectorRe
   * @returns {string | null}
   */
  function extractBlock(source, selectorRe) {
    const match = selectorRe.exec(source);
    if (!match) return null;
    const openIdx = source.indexOf('{', match.index + match[0].length - 1);
    if (openIdx < 0) return null;
    let depth = 0;
    for (let i = openIdx; i < source.length; i += 1) {
      const ch = source[i];
      if (ch === '{') depth += 1;
      else if (ch === '}') {
        depth -= 1;
        if (depth === 0) {
          return source.slice(openIdx + 1, i);
        }
      }
    }
    return null;
  }

  const rootBlock = extractBlock(stripped, /:root\s*\{/);
  if (rootBlock) collectDefs(rootBlock, root);

  const darkBlock = extractBlock(
    stripped,
    /\[data-theme\s*=\s*["']dark["']\]\s*\{/,
  );
  if (darkBlock) collectDefs(darkBlock, dark);

  return { root, dark };
}

/**
 * 收集 CSS 源中 var(--name) 用法及行号。
 *
 * Business Logic:
 *   合同要求每一个语义 var 引用都可解析到 tokens 定义。
 *
 * Code Logic:
 *   注释剥离后全局匹配 var(--name)，用前缀换行计数得行号。
 *
 * @param {string} content
 * @returns {Array<{ name: string, line: number }>}
 */
function collectVarUsages(content) {
  const stripped = stripCssComments(content);
  /** @type {Array<{ name: string, line: number }>} */
  const usages = [];
  const usageRe = /var\(\s*(--([a-zA-Z0-9_-]+))/g;
  let m;
  while ((m = usageRe.exec(stripped)) !== null) {
    const name = m[2];
    const line = stripped.slice(0, m.index).split('\n').length;
    usages.push({ name, line });
  }
  return usages;
}

/**
 * 分析 CSS 文件与 tokens 源是否满足 design token 合同。
 *
 * Business Logic:
 *   把「未知语义 token / 深色主题缺失」转成可 CI 失败的诊断列表。
 *
 * Code Logic:
 *   解析 tokens 定义 → 检查主题相关 token 双份 → 扫描 files 的 var 用法；
 *   allowlist 结构变量放行；诊断格式 `file:line --token`，主题缺失用
 *   `tokens.css:0 --token`。
 *
 * @param {Array<{ path: string, content: string }>} files
 * @param {string} tokensSource
 * @returns {string[]}
 */
export function analyzeCssTokenContract(files, tokensSource) {
  const { root, dark } = parseTokenDefinitions(tokensSource);
  /** @type {string[]} */
  const diagnostics = [];

  for (const name of root) {
    if (isThemeDependentToken(name) && !dark.has(name)) {
      diagnostics.push(`tokens.css:0 --${name}`);
    }
  }

  for (const file of files) {
    const usages = collectVarUsages(file.content);
    for (const { name, line } of usages) {
      if (RUNTIME_STRUCTURAL_ALLOWLIST.has(name)) continue;
      if (root.has(name)) continue;
      diagnostics.push(`${file.path}:${line} --${name}`);
    }
  }

  return diagnostics;
}

/**
 * 递归收集目录下全部 .css 文件路径。
 *
 * Business Logic:
 *   CLI 需要对前端源码中所有 CSS Modules / 全局样式生效。
 *
 * Code Logic:
 *   深度优先遍历，跳过 node_modules/dist。
 *
 * @param {string} dir
 * @returns {string[]}
 */
function listCssFiles(dir) {
  /** @type {string[]} */
  const out = [];
  for (const entry of readdirSync(dir)) {
    if (entry === 'node_modules' || entry === 'dist') continue;
    const full = join(dir, entry);
    const st = statSync(full);
    if (st.isDirectory()) {
      out.push(...listCssFiles(full));
    } else if (entry.endsWith('.css')) {
      out.push(full);
    }
  }
  return out;
}

/**
 * 运行独立 fixture 测试（`--self-test`）。
 *
 * Business Logic:
 *   计划要求脚本支持 `--self-test`，与既有 `node --test` 共用断言。
 *
 * Code Logic:
 *   spawn `node --test` 指向同目录 test 文件。
 *
 * @param {string} scriptDir
 * @returns {number}
 */
function runSelfTest(scriptDir) {
  const testFile = resolve(scriptDir, 'check-css-tokens.test.mjs');
  const result = spawnSync(process.execPath, ['--test', testFile], {
    cwd: resolve(scriptDir, '..'),
    stdio: 'inherit',
  });
  return result.status ?? 1;
}

/**
 * CLI 入口：扫描 web/src 并打印诊断。
 *
 * Business Logic:
 *   给 npm script / CI 提供 exit code 语义。
 *
 * Code Logic:
 *   读取 tokens.css 与全部 CSS，调用 analyzeCssTokenContract，
 *   无诊断打印成功文案 exit 0，否则打印诊断 exit 1。
 *
 * @param {string[]} [argv]
 * @returns {number}
 */
function main(argv = process.argv.slice(2)) {
  const scriptDir = dirname(fileURLToPath(import.meta.url));
  if (argv.includes('--self-test')) {
    return runSelfTest(scriptDir);
  }

  const webRoot = resolve(scriptDir, '..');
  const srcRoot = resolve(webRoot, 'src');
  const tokensPath = resolve(srcRoot, 'styles/tokens.css');
  const tokensSource = readFileSync(tokensPath, 'utf8');

  const cssFiles = listCssFiles(srcRoot).filter(
    (p) => resolve(p) !== resolve(tokensPath),
  );
  const files = cssFiles.map((absPath) => ({
    path: relative(webRoot, absPath).split('\\').join('/'),
    content: readFileSync(absPath, 'utf8'),
  }));

  const diagnostics = analyzeCssTokenContract(files, tokensSource);
  if (diagnostics.length === 0) {
    console.log('CSS token contract passed');
    return 0;
  }
  for (const d of diagnostics) {
    console.error(d);
  }
  return 1;
}

const isDirectRun =
  process.argv[1] &&
  fileURLToPath(import.meta.url) === resolve(process.argv[1]);

if (isDirectRun) {
  process.exit(main());
}
