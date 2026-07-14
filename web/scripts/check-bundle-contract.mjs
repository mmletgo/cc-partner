/**
 * check-bundle-contract.mjs — 前端拆包预算与依赖泄漏合同守卫。
 *
 * Business Logic（为什么需要）:
 *   移动端首载不得携带 xterm/CodeMirror/Tiptap/Recharts；桌面/移动 initial graph
 *   需有可 CI 验证的 gzip 预算，避免后续回归把重型依赖重新打进入口。
 *   预算必须覆盖 JS 静态闭包 **与** 入口 HTML 直接引用的 CSS，CSS 增长同样触发门禁。
 *   此外还需约束单 lazy chunk、全部 runtime JS 与 sourcemap，并用 baseline ratchet
 *   禁止在最终预算内继续膨胀；baseline 不得抬高最终硬顶。
 *
 * Code Logic（做什么）:
 *   读取 Vite 构建写出的 `dist/.vite/cc-bundle-contract.json`（entries + chunks + entryStyles），
 *   必要时再从 dist 入口 HTML 解析 stylesheet href；对每个入口沿静态 imports（排除
 *   dynamicImports）求闭包，对闭包内 JS 与入口 CSS 分别 gzip 后求和并比对预算；
 *   对非 initial 的 lazy chunk、全部 runtime JS gzip、dist `*.map` raw 体积做硬顶校验；
 *   若存在 baseline JSON，则对各指标取 min(baseline, final) 作为有效上限。
 *   mobile 闭包的 moduleIds 匹配禁止列表则失败。导出纯函数供 fixture 测试。
 *
 * Usage:
 *   node scripts/check-bundle-contract.mjs
 *   node scripts/check-bundle-contract.mjs --self-test
 *   node scripts/check-bundle-contract.mjs --write-baseline
 *   npm run check:bundle
 */

