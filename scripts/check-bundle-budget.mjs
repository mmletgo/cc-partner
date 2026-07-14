/**
 * 根层 thin wrapper：委托 `web/scripts/check-bundle-contract.mjs`。
 *
 * Business Logic（为什么需要这个函数文件）:
 *   S6 计划路径写的是根 `scripts/check-bundle-budget.mjs`；权威实现与 CI 已落在
 *   `web/scripts/check-bundle-contract.mjs`，此处只做路径兼容，避免复制逻辑。
 *
 * Code Logic（做什么）:
 *   以相同 argv 同步 spawn web 侧脚本，透传 exit code。
 */

import { spawnSync } from 'node:child_process';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

/**
 * 转发到 web 权威 bundle 合同脚本。
 *
 * Business Logic:
 *   保持根路径可调用，同时只维护一份实现。
 *
 * Code Logic:
 *   spawn `node web/scripts/check-bundle-contract.mjs ...args`。
 *
 * @returns {number}
 */
function main() {
  const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
  const target = resolve(root, 'web/scripts/check-bundle-contract.mjs');
  const result = spawnSync(process.execPath, [target, ...process.argv.slice(2)], {
    cwd: root,
    stdio: 'inherit',
  });
  return result.status ?? 1;
}

process.exit(main());
