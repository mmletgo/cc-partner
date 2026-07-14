/**
 * Desktop Orchestrator runtime snapshot hook（进程内 live 缓存 + stale guard）。
 *
 * Business Logic（为什么需要这个模块）:
 *   桌面端状态条需要展示本机/远端 owning device 的 runtime 快照；远端 offline 时可展示
 *   当前进程内最后一次 remote live 成功结果与收到时间，但不能持久化，也不能把缓存交给动作/调度逻辑。
 *
 * Code Logic（这个模块做什么）:
 *   - 模块级 Map 按精确 projectId 缓存最后一次 remote live 成功快照与 client receipt time；
 *   - display state 记录 owning projectId，render 阶段不匹配时立即返回空/loading，避免 A→B 串台；
 *   - request sequence + mounted ref 防止项目切换/卸载后的 stale 写入；
 *   - 仅 remote live 写入缓存；错误状态不依据“是否有缓存”推断，仅网络类失败 + live 缓存才 offline。
 */
import { useCallback, useEffect, useRef, useState } from 'react';

import { orchestratorApi } from '@/api/orchestrator';
import { isOrchestratorRuntimeNetworkTransportError } from '@/api/orchestratorRuntimeTransportError';
import type {
  OrchestratorRemoteRuntimeStatus,
  OrchestratorRuntimeDisplayState,
  OrchestratorRuntimeSnapshot,
} from '@/lib/types';

/**
 * Business Logic（为什么需要这个类型）:
 *   缓存只保存显示用的 remote live 成功结果，避免把 local 成功或 offline 空态误当成远端历史。
 *
 * Code Logic（字段说明）:
 *   snapshot 为上次 remote live 成功 DTO；receivedAt 为客户端收到时刻的 ISO 字符串。
 */
interface LiveRuntimeSnapshotCacheEntry {
  snapshot: OrchestratorRuntimeSnapshot;
  receivedAt: string;
}

/**
 * Business Logic（为什么需要这个类型）:
 *   display state 必须记录所属 projectId，才能在 A→B 切换的首帧立即隔离旧项目数据。
 *
 * Code Logic（字段说明）:
 *   projectId 为当前 state 归属的 shortcut id；null 表示空态。
 */
interface OwnedRuntimeDisplayState extends OrchestratorRuntimeDisplayState {
  projectId: string | null;
}

/** 模块级 remote live 成功缓存：key 为精确 project shortcut ID，不导出、不持久化。 */
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
 *   只有真正的 remote live 才值得写入 live 缓存；local 成功不得伪装成远端缓存，
 *   offline/unsupported/unavailable 空态也不能污染缓存。
 *
 * Code Logic（这个函数做什么）:
 *   remoteStatus === 'live' 时返回 true。
 */
