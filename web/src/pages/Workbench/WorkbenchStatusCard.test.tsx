/**
 * WorkbenchStatusCard 单元测试。
 *
 * Business Logic（为什么需要这个测试文件）:
 *   状态卡重构后必须保证：移除的 5 字段（命令/状态/agent/尺寸/退出码）不出现在 DOM；
 *   新增的 2 行（TokenRateRow + ContextMeter）在 metrics prop 缺失时降级为「—」。
 *
 * Code Logic（这个测试文件做什么）:
 *   - 渲染基础 props，断言 5 个旧字段的 i18n key（命令/状态/Agent/尺寸/退出码）不出现在 statusGrid 中；
 *   - 不传 ledgerEntry → TokenRateRow 显示「—」× 2，ContextMeter 显示「—」（无 ProgressBar）；
 *   - 传 ledgerEntry（终态）→ TokenRateRow 显示数值，ContextMeter 显示 cumulative / window 与 ProgressBar。
 */
// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, beforeAll, describe, expect, it } from 'vitest';
import { I18nextProvider } from 'react-i18next';

import i18n from '@/i18n';
import type { AgentLedgerEntry } from '@/lib/types/agentLedger';
import type { AgentSessionProjection } from '@/lib/types/agentRuntime';

import { WorkbenchStatusCard, type WorkbenchStatusCardProps } from './WorkbenchStatusCard';

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

afterEach(() => {
  cleanup();
});

function makeProps(overrides: Partial<WorkbenchStatusCardProps> = {}): WorkbenchStatusCardProps {
  return {
    activeProject: { id: 'p1', name: 'demo', deviceName: 'local-mac' } as never,
    activeWorktree: { id: 'wt-1', name: 'main' } as never,
    activeSession: {
      id: 's1',
      name: 'claude-1',
      status: 'running',
      startedAt: new Date().toISOString(),
      exitedAt: null,
      exitCode: null,
      cols: 80,
      rows: 24,
    } as never,
    activeRootPath: '/tmp/demo',
    remoteWriteDisabled: false,
    sessionNameDraft: '',
    setSessionNameDraft: () => undefined,
    handleRenameSession: async () => undefined,
    handleCloseSession: async () => undefined,
    runtimeVisible: true,
    activeAgent: null,
    ledgerEntry: null,
    ...overrides,
  };
}

function renderCard(props: WorkbenchStatusCardProps) {
  return render(
    <I18nextProvider i18n={i18n}>
      <WorkbenchStatusCard {...props} />
    </I18nextProvider>,
  );
}

describe('WorkbenchStatusCard — 字段重构', () => {
  it('移除的 5 字段（命令/状态/Agent/尺寸/退出码）不出现在 statusGrid', async () => {
    renderCard(makeProps());
    const html = document.body.textContent ?? '';
    expect(html).not.toMatch(/^命令$/m);
    expect(html).not.toMatch(/^状态$/m);
    expect(html).not.toMatch(/^Agent$/m);
    expect(html).not.toMatch(/^尺寸$/m);
    expect(html).not.toMatch(/^退出码$/m);
  });

  it('保留的 6 行元信息（设备/项目/Worktree/路径/会话/开始）出现在 statusGrid', async () => {
    renderCard(makeProps());
    const html = document.body.textContent ?? '';
    expect(html).toContain('设备');
    expect(html).toContain('项目');
    expect(html).toContain('Worktree');
    expect(html).toContain('工作区路径');
    expect(html).toContain('会话');
    expect(html).toContain('开始');
  });
});

function makeWorkingAgent(overrides: Partial<AgentSessionProjection> = {}): AgentSessionProjection {
  return {
    id: 'a1',
    projectId: 'p1',
    terminalSessionId: 's1',
    providerId: 'claudeCodeVisible',
    phase: 'working',
    version: 1,
    lastActivityAt: '2026-08-16T10:05:00Z',
    freshness: 'live',
    isActive: true,
    startedAt: '2026-08-16T10:00:00Z',
    usage: {
      modelId: 'claude-sonnet-4-5',
      inputTokens: 100_000,
      outputTokens: 10_000,
      cacheReadTokens: 50_000,
      cacheWriteTokens: 20_000,
      extractedAt: '2026-08-16T10:05:00Z',
    },
    ...overrides,
  };
}

