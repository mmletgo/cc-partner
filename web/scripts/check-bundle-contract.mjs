/**
 * check-bundle-contract.mjs — 前端拆包预算与依赖泄漏合同守卫。
 *
 * Business Logic（为什么需要）:
 *   移动端首载不得携带 xterm/CodeMirror/Tiptap/Recharts；桌面/移动 initial graph
 *   需有可 CI 验证的 gzip 预算，避免后续回归把重型依赖重新打进入口。
 *   预算必须覆盖 JS 静态闭包 **与** 入口 HTML 直接引用的 CSS，CSS 增长同样触发门禁。
 *
 * Code Logic（做什么）:
 *   读取 Vite 构建写出的 `dist/.vite/cc-bundle-contract.json`（entries + chunks + entryStyles），
 *   必要时再从 dist 入口 HTML 解析 stylesheet href；对每个入口沿静态 imports（排除
 *   dynamicImports）求闭包，对闭包内 JS 与入口 CSS 分别 gzip 后求和并比对预算；
 *   mobile 闭包的 moduleIds 匹配禁止列表则失败。导出纯函数供 fixture 测试。
 *
 * Usage:
 *   node scripts/check-bundle-contract.mjs
 *   npm run check:bundle
 */

import { existsSync, readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { gzipSync } from 'node:zlib';

/** 桌面 main 入口 initial graph gzip 预算（字节）。 */
export const DESKTOP_MAIN_BUDGET_BYTES = 320 * 1024;

/** 移动端 mobile 入口 initial graph gzip 预算（字节）。 */
export const MOBILE_INITIAL_BUDGET_BYTES = 280 * 1024;

/**
 * mobile initial graph 禁止出现的依赖匹配规则。
 * 覆盖 @xterm/*、@tiptap/*、@uiw/react-codemirror、codemirror/@codemirror、recharts。
 */
export const MOBILE_FORBIDDEN_PATTERNS = [
  /(?:^|[/\\])@xterm[/\\]/i,
  /(?:^|[/\\])@tiptap[/\\]/i,
  /(?:^|[/\\])@uiw[/\\]react-codemirror(?:[/\\]|$)/i,
  /(?:^|[/\\])@codemirror[/\\]/i,
  /(?:^|[/\\])codemirror(?:[/\\]|$)/i,
  /(?:^|[/\\])recharts(?:[/\\]|$)/i,
];

/**
 * 从 HTML 中提取相对 stylesheet href。
 *
 * Business Logic:
 *   生产 HTML 通过 `<link rel="stylesheet">` 挂载首载 CSS；预算必须计入这些文件。
 *
 * Code Logic:
 *   扫描全部 `<link>` 标签，要求 rel=stylesheet，读取 href；去掉 leading `/`，
 *   忽略绝对 URL 与 data URL，去重保序。
 *
 * @param {string} html
 * @returns {string[]}
 */
export function extractStylesheetHrefs(html) {
  /** @type {string[]} */
  const hrefs = [];
  if (typeof html !== 'string' || html.length === 0) {
    return hrefs;
  }
  const linkTagRe = /<link\b[^>]*>/gi;
  let match = linkTagRe.exec(html);
  while (match) {
    const tag = match[0];
    if (/\brel\s*=\s*["']stylesheet["']/i.test(tag)) {
      const hrefMatch = tag.match(/\bhref\s*=\s*["']([^"']+)["']/i);
      if (hrefMatch) {
        let href = hrefMatch[1].trim();
        if (href && !/^(?:https?:)?\/\//i.test(href) && !href.startsWith('data:')) {
          href = href.replace(/^\//, '');
          if (href && !hrefs.includes(href)) {
            hrefs.push(href);
          }
        }
      }
    }
    match = linkTagRe.exec(html);
  }
  return hrefs;
}

/**
 * 规范化入口 CSS 文件列表（字符串、去重、保序）。
 *
 * Business Logic:
 *   合同与选项可能提供重复/空值，需稳定化为可 gzip 的路径列表。
 *
 * Code Logic:
 *   过滤非字符串与空串，按首次出现去重。
 *
 * @param {unknown} files
 * @returns {string[]}
 */
export function normalizeCssFiles(files) {
  if (!Array.isArray(files)) {
    return [];
  }
  /** @type {string[]} */
  const out = [];
  const seen = new Set();
  for (const item of files) {
    if (typeof item !== 'string') {
      continue;
    }
    const fileName = item.replace(/^\//, '').trim();
    if (!fileName || seen.has(fileName)) {
      continue;
    }
    seen.add(fileName);
    out.push(fileName);
  }
  return out;
}

/**
 * 沿静态 import 边收集 chunk 闭包。
 *
 * Business Logic:
 *   预算只计入口同步加载的图；动态 import 属于路由/面板懒加载，不得计入 initial。
 *
 * Code Logic:
 *   从 entryFile 出发 BFS，只跟随 chunks[id].imports；忽略 dynamicImports 与缺失节点。
 *
 * @param {string} entryFile chunk 文件名（相对 dist）
 * @param {Record<string, { imports?: string[] }>} chunks
 * @returns {Set<string>}
 */
export function collectStaticClosure(entryFile, chunks) {
  /** @type {Set<string>} */
  const visited = new Set();
  if (!entryFile || !chunks || !chunks[entryFile]) {
    return visited;
  }

  /** @type {string[]} */
  const queue = [entryFile];
  while (queue.length > 0) {
    const current = queue.shift();
    if (!current || visited.has(current)) {
      continue;
    }
    const chunk = chunks[current];
    if (!chunk) {
      continue;
    }
    visited.add(current);
    const imports = Array.isArray(chunk.imports) ? chunk.imports : [];
    for (const next of imports) {
      if (typeof next === 'string' && !visited.has(next)) {
        queue.push(next);
      }
    }
  }
  return visited;
}

/**
 * 对文件内容做 gzip 后求和。
 *
 * Business Logic:
 *   预算以传输体积近似值（gzip）衡量，避免 raw size 失真。
 *
 * Code Logic:
 *   对每个 fileName 调用 readFile 得到 Buffer/string，zlib.gzipSync 后累加 byteLength。
 *
 * @param {Iterable<string>} fileNames
 * @param {(fileName: string) => Buffer | string} readFile
 * @returns {number}
 */
export function sumGzipBytes(fileNames, readFile) {
  let total = 0;
  for (const fileName of fileNames) {
    const raw = readFile(fileName);
    const buffer = Buffer.isBuffer(raw) ? raw : Buffer.from(String(raw), 'utf8');
    total += gzipSync(buffer).byteLength;
  }
  return total;
}

/**
 * 在 moduleIds 中找出命中禁止规则的项。
 *
 * Business Logic:
 *   mobile 首载禁止编辑器/终端/图表重型包，需精确诊断模块 id。
 *
 * Code Logic:
 *   对每个 moduleId 跑 patterns，命中则保留原 id（去重保序）。
 *
 * @param {Iterable<string>} moduleIds
 * @param {RegExp[]} patterns
 * @returns {string[]}
 */
export function findForbiddenModules(moduleIds, patterns) {
  /** @type {string[]} */
  const hits = [];
  const seen = new Set();
  for (const id of moduleIds) {
    if (typeof id !== 'string' || seen.has(id)) {
      continue;
    }
    if (patterns.some((pattern) => pattern.test(id))) {
      seen.add(id);
      hits.push(id);
    }
  }
  return hits;
}

/**
 * 把字节预算格式化为整数 KiB 标签。
 *
 * Business Logic:
 *   诊断输出与文档预算（320/280 KiB）对齐，便于人读。
 *
 * Code Logic:
 *   向下取整到 KiB。
 *
 * @param {number} bytes
 * @returns {string}
 */
export function formatBudgetKiB(bytes) {
  return `${Math.floor(bytes / 1024)} KiB`;
}

/**
 * 分析 bundle 合同，返回 entry 报告与诊断列表。
 *
 * Business Logic:
 *   CI 需要单一函数同时覆盖预算（JS 静态闭包 + 入口 HTML CSS）与 mobile 依赖泄漏。
 *
 * Code Logic:
 *   对 entries.main / entries.mobile 求静态闭包 → JS gzip；
 *   取 entryStyles（options 优先，否则 contract）→ CSS gzip；
 *   total = js + css 比预算；对 mobile 闭包聚合 moduleIds 做 forbidden 检查。
 *
 * @param {{
 *   entries?: Record<string, string>,
 *   chunks?: Record<string, {
 *     fileName?: string,
 *     imports?: string[],
 *     dynamicImports?: string[],
 *     moduleIds?: string[],
 *   }>,
 *   entryStyles?: Record<string, string[]>,
 * }} contract
 * @param {{
 *   readFile: (fileName: string) => Buffer | string,
 *   budgets?: { main?: number, mobile?: number },
 *   entryStyles?: Record<string, string[]>,
 * }} options
 * @returns {{
 *   diagnostics: string[],
 *   entryReports: Record<string, {
 *     entryFile: string,
 *     closure: string[],
 *     cssFiles: string[],
 *     jsGzipBytes: number,
 *     cssGzipBytes: number,
 *     gzipBytes: number,
 *     budgetBytes: number,
 *   }>,
 * }}
 */
export function analyzeBundleContract(contract, options) {
  const chunks = contract?.chunks ?? {};
  const entries = contract?.entries ?? {};
  const budgets = {
    main: options.budgets?.main ?? DESKTOP_MAIN_BUDGET_BYTES,
    mobile: options.budgets?.mobile ?? MOBILE_INITIAL_BUDGET_BYTES,
  };
  /** @type {Record<string, string[]>} */
  const entryStylesSource = options.entryStyles ?? contract?.entryStyles ?? {};

  /** @type {string[]} */
  const diagnostics = [];
  /** @type {Record<string, {
   *   entryFile: string,
   *   closure: string[],
   *   cssFiles: string[],
   *   jsGzipBytes: number,
   *   cssGzipBytes: number,
   *   gzipBytes: number,
   *   budgetBytes: number,
   * }>} */
  const entryReports = {};

  for (const entryName of ['main', 'mobile']) {
    const entryFile = entries[entryName];
    if (!entryFile || typeof entryFile !== 'string') {
      diagnostics.push(`${entryName} entry missing from bundle contract`);
      continue;
    }
    const closureSet = collectStaticClosure(entryFile, chunks);
    if (closureSet.size === 0) {
      diagnostics.push(`${entryName} initial graph is empty (entry=${entryFile})`);
      continue;
    }
    const closure = [...closureSet].sort();
    const cssFiles = normalizeCssFiles(entryStylesSource[entryName]);
    const budgetBytes = budgets[entryName];
    let jsGzipBytes = 0;
    let cssGzipBytes = 0;
    try {
      jsGzipBytes = sumGzipBytes(closure, options.readFile);
      cssGzipBytes = cssFiles.length > 0 ? sumGzipBytes(cssFiles, options.readFile) : 0;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      diagnostics.push(`${entryName} failed to read chunk/css files: ${message}`);
      continue;
    }
    const gzipBytes = jsGzipBytes + cssGzipBytes;
    entryReports[entryName] = {
      entryFile,
      closure,
      cssFiles,
      jsGzipBytes,
      cssGzipBytes,
      gzipBytes,
      budgetBytes,
    };
    if (gzipBytes > budgetBytes) {
      diagnostics.push(
        `${entryName} initial graph over budget: size=${gzipBytes}B (js=${jsGzipBytes}B css=${cssGzipBytes}B) budget=${budgetBytes}B (${formatBudgetKiB(budgetBytes)}) entry=${entryFile} chunks=${closure.length} cssFiles=${cssFiles.length}`,
      );
    }
  }

  // mobile forbidden：仅检查 mobile 静态闭包内 moduleIds
  const mobileEntry = entries.mobile;
  if (mobileEntry && chunks[mobileEntry]) {
    const mobileClosure = collectStaticClosure(mobileEntry, chunks);
    /** @type {string[]} */
    const moduleIds = [];
    for (const fileName of mobileClosure) {
      const ids = chunks[fileName]?.moduleIds;
      if (Array.isArray(ids)) {
        for (const id of ids) {
          if (typeof id === 'string') {
            moduleIds.push(id);
          }
        }
      }
    }
    const forbidden = findForbiddenModules(moduleIds, MOBILE_FORBIDDEN_PATTERNS);
    for (const id of forbidden) {
      diagnostics.push(`mobile initial graph forbidden module: ${id}`);
    }
  }

  return { diagnostics, entryReports };
}

/**
 * CLI 入口：读取 dist 合同与 chunk/CSS 文件并 exit 0/1。
 *
 * Business Logic:
 *   构建后必须失败于超预算或依赖泄漏，防止坏包进入 CI/发版。
 *
 * Code Logic:
 *   定位 web/dist/.vite/cc-bundle-contract.json；优先用 dist 入口 HTML 解析 stylesheet
 *   作为 entryStyles 真源（覆盖合同内列表），相对 dist 读文件，打印 JS/CSS 分项报告。
 */
function main() {
  const scriptDir = dirname(fileURLToPath(import.meta.url));
  const webRoot = resolve(scriptDir, '..');
  const distDir = resolve(webRoot, 'dist');
  const contractPath = resolve(distDir, '.vite/cc-bundle-contract.json');

  if (!existsSync(contractPath)) {
    console.error(
      `bundle contract missing: ${contractPath}\nRun "npm run build" first so Vite emits cc-bundle-contract.json.`,
    );
    process.exit(1);
  }

  /** @type {{
   *   entries?: Record<string, string>,
   *   chunks?: Record<string, object>,
   *   entryStyles?: Record<string, string[]>,
   * }} */
  let contract;
  try {
    contract = JSON.parse(readFileSync(contractPath, 'utf8'));
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    console.error(`failed to parse bundle contract: ${message}`);
    process.exit(1);
  }

  /** @type {Record<string, string[]>} */
  const entryStyles = { ...(contract.entryStyles ?? {}) };
  /** @type {Array<[string, string]>} */
  const htmlEntryMap = [
    ['main', 'index.html'],
    ['mobile', 'mobile.html'],
  ];
  for (const [entryName, htmlName] of htmlEntryMap) {
    const htmlPath = resolve(distDir, htmlName);
    if (!existsSync(htmlPath)) {
      continue;
    }
    try {
      entryStyles[entryName] = extractStylesheetHrefs(readFileSync(htmlPath, 'utf8'));
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      console.error(`failed to read entry HTML ${htmlName}: ${message}`);
      process.exit(1);
    }
  }

  const result = analyzeBundleContract(contract, {
    readFile: (fileName) => readFileSync(resolve(distDir, fileName)),
    entryStyles,
  });

  for (const [entryName, report] of Object.entries(result.entryReports)) {
    const ok = report.gzipBytes <= report.budgetBytes ? 'OK' : 'OVER';
    console.log(
      `[${ok}] ${entryName}: gzip=${report.gzipBytes}B (js=${report.jsGzipBytes}B css=${report.cssGzipBytes}B) / budget=${report.budgetBytes}B (${formatBudgetKiB(report.budgetBytes)}) chunks=${report.closure.length} cssFiles=${report.cssFiles.length} entry=${report.entryFile}`,
    );
  }

  if (result.diagnostics.length > 0) {
    console.error('Bundle contract failed:');
    for (const diagnostic of result.diagnostics) {
      console.error(`  - ${diagnostic}`);
    }
    process.exit(1);
  }

  console.log('Bundle contract passed');
  process.exit(0);
}

const isDirectRun = process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isDirectRun) {
  main();
}
