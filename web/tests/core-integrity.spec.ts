import { type Page } from '@playwright/test';
import { expect, test } from './fixtures';

/**
 * Core Product Integrity E2E（桌面浏览器模式，确定性 Tauri mock）。
 *
 * Business Logic（为什么需要这个套件）:
 *   验证文件传输 send/cancel、Prompt 乐观 mutation 失败回滚、权限首轮失败后重试
 *   与权威后端一致，且无生产专用 bypass；意外 console.error/pageerror 由 fixtures 自动失败。
 *
 * Code Logic（这个套件做什么）:
 *   在 page init 注入 `__TAURI_INTERNALS__.invoke` 假后端（精确命令名）与可切换失败开关；
 *   Transfer 通过 `plugin:dialog|open` 注入 `/tmp/report.txt`；Prompts 覆盖 create/update/delete reject；
 *   Welcome 覆盖 check_permissions 首轮失败 → 重新检查成功。
 */

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
    __coreIntegrityTestApi?: {
      setPermissionFail: (fail: boolean) => void;
      setPromptFailMode: (mode: 'none' | 'create' | 'update' | 'delete') => void;
      getLastSendArgs: () => {
        deviceId?: string;
        filePath?: string;
        clientOperationId?: string;
      } | null;
      getLastCancelTaskId: () => string | null;
      getTasks: () => unknown[];
    };
  }
}

interface CoreDeviceDto {
  id: string;
  name: string;
  address: string;
  port: number;
  online: boolean;
  lastSeen?: string;
}

interface CoreTransferTask {
  id: string;
  fileName: string;
  filePath: string;
  fileSize: number;
  direction: 'send' | 'receive';
  status: 'pending' | 'transferring' | 'completed' | 'failed' | 'cancelled';
  progress: number;
  peerDeviceId?: string;
  peerDeviceName?: string;
  speed?: number;
  errorMessage?: string;
  startedAt: string;
  completedAt?: string;
}

