/**
 * Desktop Orchestrator runtime snapshot hook（进程内 live 缓存 + stale guard）。
 *
 * Business Logic（为什么需要这个模块）:
 *   桌面端状态条需要展示本机/远端 owning device 的 runtime 快照；远端 offline 时可展示
 *   当前进程内最后一次 live 成功结果与收到时间，但不能持久化，也不能把缓存交给动作/调度逻辑。
 *
 * Code Logic（这个模块做什么）:
 *   - 模块级 Map 按精确 projectId 缓存最后一次 live 成功快照与 client receipt time；
 *   - request sequence + mounted ref 防止项目切换/卸载后的 stale 写入；
 *   - offline 且有本项目缓存时返回缓存 + cachedAt；unsupported/unavailable/cold offline 不复用缓存。
 */
import { useCallback, useEffect, useRef, useState } from 'react';

import { orchestratorApi } from '@/api/orchestrator';
import type {
  OrchestratorRemoteRuntimeStatus,
  OrchestratorRuntimeDisplayState,
  OrchestratorRuntimeSnapshot,
} from '@/lib/types';

/**
 * Business Logic（为什么需要这个类型）:
 *   缓存只保存显示用的 live 成功结果，避免把 offline 空态误当成可用历史。
 *
 * Code Logic（字段说明）:
 *   snapshot 为上次 live 成功 DTO；receivedAt 为客户端收到时刻的 ISO 字符串。
 */
interface LiveRuntimeSnapshotCacheEntry {
  snapshot: OrchestratorRuntimeSnapshot;
  receivedAt: string;
}

/** 模块级 live 成功缓存：key 为精确 project shortcut ID，不导出、不持久化。 */
const liveRuntimeSnapshotCache = new Map<string, LiveRuntimeSnapshotCacheEntry>();

/**
 * Business Logic（为什么需要这个函数）:
 *   测试需要在用例之间清空模块缓存，避免跨 test 污染。
 *
 * Code Logic（这个函数做什么）:
 *   清空模块级 Map；生产代码不应依赖该导出。
 */
export function __clearOrchestratorRuntimeSnapshotCacheForTests(): void {
  liveRuntimeSnapshotCache.clear();
}

/**
 * Business Logic（为什么需要这个类型）:
 *   hook 调用方需要传入当前项目 ID 与是否启用加载（如 projects 仍在 loading 时禁用）。
 *
 * Code Logic（字段说明）:
 *   projectId 为空时不发请求；enabled=false 时保持空态。
 */
export interface UseOrchestratorRuntimeSnapshotParams {
  projectId: string | null;
  enabled?: boolean;
}

/**
 * Business Logic（为什么需要这个类型）:
 *   页面需要展示态字段，以及手动 refresh 入口。
 *
 * Code Logic（字段说明）:
 *   继承 OrchestratorRuntimeDisplayState，并附带 refresh 方法。
 */
