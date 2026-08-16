/**
 * useTokenStatsController - Token 统计页数据与状态机
 *
 * Business Logic（为什么需要这个 hook):
 *   Token 统计是回顾性数据，按窗口/agent/模型/项目多维筛选；首屏失败要可重试，
 *   刷新失败要保留列表 + 顶部 stale 横幅（与 ActivityStats 同款）。同时要导出 CSV/JSON
 *   并把导出路径透露给 view。控制器集中所有 @/api 调用（视图不直接 import transport，
 *   符合巨型页 controller/view 拆分合同），并以 requestSeq 防 stale 写入。
 *
 * Code Logic（这个 hook 做什么):
 *   - filter 默认 7d；窗口 chip / provider/model/project 多选 / outcome 全部走
 *     `changeFilter(patch)`，按 250ms debounce 把新过滤器合并入实际 fetch。
 *   - `refresh()` 并发拉 summarize + list(cursor=null, limit=50)；成功合并 summary/
 *     entries，stale guard 通过 `requestSeqRef` 丢弃旧响应；失败按 `summary` 是否存在
 *     分流 loadFailed / staleRefreshFailed。
 *   - `loadMore()` 仅在还有 cursor 时翻页，limit=50；与 refresh 并发，单独走
 *     `listSeqRef`。
 *   - `exportNow(format)` 调 tokenStatsApi.export 把响应绝对路径返回；写盘失败标记
 *     exportError 不挡页面。
 *   - 整页 hooks 必须挂在早期 early-return 前；useVisibilityPolling(30s) 仅文档可见时
 *     拉数据。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { tokenStatsApi } from '@/api/tokenStats';
import { useVisibilityPolling } from '@/hooks/useVisibilityPolling';
import type {
  AgentLedgerFilters,
  AgentLedgerSummary,
  AgentLedgerSessionEntry,
} from '@/lib/types/tokenStats';

/** 视图层消费的 controller 错误原因；具体文案由 view 用 t() 选择。 */
export type TokenStatsRefreshState = 'idle' | 'loading' | 'stale' | 'error';

/** 单次刷新结果码（用于 stale 分流）。 */
const REFRESH_FAILURE_LOAD = 'load';
const REFRESH_FAILURE_STALE = 'stale';

export interface TokenStatsControllerResult {
  filter: AgentLedgerFilters;
  summary: AgentLedgerSummary | null;
  entries: AgentLedgerSessionEntry[];
  nextCursor: string | null;
  loading: boolean;
  refreshError: TokenStatsRefreshState;
  /** 'idle' = 不弹横幅；'loading' = 首屏加载；'stale' = 已有数据但刷新失败；'error' = 首屏失败。 */
  exporting: boolean;
  exportError: string | null;
  /** 导出成功后的绝对路径（最近一次成功），给 "在 Finder 显示" 按钮使用。 */
  lastExportPath: string | null;
  hasMore: boolean;
  onChangeFilter(patch: Partial<AgentLedgerFilters>): void;
  onLoadMore(): void;
  onRefresh(): void;
  onExport(format: 'csv' | 'json'): void;
  onDismissExport(): void;
}

/** 默认窗口 7d，与现有统计页保持一致。 */
const DEFAULT_FILTER: AgentLedgerFilters = { window: '7d' };

/** 拉取新数据时的轮询间隔（ms）。 */
const REFRESH_INTERVAL_MS = 30_000;
/** 列表分页大小，与 schema 默认 50 一致。 */
const PAGE_SIZE = 50;
/** 连续筛选防抖，避免快速切换造成未完成请求堆积。 */
const FILTER_DEBOUNCE_MS = 250;

/** 过滤器的稳定字符串表示，用于按值比较与去抖触发判断。 */
function stringifyFilter(filter: AgentLedgerFilters): string {
  const parts: string[] = [
    filter.window ?? '',
    filter.projectId ?? '',
    filter.outcome ?? '',
    filter.bucket ?? '',
    filter.startedAfter ?? '',
    filter.startedBefore ?? '',
    filter.worktreeId ?? '',
  ];
  parts.push((filter.providerIds ?? []).slice().sort().join(','));
  parts.push((filter.modelIds ?? []).slice().sort().join(','));
  parts.push((filter.projectIds ?? []).slice().sort().join(','));
  return parts.join('|');
}

