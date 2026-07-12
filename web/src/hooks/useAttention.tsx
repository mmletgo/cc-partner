/**
 * AttentionProvider — 共享 Inbox 快照状态机。
 *
 * Business Logic（为什么需要这个模块）:
 *   桌面与移动端各挂一个 Provider，但必须共用同一套 load/stale/request-sequence 语义；
 *   刷新时机：挂载、focus、可见恢复、手动 refresh、可见时每 10 秒轮询。
 *
 * Code Logic（这个模块做什么）:
 *   接受 loadSnapshot prop；用 attentionReducer + requestId 管理状态；
 *   注册 focus/visibility/interval 监听并在卸载时清理。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ReactNode } from 'react';

import type { AttentionSnapshot } from '@/lib/types';
import { AttentionContext, type AttentionContextValue } from './attentionContext';
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
 *   维护 AttentionViewState；load 时递增 requestId；旧响应与 unmount 后忽略；
 *   可见时 setInterval 10s；hidden 暂停；focus/visibilitychange 触发 refresh。
 */
export function AttentionProvider({ children, loadSnapshot }: AttentionProviderProps) {
  const [state, setState] = useState<AttentionViewState>(() => createInitialAttentionState());
  const requestIdRef = useRef(0);
  const mountedRef = useRef(true);
  const stateRef = useRef(state);
  const loadSnapshotRef = useRef(loadSnapshot);

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
   *   所有触发源（挂载/focus/可见/轮询/手动）必须走同一 load 路径与 stale guard。
   *
   * Code Logic（这个函数做什么）:
   *   递增 requestId；按是否有 snapshot 发 loadStarted；await loadSnapshot；
   *   仅当仍 mounted 且 requestId 最新时写入 succeeded/failed。
   */
  const runLoad = useCallback(async () => {
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
      if (!mountedRef.current || !isCurrentAttentionRequest(requestId, requestIdRef.current)) {
        return;
      }
      const receivedAt = new Date().toISOString();
      setState((current) =>
        attentionReducer(current, {
          type: 'loadSucceeded',
          snapshot,
          receivedAt,
        }),
      );
    } catch (reason) {
      if (!mountedRef.current || !isCurrentAttentionRequest(requestId, requestIdRef.current)) {
        return;
      }
      const error = reason instanceof Error ? reason : new Error(String(reason));
      setState((current) =>
        attentionReducer(current, {
          type: 'loadFailed',
          error,
          hasSnapshot: current.snapshot !== null || hasSnapshot,
        }),
      );
    }
  }, []);

  const refresh = useCallback(async () => {
    await runLoad();
  }, [runLoad]);

  // 首次挂载加载；卸载时使 in-flight 请求失效。
  /* eslint-disable react-hooks/set-state-in-effect -- Provider 挂载必须主动拉取首份 Inbox 快照 */
  useEffect(() => {
    queueMicrotask(() => {
      void runLoad();
    });
    return () => {
      requestIdRef.current = nextAttentionRequestId(requestIdRef.current);
    };
  }, [runLoad]);
  /* eslint-enable react-hooks/set-state-in-effect */

  // focus / visibility / 可见轮询。
  useEffect(() => {
    const handleFocus = () => {
      void runLoad();
    };
    const handleVisibility = () => {
      if (typeof document !== 'undefined' && document.visibilityState === 'visible') {
        void runLoad();
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
     *   根据 visibilityState 启停 setInterval。
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
            void runLoad();
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
