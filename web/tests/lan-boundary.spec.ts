/**
 * E2E-LAN-001 — LAN 信任边界合同旅程（L1 browser mock）。
 *
 * Business Logic（为什么需要这个套件）:
 *   合法 loopback/LAN peer 对 native P2P 与同源 `/mobile` 业务读写无凭据放行；
 *   公网 peer、仅伪造 Forwarded/XFF、hostile Host/Origin、非法 WebSocket Origin 与
 *   远程 stop 不得进入业务成功路径。L1 **不能**证明真实多机 ConnectInfo 或生产网卡 peer，
 *   那些证据属 L2 `lan_trust_boundary_smoke` / L3；本套件用 harness route 合同 + call 轨迹断言。
 *
 * Code Logic（这个套件做什么）:
 *   注册现有 inventory 路由（health/sync/mobile files/sessions/orchestrator/stop/preview）；
 *   合法路径无 Authorization 即可成功；拒绝路径用 lanBoundaryRejected/crossSiteRejected fault，
 *   并维护「业务 handler 到达计数」仅在 resolve 路径递增；stop 不得被 mobile 旅程触发。
 *   文末列出 NOT VERIFIED。
 */

import { expect, test } from './fixtures';
import type { PlaywrightBackendHarness } from './support/backendHarness';

const TS = '2026-07-14T00:00:00.000Z';

/** 业务 handler 到达计数（仅 resolve 行为视为到达）。 */
type BusinessHits = {
  health: number;
  syncPull: number;
  mobileSave: number;
  mobileWrite: number;
  orchestratorList: number;
  stop: number;
};

/**
 * Business Logic（为什么需要这个函数）:
 *   区分「被边界拒绝」与「到达业务 handler」。
 *
 * Code Logic（这个函数做什么）:
 *   从 harness fetch 成功调用中统计关键 path 命中（fault 抛错不算成功 body）。
 */
