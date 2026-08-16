// @vitest-environment jsdom
/**
 * TokenStatsView 展示合同。
 *
 * Business Logic（为什么需要这个测试）:
 *   KPI 必须区分 null 与 0、主货币优先、筛选/导出/加载更多都要落到 props 回调。
 *
 * Code Logic（这个测试做什么）:
 *   mock recharts，渲染 I18n 包裹的 TokenStatsView，断言数字格式、未提供 hint、
 *   多货币 hint、window chip / load more / export 回调。
 */

import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { I18nextProvider } from 'react-i18next';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import i18n from '@/i18n';
import type { AgentLedgerSummary } from '@/lib/types/tokenStats';
import { TokenStatsView, type TokenStatsViewProps } from './TokenStatsView';

vi.mock('recharts', () => ({
  ResponsiveContainer: ({ children }: { children: unknown }) => (
    <div data-testid="recharts-mock">{children as never}</div>
  ),
  ComposedChart: ({ children }: { children: unknown }) => <div>{children as never}</div>,
  CartesianGrid: () => null,
  XAxis: () => null,
  YAxis: () => null,
  Tooltip: () => null,
  Bar: () => null,
  Line: () => null,
}));

const SUMMARY: AgentLedgerSummary = {
  window: '7d',
  projectId: null,
  sessions: 2,
  completed: 2,
  failed: 0,
  cancelled: 0,
  disconnected: 0,
  durationMs: 0,
  inputTokens: 100,
  outputTokens: 50,
  cacheReadTokens: 30,
  cacheWriteTokens: null,
  realConsumedTokens: 150,
  cacheHitRate: 0.2307,
  requestsCount: 2,
  costByCurrency: [
    { currency: 'USD', minorUnits: 120 },
    { currency: 'CNY', minorUnits: 800 },
  ],
  totalCostByCurrency: [
    { currency: 'USD', minorUnits: 120 },
    { currency: 'CNY', minorUnits: 800 },
  ],
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
      failed: 1,
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

/**
 * Business Logic（为什么需要）:
 *   每个用例只需覆盖关心的回调，不必手写整份 props。
 *
 * Code Logic（做什么）:
 *   填默认 filter/summary/空回调，再用 partial 覆盖。
 */
function renderView(partial: Partial<TokenStatsViewProps> = {}) {
  const props: TokenStatsViewProps = {
    filter: { window: '7d' },
    summary: SUMMARY,
    entries: [
      {
        id: 'id-1',
        agentSessionId: 'a1',
        projectId: 'p1',
        worktreeId: null,
        providerId: 'claudeCodeVisible',
        modelId: 'claude-opus',
        startedAt: '2026-07-01T10:00:00Z',
        endedAt: '2026-07-01T10:05:00Z',
        durationMs: 300_000,
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
    hasMore: true,
    loading: false,
    refreshError: 'idle',
    exporting: false,
    exportError: null,
    lastExportPath: null,
    onChangeFilter: vi.fn(),
    onLoadMore: vi.fn(),
    onRefresh: vi.fn(),
    onExport: vi.fn(),
    onDismissExport: vi.fn(),
    ...partial,
  };
  return {
    props,
    ...render(
      <I18nextProvider i18n={i18n}>
        <TokenStatsView {...props} />
      </I18nextProvider>,
    ),
  };
}

beforeAll(async () => {
  await i18n.changeLanguage('zh');
});

afterEach(() => {
  cleanup();
});

describe('TokenStatsView', () => {
  it('渲染 KPI 数字、命中率与覆盖度', () => {
    renderView();
    expect(screen.getByTestId('token-stats-kpi-input').textContent).toContain('100');
    expect(screen.getByTestId('token-stats-kpi-output').textContent).toContain('50');
    expect(screen.getByTestId('token-stats-kpi-hit-rate').textContent).toContain('23.1%');
    expect(screen.getByTestId('token-stats-kpi-real').textContent).toContain('150');
    expect(screen.getByTestId('token-stats-kpi-requests').textContent).toContain('2');
    expect(screen.getByTestId('token-stats-kpi-coverage').textContent).toContain('部分');
    expect(screen.getByTestId('token-stats-kpi-cost').textContent).toContain('1.20 USD');
    expect(screen.getByTestId('token-stats-kpi-cost').textContent).toContain('8.00 CNY');
  });

  it('null token 显示占位并标未提供', () => {
    renderView({
      summary: {
        ...SUMMARY,
        inputTokens: null,
        cacheReadTokens: null,
        outputTokens: null,
        cacheHitRate: null,
        realConsumedTokens: null,
        totalCostByCurrency: [],
      },
    });
    expect(screen.getByTestId('token-stats-kpi-input').textContent).toContain('—');
    expect(screen.getByTestId('token-stats-kpi-input').textContent).toContain('未提供');
    expect(screen.getByTestId('token-stats-kpi-hit-rate').textContent).toContain('—');
    expect(screen.getByTestId('token-stats-kpi-cost').textContent).toContain('—');
  });

  it('切 30d chip 与 Load more / Export 都会回调', () => {
    const { props } = renderView();
    fireEvent.click(screen.getByTestId('token-stats-window-30d'));
    expect(props.onChangeFilter).toHaveBeenCalledWith({ window: '30d' });

    fireEvent.click(screen.getByTestId('token-stats-load-more'));
    expect(props.onLoadMore).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByTestId('token-stats-export-menu'));
    fireEvent.click(screen.getByTestId('token-stats-export-csv'));
    expect(props.onExport).toHaveBeenCalledWith('csv');
  });

  it('stale 横幅展示并可重试', () => {
    const { props } = renderView({ refreshError: 'stale' });
    expect(screen.getByTestId('token-stats-stale-banner')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: '重试' }));
    expect(props.onRefresh).toHaveBeenCalled();
  });
});
