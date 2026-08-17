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
 *   - filter 默认 7d；窗口 chip / provider/model/project 多选全部走
 *     `changeFilter(patch)`，按 250ms debounce 把新过滤器合并入实际 fetch。
 *   - `refresh()` 并发拉 summarize + list(cursor=null, limit=20)；成功合并 summary/
 *     并重置到第 1 页，stale guard 通过 `requestSeqRef` 丢弃旧响应；失败按 `summary`
 *     是否存在分流 loadFailed / staleRefreshFailed。
 *   - 会话明细按页替换（每页 20 条）：下一页用 nextCursor 拉新页并缓存，上一页走缓存；
 *     与 refresh 并发时走 `listSeqRef` 丢弃过期响应。
 *   - `exportNow(format)` 调 tokenStatsApi.export 把响应绝对路径返回；写盘失败标记
 *     exportError 不挡页面。
 *   - 整页 hooks 必须挂在早期 early-return 前；useVisibilityPolling(30s) 仅文档可见时
 *     拉数据。
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import { tokenStatsApi } from '@/api/tokenStats';
import { useVisibilityPolling } from '@/hooks/useVisibilityPolling';
import type {
  AgentLedgerFilters,
  AgentLedgerSummary,
  AgentLedgerSessionEntry,
  AgentLedgerSessionPage,
  TokenStatsFacetOptions,
} from '@/lib/types/tokenStats';

/** 视图层消费的 controller 错误原因；具体文案由 view 用 t() 选择。 */
export type TokenStatsRefreshState = 'idle' | 'loading' | 'stale' | 'error';

/** 单次刷新结果码（用于 stale 分流）。 */
const REFRESH_FAILURE_LOAD = 'load';
const REFRESH_FAILURE_STALE = 'stale';

export interface TokenStatsControllerResult {
  filter: AgentLedgerFilters;
  summary: AgentLedgerSummary | null;
  /** 时间窗内的完整 provider/model/project 选项，不受当前多选收缩。 */
  facetOptions: TokenStatsFacetOptions | null;
  entries: AgentLedgerSessionEntry[];
  /** 当前页（0-based）。 */
  pageIndex: number;
  /** 总页数；无会话时为 0。 */
  totalPages: number;
  /** 当前页第一条在全量中的 1-based 序号；空页为 0。 */
  sessionFrom: number;
  /** 当前页最后一条在全量中的 1-based 序号。 */
  sessionTo: number;
  /** 筛选窗内会话总数（来自 summary）。 */
  sessionCount: number;
  canPrevPage: boolean;
  canNextPage: boolean;
  /** 正在拉取尚未缓存的下一页。 */
  loadingPage: boolean;
  loading: boolean;
  refreshError: TokenStatsRefreshState;
  /** 'idle' = 不弹横幅；'loading' = 首屏加载；'stale' = 已有数据但刷新失败；'error' = 首屏失败。 */
  exporting: boolean;
  exportError: string | null;
  /** 导出成功后的绝对路径（最近一次成功），给 "在 Finder 显示" 按钮使用。 */
  lastExportPath: string | null;
  onChangeFilter(patch: Partial<AgentLedgerFilters>): void;
  onPrevPage(): void;
  onNextPage(): void;
  onRefresh(): void;
  onExport(format: 'csv' | 'json'): void;
  onRevealExport(): void;
  onDismissExport(): void;
}

/** 默认窗口 7d，与现有统计页保持一致。 */
const DEFAULT_FILTER: AgentLedgerFilters = { window: '7d' };

/** 拉取新数据时的轮询间隔（ms）。 */
const REFRESH_INTERVAL_MS = 30_000;
/** 会话明细每页条数。 */
export const TOKEN_STATS_PAGE_SIZE = 20;
/** 连续筛选防抖，避免快速切换造成未完成请求堆积。 */
const FILTER_DEBOUNCE_MS = 250;

