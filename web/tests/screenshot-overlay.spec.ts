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
    __snapshotInvokeState?: {
      toolbarVisible: boolean;
      selectionVisible: boolean;
    };
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   截图 Overlay 在浏览器测试环境没有真实 Tauri 后端，需要模拟抓图命令才能复现用户框选后的编辑流程。
 *
 * Code Logic（这个函数做什么）:
 *   在页面初始化前注入 `__TAURI_INTERNALS__.invoke`，让 `get_region_snapshot` 挂起到测试主动释放，
 *   并在快照命令开始时记录工具条/选区框是否可见；其他截图命令返回成功。
 */
async function installDelayedSnapshotMock(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const png =
      'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAFgwJ/lqJ5cgAAAABJRU5ErkJggg==';
    let resolveSnapshot: (() => void) | undefined;
    window.__resolveSnapshot = () => {
      resolveSnapshot?.();
    };
    /**
     * Business Logic（为什么需要这个函数）:
     *   回归测试需要在快照命令启动瞬间判断关键 UI 是否已经真实可见，而不是只存在于 DOM 中。
     *
     * Code Logic（这个函数做什么）:
     *   通过 DOMRect 与 computed style 判断元素具有尺寸且未被 display/visibility/opacity 隐藏。
     */
    const isVisible = (el: Element | null) => {
      if (!(el instanceof HTMLElement)) return false;
      const rect = el.getBoundingClientRect();
      const style = window.getComputedStyle(el);
      return rect.width > 0 && rect.height > 0 && style.display !== 'none' && style.visibility !== 'hidden' && style.opacity !== '0';
    };
    let callbackId = 0;
    window.__TAURI_INTERNALS__ = {
      invoke: async (cmd: string) => {
        if (cmd === 'plugin:event|listen') return 1;
        if (cmd === 'plugin:event|unlisten') return undefined;
        if (cmd === 'get_region_snapshot') {
          window.__snapshotInvokeState = {
            toolbarVisible: isVisible(document.querySelector('[role="toolbar"]')),
            selectionVisible: isVisible(document.querySelector('[data-testid="screenshot-selection"]')),
          };
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
  test('普通 Vite 浏览器没有 Tauri event internals 时仍可渲染遮罩', async ({ page }) => {
    const pageErrors: string[] = [];
    page.on('pageerror', (error) => pageErrors.push(error.message));

    await page.goto('/screenshot-overlay?display=0');

    await expect(page.locator('[class*="overlay"]')).toBeVisible();
    expect(pageErrors.filter((message) => message.includes('transformCallback'))).toEqual([]);
  });

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

  test('快照捕获开始前工具条和选区框已经可见', async ({ page }) => {
    await installDelayedSnapshotMock(page);
    await page.goto('/screenshot-overlay?display=0');

    await page.mouse.move(80, 90);
    await page.mouse.down();
    await page.mouse.move(300, 240);
    await page.mouse.up();

    await page.waitForFunction(() => window.__snapshotInvokeState !== undefined);
    await expect(page.evaluate(() => window.__snapshotInvokeState)).resolves.toEqual({
      toolbarVisible: true,
      selectionVisible: true,
    });
    await page.evaluate(() => window.__resolveSnapshot?.());
  });
});
