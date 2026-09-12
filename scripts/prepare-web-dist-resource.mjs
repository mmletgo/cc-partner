#!/usr/bin/env node
/**
 * prepare-web-dist-resource.mjs — Tauri bundle web-dist 静态资源准备脚本。
 *
 * Business Logic（为什么需要这个脚本）:
 *   发布安装包在其它设备上运行时，headless sidecar 靠 `CC_PARTNER_WEB_DIST`
 *   指向 bundle 内的 web-dist 才能服务 `/mobile` 静态页；编译期回退路径
 *   （源码树 ../web/dist）在用户机器上不存在。因此每次 `tauri build` 都要把
 *   `web/dist` 的产物同步到 `src-tauri/resources/web-dist/`，供
 *   bundle.resources 里 web-dist 目录的通配打包。
 *
 * Code Logic（这个脚本做什么）:
 *   清空重建 `src-tauri/resources/web-dist/`（先删除旧目录再递归复制
 *   `web/dist` 全部内容，保持 mobile.html 在根、assets/ 子目录结构）。
 *   `web/dist` 不存在时打印中文错误并以非 0 退出。`--self-test` 在系统
 *   临时目录构造假的 dist 源与目标，验证「旧文件被清掉、新文件完整复制」。
 *
 * Usage:
 *   node scripts/prepare-web-dist-resource.mjs            # 同步真实 web/dist
 *   node scripts/prepare-web-dist-resource.mjs --self-test
 */
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const WEB_DIST_DIR = resolve(REPO_ROOT, 'web', 'dist');
const RESOURCE_TARGET_DIR = resolve(REPO_ROOT, 'src-tauri', 'resources', 'web-dist');

/**
 * Business Logic（为什么需要这个函数）:
 *   脚本要同时服务真实同步与 --self-test 两条路径，参数解析必须只认已知开关，
 *   拼错的参数不能被静默忽略后误当成「直接同步」执行。
 *
 * Code Logic（这个函数做什么）:
 *   解析 `--self-test`；遇到未知参数直接抛错。
 *
 * @param {string[]} argv 命令行参数（不含 node 与脚本路径）
 * @returns {{selfTest: boolean}} 解析结果
 */