import { existsSync, readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { gzipSync } from 'node:zlib';
import { spawnSync } from 'node:child_process';

/** 桌面 main 入口 initial graph gzip 预算（字节）。 */
export const DESKTOP_MAIN_BUDGET_BYTES = 320 * 1024;

/** 移动端 mobile 入口 initial graph gzip 预算（字节）。 */
export const MOBILE_INITIAL_BUDGET_BYTES = 280 * 1024;

/** 单个 lazy JS chunk gzip 最终硬顶（字节）。 */
export const LAZY_CHUNK_BUDGET_BYTES = 700 * 1024;

/** 全部 runtime JS gzip 最终硬顶（字节）。 */
export const TOTAL_RUNTIME_JS_BUDGET_BYTES = 1400 * 1024;

/** 生产 sourcemap raw 总大小最终硬顶（字节）；无 .map 则视为 0 通过。 */
export const SOURCEMAP_BUDGET_BYTES = 2 * 1024 * 1024;

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
 * 最终硬顶指标集合（baseline 不得抬高这些值）。
 *
 * Business Logic:
 *   最终预算始终生效；ratchet 只可收紧不可放宽。
 *
 * Code Logic:
 *   返回固定字节上限映射。
 *
 * @returns {{
 *   mainInitialGzipBytes: number,
 *   mobileInitialGzipBytes: number,
 *   lazyChunkGzipBytes: number,
 *   totalRuntimeJsGzipBytes: number,
 *   sourcemapRawBytes: number,
 * }}
 */
export function getFinalBudgetTargets() {
  return {
    mainInitialGzipBytes: DESKTOP_MAIN_BUDGET_BYTES,
    mobileInitialGzipBytes: MOBILE_INITIAL_BUDGET_BYTES,
    lazyChunkGzipBytes: LAZY_CHUNK_BUDGET_BYTES,
    totalRuntimeJsGzipBytes: TOTAL_RUNTIME_JS_BUDGET_BYTES,
    sourcemapRawBytes: SOURCEMAP_BUDGET_BYTES,
  };
}

/**
 * 计算某指标的有效上限 = min(baseline, final)；无 baseline 时用 final。
 *
 * Business Logic:
 *   baseline ratchet 禁止增长，但不能成为第二套可抬高的最终阈值。
 *
 * Code Logic:
 *   若 baseline 为有限非负 number，取 Math.min；否则返回 final。
 *
 * @param {number} finalBytes
 * @param {unknown} baselineBytes
 * @returns {number}
 */
export function effectiveCeiling(finalBytes, baselineBytes) {
  if (typeof baselineBytes === 'number' && Number.isFinite(baselineBytes) && baselineBytes >= 0) {
    return Math.min(finalBytes, baselineBytes);
  }
  return finalBytes;
}

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
 * 汇总 top N 最大 chunk（按 gzip 降序）。
 *
 * Business Logic:
 *   CI 失败时需要快速指出最大 chunk，便于拆包。
 *
 * Code Logic:
 *   按 gzipBytes 降序排序后截取 limit。
 *
 * @param {Array<{ fileName: string, gzipBytes: number }>} chunkReports
 * @param {number} [limit]
 * @returns {Array<{ fileName: string, gzipBytes: number }>}
 */
export function topChunksByGzip(chunkReports, limit = 5) {
  return [...chunkReports]
    .sort((a, b) => b.gzipBytes - a.gzipBytes || a.fileName.localeCompare(b.fileName))
    .slice(0, Math.max(0, limit));
}

/**
 * 解析 baseline JSON 为可比较的 metrics（非法字段忽略）。
 *
 * Business Logic:
 *   baseline 文件是临时 ratchet 事实，需容错读取。
 *
 * Code Logic:
 *   仅接受 metrics 下的有限非负 number。
 *
 * @param {unknown} raw
 * @returns {{
 *   mainInitialGzipBytes?: number,
 *   mobileInitialGzipBytes?: number,
 *   maxLazyChunkGzipBytes?: number,
 *   totalRuntimeJsGzipBytes?: number,
 *   sourcemapRawBytes?: number,
 * } | null}
 */
export function parseBaselineMetrics(raw) {
  if (!raw || typeof raw !== 'object') {
    return null;
  }
  const metrics = /** @type {Record<string, unknown>} */ (raw).metrics;
  if (!metrics || typeof metrics !== 'object') {
    return null;
  }
  /** @type {Record<string, number>} */
  const out = {};
  for (const key of [
    'mainInitialGzipBytes',
    'mobileInitialGzipBytes',
    'maxLazyChunkGzipBytes',
    'totalRuntimeJsGzipBytes',
    'sourcemapRawBytes',
  ]) {
    const value = /** @type {Record<string, unknown>} */ (metrics)[key];
    if (typeof value === 'number' && Number.isFinite(value) && value >= 0) {
      out[key] = value;
    }
  }
  return out;
}

/**
 * 分析 bundle 合同，返回 entry 报告、扩展预算指标与诊断列表。
 *
 * Business Logic:
 *   CI 需要单一函数覆盖 initial 预算、lazy/total/sourcemap 硬顶、baseline ratchet
 *   与 mobile 依赖泄漏。
 *
 * Code Logic:
 *   对 entries.main / entries.mobile 求静态闭包 → JS gzip；
 *   取 entryStyles → CSS gzip；total = js + css 比 initial 预算；
 *   对非 initial chunk 检查 lazy 硬顶；对全部 chunk 求和查 total runtime JS；
 *   sourcemap 用 options 提供的 raw 总字节；baseline 用 effectiveCeiling 收紧。
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
 *   budgets?: { main?: number, mobile?: number, lazyChunk?: number, totalRuntimeJs?: number, sourcemap?: number },
 *   entryStyles?: Record<string, string[]>,
 *   sourcemapRawBytes?: number,
 *   baseline?: {
 *     mainInitialGzipBytes?: number,
 *     mobileInitialGzipBytes?: number,
 *     maxLazyChunkGzipBytes?: number,
 *     totalRuntimeJsGzipBytes?: number,
 *     sourcemapRawBytes?: number,
 *   } | null,
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
 *     baselineBytes: number | null,
 *     finalBudgetBytes: number,
 *   }>,
 *   chunkReports: Array<{ fileName: string, gzipBytes: number, isInitial: boolean }>,
 *   metrics: {
 *     mainInitialGzipBytes: number | null,
 *     mobileInitialGzipBytes: number | null,
 *     maxLazyChunkGzipBytes: number,
 *     totalRuntimeJsGzipBytes: number,
 *     sourcemapRawBytes: number,
 *   },
 *   ceilings: {
 *     mainInitialGzipBytes: number,
 *     mobileInitialGzipBytes: number,
 *     lazyChunkGzipBytes: number,
 *     totalRuntimeJsGzipBytes: number,
 *     sourcemapRawBytes: number,
 *   },
 *   finals: ReturnType<typeof getFinalBudgetTargets>,
 * }}
 */
export function analyzeBundleContract(contract, options) {
  const chunks = contract?.chunks ?? {};
  const entries = contract?.entries ?? {};
  const finals = getFinalBudgetTargets();
  const finalBudgets = {
    main: options.budgets?.main ?? finals.mainInitialGzipBytes,
    mobile: options.budgets?.mobile ?? finals.mobileInitialGzipBytes,
    lazyChunk: options.budgets?.lazyChunk ?? finals.lazyChunkGzipBytes,
    totalRuntimeJs: options.budgets?.totalRuntimeJs ?? finals.totalRuntimeJsGzipBytes,
    sourcemap: options.budgets?.sourcemap ?? finals.sourcemapRawBytes,
  };
  const baseline = options.baseline ?? null;
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
   *   baselineBytes: number | null,
   *   finalBudgetBytes: number,
   * }>} */
  const entryReports = {};

  /** @type {Set<string>} */
  const initialUnion = new Set();

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
    for (const fileName of closureSet) {
      initialUnion.add(fileName);
    }
    const closure = [...closureSet].sort();
    const cssFiles = normalizeCssFiles(entryStylesSource[entryName]);
    const finalBudgetBytes = finalBudgets[entryName];
    const baselineKey =
      entryName === 'main' ? 'mainInitialGzipBytes' : 'mobileInitialGzipBytes';
    const baselineBytes =
      baseline && typeof baseline[baselineKey] === 'number' ? baseline[baselineKey] : null;
    const budgetBytes = effectiveCeiling(finalBudgetBytes, baselineBytes);
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
      baselineBytes,
      finalBudgetBytes,
    };
    if (gzipBytes > budgetBytes) {
      diagnostics.push(
        `${entryName} initial graph over budget: actual=${gzipBytes}B (js=${jsGzipBytes}B css=${cssGzipBytes}B) baseline=${baselineBytes ?? 'n/a'} final=${finalBudgetBytes}B (${formatBudgetKiB(finalBudgetBytes)}) ceiling=${budgetBytes}B entry=${entryFile} chunks=${closure.length} cssFiles=${cssFiles.length}`,
      );
    }
  }

  /** @type {Array<{ fileName: string, gzipBytes: number, isInitial: boolean }>} */
  const chunkReports = [];
  let totalRuntimeJsGzipBytes = 0;
  let maxLazyChunkGzipBytes = 0;
  const chunkFileNames = Object.keys(chunks).sort();
  for (const fileName of chunkFileNames) {
    let gzipBytes = 0;
    try {
      gzipBytes = sumGzipBytes([fileName], options.readFile);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      diagnostics.push(`failed to read chunk ${fileName}: ${message}`);
      continue;
    }
    const isInitial = initialUnion.has(fileName);
    chunkReports.push({ fileName, gzipBytes, isInitial });
    totalRuntimeJsGzipBytes += gzipBytes;
    if (!isInitial) {
      maxLazyChunkGzipBytes = Math.max(maxLazyChunkGzipBytes, gzipBytes);
      const lazyCeiling = effectiveCeiling(
        finalBudgets.lazyChunk,
        baseline?.maxLazyChunkGzipBytes,
      );
      if (gzipBytes > lazyCeiling) {
        diagnostics.push(
          `lazy chunk over budget: file=${fileName} actual=${gzipBytes}B baseline=${baseline?.maxLazyChunkGzipBytes ?? 'n/a'} final=${finalBudgets.lazyChunk}B (${formatBudgetKiB(finalBudgets.lazyChunk)}) ceiling=${lazyCeiling}B`,
        );
      }
    }
  }

  const totalJsCeiling = effectiveCeiling(
    finalBudgets.totalRuntimeJs,
    baseline?.totalRuntimeJsGzipBytes,
  );
  if (totalRuntimeJsGzipBytes > totalJsCeiling) {
    diagnostics.push(
      `total runtime JS over budget: actual=${totalRuntimeJsGzipBytes}B baseline=${baseline?.totalRuntimeJsGzipBytes ?? 'n/a'} final=${finalBudgets.totalRuntimeJs}B (${formatBudgetKiB(finalBudgets.totalRuntimeJs)}) ceiling=${totalJsCeiling}B chunks=${chunkReports.length}`,
    );
  }

  const sourcemapRawBytes =
    typeof options.sourcemapRawBytes === 'number' && Number.isFinite(options.sourcemapRawBytes)
      ? Math.max(0, options.sourcemapRawBytes)
      : 0;
  const sourcemapCeiling = effectiveCeiling(finalBudgets.sourcemap, baseline?.sourcemapRawBytes);
  if (sourcemapRawBytes > sourcemapCeiling) {
    diagnostics.push(
      `sourcemap over budget: actual=${sourcemapRawBytes}B baseline=${baseline?.sourcemapRawBytes ?? 'n/a'} final=${finalBudgets.sourcemap}B (${formatBudgetKiB(finalBudgets.sourcemap)}) ceiling=${sourcemapCeiling}B (publish no .map or keep total ≤2 MiB)`,
    );
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

  return {
    diagnostics,
    entryReports,
    chunkReports,
    metrics: {
      mainInitialGzipBytes: entryReports.main?.gzipBytes ?? null,
      mobileInitialGzipBytes: entryReports.mobile?.gzipBytes ?? null,
      maxLazyChunkGzipBytes,
      totalRuntimeJsGzipBytes,
      sourcemapRawBytes,
    },
    ceilings: {
      mainInitialGzipBytes: effectiveCeiling(
        finalBudgets.main,
        baseline?.mainInitialGzipBytes,
      ),
      mobileInitialGzipBytes: effectiveCeiling(
        finalBudgets.mobile,
        baseline?.mobileInitialGzipBytes,
      ),
      lazyChunkGzipBytes: effectiveCeiling(
        finalBudgets.lazyChunk,
        baseline?.maxLazyChunkGzipBytes,
      ),
      totalRuntimeJsGzipBytes: totalJsCeiling,
      sourcemapRawBytes: sourcemapCeiling,
    },
    finals: {
      mainInitialGzipBytes: finalBudgets.main,
      mobileInitialGzipBytes: finalBudgets.mobile,
      lazyChunkGzipBytes: finalBudgets.lazyChunk,
      totalRuntimeJsGzipBytes: finalBudgets.totalRuntimeJs,
      sourcemapRawBytes: finalBudgets.sourcemap,
    },
  };
}