/** 过滤器的稳定字符串表示，用于按值比较与去抖触发判断。 */
function stringifyFilter(filter: AgentLedgerFilters): string {
  const parts: string[] = [
    filter.window ?? '',
    filter.projectId ?? '',
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

/** 时间窗键：筛选项目录只随窗口/自定义区间变化，不随维度多选收缩。 */
function timeScopeKey(filter: AgentLedgerFilters): string {
  return `${filter.window ?? ''}|${filter.startedAfter ?? ''}|${filter.startedBefore ?? ''}|${filter.bucket ?? ''}`;
}

/** 当前是否启用了 provider/model/project 维度筛选。 */
function dimensionFilterActive(filter: AgentLedgerFilters): boolean {
  return (
    (filter.providerIds?.length ?? 0) > 0 ||
    (filter.modelIds?.length ?? 0) > 0 ||
    (filter.projectIds?.length ?? 0) > 0 ||
    Boolean(filter.projectId)
  );
}

/** 只带时间窗的 summarize 请求，用来刷新筛选项目录。 */
function facetOnlyFilter(filter: AgentLedgerFilters): AgentLedgerFilters {
  return {
    window: filter.window,
    startedAfter: filter.startedAfter,
    startedBefore: filter.startedBefore,
    bucket: filter.bucket,
  };
}

/** 从 summary 抽出 chip 目录。 */
function facetsFromSummary(summary: AgentLedgerSummary): TokenStatsFacetOptions {
  return {
    providers: summary.byProvider,
    models: summary.byModel,
    projects: summary.byProject,
  };
}

/**
 * Business Logic（为什么需要）:
 *   分页控件需要总页数；summary.sessions 是权威总数，cursor 可在数据刚增长时补一页。
 *
 * Code Logic（做什么）:
 *   用 ceil(sessionCount / 20) 作下限，再与当前页、是否还有下一页取 max。
 */
export function tokenStatsTotalPages(
  sessionCount: number,
  pageIndex: number,
  hasNextPage: boolean,
): number {
  const fromCount = sessionCount > 0 ? Math.ceil(sessionCount / TOKEN_STATS_PAGE_SIZE) : 0;
  const fromCursor = hasNextPage
    ? pageIndex + 2
    : sessionCount > 0 || pageIndex > 0
      ? pageIndex + 1
      : 0;
  return Math.max(fromCount, fromCursor);
}

export function useTokenStatsController(): TokenStatsControllerResult {
  // 0. 持久化的当前过滤器（用户输入）
  const [filter, setFilterState] = useState<AgentLedgerFilters>(DEFAULT_FILTER);
  // 1. 实际驱动 fetch 的"应用过滤器"（带 debounce）
  const [appliedFilter, setAppliedFilter] = useState<AgentLedgerFilters>(DEFAULT_FILTER);
  // 2. 派生数据
  const [summary, setSummary] = useState<AgentLedgerSummary | null>(null);
  const [facetOptions, setFacetOptions] = useState<TokenStatsFacetOptions | null>(null);
  const [pages, setPages] = useState<AgentLedgerSessionPage[]>([]);
  const [pageIndex, setPageIndex] = useState(0);
  const [loadingPage, setLoadingPage] = useState(false);
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
  const facetOptionsRef = useRef<TokenStatsFacetOptions | null>(null);
  const facetTimeRef = useRef<string | null>(null);
  const pagesRef = useRef<AgentLedgerSessionPage[]>([]);
  const pageIndexRef = useRef(0);

  // 同步 ref 供 timeout 与 polling 读取
  useEffect(() => {
    summaryRef.current = summary;
  }, [summary]);
  useEffect(() => {
    appliedFilterRef.current = appliedFilter;
  }, [appliedFilter]);
  useEffect(() => {
    facetOptionsRef.current = facetOptions;
  }, [facetOptions]);
  useEffect(() => {
    pagesRef.current = pages;
  }, [pages]);
  useEffect(() => {
    pageIndexRef.current = pageIndex;
  }, [pageIndex]);

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
    listSeqRef.current += 1;
    setLoadingPage(false);
    const targetFilter = appliedFilterRef.current;
    const nextTimeKey = timeScopeKey(targetFilter);
    const shouldRefreshFacets =
      nextTimeKey !== facetTimeRef.current || facetOptionsRef.current == null;
    const needFacetFetch = dimensionFilterActive(targetFilter) && shouldRefreshFacets;
    try {
      const [nextSummary, page, facetSummary] = await Promise.all([
        tokenStatsApi.summarize(targetFilter),
        tokenStatsApi.list(targetFilter, null, TOKEN_STATS_PAGE_SIZE),
        needFacetFetch
          ? tokenStatsApi.summarize(facetOnlyFilter(targetFilter))
          : Promise.resolve(null),
      ]);
      if (seq !== requestSeqRef.current) return; // 旧请求，丢弃
      listSeqRef.current += 1;
      setLoadingPage(false);
      setSummary(nextSummary);
      summaryRef.current = nextSummary;
      if (!dimensionFilterActive(targetFilter)) {
        const nextFacets = facetsFromSummary(nextSummary);
        setFacetOptions(nextFacets);
        facetOptionsRef.current = nextFacets;
        facetTimeRef.current = nextTimeKey;
      } else if (facetSummary) {
        const nextFacets = facetsFromSummary(facetSummary);
        setFacetOptions(nextFacets);
        facetOptionsRef.current = nextFacets;
        facetTimeRef.current = nextTimeKey;
      }
      setPages([page]);
      setPageIndex(0);
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

  const goPrevPage = useCallback(() => {
    if (loadingPage) return;
    setPageIndex((index) => Math.max(0, index - 1));
  }, [loadingPage]);

  const goNextPage = useCallback(async () => {
    if (loadingPage) return;
    const index = pageIndexRef.current;
    const cached = pagesRef.current;
    if (index + 1 < cached.length) {
      setPageIndex(index + 1);
      return;
    }
    const cursor = cached[index]?.nextCursor;
    if (!cursor) return;
    const seq = ++listSeqRef.current;
    setLoadingPage(true);
    try {
      const page = await tokenStatsApi.list(
        appliedFilterRef.current,
        cursor,
        TOKEN_STATS_PAGE_SIZE,
      );
      if (seq !== listSeqRef.current) return;
      setPages((prev) => (prev.length > index + 1 ? prev : [...prev, page]));
      setPageIndex(index + 1);
    } catch {
      // 翻页失败保留当前页；用户可再点下一页。
    } finally {
      if (seq === listSeqRef.current) setLoadingPage(false);
    }
  }, [loadingPage]);

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

  const revealExport = useCallback(async () => {
    if (!lastExportPath) return;
    try {
      await tokenStatsApi.revealExport(lastExportPath);
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      setExportError(message);
    }
  }, [lastExportPath]);

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

  const entries = pages[pageIndex]?.items ?? [];
  const hasNextPage =
    pageIndex + 1 < pages.length || Boolean(pages[pageIndex]?.nextCursor);
  const sessionCount = summary?.sessions ?? 0;
  const totalPages = tokenStatsTotalPages(sessionCount, pageIndex, hasNextPage);
  const sessionFrom = entries.length === 0 ? 0 : pageIndex * TOKEN_STATS_PAGE_SIZE + 1;
  const sessionTo = pageIndex * TOKEN_STATS_PAGE_SIZE + entries.length;

  return {
    filter,
    summary,
    facetOptions,
    entries,
    pageIndex,
    totalPages,
    sessionFrom,
    sessionTo,
    sessionCount,
    canPrevPage: pageIndex > 0 && !loadingPage,
    canNextPage: hasNextPage && !loadingPage,
    loadingPage,
    loading,
    refreshError,
    exporting,
    exportError,
    lastExportPath,
    onChangeFilter: changeFilter,
    onPrevPage: goPrevPage,
    onNextPage: goNextPage,
    onRefresh: handleRefresh,
    onExport: exportNow,
    onRevealExport: revealExport,
    onDismissExport: dismissExport,
  };
}

// 仅在本文件内使用的失败常量导出；保留避免后续扩展需要时散落。
export const _TOKEN_STATS_REFRESH_FAILURE = {
  load: REFRESH_FAILURE_LOAD,
  stale: REFRESH_FAILURE_STALE,
} as const;