interface CorePrompt {
  id: string;
  title: string;
  content: string;
  tags: string[];
  updatedAt: string;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Transfer 旅程需要真实 send/cancel 命令与原生 dialog 路径，不能走生产 bypass。
 *
 * Code Logic（这个函数做什么）:
 *   注入 onboarded 标记、假设备/任务状态机、plugin:dialog|open → /tmp/report.txt。
 */
async function installTransferMocks(page: Page): Promise<void> {
  await page.addInitScript(() => {
    window.localStorage.setItem('cp-permission-onboarded', '1');
    window.localStorage.setItem('cp-lang', 'zh');
    window.localStorage.setItem('cp-theme', 'light');

    const device: CoreDeviceDto = {
      id: 'peer-1',
      name: 'Peer Mac',
      address: '192.168.1.20',
      port: 62116,
      online: true,
      lastSeen: '2026-07-14T00:00:00.000Z',
    };

    let tasks: CoreTransferTask[] = [];
    let lastSendArgs: {
      deviceId?: string;
      filePath?: string;
      clientOperationId?: string;
    } | null = null;
    let lastCancelTaskId: string | null = null;
    let taskSeq = 0;

    window.__coreIntegrityTestApi = {
      setPermissionFail: () => undefined,
      setPromptFailMode: () => undefined,
      getLastSendArgs: () => lastSendArgs,
      getLastCancelTaskId: () => lastCancelTaskId,
      getTasks: () => tasks,
    };

    let callbackId = 0;
    window.__TAURI_INTERNALS__ = {
      metadata: {
        currentWindow: { label: 'main' },
        currentWebview: { windowLabel: 'main', label: 'main' },
      },
      invoke: async (cmd: string, args?: Record<string, unknown>) => {
        if (cmd === 'check_permissions') {
          return {
            screenCapture: { granted: true },
            accessibility: { granted: true },
            inputMonitoring: { granted: true },
          };
        }
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
        if (cmd === 'acknowledge_lan_disclosure_and_start_backend') {
          return {
            actualHttpPort: 62116,
            localAddresses: ['192.168.1.10'],
            reusedExisting: false,
            version: 1,
          };
        }
        if (cmd === 'list_devices') {
          return [device];
        }
        if (cmd === 'list_transfers') {
          return tasks;
        }
        if (cmd === 'plugin:dialog|open') {
          return '/tmp/report.txt';
        }
        if (cmd === 'send_transfer') {
          const deviceId = String(args?.deviceId ?? '');
          const filePath = String(args?.filePath ?? '');
          const clientOperationId = String(args?.clientOperationId ?? '');
          lastSendArgs = { deviceId, filePath, clientOperationId };
          taskSeq += 1;
          const id = `task-${taskSeq}`;
          const fileName = filePath.split(/[/\\]/).pop() || filePath;
          tasks = [
            {
              id,
              fileName,
              filePath,
              fileSize: 1024,
              direction: 'send',
              status: 'transferring',
              progress: 0.25,
              peerDeviceId: deviceId,
              peerDeviceName: device.name,
              speed: 1024,
              startedAt: '2026-07-14T00:01:00.000Z',
            },
            ...tasks,
          ];
          return {
            accepted: true,
            deviceId,
            filePath,
            id,
          };
        }
        if (cmd === 'cancel_transfer') {
          const taskId = String(args?.taskId ?? '');
          lastCancelTaskId = taskId;
          const found = tasks.find((item) => item.id === taskId);
          if (!found) {
            throw new Error('transfer task not found');
          }
          tasks = tasks.map((item) =>
            item.id === taskId
              ? {
                  ...item,
                  status: 'cancelled',
                  progress: 0,
                  completedAt: '2026-07-14T00:02:00.000Z',
                }
              : item,
          );
          return { ok: true, id: taskId };
        }

        if (cmd === 'plugin:event|listen') return 1;
        if (cmd === 'plugin:event|unlisten') return undefined;
        if (cmd === 'get_version') return { version: '0.0.0-test' };
        if (cmd === 'list_workbench_projects') return [];
        if (cmd === 'list_workbench_sessions') return [];
        if (cmd === 'list_attention_items') {
          return {
            generatedAt: '2026-07-14T00:00:00.000Z',
            counts: { total: 0, decision: 0, blocked: 0, environment: 0 },
            items: [],
          };
        }
        if (
          cmd === 'check_workbench_dependency' ||
          cmd === 'get_workbench_dependency_install_status'
        ) {
          return {
            status: 'ready',
            available: true,
            version: '3.0',
            backend: 'native',
            path: '/usr/bin/tmux',
            installable: false,
            installCommandPreview: [],
            error: null,
            output: [],
            statusChangedAt: '2026-07-14T00:00:00.000Z',
          };
        }
        if (cmd === 'get_config' || cmd === 'get_default_config') {
          return {
            deviceId: 'self-1',
            deviceName: 'Test Device',
            receiveDir: '/tmp',
            screenshotHotkey: 'CommandOrControl+Shift+S',
            promptOptimizerHotkey: 'Control',
            promptOptimizerFillLanguage: 'zh',
            httpPort: 62116,
          };
        }
        if (cmd === 'list_github_trending_repos') {
          return { repos: [], cached: true, generatedAt: null };
        }
        if (cmd === 'plugin:notification|is_permission_granted') return true;
        if (cmd === 'plugin:notification|request_permission') return 'granted';
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

/**
 * Business Logic（为什么需要这个函数）:
 *   Prompt 库 create/update/delete 失败必须回滚乐观 UI，E2E 需可切换 reject 模式。
 *
 * Code Logic（这个函数做什么）:
 *   维护可变 prompts 列表与 failMode；精确 mock list/create/update/delete_prompt。
 */
async function installPromptMocks(page: Page): Promise<void> {
  await page.addInitScript(() => {
    window.localStorage.setItem('cp-permission-onboarded', '1');
    window.localStorage.setItem('cp-lang', 'zh');
    window.localStorage.setItem('cp-theme', 'light');

    let prompts: CorePrompt[] = [
      {
        id: 'prompt-1',
        title: '已有标题',
        content: '已有内容',
        tags: ['base'],
        updatedAt: '2026-07-14T00:00:00.000Z',
      },
    ];
    let failMode: 'none' | 'create' | 'update' | 'delete' = 'none';
    let createSeq = 0;

    window.__coreIntegrityTestApi = {
      setPermissionFail: () => undefined,
      setPromptFailMode: (mode) => {
        failMode = mode;
      },
      getLastSendArgs: () => null,
      getLastCancelTaskId: () => null,
      getTasks: () => [],
    };

    let callbackId = 0;
    window.__TAURI_INTERNALS__ = {
      metadata: {
        currentWindow: { label: 'main' },
        currentWebview: { windowLabel: 'main', label: 'main' },
      },
      invoke: async (cmd: string, args?: Record<string, unknown>) => {
        if (cmd === 'check_permissions') {
          return {
            screenCapture: { granted: true },
            accessibility: { granted: true },
            inputMonitoring: { granted: true },
          };
        }
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
        if (cmd === 'acknowledge_lan_disclosure_and_start_backend') {
          return {
            actualHttpPort: 62116,
            localAddresses: ['192.168.1.10'],
            reusedExisting: false,
            version: 1,
          };
        }
        if (cmd === 'list_prompts') {
          return prompts;
        }
        if (cmd === 'create_prompt') {
          if (failMode === 'create') {
            throw new Error('create rejected');
          }
          createSeq += 1;
          const created: CorePrompt = {
            id: `server-${createSeq}`,
            title: String(args?.title ?? ''),
            content: String(args?.content ?? ''),
            tags: Array.isArray(args?.tags) ? (args?.tags as string[]) : [],
            updatedAt: '2026-07-14T00:10:00.000Z',
          };
          prompts = [created, ...prompts];
          return created;
        }
        if (cmd === 'update_prompt') {
          if (failMode === 'update') {
            throw new Error('update rejected');
          }
          const id = String(args?.id ?? '');
          const next = prompts.map((item) =>
            item.id === id
              ? {
                  ...item,
                  title: args?.title !== undefined ? String(args.title) : item.title,
                  content:
                    args?.content !== undefined ? String(args.content) : item.content,
                  tags: Array.isArray(args?.tags) ? (args.tags as string[]) : item.tags,
                  updatedAt: '2026-07-14T00:11:00.000Z',
                }
              : item,
          );
          prompts = next;
          const updated = prompts.find((item) => item.id === id);
          if (!updated) throw new Error('prompt not found');
          return updated;
        }
        if (cmd === 'delete_prompt') {
          if (failMode === 'delete') {
            throw new Error('delete rejected');
          }
          const id = String(args?.id ?? '');
          prompts = prompts.filter((item) => item.id !== id);
          return undefined;
        }
        if (cmd === 'trigger_sync') {
          return { synced: 0 };
        }
        if (cmd === 'list_tags') {
          return Array.from(new Set(prompts.flatMap((p) => p.tags))).sort();
        }

        if (cmd === 'plugin:event|listen') return 1;
        if (cmd === 'plugin:event|unlisten') return undefined;
        if (cmd === 'get_version') return { version: '0.0.0-test' };
        if (cmd === 'list_workbench_projects') return [];
        if (cmd === 'list_workbench_sessions') return [];
        if (cmd === 'list_attention_items') {
          return {
            generatedAt: '2026-07-14T00:00:00.000Z',
            counts: { total: 0, decision: 0, blocked: 0, environment: 0 },
            items: [],
          };
        }
        if (
          cmd === 'check_workbench_dependency' ||
          cmd === 'get_workbench_dependency_install_status'
        ) {
          return {
            status: 'ready',
            available: true,
            version: '3.0',
            backend: 'native',
            path: '/usr/bin/tmux',
            installable: false,
            installCommandPreview: [],
            error: null,
            output: [],
            statusChangedAt: '2026-07-14T00:00:00.000Z',
          };
        }
        if (cmd === 'get_config' || cmd === 'get_default_config') {
          return {
            deviceId: 'self-1',
            deviceName: 'Test Device',
            receiveDir: '/tmp',
            screenshotHotkey: 'CommandOrControl+Shift+S',
            promptOptimizerHotkey: 'Control',
            promptOptimizerFillLanguage: 'zh',
            httpPort: 62116,
          };
        }
        if (cmd === 'list_github_trending_repos') {
          return { repos: [], cached: true, generatedAt: null };
        }
        if (cmd === 'plugin:notification|is_permission_granted') return true;
        if (cmd === 'plugin:notification|request_permission') return 'granted';
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

/**
 * Business Logic（为什么需要这个函数）:
 *   Welcome 权限首轮失败必须可重试，E2E 需可切换 check_permissions 成败。
 *
 * Code Logic（这个函数做什么）:
 *   不写 onboarded；permissionFail 初始 true；setPermissionFail(false) 后返回 granted=false 的状态对象。
 */
async function installPermissionMocks(page: Page): Promise<void> {
  await page.addInitScript(() => {
    window.localStorage.removeItem('cp-permission-onboarded');
    window.localStorage.setItem('cp-lang', 'zh');
    window.localStorage.setItem('cp-theme', 'light');

    let permissionFail = true;

    window.__coreIntegrityTestApi = {
      setPermissionFail: (fail) => {
        permissionFail = fail;
      },
      setPromptFailMode: () => undefined,
      getLastSendArgs: () => null,
      getLastCancelTaskId: () => null,
      getTasks: () => [],
    };

    let callbackId = 0;
    window.__TAURI_INTERNALS__ = {
      metadata: {
        currentWindow: { label: 'main' },
        currentWebview: { windowLabel: 'main', label: 'main' },
      },
      invoke: async (cmd: string) => {
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
        if (cmd === 'acknowledge_lan_disclosure_and_start_backend') {
          return {
            actualHttpPort: 62116,
            localAddresses: ['192.168.1.10'],
            reusedExisting: false,
            version: 1,
          };
        }
        if (cmd === 'check_permissions') {
          if (permissionFail) {
            throw new Error('permission check failed');
          }
          return {
            screenCapture: { granted: false },
            accessibility: { granted: false },
            inputMonitoring: { granted: false },
          };
        }
        if (cmd === 'request_permission') {
          return { requested: true };
        }
        if (cmd === 'plugin:notification|is_permission_granted') return false;
        if (cmd === 'plugin:notification|request_permission') return 'default';
        if (cmd === 'plugin:event|listen') return 1;
        if (cmd === 'plugin:event|unlisten') return undefined;
        if (cmd === 'get_version') return { version: '0.0.0-test' };
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

test.describe('Core product integrity', () => {
  test('transfer: native path pick → send → task visible → cancel', async ({ page }) => {
    await installTransferMocks(page);
    await page.goto('/transfer');

    await expect(page.getByRole('heading', { name: '文件传输' })).toBeVisible({
      timeout: 10_000,
    });
    await expect(page.getByLabel('选择目标设备')).toBeVisible();
    await expect(page.getByRole('option', { name: /Peer Mac/ })).toHaveCount(1);

    await page.getByRole('button', { name: '浏览…' }).click();
    await expect(page.getByText('已选择：report.txt')).toBeVisible({
      timeout: 5_000,
    });

    await page.getByRole('button', { name: /发送「report\.txt」/ }).click();

    // 发送成功后清空选中态；任务列表出现 basename
    await expect(page.getByText('已选择：report.txt')).toHaveCount(0, { timeout: 5_000 });
    await expect(page.getByRole('list').getByText('report.txt')).toBeVisible({ timeout: 5_000 });
    await expect(page.getByText('传输中').first()).toBeVisible();

    // 未接入 pause/retry/open：仅 cancel 可用
    await expect(page.getByRole('button', { name: '取消' })).toHaveCount(1);
    await expect(page.getByRole('button', { name: '暂停' })).toHaveCount(0);
    await expect(page.getByRole('button', { name: '重试' })).toHaveCount(0);

    const sendArgs = await page.evaluate(() => window.__coreIntegrityTestApi?.getLastSendArgs());
    expect(sendArgs?.deviceId).toBe('peer-1');
    expect(sendArgs?.filePath).toBe('/tmp/report.txt');
    expect(typeof sendArgs?.clientOperationId).toBe('string');
    expect((sendArgs?.clientOperationId ?? '').length).toBeGreaterThan(0);

    await page.getByRole('button', { name: '取消' }).click();
    await expect(page.getByText('已取消').first()).toBeVisible({ timeout: 5_000 });

    const cancelId = await page.evaluate(
      () => window.__coreIntegrityTestApi?.getLastCancelTaskId(),
    );
    expect(cancelId).toBe('task-1');
  });

  test('prompts: create/update/delete reject rolls back and keeps draft/list', async ({
    page,
  }) => {
    await installPromptMocks(page);
    await page.goto('/prompts');

    await expect(page.getByRole('heading', { name: 'Prompt 库' })).toBeVisible({
      timeout: 10_000,
    });
    await expect(page.getByText('已有标题')).toBeVisible();

    // ── create reject → 回滚列表 + 恢复草稿 ──
    await page.evaluate(() => {
      window.__coreIntegrityTestApi?.setPromptFailMode('create');
    });
    await page.getByRole('button', { name: '新建' }).click();
    await page.getByLabel('Prompt 标题').fill('新建标题');
    await page.getByLabel('Prompt 内容').fill('新建内容');
    await page.getByRole('button', { name: '保存' }).click();

    await expect(page.getByTestId('prompt-mutation-error')).toBeVisible({ timeout: 5_000 });
    await expect(page.getByText('创建失败：create rejected')).toBeVisible();
    await expect(page.getByLabel('Prompt 标题')).toHaveValue('新建标题');
    await expect(page.getByLabel('Prompt 内容')).toHaveValue('新建内容');
    // 乐观 create 已从列表回滚：仅编辑草稿存在，无展示态卡片
    await expect(page.getByRole('heading', { name: '新建标题' })).toHaveCount(0);

    // 取消草稿，回到列表
    await page.getByRole('button', { name: '取消' }).first().click();
    await expect(page.getByLabel('Prompt 内容')).toHaveCount(0);

    // ── update reject → 列表恢复 before + 草稿恢复 ──
    await page.evaluate(() => {
      window.__coreIntegrityTestApi?.setPromptFailMode('update');
    });
    await page.getByRole('button', { name: '编辑' }).click();
    await page.getByLabel('Prompt 标题').fill('改后标题');
    await page.getByLabel('Prompt 内容').fill('改后内容');
    await page.getByRole('button', { name: '保存' }).click();

    await expect(page.getByTestId('prompt-mutation-error')).toBeVisible({ timeout: 5_000 });
    await expect(page.getByText('更新失败：update rejected')).toBeVisible();
    // 失败恢复编辑草稿（用户输入保留）；列表已 rollback，取消编辑后应回到 before
    await expect(page.getByLabel('Prompt 标题')).toHaveValue('改后标题');
    await expect(page.getByLabel('Prompt 内容')).toHaveValue('改后内容');

    await page.getByRole('button', { name: '取消' }).first().click();
    await expect(page.getByText('已有标题')).toBeVisible();
    await expect(page.getByText('改后标题')).toHaveCount(0);

    // ── delete reject → 列表恢复条目 ──
    await page.evaluate(() => {
      window.__coreIntegrityTestApi?.setPromptFailMode('delete');
    });
    await page.getByRole('button', { name: '删除' }).click();
    await expect(page.getByRole('heading', { name: '删除 Prompt？' })).toBeVisible();
    await page.getByRole('button', { name: '删除' }).last().click();

    await expect(page.getByTestId('prompt-mutation-error')).toBeVisible({ timeout: 5_000 });
    await expect(page.getByText('删除失败：delete rejected')).toBeVisible();
    await expect(page.getByText('已有标题')).toBeVisible();
  });

  test('permissions: initial failure then recheck success', async ({ page }) => {
    await installPermissionMocks(page);
    await page.goto('/welcome');

    // Welcome 是独立 onboarding 路由（main），不是 modal dialog。
    await expect(page.getByRole('heading', { name: /欢迎使用/ })).toBeVisible({
      timeout: 10_000,
    });

    // 首轮失败：结束 loading，展示错误 + 重新检查
    await expect(page.getByText(/检查权限失败：permission check failed/)).toBeVisible({
      timeout: 5_000,
    });
    const recheck = page.getByRole('button', { name: '重新检查' });
    await expect(recheck).toBeVisible();

    await page.evaluate(() => {
      window.__coreIntegrityTestApi?.setPermissionFail(false);
    });
    await recheck.click();

    // 成功后展示权限卡列表（不再永久 checking）
    await expect(page.getByText(/屏幕录制/)).toBeVisible({ timeout: 5_000 });
    await expect(page.getByRole('button', { name: '重新检查' })).toBeVisible();
    await expect(page.getByText(/检查权限失败：permission check failed/)).toHaveCount(0);
  });
});
