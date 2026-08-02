import { describe, test, expect } from 'vitest';
import { readFileSync } from 'node:fs';

/**
 * Business Logic（为什么需要这个测试文件）:
 *   ProviderManager 按 controller/view 拆分：所有 `@/api` 调用集中在 controller，
 *   view 只消费投影。静态锁住 ownership，防止 view 回退直连 transport。
 *
 * Code Logic（这个测试文件做什么）:
 *   读取页面源码，断言 view 不 import `@/api` / 不调用 invoke；controller 是 transport 入口。
 */

const dir = new URL('./', import.meta.url);

function readSource(relativePath: string): string {
  return readFileSync(new URL(relativePath, dir), 'utf8');
}

const view = readSource('ProviderManager.tsx');
const controller = readSource('useProviderManagerController.ts');

describe('ProviderManager ownership (controller/view split)', () => {
  test('view must not import or call transport APIs', () => {
    // 不允许从 @/api 静态导入（注释中提到 @/api 不算违规）。
    expect(view).not.toMatch(/from\s+['"]@\/api/);
    expect(view).not.toMatch(/\binvoke\s*\(/);
  });

  test('controller is the transport entry point', () => {
    expect(controller).toContain('@/api/providerManager');
  });
});
