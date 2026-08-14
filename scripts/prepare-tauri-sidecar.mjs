/**
 * prepare-tauri-sidecar.mjs — Tauri release externalBin sidecar 准备脚本。
 *
 * Business Logic（为什么需要这个脚本）:
 *   release 打包时 Tauri 会按 target triple 查找 `src-tauri/binaries/cc-partner-backend-<target>`；
 *   该文件必须是真实 backend CLI 二进制，否则 GUI 打包后无法管理后台 sidecar。
 *
 * Code Logic（这个脚本做什么）:
 *   解析可选 `--target <triple>` 与 `--dry-run`；先构建 `cc-partner-backend` release bin，
 *   再复制到 Tauri externalBin 约定路径。未显式传 target 时用 `rustc -vV` 的 host triple。
 */
import {
  chmodSync,
  copyFileSync,
  existsSync,
  lstatSync,
  mkdirSync,
  readdirSync,
  unlinkSync,
  writeFileSync,
} from 'node:fs';
import { spawnSync } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const SRC_TAURI_DIR = resolve(REPO_ROOT, 'src-tauri');
// Tauri bundle.resources 的 `resources/browser-runtime/**/*` 要求目录至少有一个文件；
// linux aarch64 等无 Chrome for Testing 资产的目标必须落占位，否则 cargo/tauri build 直接失败。
const BROWSER_RUNTIME_ROOT = resolve(SRC_TAURI_DIR, 'resources/browser-runtime');
const BROWSER_RUNTIME_PLACEHOLDER = join(
  BROWSER_RUNTIME_ROOT,
  '.platform-unavailable',
);

/**
 * Business Logic（为什么需要这个函数）:
 *   CI matrix 和本地 release 打包都需要同一脚本，但 matrix 只有部分平台显式指定 target。
 *
 * Code Logic（这个函数做什么）:
 *   解析 `--target <triple>`、`--target=<triple>` 与 `--dry-run`；未知参数直接报错。
 */
function parseArgs(argv) {
  const options = { target: null, targetExplicit: false, dryRun: false };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--dry-run') {
      options.dryRun = true;
    } else if (arg === '--target') {
      const target = argv[index + 1];
      if (!target) {
        throw new Error('--target 需要传入 Rust target triple');
      }
      options.target = target;
      options.targetExplicit = true;
      index += 1;
    } else if (arg.startsWith('--target=')) {
      const target = arg.slice('--target='.length);
      if (!target) {
        throw new Error('--target 需要传入 Rust target triple');
      }
      options.target = target;
      options.targetExplicit = true;
    } else {
      throw new Error(`未知参数: ${arg}`);
    }
  }

  return options;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   release matrix 按 cargo target 交叉准备 sidecar 时，managed Chromium 资源必须对齐
 *   目标平台（如 linux 构建机为 mac 产物时不能下载 host chrome）。
 *
 * Code Logic（这个函数做什么）:
 *   将常见 cargo target triple 映射为 prepare-browser-runtime 的 platform id；
 *   未识别 triple 回落 `current`（本机 host 探测）。
 *
 * @param {string} triple cargo target triple
 * @returns {string} browser-runtime platform id
 */
