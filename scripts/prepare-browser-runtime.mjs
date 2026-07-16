/**
 * prepare-browser-runtime.mjs — 准备固定版本 Chrome for Testing headless-shell。
 *
 * Business Logic（为什么需要这个脚本）:
 *   浏览器自动验证需要可复现的 managed Chromium，不依赖用户本机 Chrome/Playwright。
 *   release 与 CI 必须用固定 version + SHA-256 + 相对可执行路径，避免漂移。
 *
 * Code Logic（这个脚本做什么）:
 *   - --self-test：平台映射与 sha 校验单测
 *   - --write-lock --all-platforms：下载四平台 zip，写真实 SHA-256 到 browser-runtime-lock.json
 *   - 默认/ --platform current|linux64|mac-arm64|mac-x64|win64：按 lock 校验并解压到资源目录
 */

import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import {
  createWriteStream,
  existsSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { chmod } from 'node:fs/promises';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { pipeline } from 'node:stream/promises';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';
import { spawnSync } from 'node:child_process';

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const LOCK_PATH = resolve(REPO_ROOT, 'scripts/browser-runtime-lock.json');
const RUNTIME_ROOT = resolve(REPO_ROOT, 'src-tauri/resources/browser-runtime');
const CHROME_VERSION = '150.0.7871.114';
const SUPPORTED_PLATFORMS = ['linux64', 'mac-arm64', 'mac-x64', 'win64'];

/**
 * Business Logic（为什么需要这个函数）:
 *   lock 与下载 URL 必须按官方 Chrome for Testing 平台 id 命名。
 *
 * Code Logic（这个函数做什么）:
 *   将 Node process.platform / arch 映射为 linux64|mac-arm64|mac-x64|win64；未知组合抛错。
 */
export function resolvePlatform(nodePlatform, nodeArch) {
  if (nodePlatform === 'darwin' && nodeArch === 'arm64') return 'mac-arm64';
  if (nodePlatform === 'darwin' && (nodeArch === 'x64' || nodeArch === 'amd64')) return 'mac-x64';
  if (nodePlatform === 'win32' && (nodeArch === 'x64' || nodeArch === 'amd64')) return 'win64';
  if (nodePlatform === 'linux' && (nodeArch === 'x64' || nodeArch === 'amd64')) return 'linux64';
  throw new Error(`unsupported platform: ${nodePlatform}/${nodeArch}`);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   资产 URL 必须指向官方 chrome-for-testing-public 固定版本，禁止镜像漂移。
 *
 * Code Logic（这个函数做什么）:
 *   按 platform 拼出 headless-shell zip 的官方 GCS URL。
 */
export function officialZipUrl(version, platform) {
  if (!SUPPORTED_PLATFORMS.includes(platform)) {
    throw new Error(`unsupported platform id: ${platform}`);
  }
  return `https://storage.googleapis.com/chrome-for-testing-public/${version}/${platform}/chrome-headless-shell-${platform}.zip`;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   下载后必须验证 SHA-256，防止损坏或供应链替换。
 *
 * Code Logic（这个函数做什么）:
 *   计算 buffer 的 sha256 hex；与 expected 不一致则抛错。
 */
export function verifySha256(buffer, expectedHex) {
  const actual = createHash('sha256').update(buffer).digest('hex');
  if (typeof expectedHex !== 'string' || !/^[0-9a-f]{64}$/i.test(expectedHex)) {
    throw new Error(`invalid expected sha256: ${expectedHex}`);
  }
  if (actual.toLowerCase() !== expectedHex.toLowerCase()) {
    throw new Error(`sha256 mismatch: expected ${expectedHex}, got ${actual}`);
  }
  return actual.toLowerCase();
}

/**
 * Business Logic（为什么需要这个函数）:
 *   zip 解压必须拒绝路径穿越，避免恶意 zip 写到仓库外。
 *
 * Code Logic（这个函数做什么）:
 *   规范化 entry 路径，确保落在 destRoot 内。
 */
export function safeZipEntryPath(destRoot, entryName) {
  const normalized = entryName.replace(/\\/g, '/');
  if (normalized.includes('\0') || normalized.startsWith('/') || normalized.includes('..')) {
    throw new Error(`zip entry path traversal rejected: ${entryName}`);
  }
  const full = resolve(destRoot, normalized);
  const root = resolve(destRoot) + sep;
  if (!full.startsWith(root) && full !== resolve(destRoot)) {
    throw new Error(`zip entry escapes dest: ${entryName}`);
  }
  return full;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   解压后必须定位 headless-shell 可执行文件，布局因平台而异。
 *
 * Code Logic（这个函数做什么）:
 *   返回相对解压根的可执行路径约定。
 */
export function expectedExecutableRelPath(platform) {
  switch (platform) {
    case 'mac-arm64':
    case 'mac-x64':
      return `chrome-headless-shell-${platform}/chrome-headless-shell`;
    case 'linux64':
      return `chrome-headless-shell-linux64/chrome-headless-shell`;
    case 'win64':
      return `chrome-headless-shell-win64/chrome-headless-shell.exe`;
    default:
      throw new Error(`unsupported platform: ${platform}`);
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   CI/开发者需要无需下载即可验证脚本契约。
 *
 * Code Logic（这个函数做什么）:
 *   断言平台映射与 sha 校验失败路径。
 */
function runSelfTest() {
  assert.equal(resolvePlatform('darwin', 'arm64'), 'mac-arm64');
  assert.equal(resolvePlatform('darwin', 'x64'), 'mac-x64');
  assert.equal(resolvePlatform('win32', 'x64'), 'win64');
  assert.equal(resolvePlatform('linux', 'x64'), 'linux64');
  assert.throws(() => resolvePlatform('freebsd', 'x64'));
  assert.throws(() => verifySha256(Buffer.from('bad'), '0'.repeat(64)));
  const ok = verifySha256(
    Buffer.from('abc'),
    createHash('sha256').update('abc').digest('hex'),
  );
  assert.equal(ok.length, 64);
  assert.throws(() => safeZipEntryPath('/tmp/out', '../etc/passwd'));
  assert.throws(() => safeZipEntryPath('/tmp/out', '/etc/passwd'));
  const url = officialZipUrl(CHROME_VERSION, 'mac-arm64');
  assert.ok(url.includes(CHROME_VERSION));
  assert.ok(url.includes('chrome-headless-shell-mac-arm64.zip'));
  console.log('prepare-browser-runtime self-test: ok');
}

/**
 * Business Logic（为什么需要这个函数）:
 *   读取已提交的 lock，供校验/解压使用。
 *
 * Code Logic（这个函数做什么）:
 *   解析 JSON；缺文件或结构非法时抛错。
 */
function readLock() {
  if (!existsSync(LOCK_PATH)) {
    throw new Error(`missing lock: ${LOCK_PATH}`);
  }
  const lock = JSON.parse(readFileSync(LOCK_PATH, 'utf8'));
  if (lock.version !== CHROME_VERSION) {
    throw new Error(`lock version ${lock.version} != pinned ${CHROME_VERSION}`);
  }
  if (!lock.assets || typeof lock.assets !== 'object') {
    throw new Error('lock.assets missing');
  }
  return lock;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   写 lock 需要真实下载四平台资产的 SHA-256。
 *
 * Code Logic（这个函数做什么）:
 *   对每个平台 fetch zip，算 sha，写 official URL 与 executable 相对路径。
 */
async function writeLockAllPlatforms() {
  const assets = {};
  for (const platform of SUPPORTED_PLATFORMS) {
    const url = officialZipUrl(CHROME_VERSION, platform);
    console.log(`downloading ${platform}: ${url}`);
    const buffer = await downloadWithRetry(url, 3);
    const sha256 = createHash('sha256').update(buffer).digest('hex');
    assets[platform] = {
      url,
      sha256,
      executable: expectedExecutableRelPath(platform),
      zipBytes: buffer.length,
    };
    console.log(`  sha256=${sha256} bytes=${buffer.length}`);
  }
  const lock = {
    version: CHROME_VERSION,
    product: 'chrome-headless-shell',
    source: 'https://googlechromelabs.github.io/chrome-for-testing/',
    assets,
    generatedAt: new Date().toISOString(),
  };
  writeFileSync(LOCK_PATH, `${JSON.stringify(lock, null, 2)}\n`, 'utf8');
  console.log(`wrote ${LOCK_PATH}`);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   网络抖动时下载需有限重试。
 *
 * Code Logic（这个函数做什么）:
 *   使用 fetch 拉取完整 buffer；失败退避重试。
 */
async function downloadWithRetry(url, attempts) {
  let lastError;
  for (let i = 1; i <= attempts; i += 1) {
    try {
      const res = await fetch(url);
      if (!res.ok) {
        throw new Error(`HTTP ${res.status} for ${url}`);
      }
      const ab = await res.arrayBuffer();
      return Buffer.from(ab);
    } catch (error) {
      lastError = error;
      console.warn(`download attempt ${i}/${attempts} failed: ${error}`);
      await new Promise((r) => setTimeout(r, 500 * i));
    }
  }
  throw lastError;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   本地/CI 需要把当前平台 runtime 解压到 Tauri resources。
 *
 * Code Logic（这个函数做什么）:
 *   读 lock，下载或复用缓存 zip，校验 sha，安全解压，确认可执行文件存在。
 */
async function preparePlatform(platformId) {
  const lock = readLock();
  const asset = lock.assets[platformId];
  if (!asset) {
    throw new Error(`lock missing asset for ${platformId}`);
  }
  if (!asset.url || !asset.sha256 || !asset.executable) {
    throw new Error(`incomplete lock entry for ${platformId}`);
  }
  if (asset.url !== officialZipUrl(lock.version, platformId)) {
    throw new Error(`lock url is not official for ${platformId}: ${asset.url}`);
  }
  if (!/^[0-9a-f]{64}$/i.test(asset.sha256)) {
    throw new Error(`lock sha256 invalid for ${platformId}`);
  }

  const cacheDir = resolve(REPO_ROOT, '.browser-runtime-cache');
  mkdirSync(cacheDir, { recursive: true });
  const zipPath = join(cacheDir, `chrome-headless-shell-${platformId}-${lock.version}.zip`);
  let buffer;
  if (existsSync(zipPath)) {
    buffer = readFileSync(zipPath);
    try {
      verifySha256(buffer, asset.sha256);
      console.log(`cache hit ${zipPath}`);
    } catch {
      console.log(`cache stale, re-download ${platformId}`);
      buffer = await downloadWithRetry(asset.url, 3);
      writeFileSync(zipPath, buffer);
      verifySha256(buffer, asset.sha256);
    }
  } else {
    buffer = await downloadWithRetry(asset.url, 3);
    writeFileSync(zipPath, buffer);
    verifySha256(buffer, asset.sha256);
  }

  const destRoot = resolve(RUNTIME_ROOT, platformId);
  rmSync(destRoot, { recursive: true, force: true });
  mkdirSync(destRoot, { recursive: true });
  await extractZipBuffer(buffer, destRoot);

  const exe = resolve(destRoot, asset.executable);
  if (!existsSync(exe)) {
    throw new Error(`expected executable missing after extract: ${exe}`);
  }
  if (platformId !== 'win64') {
    await chmod(exe, 0o755);
  }
  console.log(`prepared ${platformId} -> ${exe}`);
  return exe;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   解压不能依赖未锁定的系统 unzip 行为差异；优先使用 Node 内置能力。
 *
 * Code Logic（这个函数做什么）:
 *   尝试使用 `unzip` CLI（macOS/Linux 常见）；失败则报错提示安装。
 *   解压前用列表检查路径穿越。
 */
async function extractZipBuffer(buffer, destRoot) {
  // 使用系统 unzip：Node 无内置 zip 解压（避免额外依赖）。
  const tmpZip = join(destRoot, '.download.zip');
  writeFileSync(tmpZip, buffer);
  // 先 list 检查穿越
  const list = spawnSync('unzip', ['-Z1', tmpZip], { encoding: 'utf8' });
  if (list.status !== 0) {
    rmSync(tmpZip, { force: true });
    throw new Error(`unzip list failed: ${list.stderr || list.stdout}`);
  }
  for (const line of list.stdout.split('\n').map((s) => s.trim()).filter(Boolean)) {
    safeZipEntryPath(destRoot, line);
  }
  const result = spawnSync('unzip', ['-q', '-o', tmpZip, '-d', destRoot], {
    encoding: 'utf8',
  });
  rmSync(tmpZip, { force: true });
  if (result.status !== 0) {
    throw new Error(`unzip failed: ${result.stderr || result.stdout}`);
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   CLI 入口需要解析 --self-test / --write-lock / --platform。
 *
 * Code Logic（这个函数做什么）:
 *   解析 argv 并分派。
 */
async function main(argv) {
  const args = [...argv];
  if (args.includes('--self-test')) {
    runSelfTest();
    return;
  }
  if (args.includes('--write-lock') && args.includes('--all-platforms')) {
    await writeLockAllPlatforms();
    return;
  }
  let platform = 'current';
  const idx = args.indexOf('--platform');
  if (idx >= 0) {
    platform = args[idx + 1];
    if (!platform) throw new Error('--platform needs value');
  }
  if (platform === 'current') {
    platform = resolvePlatform(process.platform, process.arch);
  }
  if (!SUPPORTED_PLATFORMS.includes(platform)) {
    throw new Error(`unknown platform: ${platform}`);
  }
  await preparePlatform(platform);
}

const isDirect = process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isDirect) {
  main(process.argv.slice(2)).catch((error) => {
    console.error(error);
    process.exitCode = 1;
  });
}
