#!/usr/bin/env node
/**
 * check-quality-traceability.mjs — 质量证据矩阵可追溯性门禁。
 *
 * Business Logic（为什么需要这个脚本）:
 *   S6 要求从产品 surface 追到稳定 evidence ID、测试文件、本地命令、CI job
 *   与明确 exclusions。人工维护会漏改 ID、引用不存在的测试，或把 L3 未执行
 *   项写成已验证。本脚本对 `docs/development/quality-matrix.json` 做零依赖静态校验，
 *   并提供 --self-test 防止门禁自身回归。
 *
 * Code Logic（这个脚本做什么）:
 *   - 读取 quality-matrix.json，校验 id 唯一、level/ciJob 白名单、tests 文件存在、
 *     command 可回溯到 web/package.json script / scripts/* / cargo --test / workflow run、
 *     optional docs 路径存在、exclusions 为非空字符串数组。
 *   - L3：要求 status；PASS/VERIFIED 且 expiresAt 已过期则失败；
 *     NOT VERIFIED 允许缺少 commit/version/date/expiresAt，也允许 expiresAt 过期。
 *   - 不发明身份鉴权、不把 L1 mock 提升为 L3。
 *   - `--self-test` 用内存 fixture 覆盖 duplicate ID / missing test / unknown level|job /
 *     command 无 package/workflow 背书 / expired L3 / nonexistent doc ref / valid exclusions。
 *
 * Usage:
 *   node scripts/check-quality-traceability.mjs
 *   node scripts/check-quality-traceability.mjs --self-test
 *   node scripts/check-quality-traceability.mjs --matrix path/to/matrix.json
 */

