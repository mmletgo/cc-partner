import { describe, test } from 'vitest';
import { readFileSync, readdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

/**
 * Business Logic（为什么需要这个函数）:
 *   静态 ownership 测试需要在条件不满足时立刻失败，避免视图误 import API 或 controller 塞入 modal JSX。
 *
 * Code Logic（这个函数做什么）:
 *   condition 为 false 时抛出 Error，让 vitest 进程以失败退出。
 */
function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

const here = dirname(fileURLToPath(import.meta.url));

/**
 * Business Logic（为什么需要这个函数）:
 *   ownership 断言需要读取视图与 controller 源码做静态扫描。
 *
 * Code Logic（这个函数做什么）:
 *   相对本测试文件路径读取 UTF-8 文本。
 */
function readSource(relativePath: string): string {
  return readFileSync(join(here, relativePath), 'utf8');
}

describe('orchestratorOwnership', () => {
  test('views contain no API/transport imports', () => {
    const viewsDir = join(here, 'views');
    const viewFiles = readdirSync(viewsDir).filter((name) => name.endsWith('.tsx'));
    assert(viewFiles.length >= 4, 'expected at least four orchestrator view files');

    const forbidden = [
      "@/api/",
      "from '@/api",
      'from "@/api',
      'orchestratorApi',
      'promptOptimizerApi',
      'httpOrchestratorTransport',
      'transport',
    ];

    for (const file of viewFiles) {
      const source = readSource(join('views', file));
      for (const token of forbidden) {
        assert(
          !source.includes(token),
          `views/${file} must not contain "${token}" (views own presentation only)`,
        );
      }
    }
  });

  test('controller contains no modal/board Drawer Dialog JSX', () => {
    const source = readSource('controllers/useOrchestratorController.ts');
    const forbidden = [
      'createPortal',
      '<Drawer',
      '<Dialog',
      'role="dialog"',
      "role='dialog'",
      'styles.board',
      'styles.lane',
      'styles.taskDrawer',
      'styles.createDialog',
    ];
    for (const token of forbidden) {
      assert(
        !source.includes(token),
        `controller must not contain modal/board JSX token "${token}"`,
      );
    }
  });
});
