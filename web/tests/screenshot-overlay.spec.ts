import { type Page } from '@playwright/test';
import { expect, test } from './fixtures';

declare global {
  interface Window {
    __TAURI_INTERNALS__?: {
      invoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
      transformCallback: (callback: unknown) => number;
      unregisterCallback: (id: number) => void;
      metadata?: {
        currentWindow: { label: string };
        currentWebview?: { windowLabel: string; label: string };
      };
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
      // metadata 必须存在：canListenToTauriEvents 见 transformCallback 后会调用 getCurrentWindow()，
      // 缺 currentWindow 会 pageerror；label 非 main 让 BackendCloseChoiceListener 直接跳过。
      metadata: {
        currentWindow: { label: 'screenshot-overlay' },
        currentWebview: { windowLabel: 'screenshot-overlay', label: 'screenshot-overlay' },
      },
      invoke: async (cmd: string) => {
        if (cmd === 'plugin:event|listen') return 1;
        if (cmd === 'plugin:event|unlisten') return undefined;
        if (cmd === 'get_lan_disclosure_status') {
          return {
            required: false,
            version: 1,
            localAddresses: ['192.168.1.10'],
            preferredPort: 62116,
            mdnsPort: 5353,
            alreadyRunning: false,
            actualHttpPort: 62116,
          };
        }
        if (cmd === 'get_operational_notification_snapshot') {
          return {
            asOfCursor: { ownerInstanceId: 'owner-shot', sequence: 0 },
            items: [],
            truncated: false,
          };
        }
        if (
          cmd === 'get_orchestrator_config' ||
          cmd === 'get_default_orchestrator_config'
        ) {
          return {
            enabled: false,
            maxConcurrentTasks: 1,
            verificationCommands: [],
            autoCommit: false,
            autoPushTaskBranch: false,
            autoMergeToMain: false,
            autoPushMain: false,
            notifyHumanReview: true,
            notifyBlocked: true,
            notifyRemoteOutboxFailed: true,
            notifyTaskDone: false,
          };
        }
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
    await page.goto('/screenshot-overlay?display=0');

    await expect(page.locator('[class*="overlay"]')).toBeVisible();
  });

  test('框选完成后工具条不等待快照返回即可显示', async ({ page }) => {
    await installDelayedSnapshotMock(page);
    await page.goto('/screenshot-overlay?display=0');
    await expect(page.locator('[class*="overlay"]')).toBeVisible();

    // 在 overlay 根上拖出足够大的选区（≥10px），避免 mouse 事件未命中 root。
    const box = await page.locator('[class*="overlay"]').boundingBox();
    if (!box) throw new Error('overlay not laid out');
    const x0 = box.x + 40;
    const y0 = box.y + 40;
    const x1 = box.x + Math.min(box.width - 20, 320);
    const y1 = box.y + Math.min(box.height - 20, 260);
    await page.mouse.move(x0, y0);
    await page.mouse.down();
    await page.mouse.move(x1, y1, { steps: 8 });
    await page.mouse.up();

    await expect(page.getByRole('toolbar')).toBeVisible({ timeout: 10_000 });
    await page.evaluate(() => window.__resolveSnapshot?.());
    await expect(page.locator('canvas')).toBeVisible({ timeout: 10_000 });
  });

  test('快照捕获开始前工具条和选区框已经可见', async ({ page }) => {
    await installDelayedSnapshotMock(page);
    await page.goto('/screenshot-overlay?display=0');
    await expect(page.locator('[class*="overlay"]')).toBeVisible();

    const box = await page.locator('[class*="overlay"]').boundingBox();
    if (!box) throw new Error('overlay not laid out');
    const x0 = box.x + 40;
    const y0 = box.y + 40;
    const x1 = box.x + Math.min(box.width - 20, 320);
    const y1 = box.y + Math.min(box.height - 20, 260);
    await page.mouse.move(x0, y0);
    await page.mouse.down();
    await page.mouse.move(x1, y1, { steps: 8 });
    await page.mouse.up();

    await page.waitForFunction(() => window.__snapshotInvokeState !== undefined, null, {
      timeout: 15_000,
    });
    await expect(page.evaluate(() => window.__snapshotInvokeState)).resolves.toEqual({
      toolbarVisible: true,
      selectionVisible: true,
    });
    await page.evaluate(() => window.__resolveSnapshot?.());
  });
});
