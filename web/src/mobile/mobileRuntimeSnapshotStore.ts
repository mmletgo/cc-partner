/**
 * Mobile Orchestrator runtime snapshot 进程内缓存。
 *
 * Business Logic（为什么需要这个模块）:
 *   手机端自动化面板需要展示 remote-aware runtime 四态，并在 live→offline 时保留最后一次成功快照供显示；
 *   缓存不得写入 localStorage/磁盘，也不得驱动任务动作可用性。
 *
 * Code Logic（这个模块做什么）:
 *   提供按 projectId 键控的模块级 Map 缓存、请求序号 stale guard，以及 load 状态归并逻辑。
 */

import type {
  OrchestratorRemoteRuntimeStatus,
  OrchestratorRuntimeDisplayState,
  OrchestratorRuntimeSnapshot,
} from '@/lib/types';

/**
 * 成功缓存条目。
 *
 * Business Logic（为什么需要这个类型）:
 *   offline 时只能展示本项目上一次 live/local 成功快照，不能跨项目复用。
 *
 * Code Logic（字段说明）:
 *   snapshot 为成功响应体；cachedAt 为客户端收到成功响应时的 ISO 时间。
 */
interface MobileRuntimeSnapshotCacheEntry {
  snapshot: OrchestratorRuntimeSnapshot;
  cachedAt: string;
}

const successCache = new Map<string, MobileRuntimeSnapshotCacheEntry>();
const requestSeqByProject = new Map<string, number>();

/**
 * Business Logic（为什么需要这个函数）:
 *   测试与卸载场景需要把模块级缓存清干净，避免用例互相污染。
 *
 * Code Logic（这个函数做什么）:
 *   清空 success cache 与请求序号 Map。
 */
export function resetMobileRuntimeSnapshotStore(): void {
  successCache.clear();
  requestSeqByProject.clear();
}

/**
 * Business Logic（为什么需要这个函数）:
 *   面板在发起请求前需要一个可渲染的 loading 状态，并可能带上旧缓存做骨架显示。
 *
 * Code Logic（这个函数做什么）:
 *   读取该 project 的成功缓存（若有），返回 loading=true 的 display state。
 */
export function beginMobileRuntimeSnapshotLoad(
  projectId: string,
): OrchestratorRuntimeDisplayState {
  const cached = successCache.get(projectId) ?? null;
  // 展示层 remoteStatus 不含 local：本机成功快照的 remoteStatus 归一为 null（与桌面 hook 一致）。
  const displayRemoteStatus =
    cached?.snapshot.remoteStatus === 'live'
      ? 'live'
      : cached?.snapshot.remoteStatus === 'offline' ||
          cached?.snapshot.remoteStatus === 'unsupported' ||
          cached?.snapshot.remoteStatus === 'unavailable'
        ? cached.snapshot.remoteStatus
        : null;
  return {
    snapshot: cached?.snapshot ?? null,
    remoteStatus: displayRemoteStatus,
    cachedAt: cached?.cachedAt ?? null,
    loading: true,
    error: null,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   项目切换时旧请求返回后不得覆盖新项目状态。
 *
 * Code Logic（这个函数做什么）:
 *   递增 projectId 对应的请求序号并返回新序号。
 */
export function nextMobileRuntimeSnapshotRequestSeq(projectId: string): number {
  const next = (requestSeqByProject.get(projectId) ?? 0) + 1;
  requestSeqByProject.set(projectId, next);
  return next;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   异步响应落地前必须确认它仍是该项目最新请求。
 *
 * Code Logic（这个函数做什么）:
 *   比较 requestSeq 与模块记录，一致返回 true。
 */
export function isCurrentMobileRuntimeSnapshotRequest(
  projectId: string,
  requestSeq: number,
): boolean {
  return requestSeqByProject.get(projectId) === requestSeq;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   live/local 成功响应需要更新显示缓存；offline 时保留旧成功快照；unsupported/unavailable 不得复用旧缓存。
 *
 * Code Logic（这个函数做什么）:
 *   按 remoteStatus 归并 Map 缓存并返回 display state；stale 请求返回 null。
 */
export function applyMobileRuntimeSnapshotSuccess(
  projectId: string,
  requestSeq: number,
  snapshot: OrchestratorRuntimeSnapshot,
  receivedAt: string = new Date().toISOString(),
): OrchestratorRuntimeDisplayState | null {
  if (!isCurrentMobileRuntimeSnapshotRequest(projectId, requestSeq)) {
    return null;
  }

  const status = snapshot.remoteStatus;
  if (status === 'local' || status === 'live') {
    successCache.set(projectId, { snapshot, cachedAt: receivedAt });
    return {
      snapshot,
      // 本机 local 在展示层归一为 null；远端 live 保留 live。
      remoteStatus: status === 'live' ? 'live' : null,
      cachedAt: receivedAt,
      loading: false,
      error: null,
    };
  }

  if (status === 'offline') {
    const cached = successCache.get(projectId) ?? null;
    return {
      snapshot: cached?.snapshot ?? null,
      remoteStatus: 'offline',
      cachedAt: cached?.cachedAt ?? null,
      loading: false,
      error: null,
    };
  }

  // unsupported / unavailable：清空本项目显示缓存，避免跨状态误用。
  successCache.delete(projectId);
  return {
    snapshot: null,
    remoteStatus: status,
    cachedAt: null,
    loading: false,
    error: null,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   传输层失败（非后端四态 DTO）时面板仍要结束 loading，并保留 offline 显示缓存（若有）。
 *
 * Code Logic（这个函数做什么）:
 *   stale 请求返回 null；否则返回 offline 语义 + 可选缓存 + error。
 */
export function applyMobileRuntimeSnapshotFailure(
  projectId: string,
  requestSeq: number,
  error: Error,
): OrchestratorRuntimeDisplayState | null {
  if (!isCurrentMobileRuntimeSnapshotRequest(projectId, requestSeq)) {
    return null;
  }
  const cached = successCache.get(projectId) ?? null;
  return {
    snapshot: cached?.snapshot ?? null,
    remoteStatus: cached ? 'offline' : null,
    cachedAt: cached?.cachedAt ?? null,
    loading: false,
    error,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   测试需要检查 store 是否写入 localStorage。
 *
 * Code Logic（这个函数做什么）:
 *   返回当前缓存条目数量（不触碰 storage API）。
 */
export function getMobileRuntimeSnapshotCacheSize(): number {
  return successCache.size;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   渲染层需要把 remoteStatus 归类为可显示的语义标签。
 *
 * Code Logic（这个函数做什么）:
 *   原样返回 status；null 时返回 null。
 */
export function resolveMobileRuntimeRemoteStatus(
  status: OrchestratorRemoteRuntimeStatus | null,
): OrchestratorRemoteRuntimeStatus | null {
  return status;
}
