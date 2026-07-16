/**
 * useLanAgentFleet — 可见性感知的 LAN Fleet 快照 hook。
 *
 * Business Logic（为什么需要这个模块）:
 *   Fleet 与 Project Rail 需要 event invalidation + 30s safety reconcile；
 *   hidden 停止轮询，恢复可见立即刷新；stale 响应不得覆盖新结果。
 *
 * Code Logic（这个模块做什么）:
 *   useVisibilityPolling(30s) + requestSeq + 500ms event coalesce；
 *   调用 workbenchTransport.lanFleet.getSnapshot。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import { workbenchApi } from '@/api/workbench';
import type { LanFleetProjectSummary, LanFleetSnapshot } from '@/lib/types/lanFleet';
import { useVisibilityPolling } from './useVisibilityPolling';

/** safety reconcile 间隔（可见时）。 */
export const LAN_FLEET_RECONCILE_MS = 30_000;

/** event invalidation 合并窗口。 */
export const LAN_FLEET_EVENT_COALESCE_MS = 500;

type TauriInternalsWindow = Window & {
  __TAURI_INTERNALS__?: { transformCallback?: unknown };
};

/**
 * Business Logic（为什么需要这个函数）:
 *   普通浏览器无 Tauri internals，listen 会失败。
 *
 * Code Logic（这个函数做什么）:
 *   检测 transformCallback 是否为函数。
 */
function canListenToTauriEvents(): boolean {
  if (typeof window === 'undefined') return false;
  const internals = (window as TauriInternalsWindow).__TAURI_INTERNALS__;
  return typeof internals?.transformCallback === 'function';
}

/**
 * hook 参数。
 */
export interface UseLanAgentFleetParams {
  /** false 时不轮询（例如页面未挂载 Fleet 相关 UI）。 */
  enabled?: boolean;
  /** 测试可注入 snapshot 加载函数。 */
  loadSnapshot?: () => Promise<LanFleetSnapshot>;
}

/**
 * hook 返回值。
 */
export interface UseLanAgentFleetResult {
  snapshot: LanFleetSnapshot | null;
  loading: boolean;
  error: string | null;
  refresh: () => Promise<void>;
  /** projectId → summary 索引，供 Rail 使用。 */
  projectSummaries: Record<string, LanFleetProjectSummary>;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Rail 按 project 查摘要，避免每次 scan devices。
 *
 * Code Logic（这个函数做什么）:
 *   扁平化 devices.projects 为 map。
 */
export function indexFleetProjects(
  snapshot: LanFleetSnapshot | null,
): Record<string, LanFleetProjectSummary> {
  if (!snapshot) return {};
  const map: Record<string, LanFleetProjectSummary> = {};
  for (const device of snapshot.devices) {
    for (const project of device.projects) {
      map[project.projectId] = project;
    }
  }
  return map;
}

/**
 * Business Logic（为什么需要这个 hook）:
 *   用户打开 Workbench/Rail 时需要跨设备 Agent 异常摘要，且不得 N×M 轮询。
 *
 * Code Logic（这个 hook 做什么）:
 *   30s visible polling；hidden 停；requestSeq 丢弃 stale；500ms event coalesce。
 */
export function useLanAgentFleet(
  params: UseLanAgentFleetParams = {},
): UseLanAgentFleetResult {
  const enabled = params.enabled !== false;
  const loadSnapshot = params.loadSnapshot;
  const [snapshot, setSnapshot] = useState<LanFleetSnapshot | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const requestSeqRef = useRef(0);
  const coalesceTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const loadSnapshotRef = useRef(loadSnapshot);

  // 注入 loader 仅异步路径读取，避免 render 中写 ref
  useEffect(() => {
    loadSnapshotRef.current = loadSnapshot;
  }, [loadSnapshot]);

  const load = useCallback(async () => {
    const seq = ++requestSeqRef.current;
    setLoading(true);
    try {
      const loader =
        loadSnapshotRef.current ?? (() => workbenchApi.lanFleet.getSnapshot());
      const next = await loader();
      if (seq !== requestSeqRef.current) return;
      setSnapshot(next);
      setError(null);
    } catch (err) {
      if (seq !== requestSeqRef.current) return;
      const message = err instanceof Error ? err.message : String(err);
      setError(message || 'fleet_load_failed');
    } finally {
      if (seq === requestSeqRef.current) {
        setLoading(false);
      }
    }
  }, []);

  const { runNow } = useVisibilityPolling(load, {
    intervalMs: LAN_FLEET_RECONCILE_MS,
    enabled,
    runImmediately: true,
    refreshOnVisible: true,
  });

  // event invalidation coalesce（agent-runtime / attention 等）
  useEffect(() => {
    if (!enabled) return undefined;

    /**
     * Business Logic（为什么需要这个函数）:
     *   高频 phase 事件合并为单次 refresh，避免风暴。
     *
     * Code Logic（这个函数做什么）:
     *   500ms debounce 后 runNow({ force: true })。
     */
    const scheduleCoalescedRefresh = () => {
      if (coalesceTimerRef.current) {
        clearTimeout(coalesceTimerRef.current);
      }
      coalesceTimerRef.current = setTimeout(() => {
        coalesceTimerRef.current = null;
        void runNow({ force: true });
      }, LAN_FLEET_EVENT_COALESCE_MS);
    };

    const onCustom = () => scheduleCoalescedRefresh();
    window.addEventListener('cp-lan-fleet-invalidate', onCustom);

    const unsubs: UnlistenFn[] = [];
    if (canListenToTauriEvents()) {
      void listen('workbench:agent-runtime', () => scheduleCoalescedRefresh()).then((off) => {
        unsubs.push(off);
      });
      void listen('attention:changed', () => scheduleCoalescedRefresh()).then((off) => {
        unsubs.push(off);
      });
    }

    return () => {
      window.removeEventListener('cp-lan-fleet-invalidate', onCustom);
      if (coalesceTimerRef.current) clearTimeout(coalesceTimerRef.current);
      for (const off of unsubs) off();
    };
  }, [enabled, runNow]);

  const projectSummaries = useMemo(() => indexFleetProjects(snapshot), [snapshot]);

  return {
    snapshot,
    loading,
    error,
    refresh: () => runNow({ force: true }),
    projectSummaries,
  };
}
