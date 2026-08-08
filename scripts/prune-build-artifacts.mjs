/**
 * prune-build-artifacts.mjs — 清理 Cargo/打包中间产物中「过期或可丢弃」的部分。
 *
 * Business Logic（为什么需要这个脚本）:
 *   Cargo 稳定版不会回收被替换的 target 下 deps 指纹产物；长期 dev + 多 binary
 *   会把 src-tauri/target 堆到数百 GB。打包完成后 release 的 deps/build 也不再需要，
 *   但最终 bundle/ 与已链接 binary 仍要保留。本脚本在 dev/build 路径上自动做
 *   「阈值清 debug / 清 release 中间层 / 清陈旧 incremental」，避免再靠人手 ./start.sh clean。
 *
 * Code Logic（这个脚本做什么）:
 *   - release-intermediates：删 target 与 triple 下 release 的 deps|build|incremental|.fingerprint
 *     （保留 binary、bundle、resources、.d 旁产物等）
 *   - debug-threshold：debug 目录超过阈值时整目录移除（下次冷编译）
 *   - stale-incremental：删除 mtime 超过 N 天的 incremental 会话目录
 *   - --self-test：本地假目录演练，不碰真实 target
 *
 * 用法:
 *   node scripts/prune-build-artifacts.mjs --mode=auto
 *   node scripts/prune-build-artifacts.mjs --mode=release-intermediates
 *   node scripts/prune-build-artifacts.mjs --mode=debug-threshold --debug-max-gb=20
 *   node scripts/prune-build-artifacts.mjs --mode=stale-incremental --stale-days=14
 *   node scripts/prune-build-artifacts.mjs --self-test
 *   node scripts/prune-build-artifacts.mjs --dry-run --mode=auto
 */

import assert from 'node:assert/strict';
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  rmSync,
  statSync,
  utimesSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const DEFAULT_TARGET = resolve(REPO_ROOT, 'src-tauri/target');

/** release 下可安全删除的中间目录名（最终 bin / bundle 不在此列） */
const RELEASE_INTERMEDIATE_DIRS = ['deps', 'build', 'incremental', '.fingerprint'];

/**
 * Business Logic:
 *   CLI 需要稳定默认值，也要允许 CI/本地用环境变量或参数收紧阈值。
 * Code Logic:
 *   解析 --mode / --target-dir / --debug-max-gb / --stale-days / --dry-run / --self-test。
 */
function parseArgs(argv) {
  const options = {
    mode: 'auto',
    targetDir: process.env.CC_PARTNER_TARGET_DIR
      ? resolve(process.env.CC_PARTNER_TARGET_DIR)
      : DEFAULT_TARGET,
    debugMaxGb: Number(process.env.CC_PARTNER_DEBUG_TARGET_MAX_GB || 20),
    staleDays: Number(process.env.CC_PARTNER_INCREMENTAL_STALE_DAYS || 14),
    dryRun: false,
    selfTest: false,
  };

  for (const arg of argv) {
    if (arg === '--dry-run') {
      options.dryRun = true;
    } else if (arg === '--self-test') {
      options.selfTest = true;
    } else if (arg.startsWith('--mode=')) {
      options.mode = arg.slice('--mode='.length);
    } else if (arg.startsWith('--target-dir=')) {
      options.targetDir = resolve(arg.slice('--target-dir='.length));
    } else if (arg.startsWith('--debug-max-gb=')) {
      options.debugMaxGb = Number(arg.slice('--debug-max-gb='.length));
    } else if (arg.startsWith('--stale-days=')) {
      options.staleDays = Number(arg.slice('--stale-days='.length));
    } else if (arg === '--help' || arg === '-h') {
      options.help = true;
    } else {
      throw new Error(`未知参数: ${arg}`);
    }
  }

  if (!Number.isFinite(options.debugMaxGb) || options.debugMaxGb <= 0) {
    throw new Error(`--debug-max-gb 必须是正数，收到 ${options.debugMaxGb}`);
  }
  if (!Number.isFinite(options.staleDays) || options.staleDays < 0) {
    throw new Error(`--stale-days 必须是 >=0 的数字，收到 ${options.staleDays}`);
  }

  const allowed = new Set([
    'auto',
    'release-intermediates',
    'debug-threshold',
    'stale-incremental',
  ]);
  if (!options.selfTest && !options.help && !allowed.has(options.mode)) {
    throw new Error(
      `--mode 必须是 ${[...allowed].join('|')} 之一，收到 ${options.mode}`,
    );
  }
  return options;
}

function log(message) {
  console.log(`[prune-build-artifacts] ${message}`);
}

/**
 * Business Logic:
 *   体积判断要跨平台，不能依赖 GNU du。
 * Code Logic:
 *   递归累加文件 size；目录不存在返回 0。
 */
