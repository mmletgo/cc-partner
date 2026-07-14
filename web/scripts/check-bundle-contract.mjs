/**
 * check-bundle-contract.mjs — 前端拆包预算与依赖泄漏合同守卫。
 *
 * Business Logic（为什么需要）:
 *   移动端首载不得携带 xterm/CodeMirror/Tiptap/Recharts；桌面/移动 initial graph
 *   需有可 CI 验证的 gzip 预算，避免后续回归把重型依赖重新打进入口。
 *
 * Code Logic（做什么）:
 *   读取 Vite 构建写出的 `dist/.vite/cc-bundle-contract.json`（entries + chunks），
 *   对每个入口沿静态 imports（排除 dynamicImports）求闭包，对闭包内 JS 文件做 gzip
 *   求和并比对预算；mobile 闭包的 moduleIds 匹配禁止列表则失败。导出纯函数供 fixture 测试。
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
 * 对 chunk 文件内容做 gzip 后求和。
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
 *   CI 需要单一函数同时覆盖预算与 mobile 依赖泄漏。
 *
 * Code Logic:
 *   对 entries.main / entries.mobile 求静态闭包 → gzip 求和 → 比预算；
 *   对 mobile 闭包聚合 moduleIds 做 forbidden 检查。
 *
 * @param {{
 *   entries?: Record<string, string>,
 *   chunks?: Record<string, {
 *     fileName?: string,
 *     imports?: string[],
 *     dynamicImports?: string[],
 *     moduleIds?: string[],
 *   }>,
 * }} contract
 * @param {{
 *   readFile: (fileName: string) => Buffer | string,
 *   budgets?: { main?: number, mobile?: number },
 * }} options
 * @returns {{
 *   diagnostics: string[],
 *   entryReports: Record<string, { entryFile: string, closure: string[], gzipBytes: number, budgetBytes: number }>,
 * }}
 */
export function analyzeBundleContract(contract, options) {
  const chunks = contract?.chunks ?? {};
  const entries = contract?.entries ?? {};
  const budgets = {
    main: options.budgets?.main ?? DESKTOP_MAIN_BUDGET_BYTES,
    mobile: options.budgets?.mobile ?? MOBILE_INITIAL_BUDGET_BYTES,
  };

  /** @type {string[]} */
  const diagnostics = [];
  /** @type {Record<string, { entryFile: string, closure: string[], gzipBytes: number, budgetBytes: number }>} */
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
    const budgetBytes = budgets[entryName];
    let gzipBytes = 0;
    try {
      gzipBytes = sumGzipBytes(closure, options.readFile);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      diagnostics.push(`${entryName} failed to read chunk files: ${message}`);
      continue;
    }
    entryReports[entryName] = {
      entryFile,
      closure,
      gzipBytes,
      budgetBytes,
    };
    if (gzipBytes > budgetBytes) {
      diagnostics.push(
        `${entryName} initial graph over budget: size=${gzipBytes}B budget=${budgetBytes}B (${formatBudgetKiB(budgetBytes)}) entry=${entryFile} chunks=${closure.length}`,
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
 * CLI 入口：读取 dist 合同与 chunk 文件并 exit 0/1。
 *
 * Business Logic:
 *   构建后必须失败于超预算或依赖泄漏，防止坏包进入 CI/发版。
 *
 * Code Logic:
 *   定位 web/dist/.vite/cc-bundle-contract.json，相对 dist 读 chunk，打印报告。
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

  /** @type {{ entries?: Record<string, string>, chunks?: Record<string, object> }} */
  let contract;
  try {
    contract = JSON.parse(readFileSync(contractPath, 'utf8'));
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    console.error(`failed to parse bundle contract: ${message}`);
    process.exit(1);
  }

  const result = analyzeBundleContract(contract, {
    readFile: (fileName) => readFileSync(resolve(distDir, fileName)),
  });

  for (const [entryName, report] of Object.entries(result.entryReports)) {
    const ok = report.gzipBytes <= report.budgetBytes ? 'OK' : 'OVER';
    console.log(
      `[${ok}] ${entryName}: gzip=${report.gzipBytes}B / budget=${report.budgetBytes}B (${formatBudgetKiB(report.budgetBytes)}) chunks=${report.closure.length} entry=${report.entryFile}`,
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
