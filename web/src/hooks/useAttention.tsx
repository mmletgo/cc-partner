/**
 * AttentionProvider — 共享 Inbox 快照状态机。
 *
 * Business Logic（为什么需要这个模块）:
 *   桌面与移动端各挂一个 Provider，但必须共用同一套 load/stale/request-sequence 语义；
 *   刷新时机：挂载、focus、可见恢复、手动 refresh、可见时每 10 秒轮询。
 *
 * Code Logic（这个模块做什么）:
 *   接受 loadSnapshot prop；用 attentionReducer + requestId 管理状态；
 *   注册 focus/visibility/interval 监听并在卸载时清理；
 *   in-flight load 采用 single-flight/coalescing，避免 10s 轮询饿死慢请求；
 *   任何加载失败（硬超时或普通 reject）都丢弃轮询 pending、展示错误，
 *   不得因 poll-pending 立即连环再起请求；仅成功后才消费合并的轮询意图。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ReactNode } from 'react';

import type { AttentionSnapshot } from '@/lib/types';
import { classifyTransportFault, planFaultRecovery } from '@/lib/faultRecovery';
import { AttentionContext, type AttentionContextValue } from './attentionContext';
import { subscribeAttentionInvalidation } from './attentionInvalidation';
import {
  attentionReducer,
  createInitialAttentionState,
  isCurrentAttentionRequest,
  nextAttentionRequestId,
  type AttentionViewState,
} from './attentionState';

/** 页面可见时的兜底轮询间隔（毫秒）。 */
export const ATTENTION_POLL_INTERVAL_MS = 10_000;

/**
 * 单次 loadSnapshot 硬超时（毫秒）。
 *
 * Business Logic（为什么需要这个常量）:
 *   若 loadSnapshot 永不 settle（半开连接/挂起 invoke），single-flight 的 inFlight 会永久锁死，
 *   后续轮询/focus/手动刷新只能 mark pending 而无法恢复。
 *
 * Code Logic（这个常量做什么）:
 *   超时后 reject，finally 清除 inFlight；显式 refresh 可启动新请求。
 */
export const ATTENTION_LOAD_TIMEOUT_MS = 35_000;

/**
 * AttentionProvider props。
 *
 * Business Logic（为什么需要这个类型）:
 *   桌面注入 Tauri loader、移动注入 HTTP loader，状态机完全共享。
 *
 * Code Logic（字段说明）:
 *   loadSnapshot 返回完整 AttentionSnapshot；children 为子树。
 */