function directorySizeBytes(root) {
  if (!existsSync(root)) {
    return 0;
  }
  let total = 0;
  const stack = [root];
  while (stack.length > 0) {
    const current = stack.pop();
    let st;
    try {
      st = statSync(current);
    } catch {
      continue;
    }
    if (st.isFile()) {
      total += st.size;
      continue;
    }
    if (!st.isDirectory()) {
      continue;
    }
    let entries;
    try {
      entries = readdirSync(current, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const entry of entries) {
      stack.push(join(current, entry.name));
    }
  }
  return total;
}

function formatBytes(bytes) {
  if (bytes < 1024) {
    return `${bytes}B`;
  }
  const units = ['KB', 'MB', 'GB', 'TB'];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(value >= 10 || unit === 0 ? 0 : 1)}${units[unit]}`;
}

/**
 * Business Logic:
 *   删除必须可 dry-run；失败不应拖垮 dev 启动（调用方可忽略 exit，本函数吞单路径错误）。
 * Code Logic:
 *   dryRun 只打印；否则 rmSync recursive force。
 */
function removePath(path, { dryRun, removed }) {
  if (!existsSync(path)) {
    return;
  }
  const size = directorySizeBytes(path);
  if (dryRun) {
    log(`dry-run remove ${path} (${formatBytes(size)})`);
  } else {
    try {
      rmSync(path, { recursive: true, force: true });
      log(`removed ${path} (${formatBytes(size)})`);
    } catch (error) {
      log(
        `skip ${path}: ${error instanceof Error ? error.message : String(error)}`,
      );
      return;
    }
  }
  removed.paths.push(path);
  removed.bytes += size;
}

/**
 * Business Logic:
 *   打包产物在 bundle/ 与最终 binary 中；deps/build 仅编译期需要。
 * Code Logic:
 *   扫描 target/release 与 target/<triple>/release，删中间目录名列表。
 */
function pruneReleaseIntermediates(targetDir, { dryRun, removed }) {
  if (!existsSync(targetDir)) {
    log(`target 不存在，跳过 release-intermediates: ${targetDir}`);
    return;
  }

  const releaseRoots = [];
  const directRelease = join(targetDir, 'release');
  if (existsSync(directRelease)) {
    releaseRoots.push(directRelease);
  }

  for (const entry of readdirSync(targetDir, { withFileTypes: true })) {
    if (!entry.isDirectory()) {
      continue;
    }
    // host triple 目录：aarch64-apple-darwin 等
    if (entry.name === 'release' || entry.name === 'debug' || entry.name === 'tmp') {
      continue;
    }
    const nested = join(targetDir, entry.name, 'release');
    if (existsSync(nested)) {
      releaseRoots.push(nested);
    }
  }

  for (const releaseRoot of releaseRoots) {
    for (const name of RELEASE_INTERMEDIATE_DIRS) {
      removePath(join(releaseRoot, name), { dryRun, removed });
    }
  }
}

/**
 * Business Logic:
 *   debug deps 膨胀是主因；partial 删 deps 文件会破坏 fingerprint 图，只能整目录清。
 * Code Logic:
 *   超过阈值则 remove target/debug。
 */
function pruneDebugThreshold(targetDir, debugMaxGb, { dryRun, removed }) {
  const debugDir = join(targetDir, 'debug');
  if (!existsSync(debugDir)) {
    log('debug 目录不存在，跳过 debug-threshold');
    return;
  }
  const size = directorySizeBytes(debugDir);
  const maxBytes = debugMaxGb * 1024 * 1024 * 1024;
  log(
    `debug 体积 ${formatBytes(size)}（阈值 ${debugMaxGb}GB = ${formatBytes(maxBytes)}）`,
  );
  if (size <= maxBytes) {
    log('debug 未超阈值，保留增量缓存');
    return;
  }
  log(
    `debug 超过阈值，将清空 ${debugDir}（下次 ./start.sh / cargo 会冷编译）`,
  );
  removePath(debugDir, { dryRun, removed });
}

/**
 * Business Logic:
 *   incremental 会话可按 mtime 淘汰；比整清 debug 更温和。
 * Code Logic:
 *   遍历 target 下各 profile 的 incremental 会话目录，mtime 早于 cutoff 则删。
 */
function pruneStaleIncremental(targetDir, staleDays, { dryRun, removed }) {
  if (!existsSync(targetDir)) {
    return;
  }
  const cutoffMs = Date.now() - staleDays * 24 * 60 * 60 * 1000;
  const stack = [targetDir];
  while (stack.length > 0) {
    const current = stack.pop();
    let entries;
    try {
      entries = readdirSync(current, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const entry of entries) {
      if (!entry.isDirectory()) {
        continue;
      }
      const full = join(current, entry.name);
      if (entry.name === 'incremental') {
        let sessions;
        try {
          sessions = readdirSync(full, { withFileTypes: true });
        } catch {
          continue;
        }
        for (const session of sessions) {
          if (!session.isDirectory()) {
            continue;
          }
          const sessionPath = join(full, session.name);
          let st;
          try {
            st = statSync(sessionPath);
          } catch {
            continue;
          }
          if (st.mtimeMs < cutoffMs) {
            removePath(sessionPath, { dryRun, removed });
          }
        }
        continue;
      }
      // 不进入 deps/build 做深层扫，避免巨量 inode
      if (entry.name === 'deps' || entry.name === 'build' || entry.name === '.fingerprint') {
        continue;
      }
      stack.push(full);
    }
  }
}

function runModes(options) {
  const removed = { paths: [], bytes: 0 };
  const modes =
    options.mode === 'auto'
      ? ['stale-incremental', 'debug-threshold', 'release-intermediates']
      : [options.mode];

  for (const mode of modes) {
    log(`mode=${mode} target=${options.targetDir} dryRun=${options.dryRun}`);
    if (mode === 'release-intermediates') {
      pruneReleaseIntermediates(options.targetDir, {
        dryRun: options.dryRun,
        removed,
      });
    } else if (mode === 'debug-threshold') {
      pruneDebugThreshold(options.targetDir, options.debugMaxGb, {
        dryRun: options.dryRun,
        removed,
      });
    } else if (mode === 'stale-incremental') {
      pruneStaleIncremental(options.targetDir, options.staleDays, {
        dryRun: options.dryRun,
        removed,
      });
    }
  }

  log(
    `完成: ${removed.paths.length} 路径, 约释放 ${formatBytes(removed.bytes)}`,
  );
  return removed;
}

/**
 * Business Logic:
 *   门禁/本地可验证脚本行为，不依赖真实 200G target。
 * Code Logic:
 *   建临时 target 树，跑 release / threshold / stale 三种断言。
 */
function selfTest() {
  const root = mkdtempSync(join(tmpdir(), 'cc-partner-prune-'));
  try {
    const target = join(root, 'target');
    const release = join(target, 'release');
    const debug = join(target, 'debug');
    mkdirSync(join(release, 'deps'), { recursive: true });
    mkdirSync(join(release, 'bundle', 'macos'), { recursive: true });
    writeFileSync(join(release, 'cc-partner'), 'bin');
    writeFileSync(join(release, 'deps', 'junk.rlib'), 'x'.repeat(1024));
    writeFileSync(join(release, 'bundle', 'macos', 'app'), 'keep');

    mkdirSync(join(debug, 'deps'), { recursive: true });
    writeFileSync(join(debug, 'deps', 'big'), 'y'.repeat(4096));
    const oldInc = join(debug, 'incremental', 'old_session');
    const newInc = join(debug, 'incremental', 'new_session');
    mkdirSync(oldInc, { recursive: true });
    mkdirSync(newInc, { recursive: true });
    writeFileSync(join(oldInc, 'x'), 'old');
    writeFileSync(join(newInc, 'x'), 'new');
    const oldTime = new Date(Date.now() - 30 * 24 * 60 * 60 * 1000);
    utimesSync(oldInc, oldTime, oldTime);

    // release intermediates
    runModes({
      mode: 'release-intermediates',
      targetDir: target,
      debugMaxGb: 20,
      staleDays: 14,
      dryRun: false,
    });
    assert.equal(existsSync(join(release, 'deps')), false);
    assert.equal(existsSync(join(release, 'cc-partner')), true);
    assert.equal(existsSync(join(release, 'bundle', 'macos', 'app')), true);

    // stale incremental
    runModes({
      mode: 'stale-incremental',
      targetDir: target,
      debugMaxGb: 20,
      staleDays: 14,
      dryRun: false,
    });
    assert.equal(existsSync(oldInc), false);
    assert.equal(existsSync(newInc), true);

    // threshold: tiny max forces wipe
    runModes({
      mode: 'debug-threshold',
      targetDir: target,
      debugMaxGb: 0.0000001,
      staleDays: 14,
      dryRun: false,
    });
    assert.equal(existsSync(debug), false);

    log('self-test passed');
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function printHelp() {
  console.log(`用法: node scripts/prune-build-artifacts.mjs [选项]

选项:
  --mode=auto|release-intermediates|debug-threshold|stale-incremental
  --target-dir=<path>     默认 src-tauri/target
  --debug-max-gb=<n>      debug 超阈值整清，默认 20（环境变量 CC_PARTNER_DEBUG_TARGET_MAX_GB）
  --stale-days=<n>        incremental 会话过期天数，默认 14
  --dry-run
  --self-test
  -h, --help
`);
}

function main() {
  const options = parseArgs(process.argv.slice(2));
  if (options.help) {
    printHelp();
    return;
  }
  if (options.selfTest) {
    selfTest();
    return;
  }
  runModes(options);
}

try {
  main();
} catch (error) {
  console.error(
    `[prune-build-artifacts] ${error instanceof Error ? error.message : String(error)}`,
  );
  process.exit(1);
}
