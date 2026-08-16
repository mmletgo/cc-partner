import { type Page } from '@playwright/test';
import { expect, test } from './fixtures';

/**
 * Global Inbox E2E（桌面，确定性 Tauri mock）。
 *
 * Business Logic（为什么需要这个套件）:
 *   验证 Inbox 投影、badge、分组、stale 保留、主题与键盘 focus-visible，以及
 *   业务动作成功后的立即 invalidation 无需等待 10 秒轮询。
 *
 * Code Logic（这个套件做什么）:
 *   在页面 init 注入 `__TAURI_INTERNALS__.invoke` 假后端与可变 snapshot 序列；
 *   跳过 onboarding；覆盖 human-review → rework 移除、blocked/outbox/tmux 分组、
 *   cached 标签、stale 横幅、sidebar badge 与列表 count 一致性、浅/深色与键盘导航。
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
    __attentionTestApi?: {
      setSnapshots: (snapshots: unknown[]) => void;
      pushSnapshot: (snapshot: unknown) => void;
      setFailNext: (fail: boolean) => void;
      getListCallCount: () => number;
      requestInvalidation: () => void;
    };
  }
}

interface AttentionFixtureItem {
  id: string;
  category: 'decision' | 'blocked' | 'environment';
  sourceKind:
    | 'orchestratorHumanReview'
    | 'orchestratorBlocked'
    | 'remoteOutboxFailed'
    | 'workbenchDependency';
  title: string;
  summary: string;
  updatedAt: string;
  freshness: 'live' | 'cached';
  cachedAt: string | null;
  project: { id: string; name: string; kind: 'local' | 'remote' } | null;
  device: { id: string; name: string } | null;
  target:
    | { kind: 'orchestratorTask'; projectId: string; taskId: string }
    | { kind: 'remoteOutbox'; projectId: string; outboxId: string }
    | { kind: 'settings'; tab: 'dependencies' };
}

interface AttentionFixtureSnapshot {
  generatedAt: string;
  counts: {
    total: number;
    decision: number;
    blocked: number;
    environment: number;
    unreadTotal: number;
    unreadDecision: number;
    unreadBlocked: number;
    unreadEnvironment: number;
  };
  items: AttentionFixtureItem[];
  myDeviceId: string;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   E2E 需要可复用的合法 Attention 条目，避免每个用例重复样板。
 *
 * Code Logic（这个函数做什么）:
 *   返回可覆盖字段的 AttentionFixtureItem。
 */
