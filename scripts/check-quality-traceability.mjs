#!/usr/bin/env node
/**
 * check-quality-traceability.mjs — 质量证据矩阵可追溯性门禁。
 *
 * Business Logic（为什么需要这个脚本）:
 *   S6 要求从产品 surface 追到稳定 evidence ID、测试文件、本地命令、CI job
 *   与明确 exclusions。人工维护会漏改 ID、引用不存在的测试，或把 L3 未执行
 *   项写成已验证。本脚本对 `docs/development/quality-matrix.json` 做零依赖静态校验，
 *   并提供 --self-test 防止门禁自身回归。N8 另固定 `macos-aarch64-beta`
 *   dependency closure：只消费 Apple Silicon 架构级 execution，不把聚合 PARTIAL/
 *   canonical NOT VERIFIED 提升为 full PASS，也不让延期平台阻断本机 beta。
 *
 * Code Logic（这个脚本做什么）:
 *   - 读取 quality-matrix.json，校验 id 唯一、level/ciJob 白名单、tests 文件存在、
 *     command 可回溯到 web/package.json script / scripts/* / cargo --test / workflow run、
 *     optional docs 路径存在、exclusions 为非空字符串数组。
 *   - L3：要求 status；PASS/VERIFIED 且 expiresAt 已过期则失败；
 *     NOT VERIFIED 允许缺少 commit/version/date/expiresAt，也允许 expiresAt 过期。
 *   - claimMode/claimProfile：固定 `platform-beta` + `macos-aarch64-beta` 闭包，
 *     校验 execution manifest、RC inventory、releasable 资产与 beta-only 发布元数据。
 *   - 不发明身份鉴权、不把 L1 mock 提升为 L3。
 *   - `--self-test` 用内存 fixture 覆盖 matrix 门禁 + macos-aarch64-beta profile 契约。
 *
 * Usage:
 *   node scripts/check-quality-traceability.mjs
 *   node scripts/check-quality-traceability.mjs --self-test
 *   node scripts/check-quality-traceability.mjs --matrix path/to/matrix.json
 *   node scripts/check-quality-traceability.mjs --claim-mode platform-beta --claim-profile macos-aarch64-beta \
 *     --subject-commit <40hex> --subject-tag <tag> --rc-run-id <id> --evidence-ref <ref>
 */