import {
  existsSync,
  mkdtempSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
const REPO_ROOT = resolve(__dirname, '..');
const DEFAULT_MATRIX_REL = 'docs/development/quality-matrix.json';

/** @type {ReadonlySet<string>} */
export const KNOWN_LEVELS = Object.freeze(new Set(['L0', 'L1', 'L2', 'L3']));

/**
 * CI / 证据 job 白名单。
 * `manual` 仅用于 L3 真机认证（不在 GitHub Actions 执行）。
 * @type {ReadonlySet<string>}
 */
export const KNOWN_CI_JOBS = Object.freeze(
  new Set(['quality', 'frontend-unit', 'frontend-e2e', 'docs', 'smoke', 'manual']),
);

/** L3 已通过类状态（需要未过期 expiresAt）。 */
const L3_PASS_STATUSES = new Set(['PASS', 'PASSED', 'VERIFIED', 'OK']);

/** L3 未验证状态。 */
const L3_UNVERIFIED_STATUSES = new Set(['NOT VERIFIED', 'UNVERIFIED', 'PENDING']);

/**
 * 归一化仓库相对路径。
 *
 * Business Logic（为什么需要这个函数）:
 *   Windows 与 POSIX 混用路径会破坏矩阵 key 与 exists 检查。
 *
 * Code Logic（这个函数做什么）:
 *   将反斜杠替换为 `/`，去掉前导 `./`。
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
 * 解析 UTC 日历日 YYYY-MM-DD。
 *
 * Business Logic（为什么需要这个函数）:
 *   L3 证书 expiresAt 必须以日历日比较，避免本地时区把「今天」算成已过期。
 *
 * Code Logic（这个函数做什么）:
 *   校验格式后返回 Date（UTC 正午）；非法返回 null。
 *
 * @param {unknown} value
 * @returns {Date | null}
 */
export function parseUtcDate(value) {
  if (typeof value !== 'string' || !/^\d{4}-\d{2}-\d{2}$/.test(value)) return null;
  const d = new Date(`${value}T12:00:00.000Z`);
  if (Number.isNaN(d.getTime())) return null;
  return d;
}

/**
 * 今天的 UTC 日历日。
 *
 * Business Logic（为什么需要这个函数）:
 *   过期判定与 self-test 注入固定「今天」需要统一入口。
 *
 * Code Logic（这个函数做什么）:
 *   返回 `YYYY-MM-DD` 字符串。
 *
 * @param {Date} [now]
 * @returns {string}
 */
export function utcToday(now = new Date()) {
  return now.toISOString().slice(0, 10);
}

/**
 * 读取并解析 quality-matrix JSON。
 *
 * Business Logic（为什么需要这个函数）:
 *   门禁与文档守卫都需要同一矩阵作为权威 ID 表。
 *
 * Code Logic（这个函数做什么）:
 *   读文件、JSON.parse；结构非法时抛出带路径的 Error。
 *
 * @param {string} absPath
 * @returns {{ version?: number, entries: unknown[] }}
 */
export function loadMatrixFile(absPath) {
  if (!existsSync(absPath)) {
    throw new Error(`quality matrix missing: ${absPath}`);
  }
  const raw = readFileSync(absPath, 'utf8');
  let data;
  try {
    data = JSON.parse(raw);
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    throw new Error(`quality matrix JSON parse failed (${absPath}): ${msg}`);
  }
  if (!data || typeof data !== 'object' || !Array.isArray(data.entries)) {
    throw new Error(`quality matrix must be an object with entries[] (${absPath})`);
  }
  return data;
}

/**
 * 从 web/package.json 读取 scripts 名集合。
 *
 * Business Logic（为什么需要这个函数）:
 *   L0/L1 命令必须能回溯到真实 npm script，防止文档写了不存在的 runner。
 *
 * Code Logic（这个函数做什么）:
 *   解析 package.json 的 scripts 对象键；缺失时返回空 Set。
 *
 * @param {string} rootDir
 * @returns {Set<string>}
 */
export function loadWebPackageScripts(rootDir) {
  const pkgPath = join(rootDir, 'web', 'package.json');
  /** @type {Set<string>} */
  const out = new Set();
  if (!existsSync(pkgPath)) return out;
  try {
    const pkg = JSON.parse(readFileSync(pkgPath, 'utf8'));
    if (pkg && pkg.scripts && typeof pkg.scripts === 'object') {
      for (const key of Object.keys(pkg.scripts)) out.add(key);
    }
  } catch {
    // ignore — caller will fail command validation when needed
  }
  return out;
}

/**
 * 从 GitHub workflow YAML 粗提取 `run:` 命令块文本。
 *
 * Business Logic（为什么需要这个函数）:
 *   L2 cargo / node 命令需要证明 CI 或 smoke workflow 会执行同类命令。
 *
 * Code Logic（这个函数做什么）:
 *   扫描 `.github/workflows/*.yml`，拼接所有 `run: |` 多行块与 `run: ` 单行，返回大写折叠前的原文。
 *
 * @param {string} rootDir
 * @returns {string}
 */
export function loadWorkflowRunText(rootDir) {
  const dir = join(rootDir, '.github', 'workflows');
  if (!existsSync(dir)) return '';
  /** @type {string[]} */
  const chunks = [];
  let names;
  try {
    names = readdirSync(dir);
  } catch {
    return '';
  }
  for (const name of names) {
    if (!/\.ya?ml$/i.test(name)) continue;
    const abs = join(dir, name);
    let text;
    try {
      text = readFileSync(abs, 'utf8');
    } catch {
      continue;
    }
    chunks.push(text);
  }
  return chunks.join('\n');
}

/**
 * 判定 command 是否有 package script / 仓库脚本 / cargo test 文件 / workflow 背书。
 *
 * Business Logic（为什么需要这个函数）:
 *   矩阵 command 不能是「看起来像测试」的自由文本；否则 CI 与本地无法复现证据。
 *
 * Code Logic（这个函数做什么）:
 *   - `manual`（L3）直接通过；
 *   - `npm run X` / `npm test` 对照 web/package.json；
 *   - `node scripts/foo.mjs` 检查文件存在；
 *   - `cargo test ... --test NAME` 检查 `src-tauri/tests/NAME.rs` 或 workflow 文本包含；
 *   - 其它含 `cargo test` 的命令要求 workflow 文本包含关键片段或 ciJob 为 quality/smoke。
 *
 * @param {string} command
 * @param {{ rootDir: string, level: string, webScripts: Set<string>, workflowText: string }} ctx
 * @returns {string | null} 错误消息；null 表示通过
 */
export function validateCommandBacked(command, ctx) {
  const cmd = String(command || '').trim();
  if (!cmd) {
    if (ctx.level === 'L3') return null;
    return 'command is empty';
  }
  if (/^manual(?:\b|$)/i.test(cmd)) {
    if (ctx.level === 'L3') return null;
    return 'manual command is only allowed for L3 entries';
  }

  const npmRun = cmd.match(/\bnpm\s+run\s+([a-zA-Z0-9:_-]+)/);
  if (npmRun) {
    if (!ctx.webScripts.has(npmRun[1])) {
      return `npm run script not in web/package.json: ${npmRun[1]}`;
    }
  }
  if (/\bnpm\s+test\b/.test(cmd) && !ctx.webScripts.has('test')) {
    return 'npm test used but web/package.json has no test script';
  }

  const nodeScript = cmd.match(/\bnode\s+scripts\/([^\s]+)/);
  if (nodeScript) {
    const rel = normalizeRelPath(`scripts/${nodeScript[1]}`);
    if (!existsSync(join(ctx.rootDir, ...rel.split('/')))) {
      return `node scripts path missing: ${rel}`;
    }
  }

  const cargoTest = cmd.match(/--test\s+([A-Za-z0-9_-]+)/);
  if (cargoTest) {
    const testName = cargoTest[1];
    const testRel = `src-tauri/tests/${testName}.rs`;
    const testAbs = join(ctx.rootDir, ...testRel.split('/'));
    const inWorkflow =
      ctx.workflowText.includes(`--test ${testName}`) ||
      ctx.workflowText.includes(`--test ${testName} `) ||
      new RegExp(`--test\\s+${testName}\\b`).test(ctx.workflowText);
    if (!existsSync(testAbs) && !inWorkflow) {
      return `cargo --test ${testName} has no src-tauri/tests/${testName}.rs and is not in workflows`;
    }
  } else if (/\bcargo\s+test\b/.test(cmd)) {
    // generic cargo test: require quality/smoke job or workflow mention of a distinctive fragment
    const fragment = cmd.replace(/\s+/g, ' ').slice(0, 80);
    if (!/(quality|smoke)/i.test(ctx.level === 'L2' ? 'quality' : '') && !ctx.workflowText.includes('cargo test')) {
      return `cargo test command not backed by workflow text: ${fragment}`;
    }
    if (!ctx.workflowText.includes('cargo test') && ctx.level !== 'L0') {
      // L2 without --test still needs workflow cargo test presence
      return 'cargo test command not found in .github/workflows';
    }
  }

  // 若命令是 npm/node/cargo 之一且上面未失败，通过；纯未知命令拒绝
  if (
    /\bnpm\s+(run|test)\b/.test(cmd) ||
    /\bnode\s+scripts\//.test(cmd) ||
    /\bcargo\s+test\b/.test(cmd) ||
    /^manual\b/i.test(cmd)
  ) {
    return null;
  }
  return `command not backed by package script, node scripts/*, cargo test, or manual L3: ${cmd}`;
}

/**
 * 校验单条矩阵 entry。
 *
 * Business Logic（为什么需要这个函数）:
 *   每条 evidence 必须可从 ID 追到测试、命令、CI 与 exclusions，L3 还要诚实状态。
 *
 * Code Logic（这个函数做什么）:
 *   返回该 entry 的 findings 列表（含 JSON path 提示）。
 *
 * @param {unknown} entry
 * @param {number} index
 * @param {{ rootDir: string, todayUtc: string, webScripts: Set<string>, workflowText: string, seenIds: Set<string> }} ctx
 * @returns {string[]}
 */
export function validateEntry(entry, index, ctx) {
  /** @type {string[]} */
  const findings = [];
  const path = `entries[${index}]`;
  if (!entry || typeof entry !== 'object' || Array.isArray(entry)) {
    findings.push(`${path}: entry must be an object`);
    return findings;
  }
  /** @type {Record<string, unknown>} */
  const e = /** @type {Record<string, unknown>} */ (entry);

  const id = e.id;
  if (typeof id !== 'string' || !id.trim()) {
    findings.push(`${path}.id: required non-empty string`);
  } else {
    if (ctx.seenIds.has(id)) {
      findings.push(`${path}.id: duplicate id ${id}`);
    } else {
      ctx.seenIds.add(id);
    }
    if (!/^(L0|L1|E2E|L2|L3)-[A-Z0-9-]+$/.test(id) && !/^E2E-[A-Z0-9-]+$/.test(id)) {
      // allow E2E-* as L1 stable IDs without L1- prefix
      if (!/^E2E-[A-Z0-9-]+$/.test(id) && !/^(L0|L2|L3)-[A-Z0-9-]+$/.test(id)) {
        findings.push(`${path}.id: unexpected id shape ${id}`);
      }
    }
  }

  if (typeof e.surface !== 'string' || !e.surface.trim()) {
    findings.push(`${path}.surface: required non-empty string`);
  }

  const level = typeof e.level === 'string' ? e.level : '';
  if (!KNOWN_LEVELS.has(level)) {
    findings.push(`${path}.level: unknown level ${JSON.stringify(e.level)} (expected L0|L1|L2|L3)`);
  }

  if (!Array.isArray(e.tests)) {
    findings.push(`${path}.tests: must be an array`);
  } else {
    const allowEmptyTests = level === 'L3';
    if (e.tests.length === 0 && !allowEmptyTests) {
      findings.push(`${path}.tests: empty tests only allowed for L3`);
    }
    e.tests.forEach((t, ti) => {
      if (typeof t !== 'string' || !t.trim()) {
        findings.push(`${path}.tests[${ti}]: must be non-empty string`);
        return;
      }
      const rel = normalizeRelPath(t);
      const abs = join(ctx.rootDir, ...rel.split('/'));
      if (!existsSync(abs)) {
        findings.push(`${path}.tests[${ti}]: missing test file ${rel}`);
      }
    });
  }

  if (typeof e.command !== 'string') {
    findings.push(`${path}.command: must be string`);
  } else {
    const cmdErr = validateCommandBacked(e.command, {
      rootDir: ctx.rootDir,
      level,
      webScripts: ctx.webScripts,
      workflowText: ctx.workflowText,
    });
    if (cmdErr) findings.push(`${path}.command: ${cmdErr}`);
  }

  const ciJob = typeof e.ciJob === 'string' ? e.ciJob : '';
  if (!KNOWN_CI_JOBS.has(ciJob)) {
    findings.push(`${path}.ciJob: unknown job ${JSON.stringify(e.ciJob)}`);
  }
  if (level === 'L3' && ciJob !== 'manual' && ciJob !== 'docs') {
    // L3 may only claim manual (or docs metadata job); not automated CI green
    if (ciJob && ciJob !== 'manual') {
      findings.push(`${path}.ciJob: L3 must use ciJob "manual" (got ${ciJob})`);
    }
  }

  if (!Array.isArray(e.platforms) || e.platforms.length === 0) {
    findings.push(`${path}.platforms: required non-empty string array`);
  } else if (e.platforms.some((p) => typeof p !== 'string' || !p.trim())) {
    findings.push(`${path}.platforms: all items must be non-empty strings`);
  }

  if (!Array.isArray(e.exclusions)) {
    findings.push(`${path}.exclusions: must be an array of strings`);
  } else if (e.exclusions.some((x) => typeof x !== 'string' || !String(x).trim())) {
    findings.push(`${path}.exclusions: all items must be non-empty strings`);
  }

  if (Array.isArray(e.docs)) {
    e.docs.forEach((d, di) => {
      if (typeof d !== 'string' || !d.trim()) {
        findings.push(`${path}.docs[${di}]: must be non-empty string`);
        return;
      }
      const rel = normalizeRelPath(d);
      const abs = join(ctx.rootDir, ...rel.split('/'));
      if (!existsSync(abs)) {
        findings.push(`${path}.docs[${di}]: missing doc ref ${rel}`);
      }
    });
  } else if (e.docs != null) {
    findings.push(`${path}.docs: must be an array when present`);
  }

  if (level === 'L3') {
    const status = typeof e.status === 'string' ? e.status.trim() : '';
    if (!status) {
      findings.push(`${path}.status: L3 requires status`);
    } else {
      const upper = status.toUpperCase();
      const isPass = L3_PASS_STATUSES.has(upper);
      const isUnverified = L3_UNVERIFIED_STATUSES.has(upper);
      if (!isPass && !isUnverified && upper !== 'FAIL' && upper !== 'FAILED') {
        findings.push(`${path}.status: unknown L3 status ${status}`);
      }
      if (isPass) {
        if (e.commit == null || e.version == null || e.date == null || e.expiresAt == null) {
          findings.push(
            `${path}: L3 PASS/VERIFIED requires commit, version, date, expiresAt`,
          );
        }
      }
      if (e.expiresAt != null && e.expiresAt !== '') {
        const exp = parseUtcDate(e.expiresAt);
        if (!exp) {
          findings.push(`${path}.expiresAt: invalid date ${JSON.stringify(e.expiresAt)}`);
        } else {
          const today = parseUtcDate(ctx.todayUtc) || new Date(`${ctx.todayUtc}T12:00:00.000Z`);
          if (exp < today && isPass) {
            findings.push(
              `${path}.expiresAt: L3 certification expired on ${e.expiresAt} (today ${ctx.todayUtc})`,
            );
          }
          // NOT VERIFIED may carry expired expiresAt without failing
        }
      }
    }
  }

  return findings;
}

/**
 * 校验整份矩阵。
 *
 * Business Logic（为什么需要这个函数）:
 *   CI / 本地一条命令校验权威矩阵，防止 coverage 断链。
 *
 * Code Logic（这个函数做什么）:
 *   加载上下文，遍历 entries 聚合 findings。
 *
 * @param {{ rootDir?: string, matrixPath?: string, todayUtc?: string, matrixData?: { entries: unknown[] } }} [opts]
 * @returns {{ ok: boolean, findings: string[], entryCount: number }}
 */
export function runCheck(opts = {}) {
  const rootDir = opts.rootDir || REPO_ROOT;
  const matrixPath =
    opts.matrixPath || join(rootDir, ...DEFAULT_MATRIX_REL.split('/'));
  const todayUtc = opts.todayUtc || utcToday();
  const data = opts.matrixData || loadMatrixFile(matrixPath);
  const webScripts = loadWebPackageScripts(rootDir);
  const workflowText = loadWorkflowRunText(rootDir);
  /** @type {Set<string>} */
  const seenIds = new Set();
  /** @type {string[]} */
  const findings = [];

  if (!Array.isArray(data.entries) || data.entries.length === 0) {
    findings.push('entries: matrix must contain at least one entry');
  } else {
    data.entries.forEach((entry, index) => {
      findings.push(
        ...validateEntry(entry, index, {
          rootDir,
          todayUtc,
          webScripts,
          workflowText,
          seenIds,
        }),
      );
    });
  }

  return {
    ok: findings.length === 0,
    findings,
    entryCount: Array.isArray(data.entries) ? data.entries.length : 0,
  };
}

/**
 * 从矩阵提取已知 evidence ID 集合。
 *
 * Business Logic（为什么需要这个函数）:
 *   docs 守卫只需 ID 存在性，不重复完整 JSON 校验。
 *
 * Code Logic（这个函数做什么）:
 *   读取 entries[].id 字符串集合。
 *
 * @param {string} [rootDir]
 * @param {string} [matrixRel]
 * @returns {Set<string>}
 */
export function loadKnownEvidenceIds(
  rootDir = REPO_ROOT,
  matrixRel = DEFAULT_MATRIX_REL,
) {
  const abs = join(rootDir, ...normalizeRelPath(matrixRel).split('/'));
  if (!existsSync(abs)) return new Set();
  try {
    const data = loadMatrixFile(abs);
    /** @type {Set<string>} */
    const ids = new Set();
    for (const entry of data.entries) {
      if (entry && typeof entry === 'object' && typeof /** @type {any} */ (entry).id === 'string') {
        ids.add(/** @type {any} */ (entry).id);
      }
    }
    return ids;
  } catch {
    return new Set();
  }
}

/**
 * 构造最小合法 entry（self-test 用）。
 *
 * Business Logic（为什么需要这个函数）:
 *   self-test 需要稳定基线 entry，再逐项破坏字段验证失败模式。
 *
 * Code Logic（这个函数做什么）:
 *   返回带默认 L1 字段的浅拷贝可改对象。
 *
 * @param {Partial<Record<string, unknown>>} [overrides]
 * @returns {Record<string, unknown>}
 */
function baseEntry(overrides = {}) {
  return {
    id: 'E2E-TRANSFER-001',
    surface: 'transfer',
    level: 'L1',
    tests: ['web/tests/transfer.spec.ts'],
    command: 'cd web && npm run test:e2e -- transfer.spec.ts',
    ciJob: 'frontend-e2e',
    platforms: ['chromium-linux'],
    exclusions: ['real Tauri file dialog', 'multi-host LAN'],
    ...overrides,
  };
}

/**
 * 运行内嵌 self-test。
 *
 * Business Logic（为什么需要这个函数）:
 *   门禁脚本本身必须可回归，覆盖 plan 列出的失败与通过 fixture。
 *
 * Code Logic（这个函数做什么）:
 *   在临时目录构造 package/workflow/test 文件与矩阵变体，断言 findings。
 *
 * @returns {number} exit code
 */
export function runSelfTest() {
  /** @type {string[]} */
  const failures = [];
  const dir = mkdtempSync(join(tmpdir(), 'quality-trace-'));

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
      process.stderr.write(`  FAIL ${name}: ${msg}\n`);
    }
  }

  /**
   * @param {boolean} cond
   * @param {string} message
   */
  function assert(cond, message) {
    if (!cond) throw new Error(message);
  }

  process.stdout.write('quality-traceability self-test\n');

  // 共享最小仓库骨架
  const write = (rel, body) => {
    const abs = join(dir, rel);
    mkdirSync(dirname(abs), { recursive: true });
    writeFileSync(abs, body, 'utf8');
  };
  write(
    'web/package.json',
    JSON.stringify(
      {
        name: 'fixture',
        scripts: {
          test: 'vitest run',
          'test:e2e': 'playwright test',
        },
      },
      null,
      2,
    ),
  );
  write('web/tests/transfer.spec.ts', 'export {};\n');
  write('web/tests/other.spec.ts', 'export {};\n');
  write('src-tauri/tests/quality_faults.rs', '// fixture\n');
  write(
    '.github/workflows/ci.yml',
    'name: CI\njobs:\n  quality:\n    steps:\n      - run: cargo test --locked --test quality_faults\n  frontend-e2e:\n    steps:\n      - run: npm run test:e2e\n  frontend-unit:\n    steps:\n      - run: npm test\n',
  );
  write(
    '.github/workflows/cross-platform-smoke.yml',
    'name: Cross-Platform Smoke\njobs:\n  smoke:\n    steps:\n      - run: cargo test --locked --test quality_faults\n',
  );
  write('docs/development/testing.md', '# testing\n');
  write('scripts/check-quality-traceability.mjs', '// self\n');

  const today = '2026-07-14';

  caseRun('duplicate-id', () => {
    const result = runCheck({
      rootDir: dir,
      todayUtc: today,
      matrixData: {
        entries: [baseEntry({ id: 'E2E-TRANSFER-001' }), baseEntry({ id: 'E2E-TRANSFER-001', surface: 'x' })],
      },
    });
    assert(!result.ok, 'expected fail');
    assert(
      result.findings.some((f) => /duplicate id E2E-TRANSFER-001/.test(f)),
      `expected duplicate finding, got ${result.findings.join(' | ')}`,
    );
  });

  caseRun('missing-test-file', () => {
    const result = runCheck({
      rootDir: dir,
      todayUtc: today,
      matrixData: {
        entries: [
          baseEntry({
            id: 'E2E-MISSING-001',
            tests: ['web/tests/does-not-exist.spec.ts'],
          }),
        ],
      },
    });
    assert(!result.ok, 'expected fail');
    assert(
      result.findings.some((f) => /missing test file/.test(f)),
      `expected missing test, got ${result.findings.join(' | ')}`,
    );
  });

  caseRun('unknown-level', () => {
    const result = runCheck({
      rootDir: dir,
      todayUtc: today,
      matrixData: {
        entries: [baseEntry({ id: 'E2E-LVL-001', level: 'L9' })],
      },
    });
    assert(!result.ok, 'expected fail');
    assert(
      result.findings.some((f) => /unknown level/.test(f)),
      `expected unknown level, got ${result.findings.join(' | ')}`,
    );
  });

  caseRun('unknown-job', () => {
    const result = runCheck({
      rootDir: dir,
      todayUtc: today,
      matrixData: {
        entries: [baseEntry({ id: 'E2E-JOB-001', ciJob: 'not-a-real-job' })],
      },
    });
    assert(!result.ok, 'expected fail');
    assert(
      result.findings.some((f) => /unknown job/.test(f)),
      `expected unknown job, got ${result.findings.join(' | ')}`,
    );
  });

  caseRun('command-not-backed', () => {
    const result = runCheck({
      rootDir: dir,
      todayUtc: today,
      matrixData: {
        entries: [
          baseEntry({
            id: 'E2E-CMD-001',
            command: 'cd web && npm run totally-missing-script',
          }),
        ],
      },
    });
    assert(!result.ok, 'expected fail');
    assert(
      result.findings.some((f) => /not in web\/package\.json|not backed/.test(f)),
      `expected command backing fail, got ${result.findings.join(' | ')}`,
    );
  });

  caseRun('expired-l3', () => {
    const result = runCheck({
      rootDir: dir,
      todayUtc: today,
      matrixData: {
        entries: [
          baseEntry({
            id: 'L3-EXPIRED-001',
            level: 'L3',
            tests: [],
            command: 'manual',
            ciJob: 'manual',
            platforms: ['macos'],
            exclusions: ['hosted runner'],
            status: 'PASS',
            commit: 'deadbeef',
            version: '0.0.0',
            date: '2026-01-01',
            expiresAt: '2026-01-15',
          }),
        ],
      },
    });
    assert(!result.ok, 'expected fail');
    assert(
      result.findings.some((f) => /expired/i.test(f)),
      `expected expired L3, got ${result.findings.join(' | ')}`,
    );
  });

  caseRun('nonexistent-doc-ref', () => {
    const result = runCheck({
      rootDir: dir,
      todayUtc: today,
      matrixData: {
        entries: [
          baseEntry({
            id: 'E2E-DOC-001',
            docs: ['docs/development/no-such-guide.md'],
          }),
        ],
      },
    });
    assert(!result.ok, 'expected fail');
    assert(
      result.findings.some((f) => /missing doc ref/.test(f)),
      `expected missing doc ref, got ${result.findings.join(' | ')}`,
    );
  });

  caseRun('valid-exclusions-and-pass', () => {
    const result = runCheck({
      rootDir: dir,
      todayUtc: today,
      matrixData: {
        entries: [
          baseEntry({
            id: 'E2E-TRANSFER-001',
            docs: ['docs/development/testing.md'],
          }),
          baseEntry({
            id: 'L2-QUALITY-FAULTS-001',
            surface: 'quality-faults',
            level: 'L2',
            tests: ['src-tauri/tests/quality_faults.rs'],
            command:
              'cd src-tauri && cargo test --locked --test quality_faults -- --nocapture --test-threads=1',
            ciJob: 'quality',
            platforms: ['ubuntu-22.04'],
            exclusions: ['real disk-full hardware fault'],
          }),
          baseEntry({
            id: 'L3-MACOS-GUI-PERMISSIONS-001',
            surface: 'macos-gui-permissions',
            level: 'L3',
            tests: [],
            command: 'manual',
            ciJob: 'manual',
            platforms: ['macos'],
            exclusions: ['hosted runner substitute'],
            status: 'NOT VERIFIED',
            commit: null,
            version: null,
            date: null,
            expiresAt: '2020-01-01',
          }),
        ],
      },
    });
    assert(result.ok, `expected pass, got ${result.findings.join(' | ')}`);
    assert(result.entryCount === 3, `expected 3 entries, got ${result.entryCount}`);
  });

  caseRun('invalid-exclusion-item', () => {
    const result = runCheck({
      rootDir: dir,
      todayUtc: today,
      matrixData: {
        entries: [
          baseEntry({
            id: 'E2E-EXCL-001',
            exclusions: ['ok', ''],
          }),
        ],
      },
    });
    assert(!result.ok, 'expected fail');
    assert(
      result.findings.some((f) => /exclusions/.test(f)),
      `expected exclusions fail, got ${result.findings.join(' | ')}`,
    );
  });

  rmSync(dir, { recursive: true, force: true });

  if (failures.length > 0) {
    process.stderr.write(`\nself-test failed: ${failures.length} case(s)\n`);
    return 1;
  }
  process.stdout.write('self-test passed\n');
  return 0;
}

