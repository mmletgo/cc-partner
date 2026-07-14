#!/usr/bin/env node
/**
 * check-module-boundaries.mjs — 源模块规模 no-growth / 软硬阈值 ratchet。
 *
 * Business Logic（为什么需要）:
 *   Orchestrator/Workbench/Settings 等超大源文件继续增长会阻塞 Task 8 拆分与评审。
 *   CI 需要只读门禁：禁止受基线治理文件增长，并对 TS/Rust 施加软/硬行数上限，
 *   硬超限必须有带 owner/reason/expiresAt 的临时豁免，且最长 90 天。
 *
 * Code Logic（做什么）:
 *   扫描 git 跟踪的 .ts/.tsx/.rs（排除 test/dist/vendor/generated/node_modules/target），
 *   对照 scripts/module-boundary-baseline.json 做 no-growth 比较；
 *   软/硬阈值：TS/TSX 1000/1500，Rust 2500/5000。
 *   新文件不得超过软上限；硬上限仅允许基线内已有超硬文件（且豁免有效）。
 *   --write-baseline 写基线；--self-test 跑内嵌 fixture；普通 CI 只读不写。
 *
 * Usage:
 *   node scripts/check-module-boundaries.mjs
 *   node scripts/check-module-boundaries.mjs --write-baseline
 *   node scripts/check-module-boundaries.mjs --self-test
 *   npm run check:modules   # from web/
 */

import { execFileSync } from 'node:child_process';
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
const REPO_ROOT = resolve(__dirname, '..');
const DEFAULT_BASELINE_PATH = join(REPO_ROOT, 'scripts', 'module-boundary-baseline.json');

/** 软/硬行数上限（按扩展名）。 */
export const LINE_LIMITS = Object.freeze({
  ts: Object.freeze({ soft: 1000, hard: 1500 }),
  tsx: Object.freeze({ soft: 1000, hard: 1500 }),
  rs: Object.freeze({ soft: 2500, hard: 5000 }),
});

/** 豁免最长有效期（天）。 */
export const MAX_EXCEPTION_DAYS = 90;

/**
 * 归一化仓库相对路径（POSIX 斜杠）。
 *
 * Business Logic（为什么需要这个函数）:
 *   Windows 与 POSIX 混用路径会破坏基线 key 匹配。
 *
 * Code Logic（这个函数做什么）:
 *   将反斜杠替换为 /，去掉前导 ./。
 *
 * @param {string} filePath
 * @returns {string}
 */