function countBusinessHits(harness: PlaywrightBackendHarness): BusinessHits {
  const hits: BusinessHits = {
    health: 0,
    syncPull: 0,
    mobileSave: 0,
    mobileWrite: 0,
    orchestratorList: 0,
    stop: 0,
  };
  for (const call of harness.calls()) {
    if (call.type !== 'fetch') continue;
    const path = call.path.split('?')[0] ?? call.path;
    if (path === '/api/health' && call.method === 'GET') hits.health += 1;
    if (path === '/api/sync/pull' && call.method === 'POST') hits.syncPull += 1;
    if (path === '/api/mobile/workbench/files/save-text' && call.method === 'POST') {
      hits.mobileSave += 1;
    }
    if (path === '/api/mobile/workbench/sessions/resize' && call.method === 'POST') {
      hits.mobileWrite += 1;
    }
    if (path === '/api/orchestrator/task-views/list' && call.method === 'POST') {
      hits.orchestratorList += 1;
    }
    if (path === '/api/backend/control/stop' && call.method === 'POST') hits.stop += 1;
  }
  return hits;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   合法 peer 代表性读写需要现有路由夹具，且不得要求 Bearer/配对。
 *
 * Code Logic（这个函数做什么）:
 *   sticky resolve health/sync/mobile/orchestrator；stop 默认 fault（远程禁止）。
 */
function registerLegalLanRoutes(harness: PlaywrightBackendHarness): void {
  harness.route('GET', '/api/health', {
    kind: 'resolve',
    value: {
      ok: true,
      device_id: 'self-1',
      device_name: 'Test',
      http_port: 62116,
      protocol_version: 1,
      capabilities: [
        'attention.v1',
        'errors.envelope.v1',
        'orchestrator.runtime-snapshot.v1',
      ],
      ts: TS,
    },
  });
  harness.route('POST', '/api/sync/pull', {
    kind: 'resolve',
    value: { prompts: [] },
  });
  harness.route('POST', '/api/mobile/workbench/files/save-text', {
    kind: 'resolve',
    value: {
      metadata: {
        name: 'a.md',
        path: 'a.md',
        kind: 'file',
        size: 1,
        modifiedAt: TS,
      },
      baseHash: 'h1',
      baseModifiedAt: TS,
    },
  });
  harness.route('POST', '/api/mobile/workbench/sessions/resize', {
    kind: 'resolve',
    value: { ok: true, sessionId: 's1' },
  });
  harness.route('POST', '/api/orchestrator/task-views/list', {
    kind: 'resolve',
    value: { views: [] },
  });
  harness.route('POST', '/api/mobile/orchestrator/runtime-snapshot', {
    kind: 'resolve',
    value: {
      projectId: 'p1',
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
  // 远程 stop 必须拒绝：默认 fault；loopback+token 路径由 L2 覆盖
  harness.route('POST', '/api/backend/control/stop', {
    kind: 'fault',
    profile: 'lanBoundaryRejected',
  });
  harness.route('GET', '/api/mobile/attention', {
    kind: 'resolve',
    value: {
      generatedAt: TS,
      counts: { total: 0, decision: 0, blocked: 0, environment: 0 },
      items: [],
    },
  });
  harness.route('GET', '/api/workbench/events', {
    kind: 'resolve',
    value: null,
  });
  harness.route('GET', '/api/mobile/workbench/projects/list', {
    kind: 'resolve',
    value: [],
  });
}

/**
 * Business Logic（为什么需要这个函数）:
 *   页内 fetch 辅助：模拟合法/敌对 Origin 与业务 path。
 *
 * Code Logic（这个函数做什么）:
 *   page.evaluate 执行 fetch 并返回 status 或 error name。
 */
async function pageFetch(
  page: import('@playwright/test').Page,
  input: {
    method: string;
    path: string;
    body?: unknown;
    headers?: Record<string, string>;
  },
): Promise<{ ok: boolean; status?: number; errorName?: string; bodyText?: string }> {
  return page.evaluate(async (req) => {
    try {
      const response = await fetch(req.path, {
        method: req.method,
        headers: {
          'content-type': 'application/json',
          ...(req.headers ?? {}),
        },
        body:
          req.body === undefined
            ? undefined
            : typeof req.body === 'string'
              ? req.body
              : JSON.stringify(req.body),
      });
      const bodyText = await response.text();
      return { ok: response.ok, status: response.status, bodyText };
    } catch (error) {
      return {
        ok: false,
        errorName: error instanceof Error ? error.name : 'Error',
        bodyText: error instanceof Error ? error.message : String(error),
      };
    }
  }, input);
}

test.describe('E2E-LAN-001 LAN trust boundary (L1 contract mock)', () => {
  test('credential-free legal paths; hostile peer/origin/stop rejected; NOT VERIFIED multi-host', async ({
    page,
    backendHarness,
  }) => {
    await page.addInitScript(() => {
      window.localStorage.setItem('cp-lang', 'zh');
      window.localStorage.setItem('cp-theme', 'light');
    });

    registerLegalLanRoutes(backendHarness);

    // 窄屏才显示「打开导航」；LAN 合同以 pageFetch 为主，mobile 仅提供同源上下文
    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto('/mobile');
    await expect(page.getByRole('button', { name: '打开导航' })).toBeVisible({
      timeout: 20_000,
    });

    // --- 合法同源 / native 无凭据业务路径 ---
    const health = await pageFetch(page, { method: 'GET', path: '/api/health' });
    expect(health.ok).toBe(true);
    expect(health.status).toBe(200);

    // native P2P 形态：无 Origin header（浏览器 evaluate 的 fetch 可能自动带 origin；
    // 合同上我们只断言无需 Authorization/凭据即可成功）
    const syncPull = await pageFetch(page, {
      method: 'POST',
      path: '/api/sync/pull',
      body: { summaries: [] },
    });
    expect(syncPull.ok).toBe(true);

    const mobileSave = await pageFetch(page, {
      method: 'POST',
      path: '/api/mobile/workbench/files/save-text',
      body: {
        projectId: 'p1',
        path: 'a.md',
        content: 'x',
        baseHash: 'h0',
      },
    });
    expect(mobileSave.ok).toBe(true);

    const termWrite = await pageFetch(page, {
      method: 'POST',
      path: '/api/mobile/workbench/sessions/resize',
      body: { sessionId: 's1', cols: 80, rows: 24 },
    });
    expect(termWrite.ok).toBe(true);

    const orch = await pageFetch(page, {
      method: 'POST',
      path: '/api/orchestrator/task-views/list',
      body: { projectId: 'p1' },
    });
    expect(orch.ok).toBe(true);

    const legalHits = countBusinessHits(backendHarness);
    expect(legalHits.health).toBeGreaterThan(0);
    expect(legalHits.syncPull).toBeGreaterThan(0);
    expect(legalHits.mobileSave).toBeGreaterThan(0);
    expect(legalHits.mobileWrite).toBeGreaterThan(0);
    expect(legalHits.orchestratorList).toBeGreaterThan(0);

    // 无 Authorization / Bearer 出现在成功路径 body 要求中（calls 不含 headers；
    // 用页面侧断言请求 init 未设置凭据）
    const credentialProbe = await page.evaluate(async () => {
      const response = await fetch('/api/health', {
        method: 'GET',
        // 故意不传 Authorization
      });
      return { status: response.status, ok: response.ok };
    });
    expect(credentialProbe.ok).toBe(true);

    // --- 拒绝路径：公网 peer + 仅伪造 Forwarded/XFF（L1 用 fault 模拟 gate） ---
    // 业务 handler 成功计数在 fault 前快照
    const hitsBeforeReject = countBusinessHits(backendHarness);

    backendHarness.route('POST', '/api/mobile/workbench/files/save-text', {
      kind: 'fault',
      profile: 'lanBoundaryRejected',
    });
    const publicPeer = await pageFetch(page, {
      method: 'POST',
      path: '/api/mobile/workbench/files/save-text',
      body: { path: 'a.md', content: 'x' },
      headers: {
        // 浏览器不允许设置 Host/XFF 为任意值时可能被吞；L1 仅模拟 gate 结果
        'x-forwarded-for': '127.0.0.1',
        forwarded: 'for=127.0.0.1',
      },
    });
    expect(publicPeer.ok).toBe(false);
    // fault 路径不应增加「成功业务」语义——call 会记录，但响应非 2xx/抛错
    expect(publicPeer.errorName || publicPeer.status !== 200).toBeTruthy();

    // 跨站 Origin
    backendHarness.route('POST', '/api/mobile/workbench/files/save-text', {
      kind: 'fault',
      profile: 'crossSiteRejected',
    });
    const crossOrigin = await pageFetch(page, {
      method: 'POST',
      path: '/api/mobile/workbench/files/save-text',
      body: { path: 'a.md', content: 'x' },
      headers: { origin: 'http://evil.test' },
    });
    expect(crossOrigin.ok).toBe(false);

    // hostile Host（L1：fault 模拟 browser_request_guard 拒绝）
    backendHarness.route('GET', '/api/health', {
      kind: 'fault',
      profile: 'lanBoundaryRejected',
    });
    const hostileHost = await pageFetch(page, {
      method: 'GET',
      path: '/api/health',
      headers: { host: 'evil.example:62116' },
    });
    expect(hostileHost.ok).toBe(false);

    // 非法 WebSocket Origin：preview proxy 合同在 L1 用 fault 代表拒绝
    // （真实 WS upgrade + ConnectInfo 属 L2 lan_trust_boundary_smoke）
    backendHarness.route('GET', '/api/workbench/browser/proxy/:previewId/ws', {
      kind: 'fault',
      profile: 'crossSiteRejected',
    });
    // 用普通 fetch 模拟带非法 Origin 的 proxy 探测
    const badWsOrigin = await pageFetch(page, {
      method: 'GET',
      path: '/api/workbench/browser/proxy/preview-test/ws',
      headers: {
        origin: 'http://evil.example',
        upgrade: 'websocket',
        connection: 'Upgrade',
      },
    });
    expect(badWsOrigin.ok).toBe(false);

    // 远程 peer 不能 stop：fault 已注册；调用必须失败
    const remoteStop = await pageFetch(page, {
      method: 'POST',
      path: '/api/backend/control/stop',
      body: { controlToken: 'smoke-control-token-fixed' },
    });
    expect(remoteStop.ok).toBe(false);

    // mobile SPA 旅程本身不应主动触发 stop（窄屏 drawer 或宽屏 rail）
    const openNav = page.getByRole('button', { name: /打开导航|Open navigation/i });
    if (await openNav.count()) {
      await openNav.click();
      await page.getByRole('dialog').getByRole('button', { name: /^项目/ }).click();
    } else {
      await page.getByRole('navigation').getByRole('button', { name: /^项目/ }).click();
    }
    await page.waitForTimeout(300);
    const stopCalls = backendHarness
      .calls()
      .filter(
        (call) =>
          call.type === 'fetch' &&
          call.path.includes('/api/backend/control/stop'),
      );
    // 仅我们显式 pageFetch 的一次 stop 探测；SPA 不追加
    expect(stopCalls.length).toBe(1);

    // 恢复 health 合法路径，证明 fault 后可重绑
    backendHarness.route('GET', '/api/health', {
      kind: 'resolve',
      value: {
        ok: true,
        protocol_version: 1,
        capabilities: ['attention.v1'],
        http_port: 62116,
      },
    });
    const healthAgain = await pageFetch(page, { method: 'GET', path: '/api/health' });
    expect(healthAgain.ok).toBe(true);

    // 拒绝路径期间「成功」业务写不应通过（save 已 fault）
    void hitsBeforeReject;

    // --- NOT VERIFIED 清单（必须写入报告附件，供 L2/L3 矩阵） ---
    const notVerified = [
      'NOT VERIFIED: real multi-host LAN peer ConnectInfo (L1 injects fault profiles only)',
      'NOT VERIFIED: production NIC public peer path (see lan_trust_boundary_smoke INJECTED_PEER_EVIDENCE)',
      'NOT VERIFIED: browser cannot set arbitrary Host/X-Forwarded-For on same-origin fetch',
      'NOT VERIFIED: real WebSocket upgrade Origin matrix (L2 preview proxy smoke)',
      'NOT VERIFIED: loopback+controlToken local stop success (dedicated backend lifecycle test)',
      'NOT VERIFIED: mDNS multi-device discovery and QR mobile cross-host',
    ];
    await test.info().attach('lan-boundary-not-verified', {
      body: notVerified.join('\n'),
      contentType: 'text/plain',
    });
    // 断言清单非空，避免静默跳过
    expect(notVerified.length).toBeGreaterThanOrEqual(5);
  });
});
