// @vitest-environment node
/**
 * App 路由懒加载与错误隔离静态契约。
 *
 * Business Logic（为什么需要这个测试）:
 *   同步 import 全量页面会把重型依赖打入 initial graph；DesignSystem 不得进入生产
 *   静态依赖；每个 AppShell 路由必须由 Suspense + RouteErrorBoundary 包裹。
 *
 * Code Logic（这个测试做什么）:
 *   读取 App.tsx 源码，断言 lazy 导入、禁止同步页面 import、DesignSystem 仅 isDev、
 *   RouteErrorBoundary/Suspense 存在，且 overlay 路由也有独立 boundary。
 */

import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { describe, expect, test } from 'vitest';

const appSource = readFileSync(
  resolve(dirname(fileURLToPath(import.meta.url)), './App.tsx'),
  'utf8',
);

describe('App lazy routes contract', () => {
  test('does not statically import AppShell page modules', () => {
    const forbidden = [
      /import\s*\{\s*Home\s*\}\s*from\s*['"]\.\/pages\/Home['"]/,
      /import\s*\{\s*Transfer\s*\}\s*from\s*['"]\.\/pages\/Transfer['"]/,
      /import\s*\{\s*Prompts\s*\}\s*from\s*['"]\.\/pages\/Prompts['"]/,
      /import\s*\{\s*Workbench\s*\}\s*from\s*['"]\.\/pages\/Workbench['"]/,
      /import\s*\{\s*Settings\s*\}\s*from\s*['"]\.\/pages\/Settings['"]/,
      /import\s*\{\s*Health\s*\}\s*from\s*['"]\.\/pages\/Health['"]/,
      /import\s*\{\s*Attention\s*\}\s*from\s*['"]\.\/pages\/Attention['"]/,
      /import\s*\{\s*DesignSystem\s*\}\s*from\s*['"]\.\/pages\/DesignSystem['"]/,
    ];

    for (const pattern of forbidden) {
      expect(appSource, `static import matched ${pattern}`).not.toMatch(pattern);
    }
  });

  test('loads primary pages via React.lazy named-export adapters', () => {
    expect(appSource).toMatch(/lazy\s*\(/);
    expect(appSource).toMatch(/import\s*\(\s*['"]\.\/pages\/Home['"]\s*\)/);
    expect(appSource).toMatch(/import\s*\(\s*['"]\.\/pages\/Workbench['"]\s*\)/);
    expect(appSource).toMatch(/import\s*\(\s*['"]\.\/pages\/Settings['"]\s*\)/);
    expect(appSource).toMatch(/default:\s*module\.\w+|default:\s*m\.\w+/);
  });

  test('keeps providers and AppShell eager', () => {
    expect(appSource).toMatch(/import\s*\{\s*AppShell\s*\}\s*from/);
    expect(appSource).toMatch(/import\s*\{\s*WorkbenchProjectsProvider/);
    expect(appSource).toMatch(/import\s*\{\s*AttentionProvider/);
    expect(appSource).toMatch(/import\s*\{\s*ScratchpadAutosaveProvider/);
  });

  test('DesignSystem dynamic import is gated by isDev helper', () => {
    expect(appSource).toMatch(/isDev/);
    expect(appSource).toMatch(/pages\/DesignSystem/);
    // 不得出现顶层同步 DesignSystem 命名导入
    expect(appSource).not.toMatch(
      /^import\s*\{[^}]*DesignSystem[^}]*\}\s*from\s*['"]\.\/pages\/DesignSystem['"]/m,
    );
  });

  test('wraps AppShell routes with Suspense and RouteErrorBoundary', () => {
    expect(appSource).toMatch(/RouteErrorBoundary/);
    expect(appSource).toMatch(/Suspense/);
    expect(appSource).toMatch(/resetKey/);
  });

  test('isolates screenshot and health overlays with their own boundary', () => {
    expect(appSource).toMatch(/screenshot-overlay/);
    expect(appSource).toMatch(/health-overlay/);
    // overlay 路径附近必须出现 RouteErrorBoundary（至少两处独立包裹）
    const boundaryCount = (appSource.match(/RouteErrorBoundary/g) ?? []).length;
    expect(boundaryCount).toBeGreaterThanOrEqual(2);
  });
});
