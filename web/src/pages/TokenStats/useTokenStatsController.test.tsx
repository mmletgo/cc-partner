// @vitest-environment jsdom
/**
 * useTokenStatsController 单测
 *
 * Business Logic（为什么需要这个测试):
 *   Token 统计页是回顾性数据，控制器必须正确处理：首屏拉取 / 筛选防抖 / stale guard /
 *   Load more / 导出成功失败 / refreshError state machine。
 *
 * Code Logic（这个测试做什么):
 *   - vi.mock('@/api/tokenStats') 注入 fake tokenStatsApi；单 case 只调用必要方法。
 *   - renderHook + act 推进 React state；debounce 用真实 250ms 等待，避免
 *     fake timers 与 useVisibilityPolling 的 interval/waitFor 互相卡住。
 *   - 校验每次调用后的 filter / summary / entries / refreshError / cursor 变化。
 */

import { act, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type {
  AgentLedgerSessionEntry,
  AgentLedgerSessionPage,
  AgentLedgerSummary,
} from '@/lib/types/tokenStats';

vi.mock('@/api/tokenStats', () => ({
  tokenStatsApi: {
    summarize: vi.fn(),
    list: vi.fn(),
    clear: vi.fn(),
    export: vi.fn(),
    revealExport: vi.fn(),
  },
}));

import { tokenStatsApi } from '@/api/tokenStats';
import { useTokenStatsController } from './useTokenStatsController';

const mockedApi = tokenStatsApi as unknown as {
  summarize: ReturnType<typeof vi.fn>;
  list: ReturnType<typeof vi.fn>;
  export: ReturnType<typeof vi.fn>;
  revealExport: ReturnType<typeof vi.fn>;
};

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
  costByCurrency: [{ currency: 'USD', minorUnits: 120 }],
  totalCostByCurrency: [{ currency: 'USD', minorUnits: 120 }],
  usageCoverage: 'partial',
  byModel: [],
  byProvider: [],
  byProject: [],
  trend: [],
  bucket: 'day',
};

const PAGE: AgentLedgerSessionPage = {
  items: [
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
      terminalTitle: null,
      projectName: 'cc-partner',
    },
  ] satisfies AgentLedgerSessionEntry[],
  nextCursor: 'cursor-next',
};

