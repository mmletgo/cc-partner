// @vitest-environment node
/**
 * App 路由懒加载与错误隔离静态契约。
 *
 * Business Logic（为什么需要这个测试）:
 *   同步 import 全量页面会把重型依赖打入 initial graph；DesignSystem 不得进入生产
 *   静态依赖；每个 AppShell 路由必须由 Suspense + RouteErrorBoundary 包裹。
 *   N4 导航改造期间还必须锁定 `/`→Home/Trending 与 `/workbench` 分离，禁止 `/discover`。
 *
 * Code Logic（这个测试做什么）:
 *   读取 App.tsx / AppShell 源码，断言 lazy 导入、禁止同步页面 import、DesignSystem 仅 isDev、
 *   RouteErrorBoundary/Suspense 存在，overlay 路由独立 boundary，以及 Trending 默认首页契约。
 */

import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { describe, expect, test } from 'vitest';

const appDir = dirname(fileURLToPath(import.meta.url));
const appSource = readFileSync(resolve(appDir, './App.tsx'), 'utf8');
const appShellSource = readFileSync(
  resolve(appDir, './components/layout/AppShell/AppShell.tsx'),
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
      /import\s*\{\s*ActivityStats\s*\}\s*from\s*['"]\.\/pages\/ActivityStats['"]/,
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

  test('wraps app with LanDisclosureGate above routes and onboarding', () => {
    expect(appSource).toMatch(/import\s*\{\s*LanDisclosureGate\s*\}/);
    // export default App 内 gate 包裹 Routes 与 OnboardingGuard
    const appFn = appSource.slice(appSource.indexOf('export default function App'));
    expect(appFn).toMatch(/<LanDisclosureGate>[\s\S]*<Routes>[\s\S]*OnboardingGuard/);
    expect(appFn).toMatch(/<\/LanDisclosureGate>/);
  });

  test('keeps / as the existing Home/Trending shell route', () => {
    // 冷启动与刷新都走 path="/" → lazy Home；不得改成 dashboard 或 redirect 到 workbench
    expect(appSource).toMatch(
      /path\s*=\s*["']\/["']\s+element=\{<ShellRoute><Home\s*\/>\s*<\/ShellRoute>\}/,
    );
    expect(appSource).not.toMatch(/path\s*=\s*["']\/["']\s+element=\{<Navigate/);
    expect(appSource).toMatch(/lazyNamed\s*\(\s*\(\)\s*=>\s*import\s*\(\s*['"]\.\/pages\/Home['"]\s*\)/);
  });

  test('keeps /workbench as a separate lazy route from Home', () => {
    expect(appSource).toMatch(
      /path\s*=\s*["']\/workbench["']\s+element=\{<ShellRoute><Workbench\s*\/>\s*<\/ShellRoute>\}/,
    );
    expect(appSource).toMatch(
      /lazyNamed\s*\(\s*\(\)\s*=>\s*import\s*\(\s*['"]\.\/pages\/Workbench['"]\s*\)/,
    );
    // Home 与 Workbench 不得合并为同一 path
    const homeRoute = appSource.match(
      /path\s*=\s*["']\/["']\s+element=\{<ShellRoute><(\w+)\s*\/>/,
    );
    const workbenchRoute = appSource.match(
      /path\s*=\s*["']\/workbench["']\s+element=\{<ShellRoute><(\w+)\s*\/>/,
    );
    expect(homeRoute?.[1]).toBe('Home');
    expect(workbenchRoute?.[1]).toBe('Workbench');
    expect(homeRoute?.[1]).not.toBe(workbenchRoute?.[1]);
  });

  test('production route table has no /discover migration alias', () => {
    // 禁止为 Trending 增加 /discover 搬迁路由或 Navigate alias
    expect(appSource).not.toMatch(/path\s*=\s*["']\/discover["']/);
    expect(appSource).not.toMatch(/to\s*=\s*["']\/discover["']/);
    expect(appSource).not.toMatch(/['"]\.\/pages\/Discover['"]/);
    expect(appShellSource).not.toMatch(/to\s*=\s*["']\/discover["']/);
  });

  test('sidebar Home nav still activates / rather than a discover destination', () => {
    // 侧栏 Home 激活仍是 NavItem to="/"（end 精确匹配在 NavItem 内）
    expect(appShellSource).toMatch(
      /<NavItem\s+to\s*=\s*["']\/["']\s+label=\{t\(['"]nav:home['"]\)\}/,
    );
    expect(appShellSource).not.toMatch(/to\s*=\s*["']\/discover["']/);
    expect(appShellSource).not.toMatch(/to\s*=\s*["']\/workbench["']\s+label=\{t\(['"]nav:home['"]\)\}/);
  });

  test('keeps /activity as a separate lazy route from Health', () => {
    expect(appSource).toMatch(
      /path\s*=\s*["']\/activity["']\s+element=\{<ShellRoute><ActivityStats\s*\/>\s*<\/ShellRoute>\}/,
    );
    expect(appSource).toMatch(
      /lazyNamed\s*\(\s*\(\)\s*=>\s*import\s*\(\s*['"]\.\/pages\/ActivityStats['"]\s*\)/,
    );
    expect(appShellSource).toMatch(
      /<NavItem\s+to\s*=\s*["']\/activity["']\s+label=\{t\(['"]nav:activity['"]\)\}/,
    );
    expect(appShellSource).toMatch(
      /<NavItem\s+to\s*=\s*["']\/health["']\s+label=\{t\(['"]nav:health['"]\)\}/,
    );
  });

  test('keeps /token-stats as a separate lazy route after Activity', () => {
    expect(appSource).toMatch(
      /path\s*=\s*["']\/token-stats["']\s+element=\{<ShellRoute><TokenStats\s*\/>\s*<\/ShellRoute>\}/,
    );
    expect(appSource).toMatch(
      /lazyNamed\s*\(\s*\(\)\s*=>\s*import\s*\(\s*['"]\.\/pages\/TokenStats['"]\s*\)/,
    );
    expect(appShellSource).toMatch(
      /<NavItem\s+to\s*=\s*["']\/token-stats["']\s+label=\{t\(['"]nav:tokenStats['"]\)\}/,
    );
  });

  test('legacy /claude-code deep-links to Agent Hub assets with Claude target', () => {
    expect(appSource).toMatch(
      /path\s*=\s*["']\/claude-code["']\s+element=\{<Navigate\s+to=["']\/agent-hub\?section=assets&target=claude["']\s+replace\s*\/>\}/,
    );
  });

  test('legacy /prompt-optimizer redirects to Workbench and is not a sidebar item', () => {
    expect(appSource).toMatch(
      /path\s*=\s*["']\/prompt-optimizer["']\s+element=\{<Navigate\s+to=["']\/workbench["']\s+replace\s*\/>\}/,
    );
    expect(appSource).not.toMatch(/pages\/PromptOptimizer/);
    expect(appShellSource).not.toMatch(/to\s*=\s*["']\/prompt-optimizer["']/);
  });
});