/**
 * CLI 入口。
 *
 * Business Logic（为什么需要这个函数）:
 *   提供 --self-test / 默认矩阵检查 / --matrix 覆盖，供 CI 与本地复用。
 *
 * Code Logic（这个函数做什么）:
 *   解析 argv，分派到 self-test 或 runCheck，打印 findings 并设 exitCode。
 *
 * @param {string[]} [argv]
 * @returns {void}
 */
export function main(argv = process.argv.slice(2)) {
  if (argv.includes('--help') || argv.includes('-h')) {
    process.stdout.write(
      [
        'Usage:',
        '  node scripts/check-quality-traceability.mjs',
        '  node scripts/check-quality-traceability.mjs --self-test',
        '  node scripts/check-quality-traceability.mjs --matrix docs/development/quality-matrix.json',
        '',
        `Default matrix: ${DEFAULT_MATRIX_REL}`,
        `Known levels: ${[...KNOWN_LEVELS].join(', ')}`,
        `Known ciJobs: ${[...KNOWN_CI_JOBS].join(', ')}`,
        '',
      ].join('\n'),
    );
    return;
  }

  if (argv.includes('--self-test')) {
    process.exitCode = runSelfTest();
    return;
  }

  let matrixPath = join(REPO_ROOT, ...DEFAULT_MATRIX_REL.split('/'));
  const matrixFlag = argv.indexOf('--matrix');
  if (matrixFlag >= 0 && argv[matrixFlag + 1]) {
    matrixPath = resolve(REPO_ROOT, argv[matrixFlag + 1]);
  }

  try {
    const result = runCheck({ rootDir: REPO_ROOT, matrixPath });
    if (!result.ok) {
      for (const f of result.findings) {
        process.stderr.write(`${f}\n`);
      }
      process.stderr.write(
        `check-quality-traceability: ${result.findings.length} issue(s) in ${result.entryCount} entries\n`,
      );
      process.exitCode = 1;
      return;
    }
    process.stdout.write(
      `check-quality-traceability: ok (${result.entryCount} entries)\n`,
    );
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    process.stderr.write(`check-quality-traceability: ${msg}\n`);
    process.exitCode = 1;
  }
}

const isDirect =
  process.argv[1] &&
  resolve(process.argv[1]) === __filename;

if (isDirect) {
  main();
}