describe('useTokenStatsController', () => {
  beforeEach(() => {
    mockedApi.summarize.mockReset();
    mockedApi.list.mockReset();
    mockedApi.export.mockReset();
    mockedApi.revealExport.mockReset();
    mockedApi.summarize.mockResolvedValue(SUMMARY);
    mockedApi.list.mockResolvedValue(PAGE);
  });

  afterEach(() => {
    vi.clearAllTimers();
  });

  it('首屏默认 7d window 并拉取 summary + list', async () => {
    const { result } = renderHook(() => useTokenStatsController());
    await waitFor(() => {
      expect(result.current.summary).toEqual(SUMMARY);
    });
    expect(result.current.filter.window).toBe('7d');
    expect(result.current.entries).toEqual(PAGE.items);
    expect(result.current.nextCursor).toBe('cursor-next');
    expect(result.current.refreshError).toBe('idle');
    expect(mockedApi.summarize).toHaveBeenCalledTimes(1);
    expect(mockedApi.list).toHaveBeenCalledTimes(1);
  });

  it('changeFilter 进入 250ms debounce，再触发 fetch；连续相同字符串不重复 fetch', async () => {
    const { result } = renderHook(() => useTokenStatsController());
    await waitFor(() => expect(result.current.summary).toEqual(SUMMARY));

    // 清空已有调用计数器
    mockedApi.summarize.mockClear();
    mockedApi.list.mockClear();

    act(() => result.current.onChangeFilter({ window: '30d' }));
    // 250ms 之前不应该已发请求
    expect(mockedApi.summarize).not.toHaveBeenCalled();

    await act(async () => {
      await new Promise((resolve) => {
        setTimeout(resolve, 280);
      });
    });

    await waitFor(() => {
      expect(mockedApi.summarize).toHaveBeenCalled();
    });
    expect(mockedApi.summarize).toHaveBeenCalledTimes(1);

    // 同样 patch 不会再次触发
    act(() => result.current.onChangeFilter({ window: '30d' }));
    await act(async () => {
      await new Promise((resolve) => {
        setTimeout(resolve, 280);
      });
    });
    expect(result.current.filter.window).toBe('30d');
    expect(mockedApi.summarize).toHaveBeenCalledTimes(1);
  });

  it('refresh 失败按已有数据分流：none→error；有数据→stale', async () => {
    mockedApi.summarize.mockRejectedValueOnce(new Error('boom'));
    const { result } = renderHook(() => useTokenStatsController());
    await waitFor(() => expect(result.current.refreshError).toBe('error'));

    // 第二次 refresh 成功让 UI 回到 idle
    mockedApi.summarize.mockResolvedValueOnce(SUMMARY);
    await act(async () => {
      result.current.onRefresh();
    });
    await waitFor(() => expect(result.current.refreshError).toBe('idle'));

    // 第三次 refresh 失败但已有数据，应标 stale
    mockedApi.summarize.mockRejectedValueOnce(new Error('flaky'));
    await act(async () => {
      result.current.onRefresh();
    });
    await waitFor(() => expect(result.current.refreshError).toBe('stale'));
  });

  it('onLoadMore 在 cursor 存在时拉下一页，并 append', async () => {
    const nextPage: AgentLedgerSessionPage = {
      items: [
        {
          ...PAGE.items[0],
          id: 'id-2',
          agentSessionId: 'a2',
        },
      ],
      nextCursor: null,
    };
    mockedApi.list
      .mockResolvedValueOnce(PAGE)
      .mockResolvedValueOnce(nextPage);

    const { result } = renderHook(() => useTokenStatsController());
    await waitFor(() => expect(result.current.entries).toHaveLength(1));

    await act(async () => {
      result.current.onLoadMore();
    });

    await waitFor(() => {
      expect(result.current.entries).toHaveLength(2);
    });
    expect(result.current.entries[0].id).toBe('id-1');
    expect(result.current.entries[1].id).toBe('id-2');
    expect(result.current.nextCursor).toBeNull();
  });

  it('onExport 成功暴露 lastExportPath，失败时暴露 exportError', async () => {
    mockedApi.export.mockResolvedValueOnce('/tmp/x.csv');
    const { result } = renderHook(() => useTokenStatsController());
    await waitFor(() => expect(result.current.summary).toEqual(SUMMARY));

    await act(async () => {
      result.current.onExport('csv');
    });

    await waitFor(() => {
      expect(result.current.lastExportPath).toBe('/tmp/x.csv');
      expect(result.current.exporting).toBe(false);
      expect(result.current.exportError).toBeNull();
    });

    // 失败路径：再次调用 onExport，mock reject
    mockedApi.export.mockRejectedValueOnce(new Error('disk full'));
    await act(async () => {
      result.current.onExport('json');
    });
    await waitFor(() => {
      expect(result.current.exportError).toMatch(/disk full/);
    });
    // 失败时清空 lastExportPath
    expect(result.current.lastExportPath).toBeNull();
  });

  it('onRevealExport 调 revealExport；失败写入 exportError 并保留路径', async () => {
    mockedApi.export.mockResolvedValueOnce('/tmp/x.csv');
    mockedApi.revealExport.mockRejectedValueOnce(new Error('no finder'));
    const { result } = renderHook(() => useTokenStatsController());
    await waitFor(() => expect(result.current.summary).toEqual(SUMMARY));

    await act(async () => {
      result.current.onExport('csv');
    });
    await waitFor(() => expect(result.current.lastExportPath).toBe('/tmp/x.csv'));

    await act(async () => {
      result.current.onRevealExport();
    });
    await waitFor(() => {
      expect(mockedApi.revealExport).toHaveBeenCalledWith('/tmp/x.csv');
      expect(result.current.exportError).toMatch(/no finder/);
    });
    expect(result.current.lastExportPath).toBe('/tmp/x.csv');
  });
});