describe('WorkbenchStatusCard — ledger 指标降级', () => {
  it('ledgerEntry=null 时 TokenRateRow 与 ContextMeter 显示「未提供」', async () => {
    renderCard(makeProps({ ledgerEntry: null }));
    const rateRow = screen.getByTestId('workbench-status-token-rate-row');
    const meter = screen.getByTestId('workbench-status-context-meter');
    expect(rateRow.textContent).toContain('未提供');
    expect(meter.textContent).toContain('未提供');
    expect(document.querySelector('[role="progressbar"]')).toBeNull();
  });

  it('终态 ledgerEntry 含 input/output/cache + modelId → 显示速率与 ProgressBar', async () => {
    const entry: AgentLedgerEntry = {
      id: 'l1',
      agentSessionId: 'a1',
      projectId: 'p1',
      worktreeId: 'wt-1',
      providerId: 'claudeCodeVisible',
      modelId: 'claude-sonnet-4-5',
      startedAt: '2026-08-16T10:00:00Z',
      endedAt: '2026-08-16T10:05:00Z',
      durationMs: 300_000,
      outcome: 'completed',
      inputTokens: 100_000,
      outputTokens: 10_000,
      cacheReadTokens: 50_000,
      cacheWriteTokens: 20_000,
      terminalTitle: null,
      costMinorUnits: null,
      costCurrency: null,
      createdAt: '2026-08-16T10:05:00Z',
      updatedAt: '2026-08-16T10:05:00Z',
    };
    renderCard(makeProps({ ledgerEntry: entry }));
    const rateRow = screen.getByTestId('workbench-status-token-rate-row');
    const meter = screen.getByTestId('workbench-status-context-meter');
    // 速率：input=100k/300s=333.3 tok/s, output=10k/300s=33.3 tok/s
    expect(rateRow.textContent).toContain('333.3 tok/s');
    expect(rateRow.textContent).toContain('33.3 tok/s');
    // 累计：input+cache_read+cache_write = 170k；window 200k → 85% → ProgressBar danger
    expect(meter.textContent).toContain('170.000k');
    expect(meter.textContent).toContain('200.000k');
    const bar = document.querySelector('[role="progressbar"]');
    expect(bar?.getAttribute('aria-valuenow')).toBe('85');
    expect(bar?.getAttribute('data-tone')).toBe('danger');
  });

  it('未知 modelId → ContextMeter 显示 noWindowLabel，无 ProgressBar', async () => {
    const entry: AgentLedgerEntry = {
      id: 'l1',
      agentSessionId: 'a1',
      projectId: 'p1',
      worktreeId: 'wt-1',
      providerId: 'claudeCodeVisible',
      modelId: 'unknown-model',
      startedAt: '2026-08-16T10:00:00Z',
      endedAt: '2026-08-16T10:05:00Z',
      durationMs: 300_000,
      outcome: 'completed',
      inputTokens: 1200,
      outputTokens: 600,
      cacheReadTokens: 0,
      cacheWriteTokens: 0,
      terminalTitle: null,
      costMinorUnits: null,
      costCurrency: null,
      createdAt: '2026-08-16T10:05:00Z',
      updatedAt: '2026-08-16T10:05:00Z',
    };
    renderCard(makeProps({ ledgerEntry: entry }));
    const meter = screen.getByTestId('workbench-status-context-meter');
    expect(meter.textContent).toContain('无窗口信息');
    expect(document.querySelector('[role="progressbar"]')).toBeNull();
  });

  it('working 阶段 live usage 优先于 ledger 显示速率与 ProgressBar', async () => {
    const staleLedger: AgentLedgerEntry = {
      id: 'l1',
      agentSessionId: 'a1',
      projectId: 'p1',
      worktreeId: 'wt-1',
      providerId: 'claudeCodeVisible',
      modelId: 'unknown-model',
      startedAt: '2026-08-16T10:00:00Z',
      endedAt: '2026-08-16T10:05:00Z',
      durationMs: 300_000,
      outcome: 'completed',
      inputTokens: 1,
      outputTokens: 1,
      cacheReadTokens: 0,
      cacheWriteTokens: 0,
      terminalTitle: null,
      costMinorUnits: null,
      costCurrency: null,
      createdAt: '2026-08-16T10:05:00Z',
      updatedAt: '2026-08-16T10:05:00Z',
    };
    renderCard(makeProps({ activeAgent: makeWorkingAgent(), ledgerEntry: staleLedger }));
    const rateRow = screen.getByTestId('workbench-status-token-rate-row');
    const meter = screen.getByTestId('workbench-status-context-meter');
    expect(rateRow.textContent).toContain('333.3 tok/s');
    expect(rateRow.textContent).toContain('33.3 tok/s');
    expect(meter.textContent).toContain('170.000k');
    expect(meter.textContent).toContain('200.000k');
    const bar = document.querySelector('[role="progressbar"]');
    expect(bar?.getAttribute('aria-valuenow')).toBe('85');
    expect(bar?.getAttribute('data-tone')).toBe('danger');
  });
});