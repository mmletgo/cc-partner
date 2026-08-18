/**
 * 移动端传输面板 ownership 与懒加载契约。
 *
 * Business Logic（为什么需要这个测试）:
 *   view/panel 不得直连 `@/api/*`；MobileWorkbench 必须 lazy 加载传输面板，
 *   禁止把桌面 Transfer/Tauri 打进 mobile eager graph。
 *
 * Code Logic（这个测试做什么）:
 *   静态扫描 panel/view/controller/MobileWorkbench 源码。
 */

import { readFileSync } from 'node:fs';
import { describe, test } from 'vitest';

/**
 * Business Logic（为什么需要这个函数）:
 *   ownership 失败必须立刻可见。
 *
 * Code Logic（这个函数做什么）:
 *   condition 为 false 时抛错。
 */
function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   源码必须包含关键懒加载边界。
 *
 * Code Logic（这个函数做什么）:
 *   includes 检查。
 */
function assertContains(source: string, expected: string, message: string): void {
  assert(source.includes(expected), message);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   view 回退 import API 会破坏分层。
 *
 * Code Logic（这个函数做什么）:
 *   禁止指定子串。
 */
function assertNotContains(source: string, unexpected: string, message: string): void {
  assert(!source.includes(unexpected), message);
}

describe('MobileTransferPanel ownership', () => {
  test('panel and view must not import @/api/*; workbench lazy-loads transfer', () => {
    const panelSource = readFileSync(new URL('./MobileTransferPanel.tsx', import.meta.url), 'utf8');
    const viewSource = readFileSync(new URL('./MobileTransferView.tsx', import.meta.url), 'utf8');
    const controllerSource = readFileSync(
      new URL('../controllers/useMobileTransferController.ts', import.meta.url),
      'utf8',
    );
    const workbenchSource = readFileSync(new URL('../MobileWorkbench.tsx', import.meta.url), 'utf8');
    const desktopTransferSource = readFileSync(
      new URL('../../pages/Transfer/Transfer.tsx', import.meta.url),
      'utf8',
    );

    assertNotContains(panelSource, "from '@/api/", 'panel must not import @/api/* modules');
    assertNotContains(viewSource, "from '@/api/", 'view must not import @/api/* modules');
    assertNotContains(viewSource, 'transferHttp', 'view must not call transferHttp');
    assertContains(
      controllerSource,
      'transferHttp',
      'controller should own transferHttp calls',
    );
    assertContains(
      controllerSource,
      'useVisibilityPolling',
      'controller should poll devices/tasks with useVisibilityPolling',
    );
    assertNotContains(controllerSource, '</', 'controller must not render JSX');
    assertNotContains(controllerSource, 'React.createElement', 'controller must not render JSX');

    assertContains(
      workbenchSource,
      "import('./components/MobileTransferPanel')",
      'MobileWorkbench must React.lazy load the transfer panel',
    );
    assertNotContains(
      workbenchSource,
      "from './components/MobileTransferPanel'",
      'MobileWorkbench must not statically import the transfer panel',
    );
    assertNotContains(
      workbenchSource,
      '@/pages/Transfer/Transfer',
      'MobileWorkbench must not import desktop Transfer.tsx',
    );
    assertNotContains(
      workbenchSource,
      "from '@/pages/Transfer'",
      'MobileWorkbench must not import desktop Transfer page',
    );

    assertNotContains(
      desktopTransferSource,
      'onDownload',
      'desktop Transfer.tsx must not pass onDownload',
    );
  });
});
