#!/usr/bin/env node
/**
 * check-agent-hub-support-manifest.mjs — Agent Hub support manifest / evidence 门禁。
 *
 * Business Logic（为什么需要这个脚本）:
 *   Gate B 要求三 target 的 exact min/current 版本、Supported* 能力与 quality-matrix
 *   evidence 对齐；null 版本或空 evidence 不能在 --gate-b 下被当成已认证写能力。
 *   同时禁止 manifest 夹带 credential 明文或绝对开发者 home 路径。
 *
 * Code Logic（这个脚本做什么）:
 *   - 读取 `src-tauri/src/agent_hub/support/support-manifest.json`
 *   - 读取 `docs/development/quality-matrix.json` 收集 evidence ID
 *   - 可选比对 `src-tauri/tests/support/agent_hub_l3_snapshots/*.json` 激活命令指纹
 *   - `--gate-b`：fail-closed 合同——允许 null 版本当且仅当全部写能力 blocked 且非 certified；
 *     拒绝写能力 Supported* 却无 L3 版本证据、无序版本、缺失 target、未知 evidence
 *   - `--gate-d`：在 gate-b 基础上校验 `hookMappings`（可为空）；非空项须有 intent/双端/schema/trust/evidence
 *   - `--self-test`：内存 fixture 覆盖主要失败分支
 *   - 仅 Node 内置模块
 *
 * Usage:
 *   node scripts/check-agent-hub-support-manifest.mjs
 *   node scripts/check-agent-hub-support-manifest.mjs --gate-b
 *   node scripts/check-agent-hub-support-manifest.mjs --gate-d
 *   node scripts/check-agent-hub-support-manifest.mjs --self-test
 */

import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
const REPO_ROOT = resolve(__dirname, '..');

const MANIFEST_REL = 'src-tauri/src/agent_hub/support/support-manifest.json';
const MATRIX_REL = 'docs/development/quality-matrix.json';
const SNAPSHOT_DIR_REL = 'src-tauri/tests/support/agent_hub_l3_snapshots';

const REQUIRED_TARGETS = ['claude', 'codex', 'opencode'];

/** Supported* 族：需要 quality-matrix evidence。 */
const SUPPORTED_FAMILY = new Set([
  'supported',
  'supportedAfterRestart',
  'activationRequired',
]);

/** 写侧 capability（Gate B 对 Supported* 更严格）。 */
const WRITE_CAPS = new Set([
  'renderInstruction',
  'renderPortableAssets',
  'activatePackage',
  'deactivatePackage',
  'liveReload',
]);

/** 禁止出现在 manifest 中的 credential 形态（fixture secret 也禁）。 */
const CREDENTIAL_PATTERNS = [
  /Bearer\s+[A-Za-z0-9._~+/=-]+/i,
  /api[_-]?key\s*[:=]\s*['"][^'"]+['"]/i,
  /token\s*[:=]\s*['"][^'"]+['"]/i,
  /plain-fixture/i,
  /Authorization\s*[:=]/i,
];