/**
 * 递归收集目录下全部 `.map` 文件 raw 总字节。
 *
 * Business Logic:
 *   生产 map 不得随 dist 膨胀；无 map 则 0。
 *
 * Code Logic:
 *   DFS 跳过 node_modules；对 `.map` 累加 size。
 *
 * @param {string} dir
 * @returns {{ totalBytes: number, files: string[] }}
 */
export function collectSourcemapRawBytes(dir) {
  /** @type {string[]} */
  const files = [];
  let totalBytes = 0;
  if (!existsSync(dir)) {
    return { totalBytes: 0, files };
  }

  /**
   * @param {string} current
   */
  function walk(current) {
    for (const entry of readdirSync(current)) {
      if (entry === 'node_modules') continue;
      const full = join(current, entry);
      const st = statSync(full);
      if (st.isDirectory()) {
        walk(full);
      } else if (entry.endsWith('.map')) {
        files.push(full);
        totalBytes += st.size;
      }
    }
  }

  walk(dir);
  return { totalBytes, files };
}

/**
 * 解析 git HEAD 短 SHA（失败时返回 unknown）。
 *
 * Business Logic:
 *   baseline 需要记录采集时的 commit 以便审计。
 *
 * Code Logic:
 *   spawn `git rev-parse --short HEAD`。
 *
 * @param {string} cwd
 * @returns {string}
 */