export function useTokenStatsController(): TokenStatsControllerResult {
  // 0. 持久化的当前过滤器（用户输入）
  const [filter, setFilterState] = useState<AgentLedgerFilters>(DEFAULT_FILTER);
  // 1. 实际驱动 fetch 的"应用过滤器"（带 debounce）
  const [appliedFilter, setAppliedFilter] = useState<AgentLedgerFilters>(DEFAULT_FILTER);
  // 2. 派生数据
  const [summary, setSummary] = useState<AgentLedgerSummary | null>(null);
  const [entries, setEntries] = useState<AgentLedgerSessionEntry[]>([]);
  const [nextCursor, setNextCursor] = useState<string | null>(null);
  // 3. 状态机
  const [loading, setLoading] = useState(true);
  const [refreshError, setRefreshError] =
    useState<TokenStatsRefreshState>('loading');
  const [exporting, setExporting] = useState(false);
  const [exportError, setExportError] = useState<string | null>(null);
  const [lastExportPath, setLastExportPath] = useState<string | null>(null);

  // refs 防 stale 写入
  const requestSeqRef = useRef(0);
  const listSeqRef = useRef(0);
  const summaryRef = useRef<AgentLedgerSummary | null>(null);
  const appliedFilterRef = useRef<AgentLedgerFilters>(DEFAULT_FILTER);

  // 同步 ref 供 timeout 与 polling 读取
  useEffect(() => {
    summaryRef.current = summary;
  }, [summary]);
  useEffect(() => {
    appliedFilterRef.current = appliedFilter;
  }, [appliedFilter]);

  // 合并 filter 防抖：每次 setFilter 后 250ms 把应用过滤器推进。
  const debounceTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const changeFilter = useCallback((patch: Partial<AgentLedgerFilters>) => {
    setFilterState((prev) => {
      const next = { ...prev, ...patch };
      // 浅合并后立即比较：相同字面不再触发
      if (stringifyFilter(prev) === stringifyFilter(next)) {
        return prev;
      }
      if (debounceTimerRef.current) clearTimeout(debounceTimerRef.current);
      debounceTimerRef.current = setTimeout(() => {
        setAppliedFilter(next);
        debounceTimerRef.current = null;
      }, FILTER_DEBOUNCE_MS);
      return next;
    });
  }, []);

  // 主刷新：清旧 entries，取首屏
  const refresh = useCallback(async () => {
    const seq = ++requestSeqRef.current;
    const targetFilter = appliedFilterRef.current;
    try {
      const [nextSummary, page] = await Promise.all([
        tokenStatsApi.summarize(targetFilter),
        tokenStatsApi.list(targetFilter, null, PAGE_SIZE),
      ]);
      if (seq !== requestSeqRef.current) return; // 旧请求，丢弃
      setSummary(nextSummary);
      summaryRef.current = nextSummary;
      setEntries(page.items);
      setNextCursor(page.nextCursor);
      setRefreshError('idle');
    } catch {
      if (seq !== requestSeqRef.current) return;
      // 已有数据 → stale；否则 first-load error
      setRefreshError(summaryRef.current ? 'stale' : 'error');
    } finally {
      if (seq === requestSeqRef.current) setLoading(false);
    }
  }, []);

  // 首屏只走 appliedFilter effect；runImmediately 关掉避免与 effect 叠成双请求。
  const { runNow } = useVisibilityPolling(refresh, {
    intervalMs: REFRESH_INTERVAL_MS,
    runImmediately: false,
  });

  // 监听 appliedFilter：挂载与筛选落地时各拉一次。
  useEffect(() => {
    void runNow({ force: true }).catch(() => undefined);
  }, [appliedFilter, runNow]);

  // 列表加载更多（cursor 分页）
  const loadMore = useCallback(async () => {
    if (!nextCursor) return;
    const seq = ++listSeqRef.current;
    try {
      const page = await tokenStatsApi.list(appliedFilterRef.current, nextCursor, PAGE_SIZE);
      if (seq !== listSeqRef.current) return;
      setEntries((prev) => [...prev, ...page.items]);
      setNextCursor(page.nextCursor);
    } catch {
      // 分页失败不打断已有列表；用户可再点「加载更多」。
    }
  }, [nextCursor]);

  // 手动刷新（onRefresh 给 view 当 retry 按钮用）
  const handleRefresh = useCallback(() => {
    void runNow({ force: true }).catch(() => undefined);
  }, [runNow]);

  // 导出
  const exportNow = useCallback(
    async (format: 'csv' | 'json') => {
      if (exporting) return;
      setExporting(true);
      setExportError(null);
      try {
        const path = await tokenStatsApi.export(appliedFilterRef.current, format);
        setLastExportPath(path);
      } catch (e) {
        const message = e instanceof Error ? e.message : String(e);
        setExportError(message);
        setLastExportPath(null);
      } finally {
        setExporting(false);
      }
    },
    [exporting],
  );

  const dismissExport = useCallback(() => {
    setExportError(null);
    setLastExportPath(null);
  }, []);

  // 卸载清理 debounce 计时
  useEffect(() => {
    return () => {
      if (debounceTimerRef.current) clearTimeout(debounceTimerRef.current);
    };
  }, []);

  const hasMore = useMemo(() => nextCursor !== null, [nextCursor]);

  return {
    filter,
    summary,
    entries,
    nextCursor,
    loading,
    refreshError,
    exporting,
    exportError,
    lastExportPath,
    hasMore,
    onChangeFilter: changeFilter,
    onLoadMore: loadMore,
    onRefresh: handleRefresh,
    onExport: exportNow,
    onDismissExport: dismissExport,
  };
}

// 仅在本文件内使用的失败常量导出；保留避免后续扩展需要时散落。
export const _TOKEN_STATS_REFRESH_FAILURE = {
  load: REFRESH_FAILURE_LOAD,
  stale: REFRESH_FAILURE_STALE,
} as const;