/** 绝对开发者 home 路径（POSIX + Windows 用户目录）。 */
const ABS_HOME_PATTERNS = [
  /\/Users\/[^/"'\s]+/i,
  /\/home\/[^/"'\s]+/i,
  /[A-Za-z]:\\Users\\[^\\/"'\s]+/i,
];

/**
 * 解析 semver core（X.Y.Z），容忍前导 v 与 pre-release/build。
 *
 * Business Logic: gate 要求 exact ordered versions。
 * Code Logic: 取首个数字 token，剥离 -/+ 后缀。
 *
 * @param {string|null|undefined} raw
 * @returns {[number, number, number]|null}
 */
export function parseSemverCore(raw) {
  if (raw == null) return null;
  let s = String(raw).trim();
  if (!s) return null;
  if (s.startsWith('v') || s.startsWith('V')) s = s.slice(1).trim();
  const token =
    s
      .split(/\s+/)
      .find((t) => t.length > 0 && t[0] >= '0' && t[0] <= '9') ?? s;
  const core = token.split(/[-+]/)[0] ?? token;
  if (!/^\d+(\.\d+){0,3}$/.test(core)) return null;
  const parts = core.split('.').map((p) => Number.parseInt(p, 10));
  if (parts.some((n) => Number.isNaN(n))) return null;
  return [parts[0] ?? 0, parts[1] ?? 0, parts[2] ?? 0];
}

/**
 * 比较两个 semver core。
 * @param {[number, number, number]} a
 * @param {[number, number, number]} b
 * @returns {number}
 */
export function cmpSemver(a, b) {
  for (let i = 0; i < 3; i += 1) {
    if (a[i] !== b[i]) return a[i] < b[i] ? -1 : 1;
  }
  return 0;
}

/**
 * 加载 JSON 文件。
 * @param {string} absPath
 * @returns {unknown}
 */
function loadJson(absPath) {
  return JSON.parse(readFileSync(absPath, 'utf8'));
}

/**
 * 收集 quality-matrix 中的 evidence id 集合。
 * @param {string} repoRoot
 * @returns {Set<string>}
 */
export function loadMatrixEvidenceIds(repoRoot) {
  const matrixPath = join(repoRoot, MATRIX_REL);
  if (!existsSync(matrixPath)) {
    throw new Error(`missing quality-matrix: ${MATRIX_REL}`);
  }
  const matrix = loadJson(matrixPath);
  const ids = new Set();
  const entries = Array.isArray(matrix?.entries) ? matrix.entries : [];
  for (const entry of entries) {
    if (entry && typeof entry.id === 'string' && entry.id.trim()) {
      ids.add(entry.id.trim());
    }
  }
  return ids;
}

/**
 * 加载 L3 activation fingerprint 快照（按 target 文件名）。
 * @param {string} repoRoot
 * @returns {Record<string, Record<string, string>>}
 */
export function loadActivationSnapshots(repoRoot) {
  const dir = join(repoRoot, SNAPSHOT_DIR_REL);
  /** @type {Record<string, Record<string, string>>} */
  const out = {};
  if (!existsSync(dir)) return out;
  for (const name of readdirSync(dir)) {
    if (!name.endsWith('.json')) continue;
    const target = name.replace(/\.json$/u, '');
    const data = loadJson(join(dir, name));
    if (data && typeof data === 'object' && data.activationCommandFingerprints) {
      out[target] = { ...data.activationCommandFingerprints };
    } else if (data && typeof data === 'object') {
      out[target] = { ...data };
    }
  }
  return out;
}

/**
 * 校验单份 manifest 对象。
 *
 * Business Logic: --gate-b 拒绝开发态 null 版本；--gate-d 额外校验 hookMappings。
 *
 * @param {any} manifest
 * @param {{ gateB?: boolean, gateD?: boolean, evidenceIds?: Set<string>, snapshots?: Record<string, Record<string, string>>, rawText?: string }} options
 * @returns {string[]} errors
 */
export function validateSupportManifest(manifest, options = {}) {
  const gateB = Boolean(options.gateB);
  const gateD = Boolean(options.gateD);
  const evidenceIds = options.evidenceIds ?? new Set();
  const snapshots = options.snapshots ?? {};
  const rawText = options.rawText ?? JSON.stringify(manifest);
  /** @type {string[]} */
  const errors = [];

  if (!manifest || typeof manifest !== 'object') {
    return ['manifest_not_object'];
  }
  if (manifest.schemaVersion !== 1 && manifest.schemaVersion !== 1.0) {
    // 允许 number 1
    if (Number(manifest.schemaVersion) !== 1) {
      errors.push(`schemaVersion_invalid:${String(manifest.schemaVersion)}`);
    }
  }
  if (!Array.isArray(manifest.targets)) {
    errors.push('targets_not_array');
    return errors;
  }

  /** @type {Map<string, number>} */
  const seen = new Map();
  for (const [index, record] of manifest.targets.entries()) {
    if (!record || typeof record !== 'object') {
      errors.push(`target_record_invalid:${index}`);
      continue;
    }
    const target = String(record.target ?? '');
    if (!REQUIRED_TARGETS.includes(target)) {
      errors.push(`target_unknown:${target || `<index ${index}>`}`);
    }
    seen.set(target, (seen.get(target) ?? 0) + 1);

    const minRaw = record.minTestedVersion;
    const curRaw = record.currentTestedVersion;
    const minMissing = minRaw == null || String(minRaw).trim() === '';
    const curMissing = curRaw == null || String(curRaw).trim() === '';

    if (!record.executableProbe || typeof record.executableProbe !== 'object') {
      errors.push(`${target}:executableProbe_missing`);
    } else {
      const names = record.executableProbe.commandNames;
      if (!Array.isArray(names) || names.length === 0) {
        errors.push(`${target}:commandNames_empty`);
      }
    }

    const caps = record.capabilities;
    /** @type {string[]} */
    let writeLevels = [];
    if (!caps || typeof caps !== 'object') {
      errors.push(`${target}:capabilities_missing`);
    } else {
      writeLevels = [...WRITE_CAPS].map((cap) => String(caps[cap] ?? ''));
      const evidence = Array.isArray(record.evidenceIds)
        ? record.evidenceIds.map((x) => String(x).trim()).filter(Boolean)
        : [];
      // 无条件校验 evidence ID（即使 capability 为 blocked）：防虚假 inventory / 未来误开写能力
      if (evidenceIds.size > 0) {
        for (const id of evidence) {
          if (!evidenceIds.has(id)) {
            errors.push(`${target}:evidence_id_missing_in_matrix:${id}`);
          }
        }
      }
      // gate-d: 非空 min/current 版本必须有 evidence；L3 未证实时应使用 null
      if (gateD && (!minMissing || !curMissing) && evidence.length === 0) {
        errors.push(`${target}:version_set_without_evidence`);
      }
      for (const [cap, level] of Object.entries(caps)) {
        const lvl = String(level);
        if (SUPPORTED_FAMILY.has(lvl)) {
          if (evidence.length === 0) {
            errors.push(`${target}:${cap}:supported_without_evidence`);
          }
          if ((gateB || gateD) && WRITE_CAPS.has(cap) && evidence.length === 0) {
            errors.push(`${target}:${cap}:write_supported_without_evidence`);
          }
        }
      }
    }

    // gate-b fail-closed 合同：
    // - null/空版本允许，当且仅当全部写能力 blocked（未 L3 认证）
    // - 若写能力出现 Supported*，则必须提供 min/current 版本证据
    const allWritesBlocked =
      writeLevels.length === WRITE_CAPS.size &&
      writeLevels.every((lvl) => lvl === 'blocked' || lvl === 'readOnly');
    if (gateB) {
      const anyWriteSupported = writeLevels.some((lvl) => SUPPORTED_FAMILY.has(lvl));
      if (anyWriteSupported) {
        if (minMissing) errors.push(`${target}:minTestedVersion_null_or_empty`);
        if (curMissing) errors.push(`${target}:currentTestedVersion_null_or_empty`);
      } else if (!allWritesBlocked && (minMissing || curMissing)) {
        // 写能力字段残缺且版本 null：仍 fail-closed
        if (minMissing) errors.push(`${target}:minTestedVersion_null_or_empty`);
        if (curMissing) errors.push(`${target}:currentTestedVersion_null_or_empty`);
      }
      // allWritesBlocked + null 版本：允许（uncertified scan-only 合同）
    }

    if (!minMissing && !curMissing) {
      const minV = parseSemverCore(String(minRaw));
      const curV = parseSemverCore(String(curRaw));
      if (!minV) errors.push(`${target}:minTestedVersion_malformed:${minRaw}`);
      if (!curV) errors.push(`${target}:currentTestedVersion_malformed:${curRaw}`);
      if (minV && curV && cmpSemver(minV, curV) > 0) {
        errors.push(`${target}:min_greater_than_current`);
      }
    }

    // activation fingerprints vs snapshots
    const fps =
      record.activationCommandFingerprints &&
      typeof record.activationCommandFingerprints === 'object'
        ? record.activationCommandFingerprints
        : {};
    const snap = snapshots[target] ?? {};
    const snapKeys = Object.keys(snap);
    if (snapKeys.length > 0) {
      for (const key of snapKeys) {
        if (String(fps[key] ?? '') !== String(snap[key] ?? '')) {
          errors.push(`${target}:activation_fingerprint_mismatch:${key}`);
        }
      }
      for (const key of Object.keys(fps)) {
        if (!(key in snap)) {
          errors.push(`${target}:activation_fingerprint_extra:${key}`);
        }
      }
    }
  }

  for (const t of REQUIRED_TARGETS) {
    const count = seen.get(t) ?? 0;
    if (count === 0) errors.push(`target_missing:${t}`);
    if (count > 1) errors.push(`target_duplicate:${t}`);
  }

  // privacy / credential scan on raw text
  for (const re of CREDENTIAL_PATTERNS) {
    if (re.test(rawText)) {
      errors.push(`credential_pattern_detected:${re.source}`);
    }
  }
  for (const re of ABS_HOME_PATTERNS) {
    if (re.test(rawText)) {
      errors.push(`absolute_home_path_detected:${re.source}`);
    }
  }

  checkHookMappings(manifest, evidenceIds, errors, gateD);

  return errors;
}

/**
 * 主校验入口（仓库路径）。
 * @param {string} repoRoot
 * @param {{ gateB?: boolean, gateD?: boolean }} options
 * @returns {{ ok: boolean, errors: string[] }}
 */
export function checkRepo(repoRoot, options = {}) {
  const manifestPath = join(repoRoot, MANIFEST_REL);
  if (!existsSync(manifestPath)) {
    return { ok: false, errors: [`missing_manifest:${MANIFEST_REL}`] };
  }
  const rawText = readFileSync(manifestPath, 'utf8');
  let manifest;
  try {
    manifest = JSON.parse(rawText);
  } catch (err) {
    return { ok: false, errors: [`manifest_json_parse:${String(err)}`] };
  }
  let evidenceIds = new Set();
  try {
    evidenceIds = loadMatrixEvidenceIds(repoRoot);
  } catch (err) {
    return { ok: false, errors: [String(err)] };
  }
  const snapshots = loadActivationSnapshots(repoRoot);
  const errors = validateSupportManifest(manifest, {
    gateB: options.gateB,
    gateD: options.gateD,
    evidenceIds,
    snapshots,
    rawText,
  });
  return { ok: errors.length === 0, errors };
}

/**
 * 自测：内存 fixture 覆盖关键失败路径。
 * @returns {void}
 */
export function runSelfTest() {
  /** @type {string[]} */
  const failures = [];

  const baseTarget = (target, overrides = {}) => ({
    target,
    adapterSchemaVersion: 1,
    minTestedVersion: '1.0.0',
    currentTestedVersion: '1.0.0',
    guardedMajor: 1,
    executableProbe: { commandNames: [target], versionArgs: ['--version'] },
    activationCommandFingerprints: {},
    capabilities: {
      scanInstruction: 'readOnly',
      renderInstruction: 'blocked',
      scanPortableAssets: 'readOnly',
      renderPortableAssets: 'blocked',
      activatePackage: 'blocked',
      deactivatePackage: 'blocked',
      liveReload: 'blocked',
    },
    evidenceIds: [`L3-AGENT-HUB-${target.toUpperCase()}-001`],
    ...overrides,
  });

  const good = {
    schemaVersion: 1,
    targets: [
      baseTarget('claude'),
      baseTarget('codex'),
      baseTarget('opencode'),
    ],
  };
  const evidence = new Set([
    'L3-AGENT-HUB-CLAUDE-001',
    'L3-AGENT-HUB-CODEX-001',
    'L3-AGENT-HUB-OPENCODE-001',
  ]);
  let errs = validateSupportManifest(good, { gateB: true, evidenceIds: evidence });
  if (errs.length !== 0) failures.push(`good_should_pass:${errs.join(',')}`);

  // null min + all writes blocked → allowed in gate-b (uncertified contract)
  const nullMinBlocked = {
    schemaVersion: 1,
    targets: [
      baseTarget('claude', {
        minTestedVersion: null,
        currentTestedVersion: null,
      }),
      baseTarget('codex', {
        minTestedVersion: null,
        currentTestedVersion: null,
      }),
      baseTarget('opencode', {
        minTestedVersion: null,
        currentTestedVersion: null,
      }),
    ],
  };
  errs = validateSupportManifest(nullMinBlocked, { gateB: true, evidenceIds: evidence });
  if (errs.length !== 0) {
    failures.push(`null_versions_all_writes_blocked_should_pass:${errs.join(',')}`);
  }

  // null min + write supported → rejected
  const nullMinWriteOpen = {
    schemaVersion: 1,
    targets: [
      baseTarget('claude', {
        minTestedVersion: null,
        currentTestedVersion: null,
        capabilities: {
          scanInstruction: 'readOnly',
          renderInstruction: 'supported',
          scanPortableAssets: 'readOnly',
          renderPortableAssets: 'blocked',
          activatePackage: 'blocked',
          deactivatePackage: 'blocked',
          liveReload: 'blocked',
        },
        evidenceIds: ['L3-AGENT-HUB-CLAUDE-001'],
      }),
      baseTarget('codex'),
      baseTarget('opencode'),
    ],
  };
  errs = validateSupportManifest(nullMinWriteOpen, {
    gateB: true,
    evidenceIds: evidence,
  });
  if (!errs.some((e) => e.includes('minTestedVersion_null'))) {
    failures.push('null_min_with_write_supported_not_detected');
  }

  // supported write without evidence
  const supportedNoEv = {
    schemaVersion: 1,
    targets: [
      baseTarget('claude', {
        capabilities: {
          scanInstruction: 'supported',
          renderInstruction: 'supported',
          scanPortableAssets: 'readOnly',
          renderPortableAssets: 'blocked',
          activatePackage: 'blocked',
          deactivatePackage: 'blocked',
          liveReload: 'blocked',
        },
        evidenceIds: [],
      }),
      baseTarget('codex'),
      baseTarget('opencode'),
    ],
  };
  errs = validateSupportManifest(supportedNoEv, {
    gateB: true,
    evidenceIds: evidence,
  });
  if (!errs.some((e) => e.includes('supported_without_evidence'))) {
    failures.push('supported_without_evidence_not_detected');
  }

  // credential pattern
  errs = validateSupportManifest(good, {
    gateB: true,
    evidenceIds: evidence,
    rawText: JSON.stringify(good) + ' Bearer plain-fixture',
  });
  if (!errs.some((e) => e.includes('credential_pattern'))) {
    failures.push('credential_not_detected');
  }

  // absolute home
  errs = validateSupportManifest(good, {
    gateB: true,
    evidenceIds: evidence,
    rawText: JSON.stringify(good) + ' /Users/hans/secret',
  });
  if (!errs.some((e) => e.includes('absolute_home_path'))) {
    failures.push('home_path_not_detected');
  }

  // missing target
  const missing = {
    schemaVersion: 1,
    targets: [baseTarget('claude'), baseTarget('codex')],
  };
  errs = validateSupportManifest(missing, { gateB: true, evidenceIds: evidence });
  if (!errs.some((e) => e === 'target_missing:opencode')) {
    failures.push('missing_target_not_detected');
  }

  // min > current
  const unordered = {
    schemaVersion: 1,
    targets: [
      baseTarget('claude', {
        minTestedVersion: '2.0.0',
        currentTestedVersion: '1.0.0',
      }),
      baseTarget('codex'),
      baseTarget('opencode'),
    ],
  };
  errs = validateSupportManifest(unordered, { gateB: true, evidenceIds: evidence });
  if (!errs.some((e) => e.includes('min_greater_than_current'))) {
    failures.push('unordered_not_detected');
  }

  if (failures.length > 0) {
    console.error('self-test FAILED:');
    for (const f of failures) console.error(`  - ${f}`);
    process.exit(1);
  }
  console.log('self-test OK');
}

/**
 * 校验 hookMappings 数组（Gate D）。
 *
 * Business Logic: 初始可为空；非空项必须 evidence 进 quality-matrix，且双端不同。
 * Code Logic: 结构 + evidence 存在性。
 *
 * @param {any} manifest
 * @param {Set<string>} matrixIds
 * @param {string[]} errors
 * @param {boolean} gateD
 */
export function checkHookMappings(manifest, matrixIds, errors, gateD) {
  if (!gateD) return;
  if (manifest.hookMappings == null) {
    errors.push('hook_mappings_missing_field');
    return;
  }
  if (!Array.isArray(manifest.hookMappings)) {
    errors.push('hook_mappings_not_array');
    return;
  }
  // empty is valid
  for (let i = 0; i < manifest.hookMappings.length; i++) {
    const m = manifest.hookMappings[i];
    const prefix = `hookMappings[${i}]`;
    if (!m || typeof m !== 'object') {
      errors.push(`${prefix}_not_object`);
      continue;
    }
    for (const key of [
      'intent',
      'sourceTarget',
      'destinationTarget',
      'schemaVersion',
      'trustModel',
      'evidenceId',
    ]) {
      if (m[key] == null || m[key] === '') {
        errors.push(`${prefix}_missing_${key}`);
      }
    }
    if (m.sourceTarget === m.destinationTarget) {
      errors.push(`${prefix}_same_source_destination`);
    }
    if (Number(m.schemaVersion) === 0 || Number.isNaN(Number(m.schemaVersion))) {
      errors.push(`${prefix}_schema_version_invalid`);
    }
    if (m.trustModel && m.trustModel !== 'exactContract') {
      errors.push(`${prefix}_unknown_trust_model:${m.trustModel}`);
    }
    if (m.evidenceId && !matrixIds.has(String(m.evidenceId))) {
      errors.push(`${prefix}_evidence_not_in_matrix:${m.evidenceId}`);
    }
  }
}

/**
 * CLI 入口。
 * @returns {number}
 */
function main() {
  const args = process.argv.slice(2);
  if (args.includes('--self-test')) {
    runSelfTest();
    return 0;
  }
  // gate-d allows null/uncertified versions until L3; only --gate-b requires exact tested versions.
  const gateB = args.includes('--gate-b');
  const gateD = args.includes('--gate-d');
  const result = checkRepo(REPO_ROOT, { gateB, gateD });
  if (!result.ok) {
    console.error(
      `check-agent-hub-support-manifest: FAIL (${gateD ? 'gate-d' : gateB ? 'gate-b' : 'default'})`,
    );
    for (const e of result.errors) {
      console.error(`  - ${e}`);
    }
    return 1;
  }
  console.log(
    `check-agent-hub-support-manifest: OK (${gateD ? 'gate-d' : gateB ? 'gate-b' : 'default'})`,
  );
  return 0;
}

// 作为 CLI 直接执行时运行 main；被 import 时仅导出函数。
const isDirectRun = process.argv[1] && resolve(process.argv[1]) === __filename;
if (isDirectRun) {
  process.exit(main());
}