function resolveGitShortSha(cwd) {
  try {
    const result = spawnSync('git', ['rev-parse', '--short', 'HEAD'], {
      cwd,
      encoding: 'utf8',
    });
    if (result.status === 0 && result.stdout) {
      return result.stdout.trim();
    }
  } catch {
    // ignore
  }
  return 'unknown';
}

/**
 * 打印失败诊断：actual / baseline / final / top5 chunks。
 *
 * Business Logic:
 *   CI 失败需要一眼看出超限指标与最大拆包目标。
 *
 * Code Logic:
 *   遍历 diagnostics；附 metrics 摘要与 topChunks。
 *
 * @param {ReturnType<typeof analyzeBundleContract>} result
 * @param {{ baselinePath?: string | null }} [meta]
 */
function printFailureReport(result, meta = {}) {
  console.error('Bundle contract failed:');
  for (const diagnostic of result.diagnostics) {
    console.error(`  - ${diagnostic}`);
  }
  console.error('');
  console.error('Metrics (actual / baseline / final / ceiling):');
  const b = meta.baselinePath ? 'loaded' : 'n/a';
  const pairs = [
    ['mainInitial', result.metrics.mainInitialGzipBytes, result.ceilings.mainInitialGzipBytes, result.finals.mainInitialGzipBytes],
    ['mobileInitial', result.metrics.mobileInitialGzipBytes, result.ceilings.mobileInitialGzipBytes, result.finals.mobileInitialGzipBytes],
    ['maxLazyChunk', result.metrics.maxLazyChunkGzipBytes, result.ceilings.lazyChunkGzipBytes, result.finals.lazyChunkGzipBytes],
    ['totalRuntimeJs', result.metrics.totalRuntimeJsGzipBytes, result.ceilings.totalRuntimeJsGzipBytes, result.finals.totalRuntimeJsGzipBytes],
    ['sourcemapRaw', result.metrics.sourcemapRawBytes, result.ceilings.sourcemapRawBytes, result.finals.sourcemapRawBytes],
  ];
  for (const [name, actual, ceiling, finalBytes] of pairs) {
    console.error(
      `  - ${name}: actual=${actual ?? 'n/a'}B ceiling=${ceiling}B final=${finalBytes}B (${formatBudgetKiB(finalBytes)}) baseline=${b}`,
    );
  }
  const top = topChunksByGzip(result.chunkReports, 5);
  console.error('');
  console.error('Top 5 chunks by gzip:');
  if (top.length === 0) {
    console.error('  (none)');
  } else {
    for (const chunk of top) {
      console.error(`  - ${chunk.fileName}: ${chunk.gzipBytes}B (${formatBudgetKiB(chunk.gzipBytes)})`);
    }
  }
}

