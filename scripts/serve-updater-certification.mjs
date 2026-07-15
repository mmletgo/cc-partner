#!/usr/bin/env node
/**
 * serve-updater-certification.mjs — 本机 loopback updater metadata server。
 *
 * Business Logic（为什么需要这个脚本）:
 *   L3 GUI 认证需要临时 HTTP 服务，把生产 RC 的 app.tar.gz/.sig 暴露给 N-1 harness
 *   的 check/download/install 路径，且不得触碰 stable latest.json。
 *
 * Code Logic（这个脚本做什么）:
 *   在 127.0.0.1:62190 提供 /latest.json 与静态包下载；仅监听 loopback。
 *
 * Usage:
 *   node scripts/serve-updater-certification.mjs --package path/to.app.tar.gz --sig path/to.sig --version 0.6.7
 *   node scripts/serve-updater-certification.mjs --self-test
 */

import {
  createReadStream,
  existsSync,
  readFileSync,
  statSync,
} from 'node:fs';
import { createServer } from 'node:http';
import { basename, dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const __filename = fileURLToPath(import.meta.url);
const HOST = '127.0.0.1';
const PORT = 62190;

/**
 * 构造 Tauri updater latest.json 响应体（仅 darwin-aarch64）。
 *
 * Business Logic（为什么需要这个函数）:
 *   harness 只认证 Apple Silicon 生产 RC，不得写入 multi-platform stable metadata。
 *
 * Code Logic（这个函数做什么）:
 *   返回 {version,notes,pub_date,platforms:{darwin-aarch64:{url,signature}}}。
 *
 * @param {{
 *   version: string,
 *   packageUrl: string,
 *   signature: string,
 *   notes?: string,
 * }} opts
 * @returns {Record<string, unknown>}
 */
export function buildCertificationLatestJson(opts) {
  return {
    version: opts.version,
    notes: opts.notes || `cc-partner certification ${opts.version}`,
    pub_date: new Date().toISOString(),
    platforms: {
      'darwin-aarch64': {
        signature: opts.signature,
        url: opts.packageUrl,
      },
    },
    _ccPartnerCertification: {
      marker: 'updater-certification-harness',
      releasable: false,
      loopbackOnly: true,
    },
  };
}

/**
 * 解析 CLI 参数。
 *
 * @param {string[]} argv
 * @returns {Record<string, string>}
 */
function parseArgs(argv) {
  /** @type {Record<string, string>} */
  const out = {};
  for (let i = 0; i < argv.length; i += 1) {
    const a = argv[i];
    if (a.startsWith('--') && argv[i + 1] && !argv[i + 1].startsWith('--')) {
      out[a.slice(2)] = argv[i + 1];
      i += 1;
    } else if (a.startsWith('--')) {
      out[a.slice(2)] = 'true';
    }
  }
  return out;
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

  process.stdout.write('serve-updater-certification self-test\n');

  caseRun('latest-json-only-darwin-aarch64', () => {
    const body = buildCertificationLatestJson({
      version: '0.6.7',
      packageUrl: 'http://127.0.0.1:62190/cc-partner_aarch64.app.tar.gz',
      signature: 'sig-bytes',
    });
    const platforms = /** @type {Record<string, unknown>} */ (body.platforms);
    assert(Object.keys(platforms).length === 1, 'single platform');
    assert('darwin-aarch64' in platforms, 'darwin-aarch64 present');
    assert(
      /** @type {any} */ (body)._ccPartnerCertification.releasable === false,
      'not releasable',
    );
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
        '  node scripts/serve-updater-certification.mjs --package <app.tar.gz> --sig <.sig> --version <semver>',
        '  node scripts/serve-updater-certification.mjs --self-test',
        '',
        `Listens on http://${HOST}:${PORT}/ (loopback only)`,
        '',
      ].join('\n'),
    );
    return;
  }

  const args = parseArgs(argv);
  const packagePath = args.package ? resolve(args.package) : '';
  const sigPath = args.sig ? resolve(args.sig) : '';
  const version = args.version || '';
  if (!packagePath || !sigPath || !version) {
    process.stderr.write('required: --package --sig --version\n');
    process.exitCode = 1;
    return;
  }
  if (!existsSync(packagePath) || !existsSync(sigPath)) {
    process.stderr.write('package or sig file missing\n');
    process.exitCode = 1;
    return;
  }

  const packageName = basename(packagePath);
  const signature = readFileSync(sigPath, 'utf8').trim();
  const packageUrl = `http://${HOST}:${PORT}/${packageName}`;
  const latest = buildCertificationLatestJson({
    version,
    packageUrl,
    signature,
  });

  const server = createServer((req, res) => {
    const url = req.url || '/';
    if (url === '/latest.json' || url.startsWith('/latest.json?')) {
      const body = JSON.stringify(latest, null, 2);
      res.writeHead(200, {
        'content-type': 'application/json; charset=utf-8',
        'content-length': Buffer.byteLength(body),
        'cache-control': 'no-store',
      });
      res.end(body);
      return;
    }
    if (url === `/${packageName}` || url.startsWith(`/${packageName}?`)) {
      const st = statSync(packagePath);
      res.writeHead(200, {
        'content-type': 'application/octet-stream',
        'content-length': st.size,
        'cache-control': 'no-store',
      });
      createReadStream(packagePath).pipe(res);
      return;
    }
    res.writeHead(404, { 'content-type': 'text/plain' });
    res.end('not found\n');
  });

  server.listen(PORT, HOST, () => {
    process.stdout.write(
      `updater certification server on http://${HOST}:${PORT}/latest.json\n`,
    );
    process.stdout.write(`package: ${packageUrl}\n`);
    process.stdout.write('Ctrl-C to stop (do not expose beyond loopback)\n');
  });
}

const isDirect =
  process.argv[1] && resolve(process.argv[1]) === __filename;
if (isDirect) {
  main();
}
