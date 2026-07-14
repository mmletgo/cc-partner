import { type Page } from '@playwright/test';
import { expect, test } from './fixtures';

/**
 * Frontend Foundation E2E smoke（桌面 + 窄屏 mobile）。
 *
 * Business Logic（为什么需要这个套件）:
 *   S4 落地后需要可回归的 a11y/交互/错误隔离冒烟：Dialog 焦点环与恢复、移动 Drawer Escape、
 *   Attention 单 tab stop、终端 tab 方向键、路由崩溃恢复、reduced-motion 媒体查询。
 *   Bundle 合同只走 build-time checker，不在此做时序脆弱的体积计时。
 *
 * Code Logic（这个套件做什么）:
 *   用 page.addInitScript 注入 Tauri invoke 假后端与 onboarded 标记；覆盖六条 smoke 路径；
 *   路由崩溃夹具仅 DEV 路由 `/__cp_route_error_fixture` + sessionStorage 开关。
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
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   多数 foundation smoke 只需跳过 onboarding 并提供稳定的空数据后端。
 *
 * Code Logic（这个函数做什么）:
 *   写入 onboarded/lang/theme，并 mock 常用 invoke；可选注入 projects/sessions。
 */
async function installFoundationMocks(
  page: Page,
  options: {
    projects?: Array<Record<string, unknown>>;
    sessions?: Array<Record<string, unknown>>;
    attentionItems?: Array<Record<string, unknown>>;
  } = {},
): Promise<void> {
  await page.addInitScript((opts) => {
    window.localStorage.setItem('cp-permission-onboarded', '1');
    window.localStorage.setItem('cp-lang', 'zh');
    window.localStorage.setItem('cp-theme', 'light');

    const projects = Array.isArray(opts.projects) ? opts.projects : [];
    const sessions = Array.isArray(opts.sessions) ? opts.sessions : [];
    const attentionItems = Array.isArray(opts.attentionItems) ? opts.attentionItems : [];
    const attentionSnapshot = {
      generatedAt: '2026-07-14T00:00:00.000Z',
      counts: {
        total: attentionItems.length,
        decision: attentionItems.filter((item) => item.category === 'decision').length,
        blocked: attentionItems.filter((item) => item.category === 'blocked').length,
        environment: attentionItems.filter((item) => item.category === 'environment').length,
      },
      items: attentionItems,
    };

    const baseConfig = {
      deviceId: 'device-1',
      deviceName: 'Hans-Mac',
      receiveDir: '/tmp/files',
      screenshotHotkey: '<cmd>+<shift>+s',
      promptOptimizerHotkey: '<ctrl>',
      promptOptimizerFillLanguage: 'zh',
      httpPort: 0,
    };

    let callbackId = 0;
    window.__TAURI_INTERNALS__ = {
      metadata: {
        currentWindow: { label: 'main' },
        currentWebview: { windowLabel: 'main', label: 'main' },
      },
      invoke: async (cmd: string) => {
        if (cmd === 'plugin:event|listen') return 1;
        if (cmd === 'plugin:event|unlisten') return undefined;
        if (cmd === 'get_version') return { version: '0.0.0-test', buildDate: '2026-07-14' };
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
        if (cmd === 'list_workbench_projects') return projects;
        if (cmd === 'list_workbench_sessions') return sessions;
        if (cmd === 'list_workbench_worktrees') {
          return projects.length > 0
            ? [
                {
                  id: 'wt-main',
                  projectId: projects[0].id,
                  name: 'main',
                  branch: 'main',
                  baseBranch: null,
                  path: projects[0].path,
                  isMain: true,
                  status: {
                    branch: 'main',
                    clean: true,
                    changed: 0,
                    conflicts: 0,
                    ahead: 0,
                    behind: 0,
                    canPush: false,
                  },
                  createdAt: '2026-07-14T00:00:00.000Z',
                  updatedAt: '2026-07-14T00:00:00.000Z',
                },
              ]
            : [];
        }
        if (cmd === 'get_focused_workbench_session') {
          return sessions[0]?.id ?? null;
        }
        if (cmd === 'focus_workbench_session') return undefined;
        if (
          cmd === 'list_workbench_dir' ||
          cmd === 'list_workbench_files' ||
          cmd === 'list_workbench_git_commits'
        ) {
          return [];
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
        if (cmd === 'list_attention_items') return attentionSnapshot;
        if (cmd === 'get_config' || cmd === 'get_default_config') return baseConfig;
        if (cmd === 'get_cloud_sync_config' || cmd === 'get_default_cloud_sync_config') {
          return {
            repoUrl: null,
            enabled: false,
            auto: false,
            intervalSecs: 300,
            branch: 'main',
          };
        }
        if (
          cmd === 'get_github_trending_config' ||
          cmd === 'get_default_github_trending_config'
        ) {
          return {
            aiEnabled: false,
            claudeCliPath: 'claude',
            claudeModel: 'sonnet',
            cacheTtlHours: 24,
          };
        }
        if (cmd === 'get_health_config' || cmd === 'get_default_health_config') {
          return {
            enabled: false,
            workWindowSeconds: 1800,
            breakSeconds: 300,
            recordWindowTitle: true,
            retainDays: 14,
            notifyEnabled: true,
            dndStart: null,
            dndEnd: null,
            waterEnabled: false,
            waterIntervalSeconds: 3600,
            reminderFullscreen: true,
          };
        }
        if (cmd === 'get_orchestrator_config' || cmd === 'get_default_orchestrator_config') {
          return {
            enabled: false,
            maxConcurrentTasks: 2,
            verificationCommands: ['npm test'],
            autoCommit: false,
            autoPushTaskBranch: false,
            autoMergeToMain: false,
            autoPushMain: false,
          };
        }
        if (cmd === 'get_update_download_status') {
          return {
            status: 'idle',
            progress: 0,
            error: '',
            filePath: '',
            url: '',
            filename: '',
            size: 0,
          };
        }
        if (cmd === 'list_github_trending_repos') {
          return { repos: [], cached: true, generatedAt: null };
        }
        if (cmd === 'list_devices') return [];
        if (cmd === 'list_transfers') return [];
        if (cmd === 'get_mobile_access_info') {
          return {
            urls: ['http://127.0.0.1:62116/mobile'],
            primaryUrl: 'http://127.0.0.1:62116/mobile',
            httpPort: 62116,
            lanIps: ['127.0.0.1'],
          };
        }
        if (cmd === 'check_lan_firewall_dependency') {
          return {
            platform: 'macos',
            platformLabel: 'macOS',
            lanIp: '192.168.1.10',
            httpPort: 62116,
            mdnsPort: 5353,
            appPath: null,
            checks: [
              { id: 'httpListener', ok: true, detail: 'TCP 62116' },
              { id: 'lanIp', ok: true, detail: '192.168.1.10' },
              { id: 'tcpFirewall', ok: true, detail: 'TCP 62116' },
              { id: 'mdnsFirewall', ok: true, detail: 'UDP 5353' },
            ],
            guidance: {
              summaryKey: 'settings:lanFirewall.guidance.macos.summary',
              steps: [],
              commands: [],
            },
          };
        }
        if (cmd === 'get_runtime_diagnostics') {
          return {
            ownerInstanceId: 'owner-test',
            generation: 1,
            startedAt: '2026-07-14T00:00:00Z',
            configFingerprint: 'fp-test',
            cloudSyncPhase: 'idle',
            terminalSessionCount: 0,
            bridgeCount: 0,
            bridges: [],
            orchestrator: { latestTickAt: null, latestErrorClass: null },
          };
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
  }, options);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench terminal tab 与 Attention 行测试需要最小合法项目/会话 DTO。
 *
 * Code Logic（这个函数做什么）:
 *   返回固定 id 的 local project 与两个 running session。
 */
function buildWorkbenchFixtures(): {
  project: Record<string, unknown>;
  sessions: Array<Record<string, unknown>>;
} {
  const project = {
    id: 'proj-1',
    name: 'Demo',
    kind: 'local',
    deviceId: 'device-1',
    deviceName: 'Hans-Mac',
    path: '/tmp/demo',
    lastOpenedAt: '2026-07-14T00:00:00.000Z',
    createdAt: '2026-07-14T00:00:00.000Z',
    updatedAt: '2026-07-14T00:00:00.000Z',
  };
  const sessions = [
    {
      id: 'sess-1',
      projectId: 'proj-1',
      worktreeId: 'wt-main',
      name: 'Window 1',
      command: 'zsh',
      cwd: '/tmp/demo',
      status: 'running',
      cols: 80,
      rows: 24,
      startedAt: '2026-07-14T00:00:00.000Z',
      exitedAt: null,
      exitCode: null,
      supportsPanes: true,
      paneCount: 1,
    },
    {
      id: 'sess-2',
      projectId: 'proj-1',
      worktreeId: 'wt-main',
      name: 'Window 2',
      command: 'zsh',
      cwd: '/tmp/demo',
      status: 'running',
      cols: 80,
      rows: 24,
      startedAt: '2026-07-14T00:01:00.000Z',
      exitedAt: null,
      exitCode: null,
      supportsPanes: true,
      paneCount: 1,
    },
  ];
  return { project, sessions };
}

test.describe('frontend foundation smoke', () => {
  test('dialog traps focus, Tab loops, Escape restores trigger', async ({ page }) => {
    await installFoundationMocks(page);
    await page.goto('/');
    const trigger = page.getByRole('button', { name: '添加项目' });
    await expect(trigger).toBeVisible({ timeout: 15_000 });
    await trigger.click();

    const dialog = page.getByRole('dialog', { name: '添加项目' });
    await expect(dialog).toBeVisible();

    const firstOption = dialog.getByRole('button').first();
    await expect(firstOption).toBeFocused();

    await page.keyboard.press('Tab');
    await page.keyboard.press('Tab');
    // 焦点仍在 dialog 内（循环 trap）
    await expect
      .poll(async () => dialog.locator(':focus').count(), { timeout: 3_000 })
      .toBeGreaterThan(0);

    await page.keyboard.press('Escape');
    await expect(dialog).toHaveCount(0);
    await expect(trigger).toBeFocused();
  });

  test('mobile nav drawer closes on Escape', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    // mobile 入口走 HTTP：stub 关键 API 避免 404 触发 console.error 守卫
    // 仅拦截同源后端 `/api/*`，避免误伤 Vite `/src/api/*` 模块请求
    await page.route('http://127.0.0.1:5173/api/**', async (route) => {
      const url = route.request().url();
      if (url.includes('/api/health')) {
        await route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify({
            status: 'ok',
            protocol_version: 1,
            capabilities: ['attention.v1', 'orchestrator.runtime-snapshot.v1'],
          }),
        });
        return;
      }
      if (url.includes('/api/mobile/workbench/projects')) {
        await route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify([]),
        });
        return;
      }
      if (url.includes('/api/mobile/attention') || url.includes('/api/workbench')) {
        await route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify({
            generatedAt: '2026-07-14T00:00:00.000Z',
            counts: { total: 0, decision: 0, blocked: 0, environment: 0 },
            items: [],
          }),
        });
        return;
      }
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({}),
      });
    });
    await page.addInitScript(() => {
      window.localStorage.setItem('cp-lang', 'zh');
      window.localStorage.setItem('cp-theme', 'light');
    });
    await page.goto('/mobile');
    const openNav = page.getByRole('button', { name: /打开导航|Open navigation/i });
    await expect(openNav).toBeVisible({ timeout: 20_000 });
    await openNav.click();

    const drawer = page.getByRole('dialog');
    await expect(drawer).toBeVisible({ timeout: 10_000 });
    await page.keyboard.press('Escape');
    await expect(drawer).toHaveCount(0);
    await expect(openNav).toBeFocused();
  });

  test('attention rows expose a single tab stop per item', async ({ page }) => {
    const attentionItems = [
      {
        id: 'orchestrator:human-review:task-1',
        category: 'decision',
        sourceKind: 'orchestratorHumanReview',
        title: 'Review delivery',
        summary: 'Need human review',
        updatedAt: '2026-07-14T12:00:00.000Z',
        freshness: 'live',
        cachedAt: null,
        project: { id: 'proj-1', name: 'Demo', kind: 'local' },
        device: null,
        target: { kind: 'orchestratorTask', projectId: 'proj-1', taskId: 'task-1' },
      },
      {
        id: 'orchestrator:blocked:task-2',
        category: 'blocked',
        sourceKind: 'orchestratorBlocked',
        title: 'Blocked task',
        summary: 'Waiting on dependency',
        updatedAt: '2026-07-14T12:05:00.000Z',
        freshness: 'live',
        cachedAt: null,
        project: { id: 'proj-1', name: 'Demo', kind: 'local' },
        device: null,
        target: { kind: 'orchestratorTask', projectId: 'proj-1', taskId: 'task-2' },
      },
    ];
    await installFoundationMocks(page, { attentionItems });
    await page.goto('/attention');

    const first = page.getByTestId('attention-item-orchestrator:human-review:task-1');
    const second = page.getByTestId('attention-item-orchestrator:blocked:task-2');
    await expect(first).toBeVisible({ timeout: 15_000 });
    await expect(second).toBeVisible();

    // 每行仅一个 button；动作文案是内部 span，不可单独 tab
    await expect(first.locator('button')).toHaveCount(0);
    await expect(first).toHaveRole('button');
    await first.focus();
    await page.keyboard.press('Tab');
    await expect(second).toBeFocused();
  });

  test('workbench terminal tabs support arrow key roving', async ({ page }) => {
    const { project, sessions } = buildWorkbenchFixtures();
    await installFoundationMocks(page, { projects: [project], sessions });
    await page.addInitScript(() => {
      window.localStorage.setItem('cp-workbench-active-project-id', 'proj-1');
    });
    await page.goto('/workbench?projectId=proj-1');

    const tablist = page.getByRole('tablist', { name: '终端会话' });
    await expect(tablist).toBeVisible({ timeout: 20_000 });
    const tabs = tablist.getByRole('tab');
    await expect(tabs).toHaveCount(2);

    await tabs.nth(0).focus();
    await expect(tabs.nth(0)).toBeFocused();
    await page.keyboard.press('ArrowRight');
    await expect(tabs.nth(1)).toBeFocused();
    await expect(tabs.nth(1)).toHaveAttribute('aria-selected', 'true');
    await page.keyboard.press('Home');
    await expect(tabs.nth(0)).toBeFocused();
  });

  test('route crash fixture recovers via retry after clearing force flag', async ({ page }) => {
    await installFoundationMocks(page);
    // Error boundary 会 console.error 一次；本用例允许夹具错误，避免 auto fixture 误杀
    await page.addInitScript(() => {
      sessionStorage.setItem('cp-force-route-error', '1');
      const originalError = console.error.bind(console);
      console.error = (...args: unknown[]) => {
        const text = args.map((part) => String(part)).join(' ');
        if (text.includes('cp-force-route-error')) return;
        originalError(...args);
      };
    });
    await page.goto('/__cp_route_error_fixture');

    await expect(page.getByText('页面出错了')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId('route-error-fixture-ok')).toHaveCount(0);

    // 侧栏仍可用（shell 未被拖垮）
    await expect(page.getByRole('navigation').first()).toBeVisible();

    await page.evaluate(() => {
      sessionStorage.removeItem('cp-force-route-error');
    });
    await page.getByRole('button', { name: '重试当前页' }).click();
    await expect(page.getByTestId('route-error-fixture-ok')).toBeVisible({ timeout: 10_000 });
  });

  test('prefers-reduced-motion zeros animation and transition durations', async ({ page }) => {
    await page.emulateMedia({ reducedMotion: 'reduce' });
    await installFoundationMocks(page);
    await page.goto('/');
    await expect(page.getByRole('navigation').first()).toBeVisible({ timeout: 15_000 });

    const metrics = await page.evaluate(() => {
      // 不写 inline animation，避免覆盖 stylesheet 的 media 规则；读全局规则是否生效
      const probe = document.createElement('div');
      probe.className = 'menuButton';
      document.body.appendChild(probe);
      // 注入一次性 stylesheet 用 class 绑定 1s transition，再看 reduce 是否压短
      const styleEl = document.createElement('style');
      styleEl.textContent = `
        .cp-foundation-motion-probe {
          transition: opacity 1s linear;
          animation: cp-foundation-probe 1s ease infinite;
        }
        @keyframes cp-foundation-probe { from { opacity: 1; } to { opacity: 0.5; } }
      `;
      document.head.appendChild(styleEl);
      probe.classList.add('cp-foundation-motion-probe');
      const style = getComputedStyle(probe);
      const result = {
        animationDuration: style.animationDuration,
        transitionDuration: style.transitionDuration,
        matchMedia: window.matchMedia('(prefers-reduced-motion: reduce)').matches,
      };
      probe.remove();
      styleEl.remove();
      return result;
    });

    expect(metrics.matchMedia).toBe(true);
    // globals.css 在 reduce 下将 duration 压到 0.01ms；浏览器可能序列化为 "0.01ms" 或 "0s"
    const toMs = (value: string): number => {
      if (value.endsWith('ms')) return parseFloat(value);
      if (value.endsWith('s')) return parseFloat(value) * 1000;
      return Number.POSITIVE_INFINITY;
    };
    expect(toMs(metrics.animationDuration)).toBeLessThan(1);
    expect(toMs(metrics.transitionDuration)).toBeLessThan(1);
  });

  test('unacknowledged first launch shows LAN disclosure and blocks shell until confirm', async ({
    page,
  }) => {
    await page.addInitScript(() => {
      window.localStorage.setItem('cp-permission-onboarded', '1');
      window.localStorage.setItem('cp-lang', 'zh');
      window.localStorage.setItem('cp-theme', 'light');

      let required = true;
      let callbackId = 0;
      window.__TAURI_INTERNALS__ = {
        metadata: {
          currentWindow: { label: 'main' },
          currentWebview: { windowLabel: 'main', label: 'main' },
        },
        invoke: async (cmd: string) => {
          if (cmd === 'plugin:event|listen') return 1;
          if (cmd === 'plugin:event|unlisten') return undefined;
          if (cmd === 'get_lan_disclosure_status') {
            return {
              required,
              version: 1,
              localAddresses: ['192.168.1.10'],
              preferredPort: 62116,
              mdnsPort: 5353,
              alreadyRunning: false,
              actualHttpPort: null,
            };
          }
          if (cmd === 'acknowledge_lan_disclosure_and_start_backend') {
            required = false;
            return {
              actualHttpPort: 62116,
              localAddresses: ['192.168.1.10'],
              reusedExisting: false,
              version: 1,
            };
          }
          if (cmd === 'check_permissions') {
            return {
              screenCapture: { granted: true },
              accessibility: { granted: true },
              inputMonitoring: { granted: true },
            };
          }
          if (cmd === 'get_version') return { version: '0.0.0-test', buildDate: '2026-07-14' };
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
          if (cmd === 'list_github_trending_repos') {
            return { repos: [], cached: true, generatedAt: null };
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

    await page.goto('/');
    await expect(page.getByTestId('lan-disclosure-gate')).toBeVisible({ timeout: 15_000 });
    await expect(
      page.getByText('同一可达网络任意设备均可读写执行，系统不验证调用者身份'),
    ).toBeVisible();
    await expect(page.getByText('首选 TCP 端口 62116')).toBeVisible();
    await expect(page.getByText(/mDNS UDP 5353/)).toBeVisible();
    // 未确认前不应进入主壳
    await expect(page.getByRole('navigation')).toHaveCount(0);

    await page.getByTestId('lan-disclosure-acknowledge').click();
    await expect(page.getByTestId('lan-disclosure-gate')).toHaveCount(0, { timeout: 15_000 });
  });

  test('sidebar groups routes and content/footer do not overlap at 1280x720', async ({ page }) => {
    await page.setViewportSize({ width: 1280, height: 720 });
    await installFoundationMocks(page);
    await page.goto('/');

    const nav = page.getByRole('navigation', { name: '主导航' });
    await expect(nav).toBeVisible({ timeout: 15_000 });

    // 分组标签可见且不可聚焦
    for (const label of ['探索', '工作', '知识', '连接', '系统']) {
      await expect(page.getByText(label, { exact: true }).first()).toBeVisible();
    }

    const hrefs = await nav.locator('a[href]').evaluateAll((anchors) =>
      anchors.map((anchor) => {
        const href = anchor.getAttribute('href') ?? '';
        try {
          return new URL(href, 'http://local.test').pathname;
        } catch {
          return href;
        }
      }),
    );
    expect(hrefs[0]).toBe('/');
    expect(hrefs).toContain('/workbench');
    expect(hrefs).toContain('/attention');
    expect(hrefs).toContain('/settings');
    expect(hrefs.filter((href) => href === '/discover')).toHaveLength(0);

    // 每条主导航链接是一个 tab stop（无 tabindex=-1）
    const badTabStops = await nav.locator('a[href]').evaluateAll((anchors) =>
      anchors
        .map((anchor) => ({
          href: anchor.getAttribute('href'),
          tabIndex: (anchor as HTMLElement).tabIndex,
        }))
        .filter((item) => item.tabIndex < 0),
    );
    expect(badTabStops).toEqual([]);

    const layout = await page.evaluate(() => {
      const aside = document.querySelector('aside');
      if (!aside) return null;
      const content = aside.querySelector(':scope > div:first-child') as HTMLElement | null;
      const footer = aside.querySelector(':scope > div:last-child') as HTMLElement | null;
      if (!content || !footer || content === footer) return null;
      const contentBox = content.getBoundingClientRect();
      const footerBox = footer.getBoundingClientRect();
      const contentStyle = getComputedStyle(content);
      return {
        contentBottom: contentBox.bottom,
        footerTop: footerBox.top,
        contentOverflowY: contentStyle.overflowY,
        contentMinHeight: contentStyle.minHeight,
      };
    });

    expect(layout).not.toBeNull();
    expect(layout!.footerTop).toBeGreaterThanOrEqual(layout!.contentBottom - 1);
    expect(['auto', 'scroll']).toContain(layout!.contentOverflowY);
    expect(layout!.contentMinHeight === '0px' || layout!.contentMinHeight === '0').toBe(true);
  });

  for (const viewport of [
    { width: 1024, height: 768, name: '1024x768' },
    { width: 1280, height: 720, name: '1280x720' },
  ] as const) {
    test(`home layout has no horizontal overflow at ${viewport.name}`, async ({ page }, testInfo) => {
      await page.setViewportSize({ width: viewport.width, height: viewport.height });
      await installFoundationMocks(page);
      await page.goto('/');

      const nav = page.getByRole('navigation', { name: '主导航' });
      await expect(nav).toBeVisible({ timeout: 15_000 });

      // 主导航与「继续工作」入口可键盘到达
      await expect(nav.locator('a[href="/workbench"]').first()).toBeVisible();
      const workbenchTabIndex = await nav
        .locator('a[href="/workbench"]')
        .first()
        .evaluate((el) => (el as HTMLElement).tabIndex);
      expect(workbenchTabIndex).toBeGreaterThanOrEqual(0);

      const metrics = await page.evaluate(() => {
        const doc = document.documentElement;
        const body = document.body;
        return {
          scrollWidth: Math.max(doc.scrollWidth, body.scrollWidth),
          clientWidth: doc.clientWidth,
        };
      });
      expect(metrics.scrollWidth).toBeLessThanOrEqual(metrics.clientWidth + 1);

      const shotPath = testInfo.outputPath(`layout-home-${viewport.name}.png`);
      await page.screenshot({ path: shotPath, fullPage: true });
      await testInfo.attach(`layout-home-${viewport.name}`, {
        path: shotPath,
        contentType: 'image/png',
      });
    });
  }

  test('settings deep-linked tab scrolls inside tablist at 680px', async ({ page }) => {
    await page.setViewportSize({ width: 680, height: 900 });
    await installFoundationMocks(page);
    await page.goto('/settings?tab=dependencies');

    const tablist = page.getByRole('tablist');
    await expect(tablist).toBeVisible({ timeout: 15_000 });
    const active = tablist.getByRole('tab', { selected: true });
    await expect(active).toBeVisible();
    await expect(active).toHaveAttribute('id', /dependencies|settings-tab-dependencies/);

    // shell 在 rAF + smooth scroll 后把深链 tab 滚进 tablist 视口
    await expect
      .poll(
        async () =>
          active.evaluate((el) => {
            const tab = el as HTMLElement;
            const list = tab.closest('[role="tablist"]') as HTMLElement | null;
            if (!list) return false;
            const tabBox = tab.getBoundingClientRect();
            const listBox = list.getBoundingClientRect();
            return tabBox.left >= listBox.left - 2 && tabBox.right <= listBox.right + 2;
          }),
        { timeout: 5_000 },
      )
      .toBe(true);
  });
});