/**
 * 将当前 metrics 写成 baseline JSON。
 *
 * Business Logic:
 *   首次启用 ratchet 时固化当前体积，禁止后续无计划增长。
 *
 * Code Logic:
 *   写入 schemaVersion/commit/capturedAt/finalTargets/metrics。
 *
 * @param {string} baselinePath
 * @param {ReturnType<typeof analyzeBundleContract>['metrics']} metrics
 * @param {string} commit
 */
function writeBaselineFile(baselinePath, metrics, commit) {
  const payload = {
    schemaVersion: 1,
    commit,
    capturedAt: new Date().toISOString(),
    finalTargets: getFinalBudgetTargets(),
    metrics: {
      mainInitialGzipBytes: metrics.mainInitialGzipBytes ?? 0,
      mobileInitialGzipBytes: metrics.mobileInitialGzipBytes ?? 0,
      maxLazyChunkGzipBytes: metrics.maxLazyChunkGzipBytes,
      totalRuntimeJsGzipBytes: metrics.totalRuntimeJsGzipBytes,
      sourcemapRawBytes: metrics.sourcemapRawBytes,
    },
  };
  writeFileSync(baselinePath, `${JSON.stringify(payload, null, 2)}\n`, 'utf8');
}

/**
 * 运行独立 fixture 测试文件（`--self-test`）。
 *
 * Business Logic:
 *   计划要求脚本支持 `--self-test`；与既有 `node --test` 共用同一套断言。
 *
 * Code Logic:
 *   spawn `node --test` 指向同目录 test 文件。
 *
 * @param {string} scriptDir
 * @returns {number}
 */
function runSelfTest(scriptDir) {
  const testFile = resolve(scriptDir, 'check-bundle-contract.test.mjs');
  const result = spawnSync(process.execPath, ['--test', testFile], {
    cwd: resolve(scriptDir, '..'),
    stdio: 'inherit',
  });
  return result.status ?? 1;
}

/**
 * CLI 入口：读取 dist 合同与 chunk/CSS/map 文件并 exit 0/1。
 *
 * Business Logic:
 *   构建后必须失败于超预算或依赖泄漏，防止坏包进入 CI/发版。
 *
 * Code Logic:
 *   定位 web/dist/.vite/cc-bundle-contract.json；优先用 dist 入口 HTML 解析 stylesheet
 *   作为 entryStyles 真源；扫描 dist `*.map`；可选读取 baseline / 写 baseline。
 */