export function normalizeRelPath(filePath) {
  return String(filePath || '')
    .replace(/\\/g, '/')
    .replace(/^\.\//, '');
}

/**
 * 判断路径是否应被模块规模门禁排除。
 *
 * Business Logic（为什么需要这个函数）:
 *   测试、构建产物、vendor 与生成代码不纳入 no-growth 治理。
 *
 * Code Logic（这个函数做什么）:
 *   对归一化路径匹配 node_modules/target/dist/vendor/generated/tests 与 *test* 文件名模式。
 *
 * @param {string} relPath
 * @returns {boolean}
 */
export function isExcludedPath(relPath) {
  const p = normalizeRelPath(relPath);
  if (!p) return true;
  const segments = p.split('/');
  if (segments.some((s) => s === 'node_modules' || s === 'target' || s === 'dist' || s === 'vendor' || s === 'generated')) {
    return true;
  }
  if (segments.includes('tests') || segments.includes('__tests__')) {
    return true;
  }
  const base = segments[segments.length - 1] || '';
  if (/\.test\./i.test(base) || /\.spec\./i.test(base) || /\.stories\./i.test(base)) {
    return true;
  }
  // 文件名含 test（如 foo_test.rs、test_helpers.ts）
  if (/test/i.test(base)) {
    return true;
  }
  return false;
}

/**
 * 判断是否为受治理源文件扩展名。
 *
 * Business Logic（为什么需要这个函数）:
 *   仅对 TypeScript/TSX 与 Rust 生产源码做规模约束。
 *
 * Code Logic（这个函数做什么）:
 *   返回 .ts/.tsx/.rs 的小写扩展名，否则 null。
 *
 * @param {string} relPath
 * @returns {'ts' | 'tsx' | 'rs' | null}
 */
export function sourceExt(relPath) {
  const p = normalizeRelPath(relPath);
  const m = p.match(/\.([^.]+)$/);
  if (!m) return null;
  const ext = m[1].toLowerCase();
  if (ext === 'ts' || ext === 'tsx' || ext === 'rs') return ext;
  return null;
}

/**
 * 按 wc -l 语义统计行数（换行符个数）。
 *
 * Business Logic（为什么需要这个函数）:
 *   基线与人工 `wc -l` 对齐，避免工具间差 1 行导致误报。
 *
 * Code Logic（这个函数做什么）:
 *   统计内容中的 \\n 个数。
 *
 * @param {string} content
 * @returns {number}
 */
export function countLines(content) {
  if (typeof content !== 'string' || content.length === 0) return 0;
  let n = 0;
  for (let i = 0; i < content.length; i += 1) {
    if (content.charCodeAt(i) === 10) n += 1;
  }
  return n;
}

/**
 * 取扩展名对应软/硬上限。
 *
 * Business Logic（为什么需要这个函数）:
 *   TS 与 Rust 的可维护阈值不同，需要统一查表。
 *
 * Code Logic（这个函数做什么）:
 *   按 ext 返回 {soft, hard}；未知扩展返回 null。
 *
 * @param {'ts' | 'tsx' | 'rs' | string | null} ext
 * @returns {{ soft: number, hard: number } | null}
 */
export function limitsForExt(ext) {
  if (!ext) return null;
  const key = String(ext).toLowerCase();
  if (key === 'ts' || key === 'tsx' || key === 'rs') {
    return LINE_LIMITS[key];
  }
  return null;
}

/**
 * 解析 YYYY-MM-DD 为 UTC 日界 Date；非法返回 null。
 *
 * Business Logic（为什么需要这个函数）:
 *   豁免过期校验需要稳定的日历日比较，避免本地时区偏移。
 *
 * Code Logic（这个函数做什么）:
 *   匹配严格日期格式并构造 UTC 午夜 Date。
 *
 * @param {unknown} value
 * @returns {Date | null}
 */
export function parseIsoDateOnly(value) {
  if (typeof value !== 'string') return null;
  const m = value.trim().match(/^(\d{4})-(\d{2})-(\d{2})$/);
  if (!m) return null;
  const y = Number(m[1]);
  const mo = Number(m[2]);
  const d = Number(m[3]);
  if (!Number.isInteger(y) || !Number.isInteger(mo) || !Number.isInteger(d)) return null;
  const dt = new Date(Date.UTC(y, mo - 1, d));
  if (
    dt.getUTCFullYear() !== y ||
    dt.getUTCMonth() !== mo - 1 ||
    dt.getUTCDate() !== d
  ) {
    return null;
  }
  return dt;
}

/**
 * 将 Date 格式化为 YYYY-MM-DD（UTC）。
 *
 * Business Logic（为什么需要这个函数）:
 *   基线 expiresAt 与报告需要稳定日期字符串。
 *
 * Code Logic（这个函数做什么）:
 *   取 UTC 年月日并零填充。
 *
 * @param {Date} date
 * @returns {string}
 */
export function formatIsoDateOnly(date) {
  const y = date.getUTCFullYear();
  const m = String(date.getUTCMonth() + 1).padStart(2, '0');
  const d = String(date.getUTCDate()).padStart(2, '0');
  return `${y}-${m}-${d}`;
}

/**
 * 计算距 todayUtc 的整天差（expires - today）。
 *
 * Business Logic（为什么需要这个函数）:
 *   需要拒绝已过期豁免，以及超过 90 天的远期 expiresAt。
 *
 * Code Logic（这个函数做什么）:
 *   用 UTC 午夜毫秒差 / 86400000 取整。
 *
 * @param {Date} expiresUtc
 * @param {Date} todayUtc
 * @returns {number}
 */
export function daysBetweenUtc(expiresUtc, todayUtc) {
  const ms = expiresUtc.getTime() - todayUtc.getTime();
  return Math.floor(ms / 86_400_000);
}

/**
 * 校验硬超限豁免条目。
 *
 * Business Logic（为什么需要这个函数）:
 *   硬超限只能短期保留；缺 owner/reason 或过期必须失败，防止永久债。
 *
 * Code Logic（这个函数做什么）:
 *   检查 exception 对象的 owner/reason/expiresAt；返回错误字符串数组。
 *
 * @param {unknown} exception
 * @param {Date} todayUtc
 * @param {string} pathLabel
 * @returns {string[]}
 */
export function validateException(exception, todayUtc, pathLabel) {
  /** @type {string[]} */
  const errors = [];
  if (!exception || typeof exception !== 'object' || Array.isArray(exception)) {
    errors.push(`${pathLabel}: over hard limit requires exception {owner, reason, expiresAt}`);
    return errors;
  }
  const ex = /** @type {Record<string, unknown>} */ (exception);
  const owner = typeof ex.owner === 'string' ? ex.owner.trim() : '';
  const reason = typeof ex.reason === 'string' ? ex.reason.trim() : '';
  if (!owner) {
    errors.push(`${pathLabel}: exception.owner is required`);
  }
  if (!reason) {
    errors.push(`${pathLabel}: exception.reason is required`);
  }
  const expires = parseIsoDateOnly(ex.expiresAt);
  if (!expires) {
    errors.push(`${pathLabel}: exception.expiresAt must be YYYY-MM-DD`);
    return errors;
  }
  const delta = daysBetweenUtc(expires, todayUtc);
  if (delta < 0) {
    errors.push(
      `${pathLabel}: exception expired on ${formatIsoDateOnly(expires)} (today ${formatIsoDateOnly(todayUtc)})`,
    );
  } else if (delta > MAX_EXCEPTION_DAYS) {
    errors.push(
      `${pathLabel}: exception.expiresAt is ${delta} days out (max ${MAX_EXCEPTION_DAYS})`,
    );
  }
  return errors;
}

/**
 * 从 git 索引列出受治理源文件相对路径。
 *
 * Business Logic（为什么需要这个函数）:
 *   门禁只约束仓库跟踪源码，忽略未提交噪声与依赖树。
 *
 * Code Logic（这个函数做什么）:
 *   执行 git ls-files，过滤扩展名与排除规则，排序去重。
 *
 * @param {string} rootDir
 * @param {{ lsFiles?: () => string[] }} [opts]
 * @returns {string[]}
 */
export function listGovernedSourceFiles(rootDir, opts = {}) {
  /** @type {string[]} */
  let raw;
  if (typeof opts.lsFiles === 'function') {
    raw = opts.lsFiles();
  } else {
    const out = execFileSync('git', ['ls-files', '-z'], {
      cwd: rootDir,
      encoding: 'utf8',
      maxBuffer: 64 * 1024 * 1024,
    });
    raw = out.split('\0').filter(Boolean);
  }
  /** @type {Set<string>} */
  const set = new Set();
  for (const item of raw) {
    const rel = normalizeRelPath(item);
    if (!sourceExt(rel)) continue;
    if (isExcludedPath(rel)) continue;
    set.add(rel);
  }
  return [...set].sort((a, b) => a.localeCompare(b));
}

/**
 * 读取基线 JSON。
 *
 * Business Logic（为什么需要这个函数）:
 *   正常 CI 对照已审基线做 no-growth，不能静默缺文件。
 *
 * Code Logic（这个函数做什么）:
 *   解析 JSON，规范化 files 映射与可选 exceptions。
 *
 * @param {string} baselinePath
 * @returns {{
 *   version: number,
 *   generatedAt: string | null,
 *   files: Record<string, { lines: number, exception?: { owner: string, reason: string, expiresAt: string } }>,
 * }}
 */
export function loadBaseline(baselinePath) {
  if (!existsSync(baselinePath)) {
    throw new Error(`baseline missing: ${baselinePath} (run with --write-baseline first)`);
  }
  const raw = JSON.parse(readFileSync(baselinePath, 'utf8'));
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) {
    throw new Error(`invalid baseline root: ${baselinePath}`);
  }
  const filesIn = raw.files;
  if (!filesIn || typeof filesIn !== 'object' || Array.isArray(filesIn)) {
    throw new Error(`baseline.files must be an object: ${baselinePath}`);
  }
  /** @type {Record<string, { lines: number, exception?: { owner: string, reason: string, expiresAt: string } }>} */
  const files = {};
  for (const [key, value] of Object.entries(filesIn)) {
    const pathKey = normalizeRelPath(key);
    if (typeof value === 'number' && Number.isFinite(value)) {
      files[pathKey] = { lines: Math.trunc(value) };
      continue;
    }
    if (value && typeof value === 'object' && !Array.isArray(value)) {
      const rec = /** @type {Record<string, unknown>} */ (value);
      const lines = typeof rec.lines === 'number' && Number.isFinite(rec.lines)
        ? Math.trunc(rec.lines)
        : NaN;
      if (!Number.isFinite(lines) || lines < 0) {
        throw new Error(`baseline.files[${pathKey}].lines invalid`);
      }
      /** @type {{ lines: number, exception?: { owner: string, reason: string, expiresAt: string } }} */
      const entry = { lines };
      if (rec.exception && typeof rec.exception === 'object' && !Array.isArray(rec.exception)) {
        const ex = /** @type {Record<string, unknown>} */ (rec.exception);
        entry.exception = {
          owner: typeof ex.owner === 'string' ? ex.owner : '',
          reason: typeof ex.reason === 'string' ? ex.reason : '',
          expiresAt: typeof ex.expiresAt === 'string' ? ex.expiresAt : '',
        };
      }
      files[pathKey] = entry;
      continue;
    }
    throw new Error(`baseline.files[${pathKey}] must be number or {lines, exception?}`);
  }
  return {
    version: typeof raw.version === 'number' ? raw.version : 1,
    generatedAt: typeof raw.generatedAt === 'string' ? raw.generatedAt : null,
    files,
  };
}

