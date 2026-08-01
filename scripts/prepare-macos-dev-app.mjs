#!/usr/bin/env node
/**
 * prepare-macos-dev-app.mjs
 *
 * Business Logic（为什么需要这个脚本）:
 *   macOS TCC（输入监控/屏幕录制等）按代码签名身份记账。`tauri dev` 默认跑裸
 *   `target/debug/app`，无稳定 CFBundleIdentifier，系统设置列表名称混乱，且应用内
 *   状态与手动授权入口不可靠。开发版需要独立的 .app 包，使用与正式版不同的显示名与
 *   Bundle ID，便于单独授权、互不抢开关；固定签名只改善重建后的授权稳定性。
 *
 * Code Logic（这个脚本做什么）:
 *   组装开发包：
 *   - 默认使用 com.cc-partner.app.dev + ad-hoc，输入监控可由用户手动添加授权
 *   - 检测到固定自签名证书时保持相同产品身份，只替换签名方式
 *   - 两种签名方式都固定安装到 `~/Applications/cc-partner (Dev).app`
 *   - 将 debug GUI 二进制复制为 Contents/MacOS/cc-partner
 *   - 尽量把 debug backend 也放进 MacOS（便于 sidecar 旁路发现）
 *   - 复制 icon.icns（若有）
 *   - 内部通道签名后强制验证证书指纹与 designated requirement，不允许 ad-hoc 回退
 *
 * 用法:
 *   node scripts/prepare-macos-dev-app.mjs
 *   node scripts/prepare-macos-dev-app.mjs --self-test
 */

import { spawnSync } from 'node:child_process';
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  statSync,
  unlinkSync,
  writeFileSync,
} from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  inspectSignedApp,
  validateSigningMetadata,
} from './check-macos-signing-contract.mjs';

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(__dirname, '..');
const SRC_TAURI = join(REPO_ROOT, 'src-tauri');
const DEBUG_DIR = join(SRC_TAURI, 'target', 'debug');

export const DEV_APP_NAME = 'cc-partner (Dev).app';
export const DEV_BUNDLE_ID = 'com.cc-partner.app.dev';
export const DEV_DISPLAY_NAME = 'cc-partner (Dev)';
export const DEV_EXECUTABLE = 'cc-partner';
export const INTERNAL_SIGNING_IDENTITY = 'cc-partner Internal Code Signing';

