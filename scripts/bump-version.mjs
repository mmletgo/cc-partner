/**
 * bump-version.mjs — Claude Partner 版本号统一升级脚本（M9 发版流程）
 *
 * Business Logic（为什么需要这个脚本）:
 *   发版时需要把应用版本号同步更新到三处文件，避免不同地方版本号不一致导致的
 *   build 警告、updater 拉错版本等问题。tauri.conf.json 是版本号单一来源，
 *   Cargo.toml 必须与之完全一致（Tauri build 强制要求，否则告警/失败），
 *   web/package.json 跟随以保持前端构建元数据一致。
 *
 * Code Logic（这个脚本做什么）:
 *   接收一个参数 <新版本号>（符合语义化版本 x.y.z），正则替换三处文件的 version 字段：
 *     1. src-tauri/tauri.conf.json  → "version": "x.y.z"
 *     2. src-tauri/Cargo.toml       → version = "x.y.z"
 *     3. web/package.json           → "version": "x.y.z"
 *   每个文件替换后立即校验结果版本号是否等于目标值，不一致则报错退出（exit 1）。
 *   脚本以仓库根目录为基准定位文件（import.meta.url 解析），可在任意 cwd 执行。
 *
 * 用法:
 *   node scripts/bump-version.mjs 0.6.0          # 升级到 0.6.0
 *   node scripts/bump-version.mjs 1.0.0-beta.1   # 支持预发布号
 *   node scripts/bump-version.mjs                # 无参数 → 打印 usage 并退出
 */

import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

// 仓库根目录（scripts/ 的上一级），脚本以此定位三处文件，与执行时 cwd 无关
const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

// 三处需要同步版本号的文件及其替换规则
const FILES = [
  {
    path: 'src-tauri/tauri.conf.json',
    // 匹配 JSON 的 "version": "..."，仅替换引号内的版本号值
    replace: (content, ver) => content.replace(
      /"version"\s*:\s*"[^"]*"/,
      `"version": "${ver}"`
    ),
  },
  {
    path: 'src-tauri/Cargo.toml',
    // 匹配 [package] 段下的 version = "..."（Cargo.toml 顶部 package 段，[features]/[lib] 段之前）
    replace: (content, ver) => content.replace(
      /^(\[package\][\s\S]*?version\s*=\s*)"[^"]*"/m,
      `$1"${ver}"`
    ),
  },
  {
    path: 'web/package.json',
    // 匹配 JSON 的 "version": "..."
    replace: (content, ver) => content.replace(
      /"version"\s*:\s*"[^"]*"/,
      `"version": "${ver}"`
    ),
  },
];

// 语义化版本校验：支持预发布号（如 1.0.0-beta.1）和构建元数据（如 1.0.0+build.1）
const SEMVER_RE = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/;

/**
 * 从文件内容中提取当前版本号，用于替换后回读校验
 * @param {string} content - 文件内容
 * @param {RegExp} pattern - 提取 version 值的正则
 * @returns {string|null} 提取到的版本号，未匹配返回 null
 */
function extractVersion(content, pattern) {
  const m = content.match(pattern);
  return m ? m[1] : null;
}

function main() {
  const newVersion = process.argv[2];

  if (!newVersion) {
    console.error('用法: node scripts/bump-version.mjs <新版本号>');
    console.error('示例: node scripts/bump-version.mjs 0.6.0');
    process.exit(1);
  }

  if (!SEMVER_RE.test(newVersion)) {
    console.error(`错误: 版本号 "${newVersion}" 不符合语义化版本格式 (x.y.z[-pre][+build])`);
    process.exit(1);
  }

  console.log(`开始同步版本号到 ${newVersion} ...`);

  for (const file of FILES) {
    const absPath = resolve(REPO_ROOT, file.path);
    const original = readFileSync(absPath, 'utf8');
    const updated = file.replace(original, newVersion);

    // 替换后回读校验：确保三处版本号确实变成目标值，避免正则失配静默漏改
    const after = extractVersion(updated, /"version"\s*:\s*"([^"]*)"|"version"\s*=\s*"([^"]*)"/)
      || extractVersion(updated, /version\s*=\s*"([^"]*)"/);
    if (after !== newVersion) {
      // Cargo.toml 单独兜底校验（其格式是 version = "..." 非 JSON）
      const cargoCheck = extractVersion(updated, /version\s*=\s*"([^"]*)"/);
      if (cargoCheck !== newVersion) {
        console.error(`错误: ${file.path} 替换失败，替换后版本号 = "${after ?? cargoCheck}"（期望 ${newVersion}）`);
        process.exit(1);
      }
    }

    writeFileSync(absPath, updated, 'utf8');
    console.log(`  ✓ ${file.path}`);
  }

  console.log(`\n完成：版本号已同步到 ${newVersion}`);
  console.log(`提示：如需发版，请提交改动后推送 v* tag 触发 CI（例如 git tag v${newVersion} && git push origin v${newVersion}）`);
}

main();