function cargoTargetToBrowserPlatform(triple) {
  const map = {
    'aarch64-apple-darwin': 'mac-arm64',
    'x86_64-apple-darwin': 'mac-x64',
    'x86_64-unknown-linux-gnu': 'linux64',
    'x86_64-pc-windows-msvc': 'win64',
    // Chrome for Testing 当前无 linux-arm64 headless-shell 锁定资产；
    // 返回 null 表示明确不可用（禁止回落 host `current` 再炸一次）。
    'aarch64-unknown-linux-gnu': null,
  };
  if (Object.prototype.hasOwnProperty.call(map, triple)) {
    return map[triple];
  }
  return 'current';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   tauri.conf.json 把 browser-runtime 目录声明为 bundle resources 通配；
 *   空目录或不存在时 Tauri build.rs 会以 glob 未匹配失败，阻断整平台发版。
 *   无 managed Chromium 的目标（如 linux aarch64）仍应产出可安装包，仅 verification 不可用。
 *
 * Code Logic（这个函数做什么）:
 *   损坏/自指 symlink 或普通文件先 unlink 再 mkdir；目录为空时写入
 *   `.platform-unavailable` 占位。已有真实 runtime 内容则不动。
 *
 * @param {string} reason 写入占位时的说明文本
 */
function ensureBrowserRuntimeResourcePresent(reason) {
  replaceUnusableBrowserRuntimeRoot();
  mkdirSync(BROWSER_RUNTIME_ROOT, { recursive: true });
  let hasContent = false;
  try {
    hasContent = readdirSync(BROWSER_RUNTIME_ROOT).length > 0;
  } catch {
    hasContent = false;
  }
  if (hasContent) {
    return;
  }
  writeFileSync(
    BROWSER_RUNTIME_PLACEHOLDER,
    `${reason.trim() || 'managed browser runtime unavailable for this target'}\n`,
    'utf8',
  );
  console.log(`已写入 browser-runtime 占位: ${BROWSER_RUNTIME_PLACEHOLDER}`);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   误提交的自指 symlink 会让 mkdir 报 EEXIST，Tauri resource glob 也匹配不到文件。
 *
 * Code Logic（这个函数做什么）:
 *   lstat 到 symlink 或普通文件时 unlink；真实目录保留。read_dir 失败的损坏链接视为不可用。
 */
function replaceUnusableBrowserRuntimeRoot() {
  let stat;
  try {
    stat = lstatSync(BROWSER_RUNTIME_ROOT);
  } catch {
    return;
  }
  if (stat.isSymbolicLink() || stat.isFile()) {
    unlinkSync(BROWSER_RUNTIME_ROOT);
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   未显式传 target 的 Windows/Linux CI job 和本地打包都必须使用当前 host triple 命名 sidecar。
 *
 * Code Logic（这个函数做什么）:
 *   执行 `rustc -vV` 并从输出中提取 `host: ...`；提取失败时抛出错误。
 */
function detectHostTarget() {
  const result = spawnSync('rustc', ['-vV'], {
    cwd: REPO_ROOT,
    encoding: 'utf8',
  });

  if (result.status !== 0) {
    throw new Error(`读取 rustc host target 失败: ${result.stderr || result.stdout}`);
  }

  const match = result.stdout.match(/^host:\s*(.+)$/m);
  if (!match) {
    throw new Error(`rustc -vV 输出缺少 host 字段: ${result.stdout}`);
  }
  return match[1].trim();
}

/**
 * Business Logic（为什么需要这个函数）:
 *   release sidecar 必须来自当前源码构建出的真实 backend CLI，而不是占位文件。
 *
 * Code Logic（这个函数做什么）:
 *   按需拼接 `cargo build --release --bin cc-partner-backend [--target <triple>]` 参数。
 */
function buildCargoArgs(target, targetExplicit) {
  const args = [
    'build',
    '--manifest-path',
    resolve(SRC_TAURI_DIR, 'Cargo.toml'),
    '--bin',
    'cc-partner-backend',
    '--release',
  ];
  if (targetExplicit) {
    args.push('--target', target);
  }
  return args;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Tauri externalBin 对 Windows sidecar 文件名要求 `.exe` 后缀，其它平台不加后缀。
 *
 * Code Logic（这个函数做什么）:
 *   根据 target triple 是否包含 `windows` 返回对应可执行文件后缀。
 */
function executableExtension(target) {
  return target.includes('windows') ? '.exe' : '';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   显式 cross target 和 host target 的 Cargo 输出目录不同，复制源必须精确。
 *
 * Code Logic（这个函数做什么）:
 *   显式 target 读取 `target/<triple>/release`，host target 读取 `target/release`。
 */
function cargoOutputPath(target, targetExplicit) {
  const releaseDir = targetExplicit
    ? resolve(SRC_TAURI_DIR, 'target', target, 'release')
    : resolve(SRC_TAURI_DIR, 'target', 'release');
  return resolve(releaseDir, `cc-partner-backend${executableExtension(target)}`);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Tauri 打包只读取 `binaries/cc-partner-backend-<target>`，生成路径必须与 tauri.conf.json 对齐。
 *
 * Code Logic（这个函数做什么）:
 *   拼出目标 triple 命名的 externalBin 文件路径；Windows 追加 `.exe`。
 */
function sidecarDestinationPath(target) {
  return resolve(
    SRC_TAURI_DIR,
    'binaries',
    `cc-partner-backend-${target}${executableExtension(target)}`,
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Unix sidecar 必须可执行，否则打包后的 GUI 调用 sidecar 会在运行时失败。
 *
 * Code Logic（这个函数做什么）:
 *   非 Windows target 把复制后的 sidecar 权限设置为 755；Windows 目标跳过 chmod。
 */
function makeExecutableIfNeeded(path, target) {
  if (!target.includes('windows')) {
    chmodSync(path, 0o755);
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   构建失败必须立即阻断 CI，避免后续 Tauri build 继续使用缺失或旧 sidecar。
 *
 * Code Logic（这个函数做什么）:
 *   执行命令并继承 stdio；非零退出时抛出错误。dry-run 只打印命令不执行。
 */
function runCommand(command, args, dryRun) {
  const rendered = [command, ...args].join(' ');
  if (dryRun) {
    console.log(`[dry-run] ${rendered}`);
    return;
  }

  const result = spawnSync(command, args, {
    cwd: REPO_ROOT,
    stdio: 'inherit',
  });
  if (result.status !== 0) {
    throw new Error(`命令执行失败: ${rendered}`);
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   release/CI 打包前要一次性完成 backend sidecar 构建、复制与权限修正。
 *
 * Code Logic（这个函数做什么）:
 *   解析参数、确定 target、执行 cargo build、检查输出二进制存在并复制到 externalBin 路径。
 */
function main() {
  const options = parseArgs(process.argv.slice(2));
  const target = options.target ?? detectHostTarget();
  const cargoArgs = buildCargoArgs(target, options.targetExplicit);
  const sourcePath = cargoOutputPath(target, options.targetExplicit);
  const destinationPath = sidecarDestinationPath(target);

  console.log(`准备 Tauri backend sidecar: target=${target}`);

  // A5：按 cargo target triple 映射 browser 平台（交叉编译时不能用 host `current`）。
  // 失败不阻断 sidecar；verification 在运行时按缺失降级为 unavailable；L3 需单独认证。
  // 但 Tauri resource glob 仍要求 resources/browser-runtime 下至少有一个文件。
  if (!options.dryRun) {
    const browserPlatform = cargoTargetToBrowserPlatform(target);
    if (browserPlatform == null) {
      console.warn(
        `managed browser runtime unsupported for cargo target ${target}; packaging continues without Chromium`,
      );
      ensureBrowserRuntimeResourcePresent(
        `unsupported cargo target for managed Chromium: ${target}`,
      );
    } else {
      const prepareBrowser = spawnSync(
        process.execPath,
        [
          resolve(REPO_ROOT, 'scripts/prepare-browser-runtime.mjs'),
          '--platform',
          browserPlatform,
        ],
        { cwd: REPO_ROOT, encoding: 'utf8' },
      );
      if (prepareBrowser.status === 0) {
        console.log('managed browser runtime prepared');
        ensureBrowserRuntimeResourcePresent(
          `prepare succeeded but resource dir empty for ${browserPlatform}`,
        );
      } else {
        console.warn(
          `managed browser runtime prepare skipped/failed (verification may be unavailable): ${
            prepareBrowser.stderr || prepareBrowser.stdout || prepareBrowser.status
          }`,
        );
        ensureBrowserRuntimeResourcePresent(
          `prepare failed for platform ${browserPlatform}: ${
            prepareBrowser.stderr || prepareBrowser.stdout || prepareBrowser.status
          }`,
        );
      }
    }
  }

  // Tauri build.rs 在编译 app 包时会校验 externalBin 路径存在；cargo build --bin
  // cc-partner-backend 仍会跑 package build.rs，形成 chicken-and-egg。先写占位文件，
  // 真实二进制构建成功后再覆盖。
  if (!options.dryRun) {
    mkdirSync(dirname(destinationPath), { recursive: true });
    if (!existsSync(destinationPath)) {
      writeFileSync(destinationPath, 'cc-partner-backend-sidecar-placeholder\n', 'utf8');
      makeExecutableIfNeeded(destinationPath, target);
      console.log(`已写入 sidecar 占位: ${destinationPath}`);
    }
  } else {
    console.log(`[dry-run] ensure placeholder ${destinationPath}`);
  }

  runCommand('cargo', cargoArgs, options.dryRun);

  if (options.dryRun) {
    console.log(`[dry-run] copy ${sourcePath} -> ${destinationPath}`);
    return;
  }

  if (!existsSync(sourcePath)) {
    throw new Error(`未找到 backend release bin: ${sourcePath}`);
  }

  mkdirSync(dirname(destinationPath), { recursive: true });
  copyFileSync(sourcePath, destinationPath);
  makeExecutableIfNeeded(destinationPath, target);
  console.log(`已生成 sidecar: ${destinationPath}`);
}

try {
  main();
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exit(1);
}