/**
 * 为已知超硬历史文件生成默认豁免元数据。
 *
 * Business Logic（为什么需要这个函数）:
 *   首次写基线时，已存在的硬超限文件需要可审豁免，否则 CI 立刻全红。
 *
 * Code Logic（这个函数做什么）:
 *   按路径返回 owner/reason；未知路径返回通用拆分债说明。
 *
 * @param {string} relPath
 * @returns {{ owner: string, reason: string }}
 */
export function defaultExceptionMeta(relPath) {
  const p = normalizeRelPath(relPath);
  if (p === 'src-tauri/src/orchestrator/repo.rs') {
    return {
      owner: 'orchestrator-backend',
      reason: 'OrchestratorRepo monorepo pending Task 8 split into repo/{mod,schema,tasks,attempts,evidence,remote}',
    };
  }
  if (p === 'src-tauri/src/commands/workbench.rs') {
    return {
      owner: 'workbench-backend',
      reason: 'Workbench commands monorepo pending Task 8 directory-module split by projects/sessions/files/git/browser',
    };
  }
  if (p === 'src-tauri/src/commands/orchestrator.rs') {
    return {
      owner: 'orchestrator-backend',
      reason: 'Orchestrator commands monorepo pending Task 8 directory-module split',
    };
  }
  if (p === 'src-tauri/src/transfer/receiver.rs') {
    return {
      owner: 'transfer-backend',
      reason: 'Transfer receiver no-growth only; security/platform coupling requires dedicated split spec before refactor',
    };
  }
  if (p === 'src-tauri/src/workbench/dependencies.rs') {
    return {
      owner: 'workbench-backend',
      reason: 'Workbench dependencies no-growth only; platform install coupling requires dedicated split spec',
    };
  }
  if (p.includes('useSettingsController')) {
    return {
      owner: 'frontend-settings',
      reason: 'Settings controller residual after panel split; further domain controllers deferred past S4',
    };
  }
  if (p.includes('useOrchestratorController') || p.includes('/Orchestrator/')) {
    return {
      owner: 'frontend-orchestrator',
      reason: 'Orchestrator controller residual; keep no-growth until Task 8 board decomposition',
    };
  }
  if (p.includes('useMobileAutomationController') || p.includes('MobileAutomation')) {
    return {
      owner: 'frontend-mobile',
      reason: 'Mobile automation controller residual after view split; further domain split deferred',
    };
  }
  if (p.includes('/lib/types')) {
    return {
      owner: 'frontend-types',
      reason: 'types barrel/domain modules under size governance after domain split',
    };
  }
  return {
    owner: 'module-maintainers',
    reason: 'Oversized source pending domain decomposition; temporary hard-limit exception',
  };
}

