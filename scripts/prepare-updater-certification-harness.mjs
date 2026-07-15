#!/usr/bin/env node
/**
 * prepare-updater-certification-harness.mjs — 准备 N-1 loopback updater 认证 harness 构建输入。
 *
 * Business Logic（为什么需要这个脚本）:
 *   L3 GUI 认证需要同一 subject 的隔离 N-1 安装包，通过本机 127.0.0.1:62190
 *   临时 metadata 验签/安装生产 RC 的 app.tar.gz。harness 不得进入发布资产。
 *
 * Code Logic（这个脚本做什么）:
 *   读取 src-tauri/tauri.conf.json 与 merge 配置，生成临时 harness 配置（版本 -1 patch、
 *   仅 loopback endpoint、certification marker），并打印 tauri build 建议命令；
 *   可选写出 inventory 片段（releasable=false）。
 *
 * Usage:
 *   node scripts/prepare-updater-certification-harness.mjs
 *   node scripts/prepare-updater-certification-harness.mjs --write-config /tmp/harness.conf.json
 *   node scripts/prepare-updater-certification-harness.mjs --self-test
 */

import {
  existsSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
const REPO_ROOT = resolve(__dirname, '..');

/** 固定 loopback updater certification endpoint。 */
export const UPDATER_CERT_ENDPOINT = 'http://127.0.0.1:62190/latest.json';

/** 生产扫描时禁止出现的 certification 标记。 */
export const CERTIFICATION_MARKERS = Object.freeze([
  'http://127.0.0.1:62190',
  'dangerousInsecureTransportProtocol',
  'updater-certification-harness',
  '_ccPartnerCertification',
]);

/**
 * 将 semver 降一级 patch（N-1 harness）。
 *
 * Business Logic（为什么需要这个函数）:
 *   harness 必须以低于生产 RC 的版本安装，才能走 updater 升级路径。
 *
 * Code Logic（这个函数做什么）:
 *   解析 major.minor.patch，patch-1（最小 0.0.0）；保留预发布后缀剥离。
 *
 * @param {string} version
 * @returns {string}
 */
export function decrementPatchVersion(version) {
  const core = String(version || '0.0.1').split('-')[0];
  const parts = core.split('.').map((p) => Number.parseInt(p, 10));
  while (parts.length < 3) parts.push(0);
  let [maj, min, pat] = parts;
  if (![maj, min, pat].every((n) => Number.isFinite(n) && n >= 0)) {
    throw new Error(`invalid version: ${version}`);
  }
  if (pat > 0) {
    pat -= 1;
  } else if (min > 0) {
    min -= 1;
    pat = 0;
  } else if (maj > 0) {
    maj -= 1;
    min = 0;
    pat = 0;
  } else {
    throw new Error(`cannot decrement version below 0.0.0: ${version}`);
  }
  return `${maj}.${min}.${pat}`;
}

/**
 * 合并生产 conf 与 certification merge conf，产出 harness 配置。
 *
 * Business Logic（为什么需要这个函数）:
 *   harness 只能指向 loopback endpoint，并携带不可发布 marker。
 *
 * Code Logic（这个函数做什么）:
 *   deep-merge plugins.updater，覆盖 version 为 N-1，强制 certification 字段。
 *
 * @param {Record<string, unknown>} productionConf
 * @param {Record<string, unknown>} mergeConf
 * @returns {Record<string, unknown>}
 */
export function buildHarnessConfig(productionConf, mergeConf) {
  const out = structuredClone(productionConf);
  const prodVersion =
    typeof productionConf.version === 'string' ? productionConf.version : '0.0.1';
  out.version = decrementPatchVersion(prodVersion);
  const mergePlugins =
    mergeConf.plugins && typeof mergeConf.plugins === 'object'
      ? /** @type {Record<string, unknown>} */ (mergeConf.plugins)
      : {};
  const mergeUpdater =
    mergePlugins.updater && typeof mergePlugins.updater === 'object'
      ? /** @type {Record<string, unknown>} */ (mergePlugins.updater)
      : {};
  out.plugins = out.plugins && typeof out.plugins === 'object' ? { .../** @type {object} */ (out.plugins) } : {};
  const baseUpdater =
    /** @type {Record<string, unknown>} */ (out.plugins).updater &&
    typeof /** @type {any} */ (out.plugins).updater === 'object'
      ? { .../** @type {any} */ (out.plugins).updater }
      : {};
  /** @type {any} */ (out.plugins).updater = {
    ...baseUpdater,
    ...mergeUpdater,
    endpoints: [UPDATER_CERT_ENDPOINT],
    dangerousInsecureTransportProtocol: true,
  };
  out._ccPartnerCertification = {
    marker: 'updater-certification-harness',
    releasable: false,
    productionVersion: prodVersion,
    harnessVersion: out.version,
    endpoint: UPDATER_CERT_ENDPOINT,
  };
  return out;
}

/**
 * 扫描文本是否含 loopback/certification 污染（用于生产资产 gate）。
 *
 * Business Logic（为什么需要这个函数）:
 *   RC 生产包/配置不得包含 harness endpoint 或 insecure transport。
 *
 * Code Logic（这个函数做什么）:
 *   对 markers 做子串匹配，返回命中列表。
 *
 * @param {string} text
 * @returns {string[]}
 */
export function scanProductionContamination(text) {
  /** @type {string[]} */
  const hits = [];
  const body = String(text || '');
  for (const marker of CERTIFICATION_MARKERS) {
    if (body.includes(marker)) hits.push(marker);
  }
  return hits;
}

/**
 * self-test。
 *
 * @returns {number}
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

  process.stdout.write('prepare-updater-certification-harness self-test\n');

  caseRun('decrement-patch', () => {
    assert(decrementPatchVersion('0.6.7') === '0.6.6', '0.6.7 -> 0.6.6');
    assert(decrementPatchVersion('1.0.0') === '0.0.0', '1.0.0 -> 0.0.0');
  });

  caseRun('harness-config-loopback-only', () => {
    const harness = buildHarnessConfig(
      {
        version: '0.6.7',
        plugins: {
          updater: {
            pubkey: 'test',
            endpoints: [
              'https://github.com/mmletgo/cc-partner/releases/latest/download/latest.json',
            ],
          },
        },
      },
      {
        plugins: {
          updater: {
            endpoints: [UPDATER_CERT_ENDPOINT],
            dangerousInsecureTransportProtocol: true,
          },
        },
      },
    );
    assert(harness.version === '0.6.6', 'N-1 version');
    const endpoints = /** @type {any} */ (harness.plugins).updater.endpoints;
    assert(
      Array.isArray(endpoints) &&
        endpoints.length === 1 &&
        endpoints[0] === UPDATER_CERT_ENDPOINT,
      'only loopback endpoint',
    );
    assert(
      /** @type {any} */ (harness)._ccPartnerCertification.releasable === false,
      'not releasable',
    );
  });

  caseRun('production-scan-detects-markers', () => {
    const hits = scanProductionContamination(
      JSON.stringify({
        endpoints: [UPDATER_CERT_ENDPOINT],
        dangerousInsecureTransportProtocol: true,
      }),
    );
    assert(hits.length >= 2, `expected contamination hits, got ${hits}`);
  });

  caseRun('production-clean-config-passes', () => {
    const hits = scanProductionContamination(
      JSON.stringify({
        plugins: {
          updater: {
            endpoints: [
              'https://github.com/mmletgo/cc-partner/releases/latest/download/latest.json',
            ],
          },
        },
      }),
    );
    assert(hits.length === 0, `expected clean, got ${hits}`);
  });

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
 * @param {string[]} [argv]
 * @returns {void}
 */