function parseArgs(argv) {
  const options = { selfTest: false };
  for (const arg of argv) {
    if (arg === '--self-test') {
      options.selfTest = true;
    } else {
      throw new Error(`未知参数: ${arg}`);
    }
  }
  return options;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   bundle resources 是整目录通配打包；旧构建残留文件（如上一版已改名的
 *   hashed asset）若不被清掉，会被打进安装包且让 /mobile 引用到过期资源。
 *   同步必须等价于「删除重建」，而不是增量覆盖。
 *
 * Code Logic（这个函数做什么）:
 *   先 `rmSync` 递归删除 targetDir（不存在则忽略），再 `cpSync` 递归复制
 *   sourceDir 全部内容到 targetDir，返回 target 下的文件总数。
 *
 * @param {string} sourceDir dist 源目录（须存在且为目录）
 * @param {string} targetDir 资源目标目录（会被清空重建）
 * @returns {number} 复制后的文件总数
 */
function syncWebDistResource(sourceDir, targetDir) {
  rmSync(targetDir, { recursive: true, force: true });
  cpSync(sourceDir, targetDir, { recursive: true });
  return countFilesRecursive(targetDir);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   同步完成后需要输出可读摘要（复制了多少文件），并供 self-test 断言复制完整性。
 *
 * Code Logic（这个函数做什么）:
 *   递归遍历目录，统计普通文件数量；目录不存在返回 0。
 *
 * @param {string} dir 待统计目录
 * @returns {number} 普通文件总数
 */
function countFilesRecursive(dir) {
  if (!existsSync(dir)) {
    return 0;
  }
  let count = 0;
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (entry.isDirectory()) {
      count += countFilesRecursive(join(dir, entry.name));
    } else if (entry.isFile()) {
      count += 1;
    }
  }
  return count;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   清空重建逻辑是本脚本的唯一核心行为，必须有不依赖真实 web/dist 的自动验证：
 *   旧目标残留必须被清掉、新源内容必须完整复制，缺一不可。
 *
 * Code Logic（这个函数做什么）:
 *   在系统临时目录构造假 dist 源（mobile.html + assets/smoke.js）与带旧残留的
 *   目标（stale 页面、stale asset、多余文件），执行 syncWebDistResource 后断言：
 *   ① 目标中的旧文件全部消失；② 新源文件内容逐字节一致。任一断言失败打印
 *   中文原因并抛错（调用方以非 0 退出）。
 */
function runSelfTest() {
  const tempRoot = mkdtempSync(join(tmpdir(), 'prepare-web-dist-self-test-'));
  try {
    const sourceDir = join(tempRoot, 'fake-dist');
    const targetDir = join(tempRoot, 'fake-resource');
    mkdirSync(join(sourceDir, 'assets'), { recursive: true });
    mkdirSync(join(targetDir, 'assets'), { recursive: true });
    writeFileSync(join(sourceDir, 'mobile.html'), '<html>new-mobile-v2</html>');
    const sourceAssets = join(sourceDir, 'assets');
    writeFileSync(join(sourceAssets, 'smoke.js'), 'console.log("smoke-v2");');

    // 目标预置旧构建残留：旧页面、旧 asset、以及 dist 里已不存在的多余文件。
    writeFileSync(join(targetDir, 'mobile.html'), '<html>stale-mobile-v1</html>');
    writeFileSync(join(targetDir, 'assets', 'stale.js'), '// stale');
    writeFileSync(join(targetDir, 'leftover.txt'), 'stale leftover');

    const copied = syncWebDistResource(sourceDir, targetDir);

    // 断言 1：旧文件必须被清掉。
    for (const stale of [join(targetDir, 'leftover.txt'), join(targetDir, 'assets', 'stale.js')]) {
      if (existsSync(stale)) {
        throw new Error(`self-test 失败: 旧残留未被清掉: ${stale}`);
      }
    }
    // 断言 2：新文件必须完整复制且内容一致。
    const copiedMobile = readTextOrEmpty(join(targetDir, 'mobile.html'));
    if (copiedMobile !== '<html>new-mobile-v2</html>') {
      throw new Error(`self-test 失败: mobile.html 内容不符: ${copiedMobile}`);
    }
    const copiedJs = readTextOrEmpty(join(targetDir, 'assets', 'smoke.js'));
    if (copiedJs !== 'console.log("smoke-v2");') {
      throw new Error(`self-test 失败: assets/smoke.js 内容不符: ${copiedJs}`);
    }
    if (copied !== 2) {
      throw new Error(`self-test 失败: 复制文件数应为 2，实际 ${copied}`);
    }
    console.log('prepare-web-dist-resource self-test 通过');
  } finally {
    rmSync(tempRoot, { recursive: true, force: true });
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   self-test 断言需要读取复制结果；文件缺失时应显式失败而不是抛出难懂的 ENOENT。
 *
 * Code Logic（这个函数做什么）:
 *   文件存在则返回 UTF-8 文本，不存在返回空串（由调用方断言比较失败）。
 *
 * @param {string} filePath 待读取文件
 * @returns {string} 文件文本（缺失时为空串）
 */
function readTextOrEmpty(filePath) {
  if (!existsSync(filePath) || !statSync(filePath).isFile()) {
    return '';
  }
  return readTextSync(filePath);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   统一同步读取文本的入口，保持脚本只依赖 Node 内置 API。
 *
 * Code Logic（这个函数做什么）:
 *   `readFileSync` 以 UTF-8 读取并返回文本。
 *
 * @param {string} filePath 待读取文件
 * @returns {string} 文件文本
 */
function readTextSync(filePath) {
  return readFileSync(filePath, 'utf8');
}

/**
 * Business Logic（为什么需要这个函数）:
 *   `tauri build` 的 beforeBuildCommand 链会在每次打包时调用本脚本；
 *   web/dist 缺失（未先构建前端）必须立刻以中文错误 + 非 0 退出，
 *   不能产出空资源目录让问题延迟到安装包运行期才暴露。
 *
 * Code Logic（这个函数做什么）:
 *   解析参数；--self-test 走临时目录自检；否则校验 web/dist 存在后
 *   执行真实同步并打印摘要。任何错误以非 0 退出。
 */
function main() {
  let options;
  try {
    options = parseArgs(process.argv.slice(2));
  } catch (error) {
    console.error(`prepare-web-dist-resource: ${error.message}`);
    console.error('用法: node scripts/prepare-web-dist-resource.mjs [--self-test]');
    process.exit(1);
  }

  if (options.selfTest) {
    try {
      runSelfTest();
    } catch (error) {
      console.error(`prepare-web-dist-resource: ${error.message}`);
      process.exit(1);
    }
    return;
  }

  if (!existsSync(WEB_DIST_DIR)) {
    console.error(
      `prepare-web-dist-resource: 前端构建产物不存在: ${WEB_DIST_DIR}\n` +
        '请先执行 `cd web && npm run build` 生成 web/dist，再运行本脚本或 tauri build。',
    );
    process.exit(1);
  }

  const copied = syncWebDistResource(WEB_DIST_DIR, RESOURCE_TARGET_DIR);
  console.log(
    `prepare-web-dist-resource: 已同步 web/dist -> src-tauri/resources/web-dist (${copied} 个文件)`,
  );
}

main();