function isRemoteLiveSnapshot(snapshot: OrchestratorRuntimeSnapshot): boolean {
  return snapshot.remoteStatus === 'live';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   统一构造空显示态，避免各分支手写字段遗漏。
 *
 * Code Logic（这个函数做什么）:
 *   返回 projectId/snapshot/remoteStatus/cachedAt/error 可配置的 loading 状态。
 */
function emptyDisplayState(
  loading: boolean,
  error: Error | null = null,
  projectId: string | null = null,
): OwnedRuntimeDisplayState {
  return {
    projectId,
    snapshot: null,
    remoteStatus: null,
    cachedAt: null,
    loading,
    error,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   将后端返回与模块缓存合成桌面展示态：offline 可带 remote live 缓存，其他失败态清空。
 *
 * Code Logic（这个函数做什么）:
 *   - remote live：写缓存，返回 snapshot，cachedAt=null；
 *   - local 成功：不写 live 缓存，remoteStatus=null；
 *   - offline：若有同 project remote live 缓存则返回缓存 snapshot + cachedAt，否则 null；
 *   - unsupported/unavailable：返回 null snapshot，不读其他项目缓存；
 *   - 响应 projectId 与 target 不一致时丢弃。
 */
function resolveDisplayStateFromResponse(
  projectId: string,
  snapshot: OrchestratorRuntimeSnapshot,
  receivedAt: string,
): OwnedRuntimeDisplayState {
  if (snapshot.projectId !== projectId) {
    return emptyDisplayState(false, null, projectId);
  }

  const remoteStatus = toDisplayRemoteStatus(snapshot.remoteStatus);

  if (isRemoteLiveSnapshot(snapshot)) {
    liveRuntimeSnapshotCache.set(projectId, { snapshot, receivedAt });
    return {
      projectId,
      snapshot,
      remoteStatus: 'live',
      cachedAt: null,
      loading: false,
      error: null,
    };
  }

  if (snapshot.remoteStatus === 'local') {
    return {
      projectId,
      snapshot,
      remoteStatus: null,
      cachedAt: null,
      loading: false,
      error: null,
    };
  }

  if (remoteStatus === 'offline') {
    const cached = liveRuntimeSnapshotCache.get(projectId) ?? null;
    if (cached) {
      return {
        projectId,
        snapshot: cached.snapshot,
        remoteStatus: 'offline',
        cachedAt: cached.receivedAt,
        loading: false,
        error: null,
      };
    }
    return {
      projectId,
      snapshot: null,
      remoteStatus: 'offline',
      cachedAt: null,
      loading: false,
      error: null,
    };
  }

  // unsupported / unavailable / 未知：不复用任何缓存
  return {
    projectId,
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
 *   展示本进程内最后一次 remote live 成功结果；项目切换必须丢弃旧响应与旧显示。
 *
 * Code Logic（这个 hook 做什么）:
 *   enabled 且 projectId 非空时发请求；用 requestSeq + mountedRef + owning projectId 做 stale/unmount/switch guard；
 *   模块级 Map 只缓存 remote live；返回 display state + refresh。
 */
export function useOrchestratorRuntimeSnapshot(
  params: UseOrchestratorRuntimeSnapshotParams,
): UseOrchestratorRuntimeSnapshotResult {
  const { projectId, enabled = true } = params;
  const canLoad = Boolean(enabled && projectId);
  const [state, setState] = useState<OwnedRuntimeDisplayState>(() => emptyDisplayState(false));
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
      if (current.projectId === targetProjectId && current.snapshot?.projectId === targetProjectId) {
        return { ...current, projectId: targetProjectId, loading: true, error: null };
      }
      return emptyDisplayState(true, null, targetProjectId);
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
      // 错误身份的响应按目标 ID 丢弃，避免串写缓存。
      if (snapshot.projectId !== targetProjectId) {
        setState(emptyDisplayState(false, null, targetProjectId));
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
      const cached = liveRuntimeSnapshotCache.get(targetProjectId) ?? null;
      // 仅当 adapter 显式标记 network transport 且存在 remote live 缓存时才 warm offline。
      // 禁止 Error.message 关键词匹配；成功 DTO 的 remoteStatus 才是四态权威。
      if (cached && isOrchestratorRuntimeNetworkTransportError(err)) {
        setState({
          projectId: targetProjectId,
          snapshot: cached.snapshot,
          remoteStatus: 'offline',
          cachedAt: cached.receivedAt,
          loading: false,
          error,
        });
        return;
      }
      // 非 network transport：有/无缓存都不推断 offline；协议/未知错误保持 error 语义。
      setState({
        projectId: targetProjectId,
        snapshot: null,
        remoteStatus: cached ? 'unavailable' : null,
        cachedAt: null,
        loading: false,
        error,
      });
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

  // Gap resync：owner event bus 丢事件后强制刷新 runtime snapshot，避免静默陈旧。
  useEffect(() => {
    if (!canLoad || !projectId) return undefined;
    if (typeof window === 'undefined') return undefined;
    const internals = (window as unknown as {
      __TAURI_INTERNALS__?: { transformCallback?: unknown };
    }).__TAURI_INTERNALS__;
    if (typeof internals?.transformCallback !== 'function') return undefined;

    let disposed = false;
    let unlisten: (() => void) | undefined;
    void import('@tauri-apps/api/event').then(({ listen }) => {
      if (disposed) return;
      void listen('backend:runtime-gap', () => {
        if (disposed) return;
        const target = projectIdRef.current;
        if (target) {
          void loadSnapshot(target);
        }
      }).then((fn) => {
        if (disposed) {
          fn();
          return;
        }
        unlisten = fn;
      });
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [canLoad, loadSnapshot, projectId]);
  /* eslint-enable react-hooks/set-state-in-effect */

  // Render 阶段隔离：当前 projectId 与 state 归属不一致时，立即返回 loading/空态，不展示旧项目数据。
  let displayState: OrchestratorRuntimeDisplayState;
  if (!canLoad || !projectId) {
    displayState = emptyDisplayState(false);
  } else if (state.projectId !== projectId) {
    displayState = emptyDisplayState(true, null, projectId);
  } else {
    displayState = {
      snapshot: state.snapshot,
      remoteStatus: state.remoteStatus,
      cachedAt: state.cachedAt,
      loading: state.loading,
      error: state.error,
    };
  }

  return {
    ...displayState,
    refresh,
  };
}
