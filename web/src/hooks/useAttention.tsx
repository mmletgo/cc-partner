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
 *   in-flight load 采用 single-flight/coalescing，避免 10s 轮询饿死慢请求。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ReactNode } from 'react';

import type { AttentionSnapshot } from '@/lib/types';
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
 * Business Logic（为什么需要这个组件）:
 *   全局 Inbox 需要跨导航保持 badge 与列表一致，并在焦点/可见时刷新。
 *
 * Code Logic（这个组件做什么）:
 *   维护 AttentionViewState；显式刷新可递增 requestId 使旧响应失效；
 *   定时轮询若已有 in-flight 只标记 pending，完成后最多再跑一次；
 *   可见时 setInterval 10s；hidden 暂停；focus/visibilitychange 触发 refresh。
 */
export function AttentionProvider({ children, loadSnapshot }: AttentionProviderProps) {
  const [state, setState] = useState<AttentionViewState>(() => createInitialAttentionState());
  const requestIdRef = useRef(0);
  const mountedRef = useRef(true);
  const stateRef = useRef(state);
  const loadSnapshotRef = useRef(loadSnapshot);
  /** 是否已有 load 正在执行（single-flight 互斥）。 */
  const inFlightRef = useRef(false);
  /** 当前 in-flight 期间是否收到过新的刷新意图（coalesce 成一次后续 load）。 */
  const pendingRefreshRef = useRef(false);

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
   *   慢聚合（远端 open-project/task-list 可达 30s）不能被 10s 轮询反复废弃。
   *
   * Code Logic（这个函数做什么）:
   *   force=false（轮询）：若已 in-flight，只 mark pending 并返回；
   *   force=true（挂载/focus/可见/手动/invalidation）：可递增 requestId 使旧响应失效，
   *   但若已 in-flight 仍 coalesce，完成后只再跑一次；
   *   用 while 循环消化 pending，避免回调自引用破坏 React Compiler memoization。
   */
  const runLoad = useCallback(async (options?: { force?: boolean }) => {
    let force = options?.force === true;

    // 可能连续消化多次 pending（每次 loop 最多一个 in-flight）。
    while (true) {
      if (inFlightRef.current) {
        // 已有请求：只合并一次后续刷新意图，不启动新请求（避免风暴与饥饿）。
        pendingRefreshRef.current = true;
        if (force) {
          // 显式刷新仍推进 requestId，使更早的 in-flight 结果被判定为 stale。
          requestIdRef.current = nextAttentionRequestId(requestIdRef.current);
        }
        return;
      }

      inFlightRef.current = true;
      pendingRefreshRef.current = false;
      const requestId = nextAttentionRequestId(requestIdRef.current);
      requestIdRef.current = requestId;
      const hasSnapshot = stateRef.current.snapshot !== null;
      setState((current) =>
        attentionReducer(current, {
          type: 'loadStarted',
          hasSnapshot: current.snapshot !== null,
        }),
      );

      try {
        const snapshot = await loadSnapshotRef.current();
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
        if (mountedRef.current && isCurrentAttentionRequest(requestId, requestIdRef.current)) {
          const error = reason instanceof Error ? reason : new Error(String(reason));
          setState((current) =>
            attentionReducer(current, {
              type: 'loadFailed',
              error,
              hasSnapshot: current.snapshot !== null || hasSnapshot,
            }),
          );
        }
      } finally {
        inFlightRef.current = false;
      }

      if (!mountedRef.current || !pendingRefreshRef.current) {
        return;
      }
      // 合并期间的刷新意图在当前请求结束后再跑一次（仍走 force，保留最新意图）。
      pendingRefreshRef.current = false;
      force = true;
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
      pendingRefreshRef.current = false;
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
     *   根据 visibilityState 启停 setInterval；轮询走 non-force single-flight。
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
            // 定时轮询不强制废弃 in-flight 请求。
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

// 再导出读取 hook，便于页面 `import { useAttention, AttentionProvider } from '@/hooks/useAttention'`。
// eslint-disable-next-line react-refresh/only-export-components -- Provider 与 hook 同入口；Context 定义在 attentionContext.ts
export { useAttention } from './attentionContext';