/**
 * 扫描工作树，生成基线对象（仅收录 ≥ soft 的文件）。
 *
 * Business Logic（为什么需要这个函数）:
 *   基线只锁定已超软限文件，避免对小文件每次改动强制改 JSON。
 *
 * Code Logic（这个函数做什么）:
 *   统计受治理文件行数；≥ soft 写入 files；> hard 附加 exception（expiresAt=today+90）。
 *
 * @param {{
 *   rootDir: string,
 *   todayUtc?: Date,
 *   readFile?: (abs: string) => string,
 *   lsFiles?: () => string[],
 *   preserveExceptions?: Record<string, { owner: string, reason: string, expiresAt: string }>,
 * }} options
 * @returns {{
 *   version: number,
 *   generatedAt: string,
 *   limits: typeof LINE_LIMITS,
 *   files: Record<string, { lines: number, exception?: { owner: string, reason: string, expiresAt: string } }>,
 * }}
 */
export function buildBaseline(options) {
  const rootDir = options.rootDir;
  const todayUtc = options.todayUtc ?? utcToday();
  const expiresAt = formatIsoDateOnly(
    new Date(todayUtc.getTime() + MAX_EXCEPTION_DAYS * 86_400_000),
  );
  const readFile =
    options.readFile ??
    ((abs) => readFileSync(abs, 'utf8'));
  const paths = listGovernedSourceFiles(rootDir, { lsFiles: options.lsFiles });
  /** @type {Record<string, { lines: number, exception?: { owner: string, reason: string, expiresAt: string } }>} */
  const files = {};
  for (const rel of paths) {
    const ext = sourceExt(rel);
    const limits = limitsForExt(ext);
    if (!limits) continue;
    const abs = join(rootDir, ...rel.split('/'));
    let content = '';
    try {
      content = readFile(abs);
    } catch {
      continue;
    }
    const lines = countLines(content);
    if (lines < limits.soft) continue;
    /** @type {{ lines: number, exception?: { owner: string, reason: string, expiresAt: string } }} */
    const entry = { lines };
    if (lines > limits.hard) {
      const preserved = options.preserveExceptions?.[rel];
      if (preserved) {
        entry.exception = { ...preserved };
      } else {
        const meta = defaultExceptionMeta(rel);
        entry.exception = {
          owner: meta.owner,
          reason: meta.reason,
          expiresAt,
        };
      }
    }
    files[rel] = entry;
  }
  return {
    version: 1,
    generatedAt: new Date().toISOString(),
    limits: LINE_LIMITS,
    files,
  };
}

/**
 * UTC 当日午夜。
 *
 * Business Logic（为什么需要这个函数）:
 *   过期比较统一到日历日。
 *
 * Code Logic（这个函数做什么）:
 *   取当前 UTC 年月日构造 Date。
 *
 * @param {Date} [now]
 * @returns {Date}
 */
export function utcToday(now = new Date()) {
  return new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate()));
}