export interface AttentionProviderProps {
  children: ReactNode;
  loadSnapshot: () => Promise<AttentionSnapshot>;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   超时错误需要与业务失败区分，以便清除轮询 pending 并暂停自动重试。
 *
 * Code Logic（这个函数做什么）:
 *   识别 withAttentionLoadTimeout 抛出的超时 Error 文案。
 */
function isAttentionLoadTimeoutError(error: unknown): boolean {
  if (!(error instanceof Error)) return false;
  return error.message.includes('Attention 加载超时');
}

/**
 * Business Logic（为什么需要这个组件）:
 *   全局 Inbox 需要跨导航保持 badge 与列表一致，并在焦点/可见时刷新。
 *
 * Code Logic（这个组件做什么）:
 *   维护 AttentionViewState；显式刷新可递增 requestId 使旧响应失效；
 *   区分轮询 pending 与显式 force pending：任何失败都丢弃轮询 pending，
 *   硬超时额外暂停自动轮询（仅 force 路径恢复）；普通失败可等下个 interval 或显式 force；
 *   可见时 setInterval 10s；hidden 暂停。
 */
export function AttentionProvider({ children, loadSnapshot }: AttentionProviderProps) {
  const [state, setState] = useState<AttentionViewState>(() => createInitialAttentionState());
  const requestIdRef = useRef(0);
  const mountedRef = useRef(true);
  const stateRef = useRef(state);
  const loadSnapshotRef = useRef(loadSnapshot);
  /** 是否已有 load 正在执行（single-flight 互斥）。 */
  const inFlightRef = useRef(false);
  /**
   * 显式刷新意图（挂载/focus/可见/手动/invalidation）。
   * 超时后仍可在 in-flight 结束后继续消化一次。
   */
  const pendingForceRefreshRef = useRef(false);
  /**
   * 定时轮询刷新意图。任何失败后都必须丢弃，避免「失败→立刻再请求」死循环
   * （含移动端 30s HTTP 超时这类 timedOut=false 的普通 reject）。
   */
  const pendingPollRefreshRef = useRef(false);
  /**
   * 硬超时后暂停自动轮询，直到下一次 force 刷新（focus/手动/invalidation/可见恢复）。
   * 避免可见页面上 10s 定时器在错误态下立即再起挂起请求。
   */
  const allowAutomaticPollRef = useRef(true);

  useEffect(() => {
    stateRef.current = state;
  }, [state]);

  useEffect(() => {
    loadSnapshotRef.current = loadSnapshot;
  }, [loadSnapshot]);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   所有触发源（挂载/focus/可见/轮询/手动）必须走同一 load 路径与 stale guard；
   *   慢聚合（远端 open-project/task-list 可达 30s）不能被 10s 轮询反复废弃；
   *   永久挂起或提前 reject 的 loader 都必须释放 single-flight 并展示错误，
   *   不能靠 poll-pending 在失败后立即连环再起请求（含移动端 30s HTTP 超时）。
   *
   * Code Logic（这个函数做什么）:
   *   force=false（轮询）：若已 in-flight 只 mark poll-pending；
   *   force=true（挂载/focus/可见/手动/invalidation）：可 bump requestId，in-flight 时 mark force-pending；
   *   loadSnapshot 包 ATTENTION_LOAD_TIMEOUT_MS；
   *   **任何失败**都丢弃 poll-pending 且不自动链式；仅成功后才消费 poll-pending；
   *   硬超时额外暂停自动轮询，仅 force 路径恢复；普通失败等待下个 interval 或显式 force；
   *   while 循环只消化 force-pending，或成功后的 poll-pending。
   */
  const runLoad = useCallback(async (options?: { force?: boolean }) => {
    let force = options?.force === true;

    // 可能连续消化多次 pending（每次 loop 最多一个 in-flight）。
    while (true) {
      if (force) {
        // force 路径重新允许自动轮询（超时熔断后的恢复入口）。
        allowAutomaticPollRef.current = true;
      }

      if (inFlightRef.current) {
        // 已有请求：区分轮询与显式意图，不启动新请求（避免风暴与饥饿）。
        if (force) {
          pendingForceRefreshRef.current = true;
          // 显式刷新仍推进 requestId，使更早的 in-flight 结果被判定为 stale。
          requestIdRef.current = nextAttentionRequestId(requestIdRef.current);
        } else {
          pendingPollRefreshRef.current = true;
        }
        return;
      }

      if (!force && !allowAutomaticPollRef.current) {
        // 超时熔断期间忽略纯轮询，等待用户/焦点显式恢复。
        return;
      }

      inFlightRef.current = true;
      // 启动前清空两类 pending；本轮结束后再按需链式。
      pendingForceRefreshRef.current = false;
      pendingPollRefreshRef.current = false;
      const requestId = nextAttentionRequestId(requestIdRef.current);
      requestIdRef.current = requestId;
      const hasSnapshot = stateRef.current.snapshot !== null;
      setState((current) =>
        attentionReducer(current, {
          type: 'loadStarted',
          hasSnapshot: current.snapshot !== null,
        }),
      );

      let loadSucceeded = false;
      try {
        const snapshot = await withAttentionLoadTimeout(loadSnapshotRef.current());
        loadSucceeded = true;
        if (mountedRef.current && isCurrentAttentionRequest(requestId, requestIdRef.current)) {
          const receivedAt = new Date().toISOString();
          setState((current) =>
            attentionReducer(current, {
              type: 'loadSucceeded',
              snapshot,
              receivedAt,
            }),
          );
        }
      } catch (reason) {
        // 任何失败（硬超时或业务/HTTP reject）都必须丢弃 poll-pending：
        // 否则 in-flight 期间被 10s 轮询 mark 的 pending 会在失败后立即 force 链式，
        // 移动端 30s HTTP 超时（timedOut=false）会无限 loading 并清掉错误态。
        pendingPollRefreshRef.current = false;
        if (isAttentionLoadTimeoutError(reason)) {
          // 硬超时额外暂停自动轮询，避免可见页上 interval 立刻再起挂起请求。
          allowAutomaticPollRef.current = false;
        }
        if (mountedRef.current && isCurrentAttentionRequest(requestId, requestIdRef.current)) {
          const error = reason instanceof Error ? reason : new Error(String(reason));
          // 用 typed 故障分类决定 keepStale vs clear，避免依赖中文文案匹配。
          // Attention 刷新失败：已有快照时除 malformedJson 外一律 keepStale（unknown 默认 clear
          // 不得抹掉 Inbox 列表）；初次加载无缓存时仍可 clear。
          const classification = classifyTransportFault(reason);
          setState((current) => {
            const hasCache = current.snapshot !== null || hasSnapshot;
            const plan = planFaultRecovery({
              classification,
              hasCache,
              optimisticApplied: false,
            });
            const keepSnapshot =
              hasCache && classification.kind !== 'malformedJson' ? true : plan.keepCache;
            return attentionReducer(current, {
              type: 'loadFailed',
              error,
              hasSnapshot: keepSnapshot,
            });
          });
        }
      } finally {
        inFlightRef.current = false;
      }

      if (!mountedRef.current) {
        return;
      }

      // 显式 force pending 始终优先消化（手动/focus/invalidation 不得被失败吞掉）。
      if (pendingForceRefreshRef.current) {
        pendingForceRefreshRef.current = false;
        pendingPollRefreshRef.current = false;
        force = true;
        continue;
      }

      // 仅成功后才消费 in-flight 期间合并的轮询意图；失败路径已清空 poll-pending。
      if (loadSucceeded && pendingPollRefreshRef.current) {
        pendingPollRefreshRef.current = false;
        // 用 force 消化一次合并的轮询意图，保证拿到最新快照。
        force = true;
        continue;
      }

      pendingPollRefreshRef.current = false;
      return;
    }
  }, []);

  const refresh = useCallback(async () => {
    await runLoad({ force: true });
  }, [runLoad]);

  // 首次挂载加载；卸载时使 in-flight 请求失效。
  useEffect(() => {
    queueMicrotask(() => {
      void runLoad({ force: true });
    });
    return () => {
      requestIdRef.current = nextAttentionRequestId(requestIdRef.current);
      pendingForceRefreshRef.current = false;
      pendingPollRefreshRef.current = false;
      allowAutomaticPollRef.current = true;
      inFlightRef.current = false;
    };
  }, [runLoad]);

  // 业务动作成功后的立即失效桥：Deliver/Rework/Retry/Discard/依赖 recheck 等。
  useEffect(() => {
    return subscribeAttentionInvalidation(() => {
      void runLoad({ force: true });
    });
  }, [runLoad]);

  // focus / visibility / 可见轮询。
  useEffect(() => {
    const handleFocus = () => {
      void runLoad({ force: true });
    };
    const handleVisibility = () => {
      if (typeof document !== 'undefined' && document.visibilityState === 'visible') {
        void runLoad({ force: true });
      }
    };

    if (typeof window !== 'undefined') {
      window.addEventListener('focus', handleFocus);
    }
    if (typeof document !== 'undefined') {
      document.addEventListener('visibilitychange', handleVisibility);
    }

    let intervalId: ReturnType<typeof setInterval> | null = null;

    /**
     * Business Logic（为什么需要这个函数）:
     *   仅在页面可见时保留 10 秒兜底轮询，隐藏时暂停避免后台空转。
     *
     * Code Logic（这个函数做什么）:
     *   根据 visibilityState 启停 setInterval；轮询走 non-force single-flight；
     *   超时熔断后 interval 仍跑但 runLoad 会忽略非 force 请求。
     */
    const syncInterval = () => {
      const visible =
        typeof document === 'undefined' || document.visibilityState === 'visible';
      if (visible) {
        if (intervalId === null) {
          intervalId = setInterval(() => {
            if (typeof document !== 'undefined' && document.visibilityState !== 'visible') {
              return;
            }
            // 定时轮询不强制废弃 in-flight 请求；超时熔断后由 allowAutomaticPoll 拦截。
            void runLoad({ force: false });
          }, ATTENTION_POLL_INTERVAL_MS);
        }
      } else if (intervalId !== null) {
        clearInterval(intervalId);
        intervalId = null;
      }
    };

    syncInterval();
    if (typeof document !== 'undefined') {
      document.addEventListener('visibilitychange', syncInterval);
    }

    return () => {
      if (typeof window !== 'undefined') {
        window.removeEventListener('focus', handleFocus);
      }
      if (typeof document !== 'undefined') {
        document.removeEventListener('visibilitychange', handleVisibility);
        document.removeEventListener('visibilitychange', syncInterval);
      }
      if (intervalId !== null) {
        clearInterval(intervalId);
      }
    };
  }, [runLoad]);

  const value = useMemo<AttentionContextValue>(
    () => ({
      snapshot: state.snapshot,
      loading: state.loading,
      refreshing: state.refreshing,
      stale: state.stale,
      error: state.error,
      lastSucceededAt: state.lastSucceededAt,
      refresh,
    }),
    [state, refresh],
  );

  return <AttentionContext.Provider value={value}>{children}</AttentionContext.Provider>;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   loadSnapshot 可能永不 settle（半开 HTTP / 挂起 IPC），必须用硬超时保证 single-flight 可释放。
 *
 * Code Logic（这个函数做什么）:
 *   Promise.race 包装 loader 与 setTimeout reject；超时后抛 Error，不取消底层 Promise
 *   （浏览器 fetch 取消需 AbortSignal，见 attentionHttp；此处至少释放 inFlight）。
 */
function withAttentionLoadTimeout<T>(
  promise: Promise<T>,
  timeoutMs: number = ATTENTION_LOAD_TIMEOUT_MS,
): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    let settled = false;
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      reject(new Error(`Attention 加载超时（${timeoutMs}ms）`));
    }, timeoutMs);

    promise.then(
      (value) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        resolve(value);
      },
      (reason: unknown) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        reject(reason);
      },
    );
  });
}

// 再导出读取 hook，便于页面 `import { useAttention, AttentionProvider } from '@/hooks/useAttention'`。
// eslint-disable-next-line react-refresh/only-export-components -- Provider 与 hook 同入口；Context 定义在 attentionContext.ts
export { useAttention } from './attentionContext';
