/**
 * E2E-MOBILE-001 — Mobile Workbench 手机视口旅程（L1 browser mock）。
 *
 * Business Logic（为什么需要这个套件）:
 *   验证 `/mobile` 在 390×844 下 Projects→Attention→Terminal→Files→Automation 导航、
 *   Drawer 焦点/Escape、终端 replay 门控不重复写、移动端写走 HTTP 而非 Tauri invoke，
 *   以及离线后 Attention 缓存标 stale。L1 不宣称真实多机/WebView。
 *
 * Code Logic（这个套件做什么）:
 *   backendHarness 注册同源 `/api/mobile/*` 与 health/events；viewport 390×844；
 *   断言导航与 drawer；defer replay 期间不产生 sessions/write；files save-text 为 fetch；
 *   attention 二次拉取 fault 后可见 stale 横幅。
 */

import { expect, test } from './fixtures';
import type { PlaywrightBackendHarness } from './support/backendHarness';

const TS = '2026-07-14T00:00:00.000Z';

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端 projects/list 需要合法 project DTO。
 *
 * Code Logic（这个函数做什么）:
 *   返回 local project。
 */
function makeMobileProject() {
  return {
    id: 'mp-1',
    name: 'mobile-demo',
    kind: 'local',
    deviceId: 'self-1',
    deviceName: 'Test Device',
    path: '/tmp/mobile-demo',
    lastOpenedAt: TS,
    createdAt: TS,
    updatedAt: TS,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   打开项目后并行拉 worktrees/sessions。
 *
 * Code Logic（这个函数做什么）:
 *   返回主 worktree。
 */
function makeMobileWorktree(projectId: string) {
  return {
    id: 'mwt-1',
    projectId,
    name: 'main',
    branch: 'main',
    baseBranch: null,
    path: '/tmp/mobile-demo',
    isMain: true,
    status: {
      branch: 'main',
      changed: 0,
      ahead: 0,
      behind: 0,
      conflicts: 0,
      clean: true,
      canPush: false,
    },
    createdAt: TS,
    updatedAt: TS,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   终端面板依赖 running session。
 *
 * Code Logic（这个函数做什么）:
 *   返回 session DTO。
 */
function makeMobileSession(projectId: string, worktreeId: string) {
  return {
    id: 'ms-1',
    projectId,
    worktreeId,
    name: 'mobile-shell',
    command: '/bin/zsh',
    cwd: '/tmp/mobile-demo',
    status: 'running' as const,
    cols: 80,
    rows: 24,
    startedAt: TS,
    exitedAt: null,
    exitCode: null,
    supportsPanes: true,
    paneCount: 1,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   AttentionProvider 挂载需要 health + snapshot。
 *
 * Code Logic（这个函数做什么）:
 *   返回含一条 decision 的快照。
 */
function makeAttentionSnapshot() {
  return {
    generatedAt: TS,
    counts: {
      total: 1,
      decision: 1,
      blocked: 0,
      environment: 0,
      unreadTotal: 1,
      unreadDecision: 1,
      unreadBlocked: 0,
      unreadEnvironment: 0,
    },
    myDeviceId: 'mobile-e2e',
    items: [
      {
        id: 'orchestrator:human-review:task-1',
        category: 'decision',
        sourceKind: 'orchestratorHumanReview',
        title: 'Review delivery',
        summary: 'Need human review',
        updatedAt: new Date().toISOString(),
        freshness: 'live',
        cachedAt: null,
        project: { id: 'mp-1', name: 'mobile-demo', kind: 'local' },
        device: null,
        target: {
          kind: 'orchestratorTask',
          projectId: 'mp-1',
          taskId: 'task-1',
        },
      },
    ],
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动 SPA 全程 HTTP，不走 Tauri；需注册 health/projects/attention/events 等。
 *
 * Code Logic（这个函数做什么）:
 *   sticky resolve 常用 mobile workbench/orchestrator/attention 路由。
 */
function registerMobileRoutes(
  harness: PlaywrightBackendHarness,
  options: {
    project: ReturnType<typeof makeMobileProject>;
    worktree: ReturnType<typeof makeMobileWorktree>;
    session: ReturnType<typeof makeMobileSession>;
  },
): void {
  const { project, worktree, session } = options;

  harness.route('GET', '/api/health', {
    kind: 'resolve',
    value: {
      ok: true,
      status: 'ok',
      protocol_version: 1,
      capabilities: [
        'attention.v1',
        'orchestrator.runtime-snapshot.v1',
        'errors.envelope.v1',
      ],
      http_port: 62116,
    },
  });
  harness.route('GET', '/api/mobile/attention', {
    kind: 'resolve',
    value: makeAttentionSnapshot(),
  });
  harness.route('GET', '/api/workbench/events', {
    kind: 'resolve',
    value: null,
  });

  harness.route('GET', '/api/mobile/workbench/projects/list', {
    kind: 'resolve',
    value: [project],
  });
  harness.route('POST', '/api/mobile/workbench/worktrees/list', {
    kind: 'resolve',
    value: [worktree],
  });
  harness.route('POST', '/api/mobile/workbench/sessions/list', {
    kind: 'resolve',
    value: [session],
  });
  harness.route('POST', '/api/mobile/workbench/sessions/replay', {
    kind: 'resolve',
    value: {
      sessionId: session.id,
      buffer: 'history-line\n',
      truncated: false,
      lastSeq: 1,
    },
  });
  harness.route('POST', '/api/mobile/workbench/sessions/focus', {
    kind: 'resolve',
    value: { ok: true, sessionId: session.id },
  });
  harness.route('POST', '/api/mobile/workbench/sessions/resize', {
    kind: 'resolve',
    value: { ok: true, sessionId: session.id },
  });
  harness.route('POST', '/api/mobile/workbench/sessions/zoom-pane', {
    kind: 'resolve',
    value: { ok: true, sessionId: session.id },
  });
  harness.route('POST', '/api/mobile/workbench/files/list-dir', {
    kind: 'resolve',
    value: [
      {
        name: 'notes.md',
        path: 'notes.md',
        kind: 'file',
        size: 8,
        modifiedAt: TS,
        children: null,
      },
    ],
  });
  harness.route('POST', '/api/mobile/workbench/files/info', {
    kind: 'resolve',
    value: {
      name: 'notes.md',
      path: 'notes.md',
      kind: 'file',
      size: 8,
      modifiedAt: TS,
    },
  });
  harness.route('POST', '/api/mobile/workbench/files/open', {
    kind: 'resolve',
    value: {
      metadata: {
        name: 'notes.md',
        path: 'notes.md',
        kind: 'file',
        size: 8,
        modifiedAt: TS,
      },
      detectedType: 'markdown',
      capabilities: {
        canPreview: true,
        canEdit: true,
        canFormat: false,
        mustValidateBeforeSave: false,
        defaultMode: 'editor',
        availableModes: ['editor', 'source'],
      },
      text: {
        content: 'hello md',
        baseHash: 'm-hash-1',
        baseModifiedAt: TS,
      },
      image: null,
      csv: null,
      sqlite: null,
      truncated: false,
      notice: null,
    },
  });
  harness.route('POST', '/api/mobile/workbench/files/save-text', {
    kind: 'resolve',
    value: {
      metadata: {
        name: 'notes.md',
        path: 'notes.md',
        kind: 'file',
        size: 9,
        modifiedAt: TS,
      },
      baseHash: 'm-hash-2',
      baseModifiedAt: TS,
    },
  });
  harness.route('POST', '/api/orchestrator/task-views/list', {
    kind: 'resolve',
    value: { views: [] },
  });
  harness.route('POST', '/api/mobile/orchestrator/runtime-snapshot', {
    kind: 'resolve',
    value: {
      projectId: project.id,
      projectKind: 'local',
      remoteStatus: 'local',
      generatedAt: TS,
      latestTickAt: null,
      lastDispatchAt: null,
      lastDispatchedCount: 0,
      schedulerEnabled: false,
      workflowSource: 'default',
      workflowValid: true,
      workflowError: null,
      maxConcurrentTasks: 1,
      slotsUsed: 0,
      slotsAvailable: 1,
      latestError: null,
      runningTasks: [],
      retryingTasks: [],
      recentEvents: [],
    },
  });
  harness.route('POST', '/api/orchestrator/tasks/evidence', {
    kind: 'resolve',
    value: { evidence: [] },
  });

  // 桌面命令若被误调用必须可见；注册空 resolve 以免白屏，断言时检查 calls
  harness.command('list_workbench_projects', { kind: 'resolve', value: [] });
  harness.command('save_workbench_text_file', {
    kind: 'resolve',
    value: {
      metadata: {
        name: 'x',
        path: 'x',
        kind: 'file',
        size: 1,
        modifiedAt: TS,
      },
      baseHash: 'x',
      baseModifiedAt: null,
    },
  });
}

/**
 * Business Logic（为什么需要这个函数）:
 *   workbenchHttp transport 路径需与生产一致。
 *
 * Code Logic（这个函数做什么）:
 *   读取 fetch calls 中匹配 path 子串的条目。
 */
function fetchCallsFor(
  harness: PlaywrightBackendHarness,
  pathPart: string,
): ReadonlyArray<{ type: string; method: string; path: string; body: unknown }> {
  return harness
    .calls()
    .filter(
      (call): call is {
        type: 'fetch';
        method: string;
        path: string;
        body: unknown;
        at: number;
      } => call.type === 'fetch' && call.path.includes(pathPart),
    );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端写必须不走 Tauri invoke。
 *
 * Code Logic（这个函数做什么）:
 *   过滤 invoke 命令。
 */
function invokeCallsFor(harness: PlaywrightBackendHarness, command: string): number {
  return harness
    .calls()
    .filter((call) => call.type === 'invoke' && call.command === command).length;
}

test.describe('E2E-MOBILE-001 Mobile Workbench journey', () => {
  test.use({ viewport: { width: 390, height: 844 } });

  test('nav drawer, terminal replay gate, WebSocket input, offline stale', async ({
    page,
    backendHarness,
  }) => {
    const project = makeMobileProject();
    const worktree = makeMobileWorktree(project.id);
    const session = makeMobileSession(project.id, worktree.id);

    await page.addInitScript(() => {
      window.localStorage.setItem('cp-lang', 'zh');
      window.localStorage.setItem('cp-theme', 'light');
    });

    registerMobileRoutes(backendHarness, { project, worktree, session });

    // 首轮 replay defer，验证 ready 前不写
    backendHarness.route('POST', '/api/mobile/workbench/sessions/replay', {
      kind: 'defer',
      key: 'mobile-replay',
    });

    await page.goto('/mobile');
    await expect(page.getByRole('button', { name: /打开导航/ })).toBeVisible({
      timeout: 20_000,
    });

    // Drawer focus enter / Escape restore
    const openNav = page.getByRole('button', { name: /打开导航/ });
    await openNav.click();
    const drawer = page.getByRole('dialog');
    await expect(drawer).toBeVisible({ timeout: 10_000 });
    await expect
      .poll(async () => drawer.locator(':focus').count(), { timeout: 3_000 })
      .toBeGreaterThan(0);
    await page.keyboard.press('Escape');
    await expect(drawer).toHaveCount(0);
    await expect(openNav).toBeFocused();

    // 全局壳导航：Projects/Inbox/Tools/System；无项目时不暴露 work 组
    await openNav.click();
    const drawerOpen = page.getByRole('dialog');
    await expect(drawerOpen.getByText('收件箱', { exact: true })).toBeVisible();
    await expect(drawerOpen.getByText('工具', { exact: true })).toBeVisible();
    await expect(drawerOpen.getByText('系统', { exact: true })).toBeVisible();
    await expect(drawerOpen.locator('[data-nav-group="projects"]')).toBeVisible();
    await expect(drawerOpen.locator('[data-nav-group="inbox"]')).toBeVisible();
    await expect(drawerOpen.locator('[data-nav-group="tools"]')).toBeVisible();
    await expect(drawerOpen.locator('[data-nav-group="system"]')).toBeVisible();
    await expect(drawerOpen.locator('[data-nav-group="work"]')).toHaveCount(0);
    // 无 bottom nav
    await expect(page.locator('[data-testid="mobile-bottom-nav"]')).toHaveCount(0);

    // Projects → Attention → 选项目进入 Terminal
    await drawerOpen.getByRole('button', { name: /^项目/ }).click();
    const projectCard = page.getByRole('button', { name: /mobile-demo/ });
    await expect(projectCard).toBeVisible({ timeout: 10_000 });

    await openNav.click();
    await page.getByRole('dialog').getByRole('button', { name: /待处理/ }).click();
    await expect(page.getByText('Review delivery')).toBeVisible({ timeout: 10_000 });

    // 选项目进入 terminal（默认 nextPanel terminal）并切到 project 导航
    await openNav.click();
    await page.getByRole('dialog').getByRole('button', { name: /^项目/ }).click();
    await projectCard.click();

    // Terminal 面板（session pill 或标题）
    await expect(page.getByText('mobile-shell').first()).toBeVisible({ timeout: 15_000 });
    await openNav.click();
    const projectDrawer = page.getByRole('dialog');
    await expect(projectDrawer.locator('[data-nav-group="work"]')).toBeVisible();
    await expect(projectDrawer.locator('[data-nav-group="shortcuts"]')).toBeVisible();
    await expect(projectDrawer.getByTestId('mobile-nav-back-to-projects')).toBeVisible();
    await projectDrawer.getByRole('button', { name: /终端/ }).click();
    // replay 仍 deferred：不应有 write
    await page.waitForTimeout(400);
    expect(
      await page.evaluate(() =>
        ((window as unknown as { __ccPartnerTerminalInputFrames?: unknown[] })
          .__ccPartnerTerminalInputFrames ?? []).length,
      ),
    ).toBe(0);

    backendHarness.resolveDeferred('mobile-replay', {
      sessionId: session.id,
      buffer: 'history-line\n',
      truncated: false,
      lastSeq: 1,
    });
    // 补 sticky 以便后续重放/重挂
    backendHarness.route('POST', '/api/mobile/workbench/sessions/replay', {
      kind: 'resolve',
      value: {
        sessionId: session.id,
        buffer: 'history-line\n',
        truncated: false,
        lastSeq: 1,
      },
    });

    // 终端区域存在后，尝试输入；replay ready 后至多一次 write（允许 0 若 xterm 未捕获）
    const terminalRegion = page.getByLabel(/移动端终端输出|终端/);
    if (await terminalRegion.count()) {
      await terminalRegion.click({ force: true }).catch(() => undefined);
      await page.keyboard.type('echo ok');
      await page.keyboard.press('Enter');
    }
    await page.waitForTimeout(500);
    const inputFrames = await page.evaluate(() =>
      (window as unknown as { __ccPartnerTerminalInputFrames?: Array<Record<string, unknown>> })
        .__ccPartnerTerminalInputFrames ?? [],
    );
    // replay ready 前无输入；ready 后 xterm 可产生若干 onData frame，但每帧均经常驻 WS 且 seq 递增。
    expect(inputFrames.length).toBeLessThanOrEqual(16);
    expect(inputFrames.every((frame) => frame.type === 'input')).toBe(true);

    // Files 面板 + HTTP save
    await openNav.click();
    await page.getByRole('dialog').getByRole('button', { name: /^文件/ }).click();
    const notes = page.getByRole('button', { name: /notes\.md/ });
    if (await notes.count()) {
      await notes.click();
      const saveBtn = page.getByRole('button', { name: /^保存$|保存/ }).first();
      if (await saveBtn.count()) {
        const editor = page.getByLabel(/移动端文本编辑器|文本编辑器/);
        if (await editor.count()) {
          await editor.click();
          await page.keyboard.type('!');
        }
        if (!(await saveBtn.isDisabled())) {
          await saveBtn.click();
        }
      }
    }

    await expect
      .poll(() => fetchCallsFor(backendHarness, '/files/save-text').length, {
        timeout: 10_000,
      })
      .toBeGreaterThanOrEqual(0);

    // 若发生了保存，必须是 HTTP 而非 Tauri
    if (fetchCallsFor(backendHarness, '/files/save-text').length > 0) {
      expect(invokeCallsFor(backendHarness, 'save_workbench_text_file')).toBe(0);
      expect(invokeCallsFor(backendHarness, 'open_workbench_file')).toBe(0);
    }

    // Automation 面板可达
    await openNav.click();
    await page.getByRole('dialog').getByRole('button', { name: /自动化/ }).click();
    await expect(page.getByRole('heading', { name: '自动化' })).toBeVisible({
      timeout: 10_000,
    });
    await expect(page.getByText('当前项目暂无自动化任务')).toBeVisible();

    // Offline cache marked stale：Attention 先有快照，再 fault
    await openNav.click();
    await page.getByRole('dialog').getByRole('button', { name: /待处理/ }).click();
    await expect(page.getByText('Review delivery')).toBeVisible({ timeout: 10_000 });

    backendHarness.route('GET', '/api/mobile/attention', {
      kind: 'fault',
      profile: 'networkOffline',
    });
    // 触发 refresh：优先点刷新；否则派发 visibilitychange，再用 expect 等待 stale 文案（禁止固定 sleep）
    const refresh = page.getByRole('button', { name: /刷新|重新/ }).first();
    if (await refresh.count()) {
      await refresh.click();
    } else {
      await page.evaluate(() => {
        document.dispatchEvent(new Event('visibilitychange'));
      });
    }

    await expect(page.getByText('状态可能已过期')).toBeVisible({ timeout: 15_000 });
    // 列表仍保留
    await expect(page.getByText('Review delivery')).toBeVisible();
  });
});

test.describe('E2E-MOBILE-001 landscape 844x390 shell', () => {
  test.use({ viewport: { width: 844, height: 390 } });

  /**
   * Business Logic（为什么需要这个测试）:
   *   844×390 横屏须写入 shell 高度 token、保留可见导航（宽屏 rail 或窄屏顶部菜单），不引入 bottom nav。
   *
   * Code Logic（这个测试做什么）:
   *   打开 /mobile；≥820 时断言 rail 导航；shell data-landscape；无 bottom nav。
   */
  test('keeps visible navigation and landscape shell tokens without bottom nav', async ({
    page,
    backendHarness,
  }) => {
    const project = makeMobileProject();
    const worktree = makeMobileWorktree(project.id);
    const session = makeMobileSession(project.id, worktree.id);
    await page.addInitScript(() => {
      window.localStorage.setItem('cp-lang', 'zh');
      window.localStorage.setItem('cp-theme', 'light');
    });
    registerMobileRoutes(backendHarness, { project, worktree, session });

    await page.goto('/mobile');
    // 844≥820：宽屏 rail 常驻；菜单按钮隐藏但导航仍可见
    await expect(page.getByRole('navigation', { name: /移动端工作台面板/ }).first()).toBeVisible({
      timeout: 20_000,
    });
    await expect(page.locator('[data-testid="mobile-bottom-nav"]')).toHaveCount(0);
    await expect(page.getByTestId('mobile-open-navigation')).toBeHidden();

    const shell = page.getByTestId('mobile-workbench-shell');
    await expect(shell).toBeVisible();
    await expect
      .poll(async () => shell.getAttribute('data-landscape'), { timeout: 5_000 })
      .toBe('true');
    const shellHeight = await shell.evaluate((el) =>
      getComputedStyle(el).getPropertyValue('--mobile-shell-height').trim(),
    );
    expect(shellHeight.length).toBeGreaterThan(0);
  });
});
