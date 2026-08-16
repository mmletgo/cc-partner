/**
 * E2E-TOKEN-STATS-001 — Token 统计页 KPI / 筛选 / 导出 / stale（L1 mock）。
 *
 * Business Logic（为什么需要这个套件）:
 *   侧栏新入口必须能打开 `/token-stats`，首屏 KPI 来自 summarize，切窗口会带新
 *   filter 再拉一次，导出成功给路径 toast；刷新失败时保留上次结果并出 stale 横幅。
 *   L1 不宣称真实 SQLite / 写盘 / 双主题真机渲染。
 *
 * Code Logic（这个套件做什么）:
 *   backendHarness sticky mock summarize/list/export；打开页面断言 KPI；
 *   点 30d 后检查 invoke args.window；导出 CSV；再把 summarize reject 点刷新出 stale。
 */

import { expect, test } from './fixtures';
import { installAppLocalStorage, registerAppShellCommands } from './support/appBootstrap';

const TS = '2026-07-01T10:00:00.000Z';

const SUMMARY = {
  window: '7d',
  projectId: null,
  sessions: 2,
  completed: 2,
  failed: 0,
  cancelled: 0,
  disconnected: 0,
  durationMs: 300000,
  inputTokens: 100,
  outputTokens: 50,
  cacheReadTokens: 30,
  cacheWriteTokens: null,
  realConsumedTokens: 150,
  cacheHitRate: 0.2307,
  requestsCount: 2,
  costByCurrency: [{ currency: 'USD', minorUnits: 120 }],
  totalCostByCurrency: [{ currency: 'USD', minorUnits: 120 }],
  usageCoverage: 'partial',
  byModel: [
    {
      key: 'claude-opus',
      label: 'claude-opus',
      sessions: 2,
      completed: 2,
      failed: 0,
      cancelled: 0,
      disconnected: 0,
      inputTokens: 100,
      outputTokens: 50,
      cacheReadTokens: 30,
      cacheWriteTokens: null,
      costByCurrency: [{ currency: 'USD', minorUnits: 120 }],
    },
  ],
  byProvider: [
    {
      key: 'claudeCodeVisible',
      label: 'Claude',
      sessions: 2,
      completed: 2,
      failed: 0,
      cancelled: 0,
      disconnected: 0,
      inputTokens: 100,
      outputTokens: 50,
      cacheReadTokens: 30,
      cacheWriteTokens: null,
      costByCurrency: [{ currency: 'USD', minorUnits: 120 }],
    },
  ],
  byProject: [],
  trend: [
    {
      bucketStart: '2026-07-01T00:00:00Z',
      inputTokens: 100,
      outputTokens: 50,
      cacheReadTokens: 30,
      cacheWriteTokens: null,
      costByCurrency: [{ currency: 'USD', minorUnits: 120 }],
    },
  ],
  bucket: 'day',
};

const PAGE = {
  items: [
    {
      id: 'id-1',
      agentSessionId: 'a1',
      projectId: 'p1',
      worktreeId: null,
      providerId: 'claudeCodeVisible',
      modelId: 'claude-opus',
      startedAt: TS,
      endedAt: TS,
      durationMs: 300000,
      outcome: 'completed',
      inputTokens: 100,
      outputTokens: 50,
      cacheReadTokens: 30,
      cacheWriteTokens: null,
      costMinorUnits: 120,
      costCurrency: 'USD',
      terminalTitle: null,
    },
  ],
  nextCursor: null,
};

test.describe('E2E-TOKEN-STATS-001 Token stats journey', () => {
  test('opens KPI, switches window, exports CSV, keeps stale on refresh fail', async ({
    page,
    backendHarness,
  }) => {
    await installAppLocalStorage(page);
    registerAppShellCommands(backendHarness);
    backendHarness.command('summarize_agent_ledger', { kind: 'resolve', value: SUMMARY });
    backendHarness.command('list_agent_ledger', { kind: 'resolve', value: PAGE });
    backendHarness.command('export_token_stats', {
      kind: 'resolve',
      value: '/tmp/cc-partner-exports/token-stats.csv',
    });

    await page.goto('/token-stats');
    await expect(page.getByTestId('token-stats-page')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByRole('heading', { name: 'Token 统计' })).toBeVisible();
    await expect(page.getByTestId('token-stats-kpi-input')).toContainText('100');
    await expect(page.getByTestId('token-stats-kpi-hit-rate')).toContainText('23.1%');
    await expect(page.getByTestId('token-stats-kpi-cost')).toContainText('1.20 USD');
    await expect(page.getByTestId('token-stats-session-table')).toContainText('claude-opus');
    await expect(page.getByTestId('token-stats-trend')).toBeVisible();
    await expect(page.getByTestId('token-stats-trend')).not.toContainText('T00:00:00Z');

    await page.getByTestId('token-stats-export-menu').click();
    await page.getByTestId('token-stats-export-csv').click();
    await expect(page.getByTestId('token-stats-export-toast')).toContainText(
      '/tmp/cc-partner-exports/token-stats.csv',
    );
    await expect(page.getByTestId('token-stats-export-reveal')).toBeVisible();
    await page.getByRole('button', { name: '关闭' }).click();

    await page.getByTestId('token-stats-window-custom').click();
    await page.getByTestId('token-stats-custom-start').fill('2026-06-01T00:00');
    await expect
      .poll(() =>
        backendHarness
          .calls()
          .some(
            (call) =>
              call.type === 'invoke' &&
              call.command === 'summarize_agent_ledger' &&
              JSON.stringify(call).includes('startedAfter'),
          ),
      )
      .toBe(true);

    await page.getByTestId('token-stats-window-30d').click();
    await expect
      .poll(() =>
        backendHarness
          .calls()
          .some(
            (call) =>
              call.type === 'invoke' &&
              call.command === 'summarize_agent_ledger' &&
              JSON.stringify(call).includes('"window":"30d"'),
          ),
      )
      .toBe(true);

    backendHarness.command('summarize_agent_ledger', {
      kind: 'reject',
      error: { error: 'db busy', code: 'db_busy' },
    });
    backendHarness.command('list_agent_ledger', {
      kind: 'reject',
      error: { error: 'db busy', code: 'db_busy' },
    });
    await page.getByTestId('token-stats-refresh').click();
    await expect(page.getByTestId('token-stats-stale-banner')).toBeVisible({ timeout: 10_000 });
    await expect(page.getByTestId('token-stats-kpi-input')).toContainText('100');
  });
});
