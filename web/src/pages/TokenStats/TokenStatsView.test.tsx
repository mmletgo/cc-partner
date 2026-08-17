// @vitest-environment jsdom
/**
 * TokenStatsView 展示合同。
 *
 * Business Logic（为什么需要这个测试）:
 *   KPI 必须区分 null 与 0、主货币优先、筛选/导出/分页都要落到 props 回调；
 *   顶部不展示真实消耗 / 覆盖度；会话明细必须带标题。
 *
 * Code Logic（这个测试做什么）:
 *   mock recharts，渲染 I18n 包裹的 TokenStatsView，断言数字格式、未提供 hint、
 *   多货币 hint、window chip / 分页 / export 回调。
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
  LineChart: ({ children }: { children: unknown }) => <div>{children as never}</div>,
  CartesianGrid: () => null,
  XAxis: () => null,
  YAxis: () => null,
  Tooltip: () => null,
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
    {
      key: 'gpt-5',
      label: 'gpt-5',
      sessions: 1,
      completed: 1,
      failed: 0,
      cancelled: 0,
      disconnected: 0,
      inputTokens: 40,
      outputTokens: 20,
      cacheReadTokens: 0,
      cacheWriteTokens: null,
      costByCurrency: [],
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
    {
      key: 'codex',
      label: 'Codex',
      sessions: 1,
      completed: 1,
      failed: 0,
      cancelled: 0,
      disconnected: 0,
      inputTokens: 40,
      outputTokens: 20,
      cacheReadTokens: 0,
      cacheWriteTokens: null,
      costByCurrency: [],
    },
  ],
  byProject: [
    {
      key: 'p1',
      label: 'cc-partner',
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
    {
      key: 'p2',
      label: 'other-app',
      sessions: 1,
      completed: 1,
      failed: 0,
      cancelled: 0,
      disconnected: 0,
      inputTokens: 10,
      outputTokens: 5,
      cacheReadTokens: 0,
      cacheWriteTokens: null,
      costByCurrency: [],
    },
  ],
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
        inputTokens: 100,
        outputTokens: 50,
        cacheReadTokens: 30,
        cacheWriteTokens: null,
        costMinorUnits: 120,
        costCurrency: 'USD',
        terminalTitle: '修复登录超时',
        projectName: 'cc-partner',
      },
    ],
    pageIndex: 0,
    totalPages: 1,
    sessionFrom: 1,
    sessionTo: 1,
    sessionCount: 2,
    canPrevPage: false,
    canNextPage: true,
    loadingPage: false,
    loading: false,
    refreshError: 'idle',
    exporting: false,
    exportError: null,
    lastExportPath: null,
    onChangeFilter: vi.fn(),
    onPrevPage: vi.fn(),
    onNextPage: vi.fn(),
    onRefresh: vi.fn(),
    onExport: vi.fn(),
    onRevealExport: vi.fn(),
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
  it('渲染 KPI 数字与命中率，不展示真实消耗与覆盖度', () => {
    renderView();
    expect(screen.getByTestId('token-stats-kpi-input').textContent).toContain('100');
    expect(screen.getByTestId('token-stats-kpi-output').textContent).toContain('50');
    expect(screen.getByTestId('token-stats-kpi-hit-rate').textContent).toContain('23.1%');
    expect(screen.getByTestId('token-stats-kpi-requests').textContent).toContain('2');
    expect(screen.getByTestId('token-stats-kpi-cost').textContent).toContain('1.20 USD');
    expect(screen.getByTestId('token-stats-kpi-cost').textContent).toContain('8.00 CNY');
    expect(screen.queryByTestId('token-stats-kpi-real')).toBeNull();
    expect(screen.queryByTestId('token-stats-kpi-coverage')).toBeNull();
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

  it('切 30d chip 与分页 / Export 都会回调', () => {
    const { props } = renderView();
    fireEvent.click(screen.getByTestId('token-stats-window-30d'));
    expect(props.onChangeFilter).toHaveBeenCalledWith({
      window: '30d',
      startedAfter: null,
      startedBefore: null,
    });

    fireEvent.click(screen.getByTestId('token-stats-page-next'));
    expect(props.onNextPage).toHaveBeenCalledTimes(1);
    expect((screen.getByTestId('token-stats-page-prev') as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByTestId('token-stats-page-status').textContent).toContain('第 1 / 1 页');
    expect(screen.getByTestId('token-stats-page-range').textContent).toContain('第 1–1 条，共 2 条');

    fireEvent.click(screen.getByTestId('token-stats-export-menu'));
    fireEvent.click(screen.getByTestId('token-stats-export-csv'));
    expect(props.onExport).toHaveBeenCalledWith('csv');
  });

  it('自定义区间把本地时间转成 RFC3339 并清预设窗', () => {
    const { props } = renderView();
    fireEvent.click(screen.getByTestId('token-stats-window-custom'));
    expect(props.onChangeFilter).toHaveBeenCalledWith({ window: null });

    const { props: customProps } = renderView({ filter: { window: null } });
    fireEvent.change(screen.getByTestId('token-stats-custom-start'), {
      target: { value: '2026-06-01T08:00' },
    });
    const patch = (customProps.onChangeFilter as ReturnType<typeof vi.fn>).mock.calls.at(-1)?.[0] as {
      startedAfter: string | null;
      window: null;
    };
    expect(patch.window).toBeNull();
    expect(patch.startedAfter).toMatch(/^2026-06-0[12]T/);
  });

  it('导出成功可 reveal', () => {
    const { props } = renderView({ lastExportPath: '/tmp/x.csv' });
    fireEvent.click(screen.getByTestId('token-stats-export-reveal'));
    expect(props.onRevealExport).toHaveBeenCalledTimes(1);
  });

  it('reveal 失败保留路径并显示 revealFailed', () => {
    renderView({ lastExportPath: '/tmp/x.csv', exportError: 'no finder' });
    expect(screen.getByTestId('token-stats-export-toast').textContent).toContain('无法在访达中显示');
    expect(screen.getByTestId('token-stats-export-reveal')).toBeTruthy();
  });

  it('stale 横幅展示并可重试', () => {
    const { props } = renderView({ refreshError: 'stale' });
    expect(screen.getByTestId('token-stats-stale-banner')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: '重试' }));
    expect(props.onRefresh).toHaveBeenCalled();
  });

  it('session 明细表展示标题，项目列优先 projectName 缺失时回落到 projectId', () => {
    const { props } = renderView({
      entries: [
        {
          id: 'id-with-name',
          agentSessionId: 'a1',
          projectId: 'p1',
          worktreeId: null,
          providerId: 'claudeCodeVisible',
          modelId: 'claude-opus',
          startedAt: '2026-07-01T10:00:00Z',
          endedAt: '2026-07-01T10:05:00Z',
          durationMs: 0,
          inputTokens: null,
          outputTokens: null,
          cacheReadTokens: null,
          cacheWriteTokens: null,
          costMinorUnits: null,
          costCurrency: null,
          terminalTitle: '修复登录超时',
          projectName: 'cc-partner',
        },
        {
          id: 'id-no-name',
          agentSessionId: 'a2',
          projectId: 'p2',
          worktreeId: null,
          providerId: 'claudeCodeVisible',
          modelId: 'claude-opus',
          startedAt: '2026-07-01T11:00:00Z',
          endedAt: '2026-07-01T11:05:00Z',
          durationMs: 0,
          inputTokens: null,
          outputTokens: null,
          cacheReadTokens: null,
          cacheWriteTokens: null,
          costMinorUnits: null,
          costCurrency: null,
          terminalTitle: '   ',
          projectName: null,
        },
      ],
    });
    const table = screen.getByTestId('token-stats-session-table');
    expect(table.textContent).toContain('标题');
    expect(table.textContent).toContain('修复登录超时');
    expect(table.textContent).toContain('—');
    expect(table.textContent).toContain('cc-partner');
    expect(table.textContent).toContain('p2');
    // 标题列加入后共 9 列；不再有 outcome 表头/列
    expect(table.querySelectorAll('th').length).toBe(9);
    expect(props.onChangeFilter).toBeDefined();
  });

  it('时间 / provider / 模型 / 项目筛选各占一行', () => {
    renderView();
    const filters = screen.getByTestId('token-stats-filters');
    expect(filters.querySelector('[data-testid="token-stats-filter-row-time"]')).toBeTruthy();
    expect(filters.querySelector('[data-testid="token-stats-filter-row-provider"]')).toBeTruthy();
    expect(filters.querySelector('[data-testid="token-stats-filter-row-model"]')).toBeTruthy();
    expect(filters.querySelector('[data-testid="token-stats-filter-row-project"]')).toBeTruthy();
  });

  it('provider / 模型 / 项目可多选累加，不会互相覆盖', () => {
    const { props } = renderView({
      filter: { window: '7d', providerIds: ['claudeCodeVisible'] },
    });
    fireEvent.click(screen.getByTestId('token-stats-provider-codex'));
    expect(props.onChangeFilter).toHaveBeenCalledWith({
      providerIds: ['claudeCodeVisible', 'codex'],
    });

    fireEvent.click(screen.getByTestId('token-stats-model-gpt-5'));
    expect(props.onChangeFilter).toHaveBeenCalledWith({ modelIds: ['gpt-5'] });

    fireEvent.click(screen.getByTestId('token-stats-project-p2'));
    expect(props.onChangeFilter).toHaveBeenCalledWith({ projectIds: ['p2'] });
  });

  it('用量趋势合成一张三条曲线图', () => {
    renderView();
    expect(screen.getByTestId('token-stats-trend')).toBeTruthy();
    expect(screen.queryByTestId('token-stats-trend-input')).toBeNull();
    expect(screen.queryByTestId('token-stats-trend-cache')).toBeNull();
    expect(screen.queryByTestId('token-stats-trend-output')).toBeNull();
    const legend = screen.getByTestId('token-stats-trend-legend');
    expect(legend.textContent).toContain('新输入');
    expect(legend.textContent).toContain('缓存读取');
    expect(legend.textContent).toContain('输出');
  });
});