export function main(argv = process.argv.slice(2)) {
  if (argv.includes('--self-test')) {
    process.exitCode = runSelfTest();
    return;
  }
  if (argv.includes('--help') || argv.includes('-h')) {
    process.stdout.write(
      [
        'Usage:',
        '  node scripts/prepare-updater-certification-harness.mjs',
        '  node scripts/prepare-updater-certification-harness.mjs --write-config <path>',
        '  node scripts/prepare-updater-certification-harness.mjs --self-test',
        '',
        `Endpoint: ${UPDATER_CERT_ENDPOINT}`,
        '',
      ].join('\n'),
    );
    return;
  }

  const prodPath = join(REPO_ROOT, 'src-tauri', 'tauri.conf.json');
  const mergePath = join(
    REPO_ROOT,
    'src-tauri',
    'tauri.updater-certification.conf.json',
  );
  if (!existsSync(prodPath) || !existsSync(mergePath)) {
    process.stderr.write('missing tauri.conf.json or certification merge conf\n');
    process.exitCode = 1;
    return;
  }
  const productionConf = JSON.parse(readFileSync(prodPath, 'utf8'));
  const mergeConf = JSON.parse(readFileSync(mergePath, 'utf8'));
  const harness = buildHarnessConfig(productionConf, mergeConf);

  // 生产 conf 自身不得含 certification marker
  const prodHits = scanProductionContamination(JSON.stringify(productionConf));
  if (prodHits.length > 0) {
    process.stderr.write(
      `production tauri.conf.json contaminated with certification markers: ${prodHits.join(', ')}\n`,
    );
    process.exitCode = 1;
    return;
  }

  const writeIdx = argv.indexOf('--write-config');
  if (writeIdx >= 0 && argv[writeIdx + 1]) {
    const outPath = resolve(argv[writeIdx + 1]);
    writeFileSync(outPath, `${JSON.stringify(harness, null, 2)}\n`, 'utf8');
    process.stdout.write(`wrote harness config: ${outPath}\n`);
  } else {
    process.stdout.write(`${JSON.stringify(harness, null, 2)}\n`);
  }
  process.stdout.write(
    [
      '',
      `# Build harness (example):`,
      `#   node scripts/prepare-updater-certification-harness.mjs --write-config /tmp/tauri.harness.conf.json`,
      `#   # merge into build via tauri CLI config override / temporary replace — releasable=false`,
      `# Serve production RC metadata:`,
      `#   node scripts/serve-updater-certification.mjs --package <app.tar.gz> --sig <app.tar.gz.sig>`,
      '',
    ].join('\n'),
  );
}

const isDirect =
  process.argv[1] && resolve(process.argv[1]) === __filename;
if (isDirect) {
  main();
}