/**
 * 对单个文件应用门禁规则，收集 violations。
 *
 * Business Logic（为什么需要这个函数）:
 *   集中表达 no-growth / 软硬限 / 豁免策略，便于自测与 CI 复用。
 *
 * Code Logic（这个函数做什么）:
 *   按是否在基线、行数与上限产出结构化问题列表。
 *
 * @param {{
 *   relPath: string,
 *   lines: number,
 *   baselineEntry: { lines: number, exception?: { owner: string, reason: string, expiresAt: string } } | null,
 *   todayUtc: Date,
 * }} input
 * @returns {{ kind: string, message: string }[]}
 */
export function evaluateFile(input) {
  const { relPath, lines, baselineEntry, todayUtc } = input;
  const ext = sourceExt(relPath);
  const limits = limitsForExt(ext);
  /** @type {{ kind: string, message: string }[]} */
  const issues = [];
  if (!limits) return issues;

  const overSoft = lines > limits.soft;
  const overHard = lines > limits.hard;

  if (baselineEntry) {
    if (lines > baselineEntry.lines) {
      issues.push({
        kind: 'growth',
        message: `${relPath}: grew ${baselineEntry.lines} → ${lines} (no-growth baseline; shrink or split, then --write-baseline)`,
      });
    }
    if (overHard) {
      const exErrors = validateException(baselineEntry.exception, todayUtc, relPath);
      for (const msg of exErrors) {
        issues.push({ kind: 'exception', message: msg });
      }
    }
    return issues;
  }

  // 不在基线：视为新文件或未登记文件
  if (overHard) {
    issues.push({
      kind: 'hard',
      message: `${relPath}: ${lines} lines exceeds hard limit ${limits.hard} for .${ext}; new/unbaselined files cannot exceed soft ${limits.soft}`,
    });
  } else if (overSoft) {
    issues.push({
      kind: 'soft',
      message: `${relPath}: ${lines} lines exceeds soft limit ${limits.soft} for .${ext}; new/unbaselined files must stay ≤ soft (split or --write-baseline after review)`,
    });
  }
  return issues;
}

/**
 * 运行完整检查。
 *
 * Business Logic（为什么需要这个函数）:
 *   CI 与本地需要统一 exit 语义与报告摘要。
 *
 * Code Logic（这个函数做什么）:
 *   加载基线、扫描文件、聚合 issues，并统计 soft/hard 超限文件数。
 *
 * @param {{
 *   rootDir: string,
 *   baselinePath: string,
 *   todayUtc?: Date,
 *   readFile?: (abs: string) => string,
 *   lsFiles?: () => string[],
 *   baseline?: ReturnType<typeof loadBaseline>,
 * }} options
 * @returns {{
 *   ok: boolean,
 *   issues: { kind: string, message: string }[],
 *   trackedCount: number,
 *   overSoftCount: number,
 *   overHardCount: number,
 *   baselinedCount: number,
 *   shrinkHints: string[],
 * }}
 */
export function runCheck(options) {
  const todayUtc = options.todayUtc ?? utcToday();
  const baseline = options.baseline ?? loadBaseline(options.baselinePath);
  const readFile =
    options.readFile ??
    ((abs) => readFileSync(abs, 'utf8'));
  const paths = listGovernedSourceFiles(options.rootDir, { lsFiles: options.lsFiles });

  /** @type {{ kind: string, message: string }[]} */
  const issues = [];
  /** @type {string[]} */
  const shrinkHints = [];
  let overSoftCount = 0;
  let overHardCount = 0;
  /** @type {Set<string>} */
  const seen = new Set();

  for (const rel of paths) {
    const ext = sourceExt(rel);
    const limits = limitsForExt(ext);
    if (!limits) continue;
    const abs = join(options.rootDir, ...rel.split('/'));
    let content = '';
    try {
      content = readFile(abs);
    } catch (err) {
      issues.push({
        kind: 'io',
        message: `${rel}: failed to read (${err instanceof Error ? err.message : String(err)})`,
      });
      continue;
    }
    const lines = countLines(content);
    seen.add(rel);
    if (lines > limits.soft) overSoftCount += 1;
    if (lines > limits.hard) overHardCount += 1;
    const baselineEntry = baseline.files[rel] ?? null;
    const fileIssues = evaluateFile({
      relPath: rel,
      lines,
      baselineEntry,
      todayUtc,
    });
    issues.push(...fileIssues);
    if (baselineEntry && lines < baselineEntry.lines) {
      shrinkHints.push(
        `${rel}: shrank ${baselineEntry.lines} → ${lines} (optional: --write-baseline to ratchet down)`,
      );
    }
  }

  // 基线中仍登记但仓库已删除/排除的路径：提示但不作硬失败（避免 rename 误伤）
  for (const [rel, entry] of Object.entries(baseline.files)) {
    if (seen.has(rel)) continue;
    shrinkHints.push(
      `${rel}: baselined at ${entry.lines} but missing from governed set (optional: --write-baseline to drop)`,
    );
  }

  // 基线内硬超限条目即使文件暂时读不到也要校验 exception 形态（防御）
  for (const [rel, entry] of Object.entries(baseline.files)) {
    const ext = sourceExt(rel);
    const limits = limitsForExt(ext);
    if (!limits) continue;
    if (entry.lines > limits.hard) {
      // 若文件存在会在 evaluateFile 已校验；缺失时也要校验 schema
      if (!seen.has(rel)) {
        const exErrors = validateException(entry.exception, todayUtc, rel);
        for (const msg of exErrors) {
          issues.push({ kind: 'exception', message: msg });
        }
      }
    }
  }

  return {
    ok: issues.length === 0,
    issues,
    trackedCount: paths.length,
    overSoftCount,
    overHardCount,
    baselinedCount: Object.keys(baseline.files).length,
    shrinkHints,
  };
}

