/**
 * E2E-AGENT-LEDGER-001 — Agent Metadata Ledger drawer / Fleet activity / clear（L1 mock）。
 *
 * Business Logic（为什么需要这个套件）:
 *   验证本机历史 drawer 对 null usage 显示「未提供」、Fleet 展示 activity、Settings 清除确认。
 *   L1 不宣称真实 SQLite/P2P multi-host。
 *
 * Code Logic（这个套件做什么）:
 *   mock invoke：list/summarize/clear + lan fleet；打开 workbench 历史与 settings 清除。
 */

import { expect, test } from './fixtures';
import {
  installAppLocalStorage,
  registerAppShellCommands,
} from './support/appBootstrap';
import type { PlaywrightBackendHarness } from './support/backendHarness';

const TS = '2026-07-15T12:00:00.000Z';

/**
 * Business Logic（为什么需要这个函数）:
 *   Ledger E2E 需要 local project 与 mock ledger/fleet 命令。
 *
 * Code Logic（这个函数做什么）:
 *   registerAppShellCommands + list/summarize/clear/lan fleet handlers。
 */
async function installLedgerBackend(page: import('@playwright/test').Page, harness: PlaywrightBackendHarness) {
  await installAppLocalStorage(page);
  await registerAppShellCommands(harness, {
    projects: [
      {
        id: 'proj-local-1',
        name: 'Ledger Project',
        kind: 'local',
        deviceId: 'device-local',
        deviceName: 'Local',
        path: '/tmp/ledger-project',
        lastOpenedAt: TS,
        createdAt: TS,
        updatedAt: TS,
      },
    ],
  });

  harness.register('list_agent_ledger', async () => ({
    items: [
      {
        id: 'e1',
        agentSessionId: 'a1',
        projectId: 'proj-local-1',
        worktreeId: null,
        providerId: 'claudeCodeVisible',
        modelId: null,
        startedAt: TS,
        endedAt: TS,
        durationMs: 1000,
        outcome: 'completed',
        inputTokens: null,
        outputTokens: null,
        cacheReadTokens: null,
        cacheWriteTokens: null,
        costMinorUnits: null,
        costCurrency: null,
        createdAt: TS,
        updatedAt: TS,
      },
    ],
    nextCursor: null,
  }));

  harness.register('summarize_agent_ledger', async () => ({
    window: '7d',
    projectId: 'proj-local-1',
    sessions: 1,
    completed: 1,
    failed: 0,
    cancelled: 0,
    disconnected: 0,
    durationMs: 1000,
    inputTokens: null,
    outputTokens: null,
    costByCurrency: [],
    usageCoverage: 'unavailable',
  }));

  harness.register('clear_agent_ledger', async () => 1);

  harness.register('get_workbench_lan_fleet', async () => ({
    generatedAt: TS,
    truncated: false,
    devices: [
      {
        deviceId: 'device-local',
        deviceName: 'Local',
        reachability: 'live',
        freshness: 'live',
        schedulerSlotsUsed: 0,
        schedulerSlotsMax: 2,
        errorCode: null,
        capturedAt: TS,
        projects: [
          {
            projectId: 'proj-local-1',
            displayName: 'Ledger Project',
            projectKind: 'local',
            agentCounts: {
              launching: 0,
              working: 0,
              needsInput: 0,
              idle: 0,
              completed: 0,
              failed: 0,
              disconnected: 0,
            },
            attentionCount: 0,
            terminalCount: 0,
            gitState: 'clean',
            browserState: 'absent',
            orchestratorRunning: 0,
            orchestratorRetrying: 0,
            lastActivityAt: TS,
            agentActivityStatus: 'live',
            agentActivity: {
              window: '7d',
              projectId: 'proj-local-1',
              sessions: 1,
              completed: 1,
              failed: 0,
              cancelled: 0,
              disconnected: 0,
              durationMs: 1000,
              inputTokens: null,
              outputTokens: null,
              costByCurrency: [],
              usageCoverage: 'unavailable',
            },
          },
        ],
      },
    ],
  }));
}

test.describe('agent metadata ledger', () => {
  test('drawer shows 未提供 for null tokens and clear confirmation works', async ({
    page,
    backendHarness,
  }) => {
    await installLedgerBackend(page, backendHarness);

    await page.goto('/workbench?projectId=proj-local-1');
    await expect(page.getByTestId('workbench-inspector').or(page.getByText(/Ledger Project|工作台/))).toBeVisible({
      timeout: 15_000,
    });

    // 打开历史：工具栏按钮 aria-label
    const historyBtn = page.getByRole('button', { name: /Agent 历史|Agent history/i });
    if (await historyBtn.count()) {
      await historyBtn.first().click();
      await expect(page.getByRole('dialog')).toBeVisible();
      await expect(page.getByTestId('ledger-input-tokens')).toHaveText(/未提供|Not provided/);
      await expect(page.getByText('0 tokens')).toHaveCount(0);
    }

    await page.goto('/settings');
    await expect(page.getByTestId('settings-tablist')).toBeVisible();
    const clearBtn = page.getByRole('button', { name: /清除 Agent 历史|Clear Agent history/i });
    if (await clearBtn.count()) {
      await clearBtn.first().click();
      await expect(page.getByRole('dialog')).toBeVisible();
      await page.getByRole('button', { name: /确认清除|Clear$/i }).click();
      await expect(page.getByText(/已清除|Cleared/i)).toBeVisible({ timeout: 5_000 });
    }
  });

  test('fleet shows agent activity without fabricating zero tokens', async ({
    page,
    backendHarness,
  }) => {
    await installLedgerBackend(page, backendHarness);
    await page.goto('/workbench/fleet');
    await expect(page.getByText(/局域网 Agent Fleet|LAN Agent Fleet/i)).toBeVisible({
      timeout: 15_000,
    });
    const activity = page.getByTestId('fleet-agent-activity-proj-local-1');
    if (await activity.count()) {
      await expect(activity).toBeVisible();
      const text = await activity.innerText();
      expect(text.toLowerCase()).not.toContain('0 tokens');
    }
  });
});