import {
  existsSync,
  mkdtempSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
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

/** 40 位小写/大写十六进制 commit SHA。 */
const COMMIT_SHA_RE = /^[0-9a-fA-F]{40}$/;

/** SHA-256 十六进制。 */
const SHA256_RE = /^[0-9a-fA-F]{64}$/;

/**
 * 当前固定的平台 beta profile 闭包（checker 拥有，调用者不可改 required IDs）。
 *
 * Business Logic（为什么需要这个常量）:
 *   N8 只允许 `macos-aarch64-beta`；required executions / matrix / uncertified surfaces
 *   必须由 checker 派生，禁止调用者传任意 allowlist 伪装 full 认证。
 *
 * Code Logic（这个常量做什么）:
 *   冻结 claimMode/claimProfile/selectedMatrixIds/requiredExecutions/uncertifiedSurfaces。
 *
 * @type {Readonly<{
 *   claimMode: string,
 *   claimProfile: string,
 *   selectedMatrixIds: readonly string[],
 *   requiredExecutions: readonly string[],
 *   uncertifiedSurfaces: readonly string[],
 *   evidenceValidityDays: number,
 *   allowedReleaseMode: string,
 * }>}
 */
export const MACOS_AARCH64_BETA_PROFILE = Object.freeze({
  claimMode: 'platform-beta',
  claimProfile: 'macos-aarch64-beta',
  selectedMatrixIds: Object.freeze(['macos-aarch64']),
  requiredExecutions: Object.freeze([
    'L3-MACOS-GUI-PERMISSIONS-001@macos-aarch64',
    'L3-MACOS-VOICEOVER-001@macos-aarch64',
  ]),
  uncertifiedSurfaces: Object.freeze([
    'windows',
    'wsl',
    'ubuntu',
    'macos-x86_64',
    'dual-host',
    'ios',
    'android',
    'nvda',
    'full-release',
    'stable-release',
  ]),
  evidenceValidityDays: 90,
  allowedReleaseMode: 'beta-prerelease',
});

/**
 * 已登记 claim profile 表（当前仅 macos-aarch64-beta）。
 * @type {ReadonlyMap<string, typeof MACOS_AARCH64_BETA_PROFILE>}
 */
export const CLAIM_PROFILES = Object.freeze(
  new Map([[MACOS_AARCH64_BETA_PROFILE.claimProfile, MACOS_AARCH64_BETA_PROFILE]]),
);

/**
 * 解析固定 claim profile。
 *
 * Business Logic（为什么需要这个函数）:
 *   release gate 与 self-test 必须拒绝未知 profile 或 claimMode 错配，防止 stable/full 伪装。
 *
 * Code Logic（这个函数做什么）:
 *   仅接受 claimMode=platform-beta + 已登记 claimProfile；否则返回 findings。
 *
 * @param {string} claimMode
 * @param {string} claimProfile
 * @returns {{ ok: boolean, profile: typeof MACOS_AARCH64_BETA_PROFILE | null, findings: string[] }}
 */
export function resolveClaimProfile(claimMode, claimProfile) {
  /** @type {string[]} */
  const findings = [];
  if (claimMode !== 'platform-beta') {
    findings.push(
      `claimMode: only "platform-beta" is allowed (got ${JSON.stringify(claimMode)})`,
    );
  }
  const profile = CLAIM_PROFILES.get(String(claimProfile || ''));
  if (!profile) {
    findings.push(
      `claimProfile: unknown profile ${JSON.stringify(claimProfile)} (expected macos-aarch64-beta)`,
    );
    return { ok: false, profile: null, findings };
  }
  if (claimMode !== profile.claimMode) {
    findings.push(
      `claimMode/claimProfile mismatch: profile requires ${profile.claimMode}`,
    );
  }
  return { ok: findings.length === 0, profile, findings };
}

/**
 * 解析 `STABLE-ID@matrix` execution key。
 *
 * Business Logic（为什么需要这个函数）:
 *   required executions 以稳定 ID + 架构矩阵绑定；禁止只写聚合 row 而不带架构。
 *
 * Code Logic（这个函数做什么）:
 *   拆分 `@` 得到 stableId 与 artifactMatrixId；非法返回 null。
 *
 * @param {string} key
 * @returns {{ stableId: string, artifactMatrixId: string } | null}
 */
export function parseExecutionKey(key) {
  const raw = String(key || '').trim();
  const at = raw.lastIndexOf('@');
  if (at <= 0 || at === raw.length - 1) return null;
  const stableId = raw.slice(0, at);
  const artifactMatrixId = raw.slice(at + 1);
  if (!stableId || !artifactMatrixId) return null;
  return { stableId, artifactMatrixId };
}

/**
 * 解析 RFC3339 或 YYYY-MM-DD 时间戳为 Date。
 *
 * Business Logic（为什么需要这个函数）:
 *   execution expiresAt 可能写日历日或完整 RFC3339，过期判定需统一。
 *
 * Code Logic（这个函数做什么）:
 *   YYYY-MM-DD → UTC 正午；RFC3339 直接 Date 解析；非法返回 null。
 *
 * @param {unknown} value
 * @returns {Date | null}
 */
export function parseTimestamp(value) {
  if (typeof value !== 'string' || !value.trim()) return null;
  const s = value.trim();
  if (/^\d{4}-\d{2}-\d{2}$/.test(s)) return parseUtcDate(s);
  const d = new Date(s);
  if (Number.isNaN(d.getTime())) return null;
  return d;
}

/**
 * 校验单条架构级 execution manifest。
 *
 * Business Logic（为什么需要这个函数）:
 *   beta 只能消费匹配架构的 PASS execution；错误架构、过期、缺包 SHA 或伪 PASS 必须拒绝。
 *
 * Code Logic（这个函数做什么）:
 *   校验 stableId/matrix/subject/version/rc/package SHA/result/expires/artifactShas；
 *   不读取质量矩阵聚合 status 来决定 PASS。
 *
 * @param {unknown} manifest
 * @param {{
 *   expectedStableId: string,
 *   expectedMatrixId: string,
 *   subjectCommit: string,
 *   appVersion?: string,
 *   rcWorkflowRunId?: string,
 *   packageSha256?: string,
 *   now?: Date,
 *   evidenceValidityDays?: number,
 * }} ctx
 * @returns {string[]}
 */
export function validateExecutionManifest(manifest, ctx) {
  /** @type {string[]} */
  const findings = [];
  const label = `${ctx.expectedStableId}@${ctx.expectedMatrixId}`;
  if (!manifest || typeof manifest !== 'object' || Array.isArray(manifest)) {
    findings.push(`${label}: execution manifest must be an object`);
    return findings;
  }
  /** @type {Record<string, unknown>} */
  const m = /** @type {Record<string, unknown>} */ (manifest);

  if (m.stableId !== ctx.expectedStableId) {
    findings.push(
      `${label}: stableId mismatch (got ${JSON.stringify(m.stableId)})`,
    );
  }
  if (m.artifactMatrixId !== ctx.expectedMatrixId) {
    findings.push(
      `${label}: wrong architecture/matrix (got ${JSON.stringify(m.artifactMatrixId)}; expected ${ctx.expectedMatrixId})`,
    );
  }
  if (typeof m.subjectCommit !== 'string' || !COMMIT_SHA_RE.test(m.subjectCommit)) {
    findings.push(`${label}: subjectCommit must be 40-hex`);
  } else if (m.subjectCommit.toLowerCase() !== ctx.subjectCommit.toLowerCase()) {
    findings.push(`${label}: subjectCommit does not match claim subject`);
  }
  if (typeof m.appVersion !== 'string' || !m.appVersion.trim()) {
    findings.push(`${label}: appVersion required`);
  } else if (ctx.appVersion && m.appVersion !== ctx.appVersion) {
    findings.push(`${label}: appVersion mismatch`);
  }
  if (
    m.rcWorkflowRunId == null ||
    (typeof m.rcWorkflowRunId !== 'string' && typeof m.rcWorkflowRunId !== 'number') ||
    String(m.rcWorkflowRunId).trim() === ''
  ) {
    findings.push(`${label}: rcWorkflowRunId required`);
  } else if (
    ctx.rcWorkflowRunId != null &&
    String(m.rcWorkflowRunId) !== String(ctx.rcWorkflowRunId)
  ) {
    findings.push(`${label}: rcWorkflowRunId mismatch`);
  }
  if (typeof m.packageFilename !== 'string' || !m.packageFilename.trim()) {
    findings.push(`${label}: packageFilename required`);
  }
  if (typeof m.packageSha256 !== 'string' || !SHA256_RE.test(m.packageSha256)) {
    findings.push(`${label}: packageSha256 must be 64-hex`);
  } else if (
    ctx.packageSha256 &&
    m.packageSha256.toLowerCase() !== ctx.packageSha256.toLowerCase()
  ) {
    findings.push(`${label}: packageSha256 does not match RC inventory`);
  }
  if (typeof m.deviceClass !== 'string' || !m.deviceClass.trim()) {
    findings.push(`${label}: deviceClass required (redacted class, not hostname)`);
  }
  if (typeof m.osBuild !== 'string' || !m.osBuild.trim()) {
    findings.push(`${label}: osBuild required`);
  }
  if (typeof m.executorId !== 'string' || !m.executorId.trim()) {
    findings.push(`${label}: executorId required`);
  }
  const executedAt = parseTimestamp(m.executedAt);
  if (!executedAt) {
    findings.push(`${label}: executedAt must be RFC3339 or YYYY-MM-DD`);
  }
  const expiresAt = parseTimestamp(m.expiresAt);
  if (!expiresAt) {
    findings.push(`${label}: expiresAt must be RFC3339 or YYYY-MM-DD`);
  } else {
    const now = ctx.now || new Date();
    if (expiresAt < now) {
      findings.push(`${label}: execution expired at ${m.expiresAt}`);
    }
    if (executedAt) {
      const maxDays = ctx.evidenceValidityDays ?? 90;
      const maxMs = maxDays * 24 * 60 * 60 * 1000;
      if (expiresAt.getTime() - executedAt.getTime() > maxMs + 24 * 60 * 60 * 1000) {
        findings.push(
          `${label}: expiresAt window exceeds ${maxDays} days from executedAt`,
        );
      }
    }
  }
  const result = typeof m.result === 'string' ? m.result.trim().toUpperCase() : '';
  if (result !== 'PASS') {
    findings.push(
      `${label}: result must be PASS for beta consumption (got ${JSON.stringify(m.result)})`,
    );
  }
  // 禁止仅靠聚合 prose/canonical status 冒充 execution PASS
  if (m.aggregateStatus != null || m.canonicalStatus != null || m.matrixRowStatus != null) {
    findings.push(
      `${label}: aggregate/canonical matrix status fields are not accepted as execution PASS`,
    );
  }
  if (!m.artifactShas || typeof m.artifactShas !== 'object' || Array.isArray(m.artifactShas)) {
    findings.push(`${label}: artifactShas object required with non-empty checksums`);
  } else {
    const shas = /** @type {Record<string, unknown>} */ (m.artifactShas);
    const keys = Object.keys(shas);
    if (keys.length === 0) {
      findings.push(`${label}: artifactShas must be non-empty`);
    }
    for (const k of keys) {
      if (typeof shas[k] !== 'string' || !SHA256_RE.test(/** @type {string} */ (shas[k]))) {
        findings.push(`${label}: artifactShas[${k}] must be 64-hex`);
      }
    }
  }
  return findings;
}

/**
 * 校验 RC artifact inventory（production releasable + harness non-releasable）。
 *
 * Business Logic（为什么需要这个函数）:
 *   beta 只能发布 releasable=true 的 macos-aarch64 生产资产；harness 必须 releasable=false，
 *   且不得进入发布集合。
 *
 * Code Logic（这个函数做什么）:
 *   校验 matrix、subject、资产 SHA、releasable 标记与禁止的跨平台 matrix。
 *
 * @param {unknown} inventory
 * @param {{
 *   subjectCommit: string,
 *   allowedMatrixIds: readonly string[],
 *   requireProductionReleasable?: boolean,
 * }} ctx
 * @returns {string[]}
 */
export function validateRcInventory(inventory, ctx) {
  /** @type {string[]} */
  const findings = [];
  if (!inventory || typeof inventory !== 'object' || Array.isArray(inventory)) {
    findings.push('rcInventory: must be an object');
    return findings;
  }
  /** @type {Record<string, unknown>} */
  const inv = /** @type {Record<string, unknown>} */ (inventory);
  if (typeof inv.subjectCommit !== 'string' || !COMMIT_SHA_RE.test(inv.subjectCommit)) {
    findings.push('rcInventory.subjectCommit must be 40-hex');
  } else if (inv.subjectCommit.toLowerCase() !== ctx.subjectCommit.toLowerCase()) {
    findings.push('rcInventory.subjectCommit does not match claim subject');
  }
  if (typeof inv.matrixId !== 'string' || !ctx.allowedMatrixIds.includes(inv.matrixId)) {
    findings.push(
      `rcInventory.matrixId: only ${ctx.allowedMatrixIds.join(',')} allowed (got ${JSON.stringify(inv.matrixId)})`,
    );
  }
  if (typeof inv.rcWorkflowRunId !== 'string' && typeof inv.rcWorkflowRunId !== 'number') {
    findings.push('rcInventory.rcWorkflowRunId required');
  }
  if (!Array.isArray(inv.assets) || inv.assets.length === 0) {
    findings.push('rcInventory.assets: non-empty array required');
    return findings;
  }
  let productionCount = 0;
  let harnessCount = 0;
  inv.assets.forEach((asset, i) => {
    const path = `rcInventory.assets[${i}]`;
    if (!asset || typeof asset !== 'object' || Array.isArray(asset)) {
      findings.push(`${path}: must be object`);
      return;
    }
    /** @type {Record<string, unknown>} */
    const a = /** @type {Record<string, unknown>} */ (asset);
    if (typeof a.name !== 'string' || !a.name.trim()) {
      findings.push(`${path}.name required`);
    }
    if (typeof a.sha256 !== 'string' || !SHA256_RE.test(a.sha256)) {
      findings.push(`${path}.sha256 must be 64-hex`);
    }
    if (typeof a.releasable !== 'boolean') {
      findings.push(`${path}.releasable must be boolean`);
    }
    const name = typeof a.name === 'string' ? a.name : '';
    const isHarness =
      a.role === 'updater-harness' ||
      /harness/i.test(name) ||
      a.certificationMarker === true;
    if (a.releasable === true) {
      productionCount += 1;
      if (isHarness) {
        findings.push(
          `${path}: harness/certification asset must be releasable=false`,
        );
      }
      if (/(windows|linux|x86_64|x64|amd64|intel)/i.test(name) && !/aarch64/i.test(name)) {
        findings.push(
          `${path}: Windows/Linux/Intel asset not allowed for macos-aarch64-beta`,
        );
      }
    } else if (a.releasable === false) {
      harnessCount += 1;
    }
  });
  if (ctx.requireProductionReleasable !== false && productionCount === 0) {
    findings.push('rcInventory: at least one releasable=true production asset required');
  }
  // harness 可选但若存在必须 releasable=false（上面已检查）
  if (harnessCount > 0 && productionCount === 0) {
    findings.push(
      'rcInventory: cannot publish when only releasable=false assets are present',
    );
  }
  return findings;
}

/**
 * 校验 release-claim.json 结构（required IDs 只能由 profile 派生）。
 *
 * Business Logic（为什么需要这个函数）:
 *   调用者不得传入任意 requiredExecutions allowlist 来绕过 VoiceOver/GUI 依赖。
 *
 * Code Logic（这个函数做什么）:
 *   固定 claimMode/profile/selectedMatrix；若调用方带 requiredExecutions 必须与 profile 完全一致；
 *   拒绝 stable metadata / non-beta releaseMode。
 *
 * @param {unknown} claim
 * @param {typeof MACOS_AARCH64_BETA_PROFILE} profile
 * @returns {string[]}
 */
export function validateReleaseClaimDocument(claim, profile) {
  /** @type {string[]} */
  const findings = [];
  if (!claim || typeof claim !== 'object' || Array.isArray(claim)) {
    findings.push('releaseClaim: must be an object');
    return findings;
  }
  /** @type {Record<string, unknown>} */
  const c = /** @type {Record<string, unknown>} */ (claim);
  if (c.claimMode !== profile.claimMode) {
    findings.push(
      `releaseClaim.claimMode must be ${profile.claimMode} (got ${JSON.stringify(c.claimMode)})`,
    );
  }
  if (c.claimProfile !== profile.claimProfile) {
    findings.push(
      `releaseClaim.claimProfile must be ${profile.claimProfile} (got ${JSON.stringify(c.claimProfile)})`,
    );
  }
  if (!Array.isArray(c.selectedMatrixIds)) {
    findings.push('releaseClaim.selectedMatrixIds must be an array');
  } else {
    const got = c.selectedMatrixIds.map(String).sort().join(',');
    const exp = [...profile.selectedMatrixIds].sort().join(',');
    if (got !== exp) {
      findings.push(
        `releaseClaim.selectedMatrixIds must be exactly [${profile.selectedMatrixIds.join(',')}] (got ${JSON.stringify(c.selectedMatrixIds)})`,
      );
    }
    for (const mid of c.selectedMatrixIds) {
      if (
        /windows|linux|ubuntu|x86_64|intel/i.test(String(mid)) &&
        String(mid) !== 'macos-aarch64'
      ) {
        findings.push(
          `releaseClaim.selectedMatrixIds rejects Windows/Linux/Intel matrix ${JSON.stringify(mid)}`,
        );
      }
    }
  }
  if (c.requiredExecutions != null) {
    if (!Array.isArray(c.requiredExecutions)) {
      findings.push('releaseClaim.requiredExecutions must be an array when present');
    } else {
      const got = [...c.requiredExecutions].map(String).sort();
      const exp = [...profile.requiredExecutions].map(String).sort();
      if (got.length !== exp.length || got.some((v, i) => v !== exp[i])) {
        findings.push(
          'releaseClaim.requiredExecutions must equal checker-owned profile closure; arbitrary allowlists are rejected',
        );
      }
    }
  }
  if (c.releaseMode != null && c.releaseMode !== profile.allowedReleaseMode) {
    findings.push(
      `releaseClaim.releaseMode must be ${profile.allowedReleaseMode} or omitted (got ${JSON.stringify(c.releaseMode)})`,
    );
  }
  if (c.stableMetadata === true || c.mutateLatestJson === true || c.publishLatestJson === true) {
    findings.push(
      'releaseClaim: stable metadata / latest.json mutation is forbidden for beta profile',
    );
  }
  if (c.prerelease === false) {
    findings.push('releaseClaim.prerelease must not be false for beta profile');
  }
  if (!Array.isArray(c.uncertifiedSurfaces) || c.uncertifiedSurfaces.length === 0) {
    findings.push(
      'releaseClaim.uncertifiedSurfaces: must list deferred surfaces (windows/wsl/ubuntu/intel/...)',
    );
  }
  return findings;
}

/**
 * 运行 macos-aarch64-beta go/no-go 检查。
 *
 * Business Logic（为什么需要这个函数）:
 *   Task 5/6 与 self-test 需要统一入口：两 execution + RC inventory + claim 文档 + 拒绝延期平台。
 *
 * Code Logic（这个函数做什么）:
 *   resolve profile → 校验 claim → 逐 required execution 校验 manifest → inventory → 汇总 findings。
 *
 * @param {{
 *   claimMode: string,
 *   claimProfile: string,
 *   subjectCommit: string,
 *   subjectTag?: string,
 *   rcWorkflowRunId?: string,
 *   evidenceRef?: string,
 *   expectedEvidenceCommit?: string,
 *   appVersion?: string,
 *   packageSha256?: string,
 *   executions?: Record<string, unknown>,
 *   rcInventory?: unknown,
 *   releaseClaim?: unknown,
 *   now?: Date,
 * }} opts
 * @returns {{ ok: boolean, decision: 'GO' | 'NO-GO', findings: string[], profile: typeof MACOS_AARCH64_BETA_PROFILE | null }}
 */
export function runBetaClaimCheck(opts) {
  /** @type {string[]} */
  const findings = [];
  const resolved = resolveClaimProfile(opts.claimMode, opts.claimProfile);
  findings.push(...resolved.findings);
  const profile = resolved.profile;
  if (!profile) {
    return { ok: false, decision: 'NO-GO', findings, profile: null };
  }
  if (typeof opts.subjectCommit !== 'string' || !COMMIT_SHA_RE.test(opts.subjectCommit)) {
    findings.push('subjectCommit must be 40-hex');
  }
  if (opts.subjectTag != null && (typeof opts.subjectTag !== 'string' || !opts.subjectTag.trim())) {
    findings.push('subjectTag must be non-empty when provided');
  }
  if (opts.expectedEvidenceCommit != null) {
    if (
      typeof opts.expectedEvidenceCommit !== 'string' ||
      !COMMIT_SHA_RE.test(opts.expectedEvidenceCommit)
    ) {
      findings.push('expectedEvidenceCommit must be 40-hex when provided');
    }
  }
  if (opts.releaseClaim != null) {
    findings.push(...validateReleaseClaimDocument(opts.releaseClaim, profile));
  }
  if (opts.rcInventory != null) {
    findings.push(
      ...validateRcInventory(opts.rcInventory, {
        subjectCommit: opts.subjectCommit,
        allowedMatrixIds: profile.selectedMatrixIds,
      }),
    );
  }
  const executions = opts.executions || {};
  for (const key of profile.requiredExecutions) {
    const parsed = parseExecutionKey(key);
    if (!parsed) {
      findings.push(`required execution key malformed: ${key}`);
      continue;
    }
    const manifest = executions[key] ?? executions[parsed.stableId];
    if (manifest == null) {
      findings.push(`missing required execution: ${key}`);
      continue;
    }
    findings.push(
      ...validateExecutionManifest(manifest, {
        expectedStableId: parsed.stableId,
        expectedMatrixId: parsed.artifactMatrixId,
        subjectCommit: opts.subjectCommit,
        appVersion: opts.appVersion,
        rcWorkflowRunId: opts.rcWorkflowRunId,
        packageSha256: opts.packageSha256,
        now: opts.now,
        evidenceValidityDays: profile.evidenceValidityDays,
      }),
    );
  }
  // 显式拒绝把聚合 prose 当 PASS
  if (opts.releaseClaim && /** @type {any} */ (opts.releaseClaim).aggregateProsePass === true) {
    findings.push(
      'aggregate prose PASS without architecture execution PASS is rejected',
    );
  }
  const ok = findings.length === 0;
  return {
    ok,
    decision: ok ? 'GO' : 'NO-GO',
    findings,
    profile,
  };
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
 * 构造合法 Apple Silicon execution fixture。
 *
 * Business Logic（为什么需要这个函数）:
 *   self-test 需要可复用的 PASS execution 基线，再逐项破坏验证拒绝路径。
 *
 * Code Logic（这个函数做什么）:
 *   返回满足 validateExecutionManifest 的对象浅拷贝。
 *
 * @param {string} stableId
 * @param {Partial<Record<string, unknown>>} [overrides]
 * @returns {Record<string, unknown>}
 */
function baseExecution(stableId, overrides = {}) {
  return {
    stableId,
    artifactMatrixId: 'macos-aarch64',
    subjectCommit: 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
    appVersion: '0.6.7',
    rcWorkflowRunId: '1234567890',
    packageFilename: 'cc-partner_0.6.7_aarch64.dmg',
    packageSha256: 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
    deviceClass: 'apple-silicon-mac',
    osBuild: 'macOS 15.5 (24F74)',
    executorId: 'operator-self-test',
    executedAt: '2026-07-14T12:00:00.000Z',
    expiresAt: '2026-10-12T12:00:00.000Z',
    result: 'PASS',
    checklist: { sample: 'pass' },
    artifactShas: {
      'notes.md': 'cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc',
    },
    ...overrides,
  };
}

/**
 * 构造合法 RC inventory fixture。
 *
 * @param {Partial<Record<string, unknown>>} [overrides]
 * @returns {Record<string, unknown>}
 */
function baseRcInventory(overrides = {}) {
  return {
    subjectCommit: 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
    subjectTag: 'v0.6.7-rc.1',
    rcWorkflowRunId: '1234567890',
    matrixId: 'macos-aarch64',
    assets: [
      {
        name: 'cc-partner_0.6.7_aarch64.dmg',
        sha256: 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
        releasable: true,
        role: 'production-dmg',
      },
      {
        name: 'cc-partner_aarch64.app.tar.gz',
        sha256: 'dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd',
        releasable: true,
        role: 'production-updater',
      },
      {
        name: 'cc-partner_aarch64.app.tar.gz.sig',
        sha256: 'eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee',
        releasable: true,
        role: 'production-sig',
      },
      {
        name: 'updater-harness_0.6.6_aarch64.dmg',
        sha256: 'ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff',
        releasable: false,
        role: 'updater-harness',
        certificationMarker: true,
      },
    ],
    expiresAt: '2026-10-12T12:00:00.000Z',
    signingSummary: 'ad-hoc signingIdentity=-',
    ...overrides,
  };
}

/**
 * 构造合法 release claim fixture。
 *
 * @param {Partial<Record<string, unknown>>} [overrides]
 * @returns {Record<string, unknown>}
 */
function baseReleaseClaim(overrides = {}) {
  return {
    claimMode: 'platform-beta',
    claimProfile: 'macos-aarch64-beta',
    selectedMatrixIds: ['macos-aarch64'],
    releaseMode: 'beta-prerelease',
    prerelease: true,
    uncertifiedSurfaces: [
      'windows',
      'wsl',
      'ubuntu',
      'macos-x86_64',
      'dual-host',
      'ios',
      'android',
      'nvda',
      'full-release',
      'stable-release',
    ],
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

  // ── macos-aarch64-beta profile contract ─────────────────────────────
  const subject = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa';
  const pkgSha = 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb';
  const now = new Date('2026-07-15T12:00:00.000Z');

  caseRun('beta-accepts-apple-silicon-gui-and-voiceover', () => {
    const result = runBetaClaimCheck({
      claimMode: 'platform-beta',
      claimProfile: 'macos-aarch64-beta',
      subjectCommit: subject,
      subjectTag: 'v0.6.7-rc.1',
      rcWorkflowRunId: '1234567890',
      appVersion: '0.6.7',
      packageSha256: pkgSha,
      now,
      executions: {
        'L3-MACOS-GUI-PERMISSIONS-001@macos-aarch64': baseExecution(
          'L3-MACOS-GUI-PERMISSIONS-001',
        ),
        'L3-MACOS-VOICEOVER-001@macos-aarch64': baseExecution(
          'L3-MACOS-VOICEOVER-001',
        ),
      },
      rcInventory: baseRcInventory(),
      releaseClaim: baseReleaseClaim(),
    });
    assert(result.ok, `expected GO, got ${result.findings.join(' | ')}`);
    assert(result.decision === 'GO', 'decision must be GO');
    assert(
      result.profile?.claimProfile === 'macos-aarch64-beta',
      'profile must be fixed macos-aarch64-beta',
    );
  });

  caseRun('beta-rejects-missing-voiceover-dependency', () => {
    const result = runBetaClaimCheck({
      claimMode: 'platform-beta',
      claimProfile: 'macos-aarch64-beta',
      subjectCommit: subject,
      now,
      executions: {
        'L3-MACOS-GUI-PERMISSIONS-001@macos-aarch64': baseExecution(
          'L3-MACOS-GUI-PERMISSIONS-001',
        ),
      },
      rcInventory: baseRcInventory(),
      releaseClaim: baseReleaseClaim(),
    });
    assert(!result.ok, 'expected NO-GO');
    assert(
      result.findings.some((f) => /missing required execution: L3-MACOS-VOICEOVER-001@macos-aarch64/.test(f)),
      `expected missing VoiceOver, got ${result.findings.join(' | ')}`,
    );
  });

  caseRun('beta-rejects-wrong-architecture-execution', () => {
    const result = runBetaClaimCheck({
      claimMode: 'platform-beta',
      claimProfile: 'macos-aarch64-beta',
      subjectCommit: subject,
      now,
      executions: {
        'L3-MACOS-GUI-PERMISSIONS-001@macos-aarch64': baseExecution(
          'L3-MACOS-GUI-PERMISSIONS-001',
          { artifactMatrixId: 'macos-x86_64' },
        ),
        'L3-MACOS-VOICEOVER-001@macos-aarch64': baseExecution(
          'L3-MACOS-VOICEOVER-001',
        ),
      },
      releaseClaim: baseReleaseClaim(),
    });
    assert(!result.ok, 'expected NO-GO');
    assert(
      result.findings.some((f) => /wrong architecture/.test(f)),
      `expected wrong architecture, got ${result.findings.join(' | ')}`,
    );
  });

  caseRun('beta-rejects-aggregate-prose-without-execution-pass', () => {
    const result = runBetaClaimCheck({
      claimMode: 'platform-beta',
      claimProfile: 'macos-aarch64-beta',
      subjectCommit: subject,
      now,
      executions: {
        'L3-MACOS-GUI-PERMISSIONS-001@macos-aarch64': baseExecution(
          'L3-MACOS-GUI-PERMISSIONS-001',
          { result: 'NOT VERIFIED', aggregateStatus: 'PARTIAL' },
        ),
        'L3-MACOS-VOICEOVER-001@macos-aarch64': baseExecution(
          'L3-MACOS-VOICEOVER-001',
        ),
      },
      releaseClaim: baseReleaseClaim({ aggregateProsePass: true }),
    });
    assert(!result.ok, 'expected NO-GO');
    assert(
      result.findings.some((f) => /result must be PASS|aggregate prose PASS/.test(f)),
      `expected aggregate prose rejection, got ${result.findings.join(' | ')}`,
    );
  });

  caseRun('beta-rejects-expired-execution', () => {
    const result = runBetaClaimCheck({
      claimMode: 'platform-beta',
      claimProfile: 'macos-aarch64-beta',
      subjectCommit: subject,
      now,
      executions: {
        'L3-MACOS-GUI-PERMISSIONS-001@macos-aarch64': baseExecution(
          'L3-MACOS-GUI-PERMISSIONS-001',
          { expiresAt: '2026-01-01T00:00:00.000Z' },
        ),
        'L3-MACOS-VOICEOVER-001@macos-aarch64': baseExecution(
          'L3-MACOS-VOICEOVER-001',
        ),
      },
      releaseClaim: baseReleaseClaim(),
    });
    assert(!result.ok, 'expected NO-GO');
    assert(
      result.findings.some((f) => /expired/i.test(f)),
      `expected expired execution, got ${result.findings.join(' | ')}`,
    );
  });

  caseRun('beta-rejects-mismatched-package-sha', () => {
    const result = runBetaClaimCheck({
      claimMode: 'platform-beta',
      claimProfile: 'macos-aarch64-beta',
      subjectCommit: subject,
      packageSha256: pkgSha,
      now,
      executions: {
        'L3-MACOS-GUI-PERMISSIONS-001@macos-aarch64': baseExecution(
          'L3-MACOS-GUI-PERMISSIONS-001',
          {
            packageSha256:
              '1111111111111111111111111111111111111111111111111111111111111111',
          },
        ),
        'L3-MACOS-VOICEOVER-001@macos-aarch64': baseExecution(
          'L3-MACOS-VOICEOVER-001',
        ),
      },
      releaseClaim: baseReleaseClaim(),
    });
    assert(!result.ok, 'expected NO-GO');
    assert(
      result.findings.some((f) => /packageSha256 does not match/.test(f)),
      `expected package SHA mismatch, got ${result.findings.join(' | ')}`,
    );
  });

  caseRun('beta-rejects-arbitrary-required-id-allowlist', () => {
    const result = runBetaClaimCheck({
      claimMode: 'platform-beta',
      claimProfile: 'macos-aarch64-beta',
      subjectCommit: subject,
      now,
      executions: {
        'L3-MACOS-GUI-PERMISSIONS-001@macos-aarch64': baseExecution(
          'L3-MACOS-GUI-PERMISSIONS-001',
        ),
        'L3-MACOS-VOICEOVER-001@macos-aarch64': baseExecution(
          'L3-MACOS-VOICEOVER-001',
        ),
      },
      releaseClaim: baseReleaseClaim({
        requiredExecutions: ['L3-MACOS-GUI-PERMISSIONS-001@macos-aarch64'],
      }),
    });
    assert(!result.ok, 'expected NO-GO');
    assert(
      result.findings.some((f) => /arbitrary allowlists are rejected|requiredExecutions must equal/.test(f)),
      `expected allowlist rejection, got ${result.findings.join(' | ')}`,
    );
  });

  caseRun('beta-rejects-releasable-false-production-path', () => {
    const result = runBetaClaimCheck({
      claimMode: 'platform-beta',
      claimProfile: 'macos-aarch64-beta',
      subjectCommit: subject,
      now,
      executions: {
        'L3-MACOS-GUI-PERMISSIONS-001@macos-aarch64': baseExecution(
          'L3-MACOS-GUI-PERMISSIONS-001',
        ),
        'L3-MACOS-VOICEOVER-001@macos-aarch64': baseExecution(
          'L3-MACOS-VOICEOVER-001',
        ),
      },
      rcInventory: baseRcInventory({
        assets: [
          {
            name: 'updater-harness_only.dmg',
            sha256: 'ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff',
            releasable: false,
            role: 'updater-harness',
            certificationMarker: true,
          },
        ],
      }),
      releaseClaim: baseReleaseClaim(),
    });
    assert(!result.ok, 'expected NO-GO');
    assert(
      result.findings.some((f) => /releasable=true production asset|only releasable=false/.test(f)),
      `expected releasable=false rejection, got ${result.findings.join(' | ')}`,
    );
  });

  caseRun('beta-rejects-windows-linux-intel-selected-matrix', () => {
    const result = runBetaClaimCheck({
      claimMode: 'platform-beta',
      claimProfile: 'macos-aarch64-beta',
      subjectCommit: subject,
      now,
      executions: {
        'L3-MACOS-GUI-PERMISSIONS-001@macos-aarch64': baseExecution(
          'L3-MACOS-GUI-PERMISSIONS-001',
        ),
        'L3-MACOS-VOICEOVER-001@macos-aarch64': baseExecution(
          'L3-MACOS-VOICEOVER-001',
        ),
      },
      rcInventory: baseRcInventory({ matrixId: 'windows-x86_64' }),
      releaseClaim: baseReleaseClaim({
        selectedMatrixIds: ['windows-x86_64', 'linux-x86_64', 'macos-x86_64'],
      }),
    });
    assert(!result.ok, 'expected NO-GO');
    assert(
      result.findings.some((f) => /selectedMatrixIds|matrixId/.test(f)),
      `expected matrix rejection, got ${result.findings.join(' | ')}`,
    );
  });

  caseRun('beta-rejects-stable-metadata-or-non-beta-release-mode', () => {
    const result = runBetaClaimCheck({
      claimMode: 'platform-beta',
      claimProfile: 'macos-aarch64-beta',
      subjectCommit: subject,
      now,
      executions: {
        'L3-MACOS-GUI-PERMISSIONS-001@macos-aarch64': baseExecution(
          'L3-MACOS-GUI-PERMISSIONS-001',
        ),
        'L3-MACOS-VOICEOVER-001@macos-aarch64': baseExecution(
          'L3-MACOS-VOICEOVER-001',
        ),
      },
      releaseClaim: baseReleaseClaim({
        releaseMode: 'stable',
        mutateLatestJson: true,
        prerelease: false,
      }),
    });
    assert(!result.ok, 'expected NO-GO');
    assert(
      result.findings.some((f) => /stable metadata|releaseMode|prerelease/.test(f)),
      `expected stable metadata rejection, got ${result.findings.join(' | ')}`,
    );
  });

  caseRun('beta-rejects-unknown-claim-profile', () => {
    const result = runBetaClaimCheck({
      claimMode: 'platform-beta',
      claimProfile: 'full-stable',
      subjectCommit: subject,
      now,
      executions: {},
    });
    assert(!result.ok, 'expected NO-GO');
    assert(
      result.findings.some((f) => /unknown profile/.test(f)),
      `expected unknown profile, got ${result.findings.join(' | ')}`,
    );
  });

  caseRun('beta-rejects-harness-marked-releasable', () => {
    const findings = validateRcInventory(
      baseRcInventory({
        assets: [
          {
            name: 'updater-harness_0.6.6_aarch64.dmg',
            sha256: 'ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff',
            releasable: true,
            role: 'updater-harness',
            certificationMarker: true,
          },
          {
            name: 'cc-partner_0.6.7_aarch64.dmg',
            sha256: pkgSha,
            releasable: true,
            role: 'production-dmg',
          },
        ],
      }),
      { subjectCommit: subject, allowedMatrixIds: ['macos-aarch64'] },
    );
    assert(findings.length > 0, 'expected findings');
    assert(
      findings.some((f) => /releasable=false/.test(f)),
      `expected harness releasable rejection, got ${findings.join(' | ')}`,
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
/**
 * 读取 CLI 命名参数值。
 *
 * Business Logic（为什么需要这个函数）:
 *   beta claim 检查需要多个可选 flag，统一解析避免重复 indexOf 逻辑。
 *
 * Code Logic（这个函数做什么）:
 *   返回 `--name value` 的 value；缺失返回 undefined。
 *
 * @param {string[]} argv
 * @param {string} name
 * @returns {string | undefined}
 */
function readArg(argv, name) {
  const i = argv.indexOf(name);
  if (i >= 0 && argv[i + 1] && !String(argv[i + 1]).startsWith('--')) {
    return argv[i + 1];
  }
  return undefined;
}

/**
 * 从 evidence 目录加载 execution manifests（可选，供 claim 检查）。
 *
 * Business Logic（为什么需要这个函数）:
 *   Task 5 需要从 docs/development/evidence/** 读取架构级 execution，而不是聚合矩阵 status。
 *
 * Code Logic（这个函数做什么）:
 *   扫描 evidence 下各稳定 ID 的 execution-*.json / manifest.json。
 *
 * @param {string} rootDir
 * @returns {Record<string, unknown>}
 */
export function loadEvidenceExecutions(rootDir = REPO_ROOT) {
  /**
   * Business Logic（为什么需要这个函数）:
   *   beta claim 必须从磁盘 evidence 加载 architecture execution，布局为
   *   `docs/development/evidence/<stableId>/<matrixId>/execution.json`（Plan N8），
   *   也兼容扁平 `evidence/<stableId>/execution.json`。
   *
   * Code Logic（这个函数做什么）:
   *   扫描 evidence 下 stableId 目录；读取该目录 JSON 与一层 matrix 子目录中的
   *   execution/manifest JSON；以 stableId@artifactMatrixId 为键。
   */
  /** @type {Record<string, unknown>} */
  const out = {};
  const base = join(rootDir, 'docs', 'development', 'evidence');
  if (!existsSync(base)) return out;

  /**
   * @param {string} filePath
   * @param {string} fallbackId
   * @param {string} [fallbackMatrix]
   */
  function ingestExecutionFile(filePath, fallbackId, fallbackMatrix) {
    const baseName = filePath.split(/[/\\]/).pop() || '';
    if (!/\.json$/i.test(baseName)) return;
    if (!/execution|manifest/i.test(baseName) && baseName !== 'manifest.json') return;
    try {
      const raw = JSON.parse(readFileSync(filePath, 'utf8'));
      if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return;
      const stableId =
        typeof raw.stableId === 'string' && raw.stableId.trim()
          ? raw.stableId
          : fallbackId;
      const matrix =
        typeof raw.artifactMatrixId === 'string' && raw.artifactMatrixId.trim()
          ? raw.artifactMatrixId
          : fallbackMatrix || 'macos-aarch64';
      out[`${stableId}@${matrix}`] = raw;
    } catch {
      // skip unreadable evidence
    }
  }

  let ids;
  try {
    ids = readdirSync(base);
  } catch {
    return out;
  }
  for (const id of ids) {
    const dir = join(base, id);
    let st;
    try {
      st = statSync(dir);
    } catch {
      continue;
    }
    if (!st.isDirectory()) continue;
    let entries;
    try {
      entries = readdirSync(dir);
    } catch {
      continue;
    }
    for (const entry of entries) {
      const abs = join(dir, entry);
      let est;
      try {
        est = statSync(abs);
      } catch {
        continue;
      }
      if (est.isFile()) {
        ingestExecutionFile(abs, id);
        continue;
      }
      if (!est.isDirectory()) continue;
      // Plan layout: evidence/<stableId>/<matrixId>/execution.json
      let nested;
      try {
        nested = readdirSync(abs);
      } catch {
        continue;
      }
      for (const file of nested) {
        ingestExecutionFile(join(abs, file), id, entry);
      }
    }
  }
  return out;
}

/**
 * CLI 入口。
 *
 * Business Logic（为什么需要这个函数）:
 *   提供 --self-test / 默认矩阵检查 / --matrix 覆盖 / beta claim 检查，供 CI 与本地复用。
 *
 * Code Logic（这个函数做什么）:
 *   解析 argv，分派到 self-test、runCheck 或 runBetaClaimCheck，打印 findings 并设 exitCode。
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
        '  node scripts/check-quality-traceability.mjs --claim-mode platform-beta --claim-profile macos-aarch64-beta \\',
        '    --subject-commit <40hex> [--subject-tag <tag>] [--rc-run-id <id>] [--evidence-ref <ref>]',
        '',
        `Default matrix: ${DEFAULT_MATRIX_REL}`,
        `Known levels: ${[...KNOWN_LEVELS].join(', ')}`,
        `Known ciJobs: ${[...KNOWN_CI_JOBS].join(', ')}`,
        `Fixed claim profile: ${MACOS_AARCH64_BETA_PROFILE.claimProfile}`,
        '',
      ].join('\n'),
    );
    return;
  }

  if (argv.includes('--self-test')) {
    process.exitCode = runSelfTest();
    return;
  }

  const claimMode = readArg(argv, '--claim-mode');
  const claimProfile = readArg(argv, '--claim-profile');
  if (claimMode || claimProfile) {
    try {
      const subjectCommit = readArg(argv, '--subject-commit') || '';
      const subjectTag = readArg(argv, '--subject-tag');
      const rcRunId = readArg(argv, '--rc-run-id');
      const evidenceRef = readArg(argv, '--evidence-ref');
      const expectedEvidenceCommit = readArg(argv, '--expected-evidence-commit');
      const appVersion = readArg(argv, '--app-version');
      const packageSha256 = readArg(argv, '--package-sha256');
      const claimPath = readArg(argv, '--release-claim');
      const inventoryPath = readArg(argv, '--rc-inventory');
      /** @type {unknown} */
      let releaseClaim;
      if (claimPath) {
        releaseClaim = JSON.parse(
          readFileSync(resolve(REPO_ROOT, claimPath), 'utf8'),
        );
      } else {
        const defaultClaim = join(
          REPO_ROOT,
          'docs',
          'development',
          'release-claim.json',
        );
        if (existsSync(defaultClaim)) {
          releaseClaim = JSON.parse(readFileSync(defaultClaim, 'utf8'));
        }
      }
      /** @type {unknown} */
      let rcInventory;
      if (inventoryPath) {
        rcInventory = JSON.parse(
          readFileSync(resolve(REPO_ROOT, inventoryPath), 'utf8'),
        );
      }
      const result = runBetaClaimCheck({
        claimMode: claimMode || 'platform-beta',
        claimProfile: claimProfile || 'macos-aarch64-beta',
        subjectCommit,
        subjectTag,
        rcWorkflowRunId: rcRunId,
        evidenceRef,
        expectedEvidenceCommit,
        appVersion,
        packageSha256,
        executions: loadEvidenceExecutions(REPO_ROOT),
        rcInventory,
        releaseClaim,
      });
      if (!result.ok) {
        for (const f of result.findings) process.stderr.write(`${f}\n`);
        process.stderr.write(
          `check-quality-traceability: ${result.decision} (${result.findings.length} issue(s))\n`,
        );
        process.exitCode = 1;
        return;
      }
      process.stdout.write(
        `check-quality-traceability: ${result.decision} profile=${result.profile?.claimProfile}\n`,
      );
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      process.stderr.write(`check-quality-traceability: ${msg}\n`);
      process.exitCode = 1;
    }
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