function main(argv = process.argv.slice(2)) {
  const scriptDir = dirname(fileURLToPath(import.meta.url));
  const webRoot = resolve(scriptDir, '..');
  const repoRoot = resolve(webRoot, '..');

  if (argv.includes('--self-test')) {
    process.exit(runSelfTest(scriptDir));
  }

  const writeBaseline = argv.includes('--write-baseline');
  const distDir = resolve(webRoot, 'dist');
  const contractPath = resolve(distDir, '.vite/cc-bundle-contract.json');
  // 优先 web/scripts，其次仓库根 scripts（计划路径兼容）
  const baselineCandidates = [
    resolve(scriptDir, 'bundle-budget-baseline.json'),
    resolve(repoRoot, 'scripts/bundle-budget-baseline.json'),
  ];
  const baselinePath = baselineCandidates.find((p) => existsSync(p)) ?? baselineCandidates[0];

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

  /** @type {ReturnType<typeof parseBaselineMetrics>} */
  let baseline = null;
  if (!writeBaseline && existsSync(baselinePath)) {
    try {
      baseline = parseBaselineMetrics(JSON.parse(readFileSync(baselinePath, 'utf8')));
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      console.error(`failed to parse baseline ${baselinePath}: ${message}`);
      process.exit(1);
    }
  }

  const sourcemap = collectSourcemapRawBytes(distDir);

  const result = analyzeBundleContract(contract, {
    readFile: (fileName) => readFileSync(resolve(distDir, fileName)),
    entryStyles,
    sourcemapRawBytes: sourcemap.totalBytes,
    baseline,
  });

  for (const [entryName, report] of Object.entries(result.entryReports)) {
    const ok = report.gzipBytes <= report.budgetBytes ? 'OK' : 'OVER';
    console.log(
      `[${ok}] ${entryName}: actual=${report.gzipBytes}B (js=${report.jsGzipBytes}B css=${report.cssGzipBytes}B) baseline=${report.baselineBytes ?? 'n/a'} final=${report.finalBudgetBytes}B (${formatBudgetKiB(report.finalBudgetBytes)}) ceiling=${report.budgetBytes}B chunks=${report.closure.length} cssFiles=${report.cssFiles.length} entry=${report.entryFile}`,
    );
  }
  console.log(
    `[${result.metrics.maxLazyChunkGzipBytes <= result.ceilings.lazyChunkGzipBytes ? 'OK' : 'OVER'}] maxLazyChunk: actual=${result.metrics.maxLazyChunkGzipBytes}B baseline=${baseline?.maxLazyChunkGzipBytes ?? 'n/a'} final=${result.finals.lazyChunkGzipBytes}B (${formatBudgetKiB(result.finals.lazyChunkGzipBytes)}) ceiling=${result.ceilings.lazyChunkGzipBytes}B`,
  );
  console.log(
    `[${result.metrics.totalRuntimeJsGzipBytes <= result.ceilings.totalRuntimeJsGzipBytes ? 'OK' : 'OVER'}] totalRuntimeJs: actual=${result.metrics.totalRuntimeJsGzipBytes}B baseline=${baseline?.totalRuntimeJsGzipBytes ?? 'n/a'} final=${result.finals.totalRuntimeJsGzipBytes}B (${formatBudgetKiB(result.finals.totalRuntimeJsGzipBytes)}) ceiling=${result.ceilings.totalRuntimeJsGzipBytes}B`,
  );
  console.log(
    `[${result.metrics.sourcemapRawBytes <= result.ceilings.sourcemapRawBytes ? 'OK' : 'OVER'}] sourcemapRaw: actual=${result.metrics.sourcemapRawBytes}B maps=${sourcemap.files.length} baseline=${baseline?.sourcemapRawBytes ?? 'n/a'} final=${result.finals.sourcemapRawBytes}B (${formatBudgetKiB(result.finals.sourcemapRawBytes)}) ceiling=${result.ceilings.sourcemapRawBytes}B`,
  );

  if (writeBaseline) {
    // 写到 web/scripts（与 checker 同目录）；同时镜像到根 scripts 以兼容计划路径
    const webBaseline = resolve(scriptDir, 'bundle-budget-baseline.json');
    const rootBaseline = resolve(repoRoot, 'scripts/bundle-budget-baseline.json');
    const commit = resolveGitShortSha(repoRoot);
    writeBaselineFile(webBaseline, result.metrics, commit);
    writeBaselineFile(rootBaseline, result.metrics, commit);
    console.log(`Wrote baseline: ${relative(repoRoot, webBaseline)} and ${relative(repoRoot, rootBaseline)} (commit=${commit})`);
  }

  if (result.diagnostics.length > 0) {
    printFailureReport(result, {
      baselinePath: baseline ? baselinePath : null,
    });
    process.exit(1);
  }

  console.log('Bundle contract passed');
  process.exit(0);
}

const isDirectRun = process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isDirectRun) {
  main();
}