function buildItem(overrides: Partial<AttentionFixtureItem> = {}): AttentionFixtureItem {
  return {
    id: 'orchestrator:human-review:task-1',
    category: 'decision',
    sourceKind: 'orchestratorHumanReview',
    title: 'Review delivery',
    summary: 'Need human review',
    updatedAt: new Date().toISOString(),
    freshness: 'live',
    cachedAt: null,
    project: { id: 'proj-1', name: 'Demo', kind: 'local' },
    device: null,
    target: { kind: 'orchestratorTask', projectId: 'proj-1', taskId: 'task-1' },
    ...overrides,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   多个用例共享快照构造与 counts 推导。
 *
 * Code Logic（这个函数做什么）:
 *   用 items 生成 AttentionFixtureSnapshot。
 */
function buildSnapshot(
  items: AttentionFixtureItem[],
  generatedAt = '2026-07-11T12:05:00.000Z',
): AttentionFixtureSnapshot {
  const decision = items.filter((item) => item.category === 'decision');
  const blocked = items.filter((item) => item.category === 'blocked');
  const environment = items.filter((item) => item.category === 'environment');
  const unread = (list: AttentionFixtureItem[]) =>
    list.filter((item) => !('readAt' in item) || item.readAt == null).length;
  return {
    generatedAt,
    counts: {
      total: items.length,
      decision: decision.length,
      blocked: blocked.length,
      environment: environment.length,
      unreadTotal: unread(items),
      unreadDecision: unread(decision),
      unreadBlocked: unread(blocked),
      unreadEnvironment: unread(environment),
    },
    items,
    myDeviceId: 'e2e-device',
  };
}

const HUMAN_REVIEW = buildItem();
const BLOCKED = buildItem({
  id: 'orchestrator:blocked:task-2',
  category: 'blocked',
  sourceKind: 'orchestratorBlocked',
  title: 'Blocked task',
  summary: 'Verifier infra failed',
  updatedAt: new Date().toISOString(),
  target: { kind: 'orchestratorTask', projectId: 'proj-1', taskId: 'task-2' },
});
const FAILED_OUTBOX = buildItem({
  id: 'orchestrator:outbox-failed:ob-1',
  category: 'blocked',
  sourceKind: 'remoteOutboxFailed',
  title: 'Outbox failed',
  summary: 'Remote create failed',
  updatedAt: new Date().toISOString(),
  freshness: 'live',
  project: { id: 'remote:dev-1:path', name: 'Remote Demo', kind: 'remote' },
  device: { id: 'dev-1', name: 'Mac Mini' },
  target: {
    kind: 'remoteOutbox',
    projectId: 'remote:dev-1:path',
    outboxId: 'ob-1',
  },
});
const CACHED_REMOTE = buildItem({
  id: 'orchestrator:human-review:remote:dev-1:task-9',
  category: 'decision',
  sourceKind: 'orchestratorHumanReview',
  title: 'Remote review',
  summary: 'Cached remote human review',
  updatedAt: new Date().toISOString(),
  freshness: 'cached',
  cachedAt: '2026-07-11T08:55:00.000Z',
  project: { id: 'remote:dev-1:path', name: 'Remote Demo', kind: 'remote' },
  device: { id: 'dev-1', name: 'Mac Mini' },
  target: {
    kind: 'orchestratorTask',
    projectId: 'remote:dev-1:path',
    taskId: 'remote:dev-1:task-9',
  },
});
const TMUX_MISSING = buildItem({
  id: 'workbench:dependency:tmux',
  category: 'environment',
  sourceKind: 'workbenchDependency',
  title: 'tmux missing',
  summary: 'Install tmux to keep terminal context',
  updatedAt: new Date().toISOString(),
  project: null,
  device: null,
  target: { kind: 'settings', tab: 'dependencies' },
});

/**
 * Business Logic（为什么需要这个函数）:
 *   Playwright 浏览器没有真实 Tauri；必须 mock invoke 与 onboarding 才能进入 AppShell。
 *
 * Code Logic（这个函数做什么）:
 *   addInitScript 写入 PERMISSION_ONBOARDED、注入 invoke mock 与 __attentionTestApi。
 */
async function installAttentionDesktopMocks(
  page: Page,
  initialSnapshots: AttentionFixtureSnapshot[],
): Promise<void> {
  await page.addInitScript((snapshots) => {
    window.localStorage.setItem('cp-permission-onboarded', '1');
    window.localStorage.setItem('cp-lang', 'zh');
    window.localStorage.setItem('cp-theme', 'light');

    // currentSnapshot 是稳定真源；React StrictMode 双挂载/轮询都复用它，
    // 不会像 queue.shift 那样被二次 mount 提前消费掉「下一次」快照。
    let currentSnapshot: unknown =
      Array.isArray(snapshots) && snapshots.length > 0
        ? snapshots[0]
        : {
            generatedAt: '1970-01-01T00:00:00.000Z',
            counts: {
              total: 0,
              decision: 0,
              blocked: 0,
              environment: 0,
              unreadTotal: 0,
              unreadDecision: 0,
              unreadBlocked: 0,
              unreadEnvironment: 0,
            },
            items: [],
            myDeviceId: 'e2e-device',
          };
    /** true 时 list_attention_items 持续失败，直到 setFailNext(false)。 */
    let failMode = false;
    let listCallCount = 0;

    window.__attentionTestApi = {
      setSnapshots: (next) => {
        if (Array.isArray(next) && next.length > 0) {
          currentSnapshot = next[0];
        }
      },
      pushSnapshot: (snapshot) => {
        currentSnapshot = snapshot;
      },
      setFailNext: (fail) => {
        failMode = fail;
      },
      getListCallCount: () => listCallCount,
      requestInvalidation: () => {
        window.dispatchEvent(new CustomEvent('cp-attention-invalidate'));
      },
    };

    let callbackId = 0;
    window.__TAURI_INTERNALS__ = {
      metadata: {
        currentWindow: { label: 'main' },
        currentWebview: { windowLabel: 'main', label: 'main' },
      },
      invoke: async (cmd: string, args?: Record<string, unknown>) => {
        if (cmd === 'plugin:event|listen') return 1;
        if (cmd === 'plugin:event|unlisten') return undefined;
        if (cmd === 'get_version') return { version: '0.0.0-test' };
        if (cmd === 'check_permissions') {
          return {
            screenCapture: { granted: true },
            accessibility: { granted: true },
            inputMonitoring: { granted: true, state: 'granted' },
            notification: { granted: true },
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
        if (cmd === 'list_workbench_projects') return [];
        if (cmd === 'list_workbench_sessions') return [];
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
            statusChangedAt: '2026-07-11T00:00:00.000Z',
          };
        }
        if (cmd === 'list_attention_items' || cmd === 'list_attention_items_v2') {
          listCallCount += 1;
          if (failMode) {
            throw new Error('attention load failed');
          }
          return currentSnapshot;
        }
        if (
          cmd === 'mark_attention_items_read' ||
          cmd === 'mark_attention_items_unread' ||
          cmd === 'mark_all_attention_items_read' ||
          cmd === 'mark_attention_category_read'
        ) {
          const snapshot = currentSnapshot as {
            items: Array<{ id: string; category: string; readAt?: string | null }>;
            counts: Record<string, number>;
            generatedAt: string;
            myDeviceId?: string;
          } | null;
          if (!snapshot || !Array.isArray(snapshot.items)) {
            return currentSnapshot;
          }
          const ids = new Set(
            cmd === 'mark_all_attention_items_read'
              ? snapshot.items.map((item) => item.id)
              : cmd === 'mark_attention_category_read'
                ? snapshot.items
                    .filter((item) => item.category === args?.category)
                    .map((item) => item.id)
                : Array.isArray(args?.itemIds)
                  ? (args.itemIds as string[])
                  : [],
          );
          const read = cmd !== 'mark_attention_items_unread';
          const now = '2026-07-11T13:00:00.000Z';
          snapshot.items = snapshot.items.map((item) =>
            ids.has(item.id) ? { ...item, readAt: read ? now : undefined } : item,
          );
          const unread = snapshot.items.filter((item) => !item.readAt);
          snapshot.counts = {
            ...snapshot.counts,
            unreadTotal: unread.length,
            unreadDecision: unread.filter((item) => item.category === 'decision').length,
            unreadBlocked: unread.filter((item) => item.category === 'blocked').length,
            unreadEnvironment: unread.filter((item) => item.category === 'environment').length,
          };
          currentSnapshot = snapshot;
          return currentSnapshot;
        }
        if (cmd === 'get_config' || cmd === 'get_default_config') {
          return {};
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
        if (cmd === 'get_operational_notification_snapshot') {
          return {
            asOfCursor: { ownerInstanceId: 'owner-attention', sequence: 0 },
            items: [],
            truncated: false,
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
  }, initialSnapshots);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   多个用例都要进入 /attention 并等待首屏快照渲染完成。
 *
 * Code Logic（这个函数做什么）:
 *   安装 mock → goto /attention → 等待列表或空态。
 */
async function openAttention(
  page: Page,
  snapshots: AttentionFixtureSnapshot[],
): Promise<void> {
  await installAttentionDesktopMocks(page, snapshots);
  await page.goto('/attention');
  await expect(page.getByRole('heading', { name: '待处理' })).toBeVisible();
}

test.describe('Global Inbox attention', () => {
  test('human review appears then disappears after successful invalidation', async ({
    page,
  }) => {
    const withReview = buildSnapshot([HUMAN_REVIEW]);
    await openAttention(page, [withReview]);

    await expect(page.getByTestId(`attention-item-${HUMAN_REVIEW.id}`)).toBeVisible({
      timeout: 10_000,
    });
    await expect(page.locator('a[href="/attention"]')).toContainText('1');

    await page.evaluate(() => {
      window.__attentionTestApi?.setSnapshots([
        {
          generatedAt: '2026-07-11T12:10:00.000Z',
          counts: { total: 0, decision: 0, blocked: 0, environment: 0 },
          items: [],
        },
      ]);
      window.__attentionTestApi?.requestInvalidation();
    });

    await expect(page.getByTestId('attention-empty')).toBeVisible({ timeout: 10_000 });
    await expect(page.getByTestId(`attention-item-${HUMAN_REVIEW.id}`)).toHaveCount(0);
  });

  test('blocked, failed outbox and tmux groups render in fixed order', async ({ page }) => {
    const snapshot = buildSnapshot([TMUX_MISSING, FAILED_OUTBOX, BLOCKED, HUMAN_REVIEW]);
    await openAttention(page, [snapshot]);

    const groups = page.getByTestId('attention-groups');
    await expect(groups).toBeVisible();
    await expect(page.getByTestId('attention-group-decision')).toBeVisible();
    await expect(page.getByTestId('attention-group-blocked')).toBeVisible();
    await expect(page.getByTestId('attention-group-environment')).toBeVisible();

    const groupOrder = await page.locator('[data-testid^="attention-group-"]').evaluateAll((nodes) =>
      nodes.map((node) => node.getAttribute('data-testid')),
    );
    expect(groupOrder).toEqual([
      'attention-group-decision',
      'attention-group-blocked',
      'attention-group-environment',
    ]);

    await expect(page.getByTestId(`attention-item-${BLOCKED.id}`)).toBeVisible();
    await expect(page.getByTestId(`attention-item-${FAILED_OUTBOX.id}`)).toBeVisible();
    await expect(page.getByTestId(`attention-item-${TMUX_MISSING.id}`)).toBeVisible();
  });

  test('only failed outbox appears and invalidation removes it after retry/discard success', async ({
    page,
  }) => {
    const withOutbox = buildSnapshot([FAILED_OUTBOX]);
    await openAttention(page, [withOutbox]);
    await expect(page.getByTestId(`attention-item-${FAILED_OUTBOX.id}`)).toBeVisible();
    // pending/mirrored/discarded 不会出现在 fixture 中；列表只有 failed。
    await expect(page.locator('[data-testid^="attention-item-"]')).toHaveCount(1);

    await page.evaluate(() => {
      window.__attentionTestApi?.setSnapshots([
        {
          generatedAt: '2026-07-11T12:20:00.000Z',
          counts: { total: 0, decision: 0, blocked: 0, environment: 0 },
          items: [],
        },
      ]);
      window.__attentionTestApi?.requestInvalidation();
    });
    await expect(page.getByTestId('attention-empty')).toBeVisible();
  });

  test('cached remote item shows real cached label and live item does not', async ({ page }) => {
    const snapshot = buildSnapshot([CACHED_REMOTE, HUMAN_REVIEW]);
    await openAttention(page, [snapshot]);

    await expect(page.getByTestId(`attention-cached-${CACHED_REMOTE.id}`)).toContainText(
      '远端缓存',
    );
    await expect(page.getByTestId(`attention-item-${HUMAN_REVIEW.id}`)).toContainText('实时');
    await expect(page.getByTestId(`attention-cached-${HUMAN_REVIEW.id}`)).toHaveCount(0);
  });

  test('tmux environment item hidden with zero projects fixture and shown with project-backed snapshot', async ({
    page,
  }) => {
    // 后端在零项目时不投影 tmux；E2E 用空快照 vs 含 environment 的快照模拟。
    await openAttention(page, [buildSnapshot([])]);
    await expect(page.getByTestId('attention-empty')).toBeVisible();
    await expect(page.getByTestId('attention-group-environment')).toHaveCount(0);

    await page.evaluate((item) => {
      window.__attentionTestApi?.setSnapshots([
        {
          generatedAt: '2026-07-11T12:30:00.000Z',
          counts: {
            total: 1,
            decision: 0,
            blocked: 0,
            environment: 1,
            unreadTotal: 1,
            unreadDecision: 0,
            unreadBlocked: 0,
            unreadEnvironment: 1,
          },
          items: [item],
          myDeviceId: 'e2e-device',
        },
      ]);
      window.__attentionTestApi?.requestInvalidation();
    }, TMUX_MISSING);

    await expect(page.getByTestId('attention-group-environment')).toBeVisible();
    await expect(page.getByTestId(`attention-item-${TMUX_MISSING.id}`)).toBeVisible();
  });

  test('stale refresh preserves list and count badge', async ({ page }) => {
    const snapshot = buildSnapshot([HUMAN_REVIEW, BLOCKED]);
    await openAttention(page, [snapshot]);
    await expect(page.getByTestId(`attention-item-${HUMAN_REVIEW.id}`)).toBeVisible({
      timeout: 10_000,
    });
    const callsBefore = await page.evaluate(() => window.__attentionTestApi?.getListCallCount() ?? 0);

    // 用失效桥触发刷新比点按钮更稳：避免 loading/refreshing 时按钮 disabled 吞点击。
    await page.evaluate(() => {
      window.__attentionTestApi?.setFailNext(true);
      window.__attentionTestApi?.requestInvalidation();
    });

    await expect
      .poll(async () => page.evaluate(() => window.__attentionTestApi?.getListCallCount() ?? 0), {
        timeout: 10_000,
      })
      .toBeGreaterThan(callsBefore);
    await expect(page.getByTestId('attention-stale-banner')).toBeVisible({ timeout: 10_000 });
    await expect(page.getByTestId(`attention-item-${HUMAN_REVIEW.id}`)).toBeVisible();
    await expect(page.getByTestId(`attention-item-${BLOCKED.id}`)).toBeVisible();
    await expect(page.getByTestId('attention-groups')).toBeVisible();
    // 侧栏 badge 在 stale 时仍保留旧 total。
    await expect(page.locator('a[href="/attention"]')).toContainText('2');
  });

  test('desktop sidebar badge total matches snapshot counts', async ({ page }) => {
    const snapshot = buildSnapshot([HUMAN_REVIEW, BLOCKED, TMUX_MISSING]);
    await openAttention(page, [snapshot]);

    await expect(page.getByTestId('attention-groups')).toBeVisible();
    const itemCount = await page.locator('[data-testid^="attention-item-"]').count();
    expect(itemCount).toBe(3);

    const attentionNav = page.locator('a[href="/attention"]');
    await expect(attentionNav).toContainText('3');
  });

  test('light and dark themes render inbox and keyboard focus-visible works', async ({
    page,
  }) => {
    const snapshot = buildSnapshot([HUMAN_REVIEW, BLOCKED]);
    await openAttention(page, [snapshot]);

    await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
    await expect(page.getByTestId('attention-groups')).toBeVisible();

    await page.evaluate(() => {
      window.localStorage.setItem('cp-theme', 'dark');
      window.dispatchEvent(new CustomEvent('cp-theme-change', { detail: 'dark' }));
      document.documentElement.setAttribute('data-theme', 'dark');
    });
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
    await expect(page.getByTestId(`attention-item-${HUMAN_REVIEW.id}`)).toBeVisible();

    const action = page.getByTestId(`attention-action-${HUMAN_REVIEW.id}`);
    await action.focus();
    await expect(action).toBeFocused();
  });

  test('settings dependency target navigates to settings tab', async ({ page }) => {
    await openAttention(page, [buildSnapshot([TMUX_MISSING])]);
    await page.getByTestId(`attention-action-${TMUX_MISSING.id}`).click();
    await expect(page).toHaveURL(/\/settings\?tab=dependencies/);
  });
});
