import { expect, test, type Page } from '@playwright/test';

declare global {
  interface Window {
    __TAURI_INTERNALS__?: {
      invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
      transformCallback: (callback: unknown) => number;
      unregisterCallback: (id: number) => void;
    };
    __TAURI_EVENT_PLUGIN_INTERNALS__?: {
      unregisterListener: (event: string, eventId: number) => void;
    };
    __resolveSnapshot?: () => void;
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   截图 Overlay 在浏览器测试环境没有真实 Tauri 后端，需要模拟抓图命令才能复现用户框选后的编辑流程。
 *
 * Code Logic（这个函数做什么）:
 *   在页面初始化前注入 `__TAURI_INTERNALS__.invoke`，让 `get_region_snapshot` 挂起到测试主动释放，
 *   其他截图命令返回成功，从而验证工具条是否被快照等待阻塞。
 */
async function installDelayedSnapshotMock(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const png =
      'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAFgwJ/lqJ5cgAAAABJRU5ErkJggg==';
    let resolveSnapshot: (() => void) | undefined;
    window.__resolveSnapshot = () => {
      resolveSnapshot?.();
    };
    let callbackId = 0;
    window.__TAURI_INTERNALS__ = {
      invoke: async (cmd: string) => {
        if (cmd === 'plugin:event|listen') return 1;
        if (cmd === 'plugin:event|unlisten') return undefined;
        if (cmd === 'get_region_snapshot') {
          await new Promise<void>((resolve) => {
            resolveSnapshot = resolve;
          });
          return png;
        }
        return undefined;
      },
      transformCallback: () => {
        callbackId += 1;
        return callbackId;
      },
      unregisterCallback: () => undefined,
    };
    window.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
      unregisterListener: () => undefined,
    };
  });
}

test.describe('截图选区 Overlay', () => {
  test('框选完成后工具条不等待快照返回即可显示', async ({ page }) => {
    await installDelayedSnapshotMock(page);
    await page.goto('/screenshot-overlay?display=0');

    await page.mouse.move(80, 90);
    await page.mouse.down();
    await page.mouse.move(300, 240);
    await page.mouse.up();

    await expect(page.getByRole('toolbar')).toBeVisible();
    await page.evaluate(() => window.__resolveSnapshot?.());
    await expect(page.locator('canvas')).toBeVisible();
  });
});