/** 根据显式环境变量选择 ad-hoc 或固定签名，同时保持唯一开发产品身份。 */
export function resolveDevSigningChannel(env = process.env) {
  const identity = env.CC_PARTNER_INTERNAL_SIGNING_IDENTITY?.trim();
  if (!identity) {
    return {
      appName: DEV_APP_NAME,
      bundleId: DEV_BUNDLE_ID,
      displayName: DEV_DISPLAY_NAME,
      signingIdentity: '-',
      fixedSigning: false,
    };
  }
  if (identity !== INTERNAL_SIGNING_IDENTITY) {
    throw new Error(
      `macOS 固定签名 identity 必须为 ${INTERNAL_SIGNING_IDENTITY}，实际为 ${identity}`,
    );
  }
  return {
    appName: DEV_APP_NAME,
    bundleId: DEV_BUNDLE_ID,
    displayName: DEV_DISPLAY_NAME,
    signingIdentity: identity,
    fixedSigning: true,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   macOS 26 可能不把公开 Input Monitoring Request 自动登记到系统设置列表，用户需要
 *   通过列表下方「+」手动选择应用。无论是否固定签名，开发壳都必须位于稳定、易找到的
 *   Applications 路径，不能随 Cargo target 目录漂移。
 *
 * Code Logic（这个函数做什么）:
 *   两种签名方式统一返回用户 Applications 下的 canonical Dev 壳。
 *
 * @param {string} userHome
 * @returns {string}
 */
export function resolveDevAppPath(userHome = homedir()) {
  return join(userHome, 'Applications', DEV_APP_NAME);
}

/**
 * Business Logic: 定位当前 debug GUI 产物（Cargo 包名 app，打包后才是 cc-partner）。
 * Code Logic: 按体积优先选择真实 Mach-O，跳过过小占位文件。
 */
export function resolveDebugGuiBinary(debugDir = DEBUG_DIR) {
  const candidates = [
    join(debugDir, 'app'),
    join(debugDir, 'cc-partner'),
  ];
  /** @type {{ path: string, size: number }[]} */
  const found = [];
  for (const p of candidates) {
    if (!existsSync(p)) continue;
    const st = statSync(p);
    if (!st.isFile() || st.size < 1_000_000) continue;
    found.push({ path: p, size: st.size });
  }
  if (found.length === 0) {
    return null;
  }
  found.sort((a, b) => b.size - a.size);
  return found[0].path;
}

/**
 * Business Logic: Info.plist 决定系统设置中的显示名与 Bundle ID。
 * Code Logic: 生成最小 XML plist（不依赖 plutil 写）。
 */
export function buildInfoPlistXml({
  bundleId = DEV_BUNDLE_ID,
  displayName = DEV_DISPLAY_NAME,
  executable = DEV_EXECUTABLE,
  version = '0.0.0-dev',
} = {}) {
  return `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>en</string>
  <key>CFBundleDisplayName</key>
  <string>${escapeXml(displayName)}</string>
  <key>CFBundleExecutable</key>
  <string>${escapeXml(executable)}</string>
  <key>CFBundleIconFile</key>
  <string>AppIcon</string>
  <key>CFBundleIdentifier</key>
  <string>${escapeXml(bundleId)}</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>${escapeXml(displayName)}</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>${escapeXml(version)}</string>
  <key>CFBundleVersion</key>
  <string>${escapeXml(version)}</string>
  <key>LSHighResolutionCapable</key>
  <true/>
  <key>NSPrincipalClass</key>
  <string>NSApplication</string>
</dict>
</plist>
`;
}

/**
 * @param {string} s
 */
function escapeXml(s) {
  return s
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;');
}

/**
 * Business Logic: 从 tauri.conf 读版本号，保持开发包元数据可读。
 * Code Logic: 宽松 JSON 解析 version 字段。
 */
function readAppVersion() {
  try {
    const conf = JSON.parse(
      readFileSync(join(SRC_TAURI, 'tauri.conf.json'), 'utf8'),
    );
    if (typeof conf.version === 'string' && conf.version.trim()) {
      return conf.version.trim();
    }
  } catch {
    // ignore
  }
  return '0.0.0-dev';
}

/**
 * Business Logic: 组装并签名开发 .app，供 runner 启动。
 * Code Logic: 清空/重建包结构 → 复制二进制 → 写 plist → 可选 icon/backend → codesign。
 *
 * @param {{ debugDir?: string, force?: boolean }} [opts]
 * @returns {{ appPath: string, guiSource: string, bundleId: string, displayName: string }}
 */
export function prepareMacosDevApp(opts = {}) {
  if (process.platform !== 'darwin') {
    throw new Error('prepare-macos-dev-app 仅支持 macOS');
  }

  const debugDir = opts.debugDir ?? DEBUG_DIR;
  const channel = resolveDevSigningChannel();
  const guiSource = resolveDebugGuiBinary(debugDir);
  if (!guiSource) {
    throw new Error(
      `未找到 debug GUI 二进制（期望 ${join(debugDir, 'app')} 或 cc-partner，且体积 ≥1MB）。请先 cargo build / tauri dev 编译。`,
    );
  }

  const appPath = resolveDevAppPath();
  const contents = join(appPath, 'Contents');
  const macos = join(contents, 'MacOS');
  const resources = join(contents, 'Resources');

  mkdirSync(dirname(appPath), { recursive: true });
  mkdirSync(macos, { recursive: true });
  mkdirSync(resources, { recursive: true });

  // 清掉历史探测残留（id_probe / tcc_* 等），避免 codesign 体积膨胀与无关可执行文件
  try {
    for (const name of readdirSync(macos)) {
      if (name === DEV_EXECUTABLE || name === 'cc-partner-backend') continue;
      try {
        unlinkSync(join(macos, name));
      } catch {
        // ignore
      }
    }
  } catch {
    // ignore
  }

  const guiDest = join(macos, DEV_EXECUTABLE);
  copyFileSync(guiSource, guiDest);
  // 保持可执行位
  try {
    const mode = statSync(guiSource).mode;
    // eslint-disable-next-line no-bitwise
    spawnSync('chmod', ['u+x', guiDest], { stdio: 'ignore' });
    void mode;
  } catch {
    spawnSync('chmod', ['u+x', guiDest], { stdio: 'ignore' });
  }

  // backend 旁路：放进 MacOS，current_exe 同目录可直接发现
  const backendName = 'cc-partner-backend';
  const backendSource = join(debugDir, backendName);
  if (existsSync(backendSource) && statSync(backendSource).size >= 1_000_000) {
    copyFileSync(backendSource, join(macos, backendName));
    spawnSync('chmod', ['u+x', join(macos, backendName)], { stdio: 'ignore' });
  }

  const icns = join(SRC_TAURI, 'icons', 'icon.icns');
  if (existsSync(icns)) {
    copyFileSync(icns, join(resources, 'AppIcon.icns'));
  }

  writeFileSync(
    join(contents, 'Info.plist'),
    buildInfoPlistXml({
      bundleId: channel.bundleId,
      displayName: channel.displayName,
      version: readAppVersion(),
    }),
    'utf8',
  );

  const sign = spawnSync(
    'codesign',
    [
      '--force',
      '--deep',
      '--options',
      'runtime',
      '--sign',
      channel.signingIdentity,
      appPath,
    ],
    { encoding: 'utf8' },
  );
  if (sign.status !== 0) {
    throw new Error(
      `codesign 失败: ${sign.stderr || sign.stdout || `exit ${sign.status}`}`,
    );
  }

  if (channel.fixedSigning) {
    const expectedCertSha256 = process.env.CC_PARTNER_INTERNAL_CERT_SHA256?.trim();
    if (!expectedCertSha256) {
      throw new Error('macOS 固定签名要求 CC_PARTNER_INTERNAL_CERT_SHA256');
    }
    validateSigningMetadata(inspectSignedApp(appPath), {
      expectedIdentifier: channel.bundleId,
      expectedCertSha256,
    });
  }

  return {
    appPath,
    guiSource,
    bundleId: channel.bundleId,
    displayName: channel.displayName,
  };
}

/**
 * Business Logic: 无完整 cargo 时也能验证 plist 生成与 XML 转义。
 * Code Logic: 断言关键字段与 escape。
 */
function selfTest() {
  const xml = buildInfoPlistXml({
    bundleId: DEV_BUNDLE_ID,
    displayName: 'cc-partner (Dev)',
    executable: 'cc-partner',
    version: '1.2.3',
  });
  if (!xml.includes(`<string>${DEV_BUNDLE_ID}</string>`)) {
    throw new Error('self-test: missing bundle id');
  }
  if (!xml.includes('<string>cc-partner (Dev)</string>')) {
    throw new Error('self-test: missing display name');
  }
  const escaped = buildInfoPlistXml({ displayName: 'A&B<C>' });
  if (!escaped.includes('A&amp;B&lt;C&gt;')) {
    throw new Error('self-test: xml escape failed');
  }
  console.log('prepare-macos-dev-app self-test passed');
}

function main() {
  const args = process.argv.slice(2);
  if (args.includes('--self-test')) {
    selfTest();
    return;
  }
  if (process.platform !== 'darwin') {
    console.log('skip prepare-macos-dev-app on non-macOS');
    return;
  }
  const result = prepareMacosDevApp();
  console.log(
    `[prepare-macos-dev-app] ${result.displayName}  ${result.bundleId}\n  app: ${result.appPath}\n  from: ${result.guiSource}`,
  );
}

// 仅作为 CLI 入口时执行；被 import 时只导出函数
const isMain =
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  try {
    main();
  } catch (err) {
    console.error(`[prepare-macos-dev-app] ${err instanceof Error ? err.message : err}`);
    process.exit(1);
  }
}