export interface UseOrchestratorRuntimeSnapshotResult extends OrchestratorRuntimeDisplayState {
  refresh: () => Promise<void>;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   后端 DTO remoteStatus 同时承载本机 local 与远端四态，hook 展示层需要归一化为
 *   “本机 → null / 远端 → 四态”。
 *
 * Code Logic（这个函数做什么）:
 *   local 返回 null；live/unsupported/offline/unavailable 原样返回；未知值当 unavailable。
 */
function toDisplayRemoteStatus(
  status: OrchestratorRuntimeSnapshot['remoteStatus'],
): OrchestratorRemoteRuntimeStatus | null {
  if (status === 'local') return null;
  if (
    status === 'live' ||
    status === 'unsupported' ||
    status === 'offline' ||
    status === 'unavailable'
  ) {
    return status;
  }
  return 'unavailable';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   live 成功才值得缓存；offline/unsupported/unavailable 空态不能污染 live 缓存。
 *
 * Code Logic（这个函数做什么）:
 *   remoteStatus 为 local 或 live 时返回 true。
 */
function isSuccessfulLiveSnapshot(snapshot: OrchestratorRuntimeSnapshot): boolean {
  return snapshot.remoteStatus === 'local' || snapshot.remoteStatus === 'live';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   统一构造空显示态，避免各分支手写字段遗漏。
 *
 * Code Logic（这个函数做什么）:
 *   返回 snapshot/remoteStatus/cachedAt/error 均为空的 loading 可配置状态。
 */
function emptyDisplayState(
  loading: boolean,
  error: Error | null = null,
): OrchestratorRuntimeDisplayState {
  return {
    snapshot: null,
    remoteStatus: null,
    cachedAt: null,
    loading,
    error,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   将后端返回与模块缓存合成桌面展示态：offline 可带缓存，其他失败态清空。
 *
 * Code Logic（这个函数做什么）:
 *   - live/local：写缓存，返回 snapshot，cachedAt=null；
 *   - offline：若有同 project 缓存则返回缓存 snapshot + cachedAt，否则 null；
 *   - unsupported/unavailable：返回 null snapshot，不读其他项目缓存。
 */
function resolveDisplayStateFromResponse(
  projectId: string,
  snapshot: OrchestratorRuntimeSnapshot,
  receivedAt: string,
): OrchestratorRuntimeDisplayState {
  const remoteStatus = toDisplayRemoteStatus(snapshot.remoteStatus);

  if (isSuccessfulLiveSnapshot(snapshot)) {
    liveRuntimeSnapshotCache.set(projectId, { snapshot, receivedAt });
    return {
      snapshot,
      remoteStatus,
      cachedAt: null,
      loading: false,
      error: null,
    };
  }

  if (remoteStatus === 'offline') {
    const cached = liveRuntimeSnapshotCache.get(projectId) ?? null;
    if (cached) {
      return {
        snapshot: cached.snapshot,
        remoteStatus: 'offline',
        cachedAt: cached.receivedAt,
        loading: false,
        error: null,
      };
    }
    return {
      snapshot: null,
      remoteStatus: 'offline',
      cachedAt: null,
      loading: false,
      error: null,
    };
  }

  // unsupported / unavailable / 未知：不复用任何缓存
  return {
    snapshot: null,
    remoteStatus,
    cachedAt: null,
    loading: false,
    error: null,
  };
}

/**
 * Business Logic（为什么需要这个 hook）:
 *   Orchestrator 面板需要按当前 active project 拉取 runtime snapshot，并在远端 offline 时
 *   展示本进程内最后一次 live 成功结果；项目切换必须丢弃旧响应。
 *
 * Code Logic（这个 hook 做什么）:
 *   enabled 且 projectId 非空时发请求；用 requestSeq + mountedRef 做 stale/unmount guard；
 *   模块级 Map 只缓存 live/local 成功；返回 display state + refresh。
 */
export function useOrchestratorRuntimeSnapshot(
  params: UseOrchestratorRuntimeSnapshotParams,
): UseOrchestratorRuntimeSnapshotResult {
  const { projectId, enabled = true } = params;
  const canLoad = Boolean(enabled && projectId);
  const [state, setState] = useState<OrchestratorRuntimeDisplayState>(() => emptyDisplayState(false));
  const requestSeqRef = useRef(0);
  const mountedRef = useRef(true);
  const projectIdRef = useRef(projectId);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  useEffect(() => {
    projectIdRef.current = projectId;
  }, [projectId]);

  const loadSnapshot = useCallback(async (targetProjectId: string) => {
    const requestSeq = ++requestSeqRef.current;
    setState((current) => {
      // 切换项目时先清空旧项目数据，避免短暂串台；同项目 refresh 可保留旧 snapshot 作过渡。
      if (current.snapshot?.projectId === targetProjectId) {
        return { ...current, loading: true, error: null };
      }
      return emptyDisplayState(true);
    });

    try {
      const snapshot = await orchestratorApi.getRuntimeSnapshot(targetProjectId);
      if (
        !mountedRef.current ||
        requestSeq !== requestSeqRef.current ||
        projectIdRef.current !== targetProjectId
      ) {
        return;
      }
      const receivedAt = new Date().toISOString();
      setState(resolveDisplayStateFromResponse(targetProjectId, snapshot, receivedAt));
    } catch (err) {
      if (
        !mountedRef.current ||
        requestSeq !== requestSeqRef.current ||
        projectIdRef.current !== targetProjectId
      ) {
        return;
      }
      const error = err instanceof Error ? err : new Error(String(err));
      // 请求抛错时：若本项目已有 live 缓存，按 offline 缓存展示；否则空态 + error。
      const cached = liveRuntimeSnapshotCache.get(targetProjectId) ?? null;
      if (cached) {
        setState({
          snapshot: cached.snapshot,
          remoteStatus: 'offline',
          cachedAt: cached.receivedAt,
          loading: false,
          error,
        });
      } else {
        setState({
          snapshot: null,
          remoteStatus: null,
          cachedAt: null,
          loading: false,
          error,
        });
      }
    }
  }, []);

  const refresh = useCallback(async () => {
    if (!canLoad || !projectId) return;
    await loadSnapshot(projectId);
  }, [canLoad, loadSnapshot, projectId]);

  /* eslint-disable react-hooks/set-state-in-effect -- 合法 fetch-in-effect；loading setState 在请求启动路径，结果 setState 在 await 后 */
  useEffect(() => {
    if (!canLoad || !projectId) {
      // 使任何 in-flight 请求失效；空态由返回值派生，避免在 effect 内同步 setState。
      requestSeqRef.current += 1;
      return undefined;
    }

    void loadSnapshot(projectId);
    return () => {
      // 使 in-flight 请求失效；不在 cleanup 清空 Map（缓存跨挂载实例保留）。
      requestSeqRef.current += 1;
    };
  }, [canLoad, loadSnapshot, projectId]);
  /* eslint-enable react-hooks/set-state-in-effect */

  const displayState = canLoad ? state : emptyDisplayState(false);

  return {
    ...displayState,
    refresh,
  };
}