/**
 * 写基线 JSON（仅 --write-baseline）。
 *
 * Business Logic（为什么需要这个函数）:
 *   审阅后更新 no-growth 水位；CI 禁止隐式改写。
 *
 * Code Logic（这个函数做什么）:
 *   生成 baseline 对象，稳定 key 排序后写磁盘。
 *
 * @param {{
 *   rootDir: string,
 *   baselinePath: string,
 *   todayUtc?: Date,
 *   preserveExisting?: boolean,
 * }} options
 * @returns {ReturnType<typeof buildBaseline>}
 */
export function writeBaseline(options) {
  /** @type {Record<string, { owner: string, reason: string, expiresAt: string }> | undefined} */
  let preserve;
  if (options.preserveExisting && existsSync(options.baselinePath)) {
    try {
      const existing = loadBaseline(options.baselinePath);
      preserve = {};
      for (const [rel, entry] of Object.entries(existing.files)) {
        if (entry.exception) {
          preserve[rel] = { ...entry.exception };
        }
      }
    } catch {
      preserve = undefined;
    }
  }
  const baseline = buildBaseline({
    rootDir: options.rootDir,
    todayUtc: options.todayUtc,
    preserveExceptions: preserve,
  });
  /** @type {Record<string, { lines: number, exception?: { owner: string, reason: string, expiresAt: string } }>} */
  const sortedFiles = {};
  for (const key of Object.keys(baseline.files).sort((a, b) => a.localeCompare(b))) {
    sortedFiles[key] = baseline.files[key];
  }
  const payload = {
    version: baseline.version,
    generatedAt: baseline.generatedAt,
    limits: baseline.limits,
    files: sortedFiles,
  };
  mkdirSync(dirname(options.baselinePath), { recursive: true });
  writeFileSync(options.baselinePath, `${JSON.stringify(payload, null, 2)}\n`, 'utf8');
  return { ...baseline, files: sortedFiles };
}

/**
 * 创建内存/临时文件 fixture 并断言检查结果。
 *
 * Business Logic（为什么需要这个函数）:
 *   门禁本身必须可自测，避免规则回归时无人发现。
 *
 * Code Logic（这个函数做什么）:
 *   在 tmp 目录写假源文件与基线，调用 runCheck/evaluateFile 断言。
 *
 * @returns {void}
 */
