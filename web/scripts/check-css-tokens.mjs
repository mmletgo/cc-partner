/**
 * check-css-tokens.mjs — 前端 design token 合同守卫。
 *
 * Business Logic（为什么需要）:
 *   组件曾引用未定义的语义 token（如 --bg-2 / --fg-muted），在浏览器中静默失效。
 *   需要 CI 可执行的合同：语义 token 必须在 tokens.css 定义；运行时结构变量只允许
 *   明确 allowlist；主题相关颜色/阴影必须浅/深双份；普通正文色对对比度 ≥4.5；
 *   `--meta` 仅允许已评审的 disabled/decorative 文本，避免低对比承载必要信息。
 *
 * Code Logic（做什么）:
 *   剥离 CSS 注释后用正则收集 token 定义与 var(--name) 用法及行号；
 *   校验用法是否在 :root 定义或 allowlist 中；校验主题相关 token 是否同时
 *   出现在 :root 与 [data-theme="dark"]；解析可解析 hex 并检查正文色对；
 *   扫描 color: var(--meta) 是否落在 META_COLOR_ALLOWLIST。
 *   导出 analyzeCssTokenContract 供测试；CLI 扫描 web/src 下全部 .css，exit 0/1。
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
  // Mobile shell visualViewport 注入（高度/键盘占用/实际上移量/终端最小高度）
  'mobile-shell-height',
  'mobile-keyboard-inset',
  'mobile-keyboard-shift',
  'mobile-terminal-min-height',
  // Mobile terminal FAB radial layout (inline --fab-angle / delay; CSS --fab-radius)
  'fab-angle',
  'fab-radius',
  'fab-delay-open',
  'fab-delay-close',
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
  'fg-muted-readable',
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
  'overlay-scrim',
  'overlay-on',
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
 * 必须达到 WCAG 2.2 普通文本 4.5:1 的前景/背景语义对。
 * `--meta` 明确排除：只允许 disabled/decorative，不得承载必要信息。
 */
const REQUIRED_TEXT_CONTRAST_PAIRS = [
  ['fg', 'bg'],
  ['fg', 'surface'],
  ['fg-2', 'bg'],
  ['fg-2', 'surface'],
  ['muted', 'bg'],
  ['muted', 'surface'],
  ['fg-muted-readable', 'bg'],
  ['fg-muted-readable', 'surface'],
];

const MIN_NORMAL_TEXT_CONTRAST = 4.5;

/**
 * 已评审的 `color: var(--meta)` 用法（path:selector）。
 * path 相对 web/；selector 为规则选择器原文（不含 `{`）。
 * 仅 disabled/decorative/placeholder；新增语义文本必须改用 --fg-muted-readable。
 */
const META_COLOR_ALLOWLIST = new Set([
  // placeholders / disabled
  'src/components/domain/WorkbenchSessionSearch/WorkbenchSessionSearch.module.css:.input::placeholder',
  'src/components/primitives/Input/Input.module.css:.input::placeholder',
  'src/components/primitives/Input/Input.module.css:.iconLeft, .iconRight',
  'src/pages/Orchestrator/Orchestrator.module.css:.textarea::placeholder',
  'src/pages/Scratchpad/Scratchpad.module.css:.editor::placeholder',
  'src/pages/Transfer/Transfer.module.css:.select:disabled',
  'src/pages/Workbench/Workbench.module.css:.promptOptimizerInput::placeholder',
  // pure decorative chrome（不承载必要文案）
  'src/pages/Workbench/Workbench.module.css:.worktreeBranchSlash',
  'src/pages/Workbench/Workbench.module.css:.treeChevron',
  // DesignSystem 为 dev-only 展示页
  'src/pages/DesignSystem/DesignSystem.module.css:.fontSize',
  'src/pages/DesignSystem/DesignSystem.module.css:.btnRowLabel',
  'src/pages/DesignSystem/DesignSystem.module.css:.formLabel',
  'src/pages/DesignSystem/DesignSystem.module.css:.tagGroupLabel',
  'src/pages/DesignSystem/DesignSystem.module.css:.statusGroupLabel',
  'src/pages/DesignSystem/DesignSystem.module.css:.domainLabel',
  'src/pages/DesignSystem/DesignSystem.module.css:.layoutCardTitle',
  'src/pages/DesignSystem/DesignSystem.module.css:.pageFooter',
]);

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
 * 解析主题块中的 token 原始值（含 var 引用）。
 *
 * Business Logic:
 *   对比度检查需要解析到最终 #hex。
 *
 * Code Logic:
 *   提取 `--name: value;` 对，value 取到分号前 trim。
 *
 * @param {string} block
 * @returns {Map<string, string>}
 */