export function runSelfTest() {
  /** @type {string[]} */
  const failures = [];
  /**
   * @param {string} name
   * @param {() => void} fn
   */
  function caseRun(name, fn) {
    try {
      fn();
      process.stdout.write(`  ok  ${name}\n`);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      failures.push(`${name}: ${msg}`);
      process.stdout.write(`  FAIL ${name}: ${msg}\n`);
    }
  }

  /**
   * @param {boolean} cond
   * @param {string} message
   */
  function assert(cond, message) {
    if (!cond) throw new Error(message);
  }

  process.stdout.write('module-boundary self-test\n');

  caseRun('exclude-tests-dist-vendor', () => {
    assert(isExcludedPath('web/src/foo.test.ts'), 'test file');
    assert(isExcludedPath('web/src/foo.spec.tsx'), 'spec file');
    assert(isExcludedPath('src-tauri/tests/foo.rs'), 'tests dir');
    assert(isExcludedPath('web/dist/assets/x.ts'), 'dist');
    assert(isExcludedPath('vendor/lib.rs'), 'vendor');
    assert(isExcludedPath('web/node_modules/x/index.ts'), 'node_modules');
    assert(isExcludedPath('src-tauri/target/debug/build/x.rs'), 'target');
    assert(isExcludedPath('web/src/generated/types.ts'), 'generated');
    assert(isExcludedPath('src-tauri/src/foo_test.rs'), 'name contains test');
    assert(!isExcludedPath('web/src/pages/Settings/useSettingsController.ts'), 'prod ts');
    assert(!isExcludedPath('src-tauri/src/orchestrator/repo.rs'), 'prod rs');
  });

  caseRun('count-lines-wc-semantics', () => {
    assert(countLines('') === 0, 'empty');
    assert(countLines('a\n') === 1, 'one nl');
    assert(countLines('a\nb\n') === 2, 'two nl');
    assert(countLines('a\nb') === 1, 'no trailing nl counts newlines only');
  });

  caseRun('new-ts-over-soft-fails', () => {
    const today = utcToday(new Date('2026-07-14T12:00:00Z'));
    const lines = 1001;
    const issues = evaluateFile({
      relPath: 'web/src/NewBig.ts',
      lines,
      baselineEntry: null,
      todayUtc: today,
    });
    assert(issues.some((i) => i.kind === 'soft'), `expected soft fail, got ${JSON.stringify(issues)}`);
  });

  caseRun('new-ts-over-hard-fails', () => {
    const today = utcToday(new Date('2026-07-14T12:00:00Z'));
    const issues = evaluateFile({
      relPath: 'web/src/NewHuge.tsx',
      lines: 1501,
      baselineEntry: null,
      todayUtc: today,
    });
    assert(issues.some((i) => i.kind === 'hard'), `expected hard fail, got ${JSON.stringify(issues)}`);
  });

  caseRun('baseline-growth-fails', () => {
    const today = utcToday(new Date('2026-07-14T12:00:00Z'));
    const issues = evaluateFile({
      relPath: 'web/src/Known.ts',
      lines: 1201,
      baselineEntry: { lines: 1200 },
      todayUtc: today,
    });
    assert(issues.some((i) => i.kind === 'growth'), `expected growth fail, got ${JSON.stringify(issues)}`);
  });

  caseRun('baseline-shrink-passes-with-hint-path', () => {
    const today = utcToday(new Date('2026-07-14T12:00:00Z'));
    const issues = evaluateFile({
      relPath: 'web/src/Known.ts',
      lines: 1100,
      baselineEntry: { lines: 1200 },
      todayUtc: today,
    });
    assert(issues.length === 0, `shrink should pass evaluateFile, got ${JSON.stringify(issues)}`);
  });

  caseRun('expired-exception-fails', () => {
    const today = utcToday(new Date('2026-07-14T12:00:00Z'));
    const issues = evaluateFile({
      relPath: 'src-tauri/src/big.rs',
      lines: 6000,
      baselineEntry: {
        lines: 6000,
        exception: {
          owner: 'team',
          reason: 'temp',
          expiresAt: '2026-07-01',
        },
      },
      todayUtc: today,
    });
    assert(
      issues.some((i) => i.kind === 'exception' && /expired/i.test(i.message)),
      `expected expired exception, got ${JSON.stringify(issues)}`,
    );
  });

  caseRun('missing-owner-reason-fails', () => {
    const today = utcToday(new Date('2026-07-14T12:00:00Z'));
    const issues = evaluateFile({
      relPath: 'src-tauri/src/big.rs',
      lines: 6000,
      baselineEntry: {
        lines: 6000,
        exception: {
          owner: '',
          reason: '',
          expiresAt: '2026-10-01',
        },
      },
      todayUtc: today,
    });
    assert(
      issues.some((i) => /owner/i.test(i.message)),
      `expected owner fail, got ${JSON.stringify(issues)}`,
    );
    assert(
      issues.some((i) => /reason/i.test(i.message)),
      `expected reason fail, got ${JSON.stringify(issues)}`,
    );
  });

  caseRun('exception-too-far-fails', () => {
    const today = utcToday(new Date('2026-07-14T12:00:00Z'));
    const errs = validateException(
      { owner: 'a', reason: 'b', expiresAt: '2027-01-01' },
      today,
      'x.rs',
    );
    assert(errs.some((e) => /max 90/.test(e)), `expected max 90, got ${errs.join('; ')}`);
  });

  caseRun('baselined-over-hard-with-valid-exception-passes', () => {
    const today = utcToday(new Date('2026-07-14T12:00:00Z'));
    const issues = evaluateFile({
      relPath: 'src-tauri/src/big.rs',
      lines: 6000,
      baselineEntry: {
        lines: 6000,
        exception: {
          owner: 'team',
          reason: 'pending split',
          expiresAt: '2026-10-12',
        },
      },
      todayUtc: today,
    });
    assert(issues.length === 0, `expected pass, got ${JSON.stringify(issues)}`);
  });

  caseRun('fixture-dir-integration', () => {
    const dir = mkdtempSync(join(tmpdir(), 'module-boundary-'));
    try {
      const write = (rel, body) => {
        const abs = join(dir, rel);
        mkdirSync(dirname(abs), { recursive: true });
        writeFileSync(abs, body, 'utf8');
      };
      // 1001 行：1000 个换行 → wc -l = 1000；需要 1001 个 \n 才 > soft 1000
      write('web/src/TooSoft.ts', `${'x\n'.repeat(1001)}`);
      write('web/src/ok.ts', 'export const x = 1;\n');
      write('web/src/skip.test.ts', `${'x\n'.repeat(2000)}`);
      write('web/dist/out.ts', `${'x\n'.repeat(2000)}`);
      write('vendor/lib.rs', `${'x\n'.repeat(6000)}`);

      const lsFiles = () => [
        'web/src/TooSoft.ts',
        'web/src/ok.ts',
        'web/src/skip.test.ts',
        'web/dist/out.ts',
        'vendor/lib.rs',
      ];

      const baselinePath = join(dir, 'scripts', 'module-boundary-baseline.json');
      mkdirSync(dirname(baselinePath), { recursive: true });
      // 空基线：TooSoft 应 soft fail；排除项不进入
      writeFileSync(
        baselinePath,
        JSON.stringify({ version: 1, generatedAt: '2026-07-14T00:00:00.000Z', files: {} }, null, 2),
        'utf8',
      );

      const result = runCheck({
        rootDir: dir,
        baselinePath,
        todayUtc: utcToday(new Date('2026-07-14T12:00:00Z')),
        lsFiles,
      });
      assert(!result.ok, 'expected fail');
      assert(
        result.issues.some((i) => i.kind === 'soft' && i.message.includes('TooSoft.ts')),
        `expected TooSoft soft issue, got ${JSON.stringify(result.issues)}`,
      );
      assert(
        !result.issues.some((i) => i.message.includes('skip.test') || i.message.includes('dist/') || i.message.includes('vendor/')),
        'excluded paths must not appear',
      );

      // growth fail
      writeFileSync(
        baselinePath,
        JSON.stringify(
          {
            version: 1,
            files: {
              'web/src/TooSoft.ts': { lines: 1000 },
            },
          },
          null,
          2,
        ),
        'utf8',
      );
      // bump one line
      write('web/src/TooSoft.ts', `${'x\n'.repeat(1002)}`);
      const grown = runCheck({
        rootDir: dir,
        baselinePath,
        todayUtc: utcToday(new Date('2026-07-14T12:00:00Z')),
        lsFiles,
      });
      assert(
        grown.issues.some((i) => i.kind === 'growth'),
        `expected growth, got ${JSON.stringify(grown.issues)}`,
      );
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  if (failures.length > 0) {
    process.stderr.write(`\nself-test failed: ${failures.length} case(s)\n`);
    process.exitCode = 1;
    return;
  }
  process.stdout.write('self-test passed\n');
}

/**
 * CLI 入口。
 *
 * Business Logic（为什么需要这个函数）:
 *   提供 --self-test / --write-baseline / 默认检查三种本地与 CI 用法。
 *
 * Code Logic（这个函数做什么）:
 *   解析 argv，分派到自测、写基线或 runCheck，设置 exitCode。
 *
 * @param {string[]} argv
 * @returns {void}
 */
export function main(argv = process.argv.slice(2)) {
  if (argv.includes('--help') || argv.includes('-h')) {
    process.stdout.write(
      [
        'Usage:',
        '  node scripts/check-module-boundaries.mjs',
        '  node scripts/check-module-boundaries.mjs --write-baseline',
        '  node scripts/check-module-boundaries.mjs --self-test',
        '',
        'Limits: TS/TSX soft 1000 / hard 1500; Rust soft 2500 / hard 5000',
        'Baseline: scripts/module-boundary-baseline.json (read-only unless --write-baseline)',
        '',
      ].join('\n'),
    );
    return;
  }

  if (argv.includes('--self-test')) {
    runSelfTest();
    return;
  }

  const baselinePath = DEFAULT_BASELINE_PATH;
  const rootDir = REPO_ROOT;
  // 固定“今天”便于与人工审阅的 expiresAt 对齐；也可用系统日期
  const todayUtc = utcToday();

  if (argv.includes('--write-baseline')) {
    const baseline = writeBaseline({
      rootDir,
      baselinePath,
      todayUtc,
      preserveExisting: true,
    });
    const count = Object.keys(baseline.files).length;
    const hard = Object.entries(baseline.files).filter(([rel, e]) => {
      const lim = limitsForExt(sourceExt(rel));
      return lim && e.lines > lim.hard;
    });
    process.stdout.write(
      `Wrote baseline ${relative(rootDir, baselinePath) || baselinePath} with ${count} over-soft file(s); ${hard.length} over-hard exception(s)\n`,
    );
    for (const [rel, entry] of hard) {
      process.stdout.write(
        `  HARD ${rel}: ${entry.lines} owner=${entry.exception?.owner ?? '?'} expires=${entry.exception?.expiresAt ?? '?'}\n`,
      );
    }
    return;
  }

  // 正常 CI：只读
  const result = runCheck({
    rootDir,
    baselinePath,
    todayUtc,
  });

  process.stdout.write(
    `Module boundary check: tracked=${result.trackedCount} baselined=${result.baselinedCount} overSoft=${result.overSoftCount} overHard=${result.overHardCount}\n`,
  );
  for (const hint of result.shrinkHints) {
    process.stdout.write(`  note: ${hint}\n`);
  }
  if (!result.ok) {
    process.stderr.write(`Module boundary violations (${result.issues.length}):\n`);
    for (const issue of result.issues) {
      process.stderr.write(`  [${issue.kind}] ${issue.message}\n`);
    }
    process.exitCode = 1;
    return;
  }
  process.stdout.write('Module boundary ratchet passed\n');
}

const isDirect = process.argv[1] && resolve(process.argv[1]) === __filename;
if (isDirect) {
  main();
}