function parseTokenRawValues(block) {
  /** @type {Map<string, string>} */
  const map = new Map();
  const defRe = /--([a-zA-Z0-9_-]+)\s*:\s*([^;]+);/g;
  let m;
  while ((m = defRe.exec(block)) !== null) {
    map.set(m[1], m[2].trim());
  }
  return map;
}

/**
 * 从 tokens 源提取 :root / dark 原始值 map。
 *
 * @param {string} tokensSource
 * @returns {{ root: Map<string, string>, dark: Map<string, string> }}
 */
function parseTokenValueMaps(tokensSource) {
  const stripped = stripCssComments(tokensSource);

  /**
   * @param {string} source
   * @param {RegExp} selectorRe
   * @returns {string}
   */
  function extractBlock(source, selectorRe) {
    const match = selectorRe.exec(source);
    if (!match) return '';
    const openIdx = source.indexOf('{', match.index + match[0].length - 1);
    if (openIdx < 0) return '';
    let depth = 0;
    for (let i = openIdx; i < source.length; i += 1) {
      const ch = source[i];
      if (ch === '{') depth += 1;
      else if (ch === '}') {
        depth -= 1;
        if (depth === 0) return source.slice(openIdx + 1, i);
      }
    }
    return '';
  }

  return {
    root: parseTokenRawValues(extractBlock(stripped, /:root\s*\{/)),
    dark: parseTokenRawValues(
      extractBlock(stripped, /\[data-theme\s*=\s*["']dark["']\]\s*\{/),
    ),
  };
}

/**
 * 将 token 值解析为 #rrggbb（支持简单 var(--x) 链）。
 *
 * Business Logic:
 *   对比度合同只覆盖可静态解析的纯色语义对；color-mix 不参与本检查。
 *
 * Code Logic:
 *   跟随 var() 最多 8 步；#rgb/#rrggbb 归一为 #rrggbb 小写。
 *
 * @param {string} name
 * @param {Map<string, string>} values
 * @returns {string | null}
 */
function resolveHexColor(name, values) {
  let current = name;
  for (let hop = 0; hop < 8; hop += 1) {
    const raw = values.get(current);
    if (!raw) return null;
    const hexMatch = raw.match(/^#([0-9a-fA-F]{3}|[0-9a-fA-F]{6})$/);
    if (hexMatch) {
      const body = hexMatch[1];
      if (body.length === 3) {
        return `#${body
          .split('')
          .map((c) => c + c)
          .join('')
          .toLowerCase()}`;
      }
      return `#${body.toLowerCase()}`;
    }
    const varMatch = raw.match(/^var\(\s*--([a-zA-Z0-9_-]+)\s*\)$/);
    if (!varMatch) return null;
    current = varMatch[1];
  }
  return null;
}

/**
 * sRGB 通道转线性。
 *
 * @param {number} channel 0–1
 * @returns {number}
 */
function srgbChannelToLinear(channel) {
  return channel <= 0.04045
    ? channel / 12.92
    : ((channel + 0.055) / 1.055) ** 2.4;
}

/**
 * 计算 #rrggbb 相对亮度（WCAG）。
 *
 * Business Logic:
 *   对比度公式依赖相对亮度。
 *
 * Code Logic:
 *   标准 WCAG 相对亮度。
 *
 * @param {string} hex
 * @returns {number}
 */
function relativeLuminance(hex) {
  const body = hex.slice(1);
  const r = parseInt(body.slice(0, 2), 16) / 255;
  const g = parseInt(body.slice(2, 4), 16) / 255;
  const b = parseInt(body.slice(4, 6), 16) / 255;
  const R = srgbChannelToLinear(r);
  const G = srgbChannelToLinear(g);
  const B = srgbChannelToLinear(b);
  return 0.2126 * R + 0.7152 * G + 0.0722 * B;
}

/**
 * 计算两色对比度比值。
 *
 * Business Logic:
 *   普通文本需 ≥4.5:1。
 *
 * Code Logic:
 *   (L1+0.05)/(L2+0.05)，L1≥L2。
 *
 * @param {string} a
 * @param {string} b
 * @returns {number}
 */
function contrastRatio(a, b) {
  const l1 = relativeLuminance(a);
  const l2 = relativeLuminance(b);
  const lighter = Math.max(l1, l2);
  const darker = Math.min(l1, l2);
  return (lighter + 0.05) / (darker + 0.05);
}

/**
 * 检查主题内正文语义色对对比度。
 *
 * Business Logic:
 *   防止 --meta 级低对比被当正文；强制 fg/muted/fg-muted-readable 可读。
 *
 * Code Logic:
 *   对 REQUIRED_TEXT_CONTRAST_PAIRS 解析 hex 并断言 ≥4.5。
 *
 * @param {Map<string, string>} values
 * @param {string} themeLabel
 * @returns {string[]}
 */
function analyzeThemeTextContrast(values, themeLabel) {
  /** @type {string[]} */
  const diagnostics = [];
  for (const [fgName, bgName] of REQUIRED_TEXT_CONTRAST_PAIRS) {
    const fg = resolveHexColor(fgName, values);
    const bg = resolveHexColor(bgName, values);
    if (!fg || !bg) {
      diagnostics.push(
        `tokens.css:0 contrast ${themeLabel} --${fgName}/--${bgName} unresolved`,
      );
      continue;
    }
    const ratio = contrastRatio(fg, bg);
    if (ratio + 1e-9 < MIN_NORMAL_TEXT_CONTRAST) {
      diagnostics.push(
        `tokens.css:0 contrast ${themeLabel} --${fgName}/--${bgName} ${ratio.toFixed(2)} < ${MIN_NORMAL_TEXT_CONTRAST}`,
      );
    }
  }
  return diagnostics;
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
 * 收集 color: var(--meta) 的选择器与行号。
 *
 * Business Logic:
 *   未评审的语义 --meta 文本必须失败，避免再次把低对比当正文。
 *
 * Code Logic:
 *   顺序扫描规则块，记录选择器；命中 color:var(--meta) 时输出 path:selector。
 *
 * @param {string} content
 * @returns {Array<{ selector: string, line: number }>}
 */
function collectMetaColorUsages(content) {
  const stripped = stripCssComments(content);
  /** @type {Array<{ selector: string, line: number }>} */
  const out = [];
  /** @type {string[]} */
  const selectorStack = [];
  let pendingSelector = '';
  let i = 0;
  while (i < stripped.length) {
    const ch = stripped[i];
    if (ch === '{') {
      const sel = pendingSelector.trim().replace(/\s+/g, ' ');
      selectorStack.push(sel);
      pendingSelector = '';
      i += 1;
      continue;
    }
    if (ch === '}') {
      selectorStack.pop();
      pendingSelector = '';
      i += 1;
      continue;
    }
    if (selectorStack.length === 0) {
      pendingSelector += ch;
      i += 1;
      continue;
    }
    // inside a rule body: look for color: var(--meta)（避免匹配 background-color）
    const slice = stripped.slice(i);
    const prev = i > 0 ? stripped[i - 1] : '';
    const boundaryOk = !/[a-zA-Z0-9_-]/.test(prev);
    const colorMatch = boundaryOk
      ? slice.match(/^color\s*:\s*var\(\s*--meta\s*\)\s*;?/)
      : null;
    if (colorMatch) {
      const selector = selectorStack[selectorStack.length - 1] || '';
      const line = stripped.slice(0, i).split('\n').length;
      out.push({ selector, line });
      i += colorMatch[0].length;
      continue;
    }
    i += 1;
  }
  return out;
}

/**
 * 分析 CSS 文件与 tokens 源是否满足 design token 合同。
 *
 * Business Logic:
 *   把「未知语义 token / 深色主题缺失 / 正文对比不足 / 未评审 meta 文本」
 *   转成可 CI 失败的诊断列表。
 *
 * Code Logic:
 *   解析 tokens 定义 → 主题双份 → 对比度 → 扫描 files 的 var 用法与 meta color；
 *   allowlist 结构变量放行；诊断格式 `file:line --token` 或 contrast/meta 文案。
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

  const valueMaps = parseTokenValueMaps(tokensSource);
  diagnostics.push(...analyzeThemeTextContrast(valueMaps.root, 'light'));
  diagnostics.push(...analyzeThemeTextContrast(valueMaps.dark, 'dark'));

  for (const file of files) {
    const usages = collectVarUsages(file.content);
    for (const { name, line } of usages) {
      if (RUNTIME_STRUCTURAL_ALLOWLIST.has(name)) continue;
      if (root.has(name)) continue;
      diagnostics.push(`${file.path}:${line} --${name}`);
    }

    const metaUsages = collectMetaColorUsages(file.content);
    for (const { selector, line } of metaUsages) {
      const key = `${file.path}:${selector}`;
      if (META_COLOR_ALLOWLIST.has(key)) continue;
      diagnostics.push(
        `${file.path}:${line} meta-color not allowlisted (${selector || '<unknown>'})`,
      );
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

export {
  REQUIRED_TEXT_CONTRAST_PAIRS,
  MIN_NORMAL_TEXT_CONTRAST,
  META_COLOR_ALLOWLIST,
  contrastRatio,
  resolveHexColor,
  parseTokenValueMaps,
  analyzeThemeTextContrast,
  collectMetaColorUsages,
};
