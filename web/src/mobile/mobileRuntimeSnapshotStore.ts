/**
 * Mobile Orchestrator runtime snapshot 进程内缓存。
 *
 * Business Logic（为什么需要这个模块）:
 *   手机端自动化面板需要展示 remote-aware runtime 四态，并在 live→offline 时保留最后一次 remote live 快照供显示；
 *   缓存不得写入 localStorage/磁盘，也不得驱动任务动作可用性。
 *
 * Code Logic（这个模块做什么）:
 *   提供按 projectId 键控的模块级 Map 缓存、请求序号 stale guard，以及 load 状态归并逻辑。
 *   仅 remote live 写入 success cache；错误状态不依据“是否有缓存”推断 offline。
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
 *   offline 时只能展示本项目上一次 remote live 成功快照，不能跨项目复用，也不能把 local 当 live cache。
 *
 * Code Logic（字段说明）:
 *   snapshot 为 remote live 响应体；cachedAt 为客户端收到成功响应时的 ISO 时间。
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
 *   面板在发起请求前需要一个可渲染的 loading 状态，并可能带上旧 remote live 缓存做骨架显示。
 *
 * Code Logic（这个函数做什么）:
 *   读取该 project 的 remote live 缓存（若有），返回 loading=true 的 display state。
 */
export function beginMobileRuntimeSnapshotLoad(
  projectId: string,
): OrchestratorRuntimeDisplayState {
  const cached = successCache.get(projectId) ?? null;
  return {
    snapshot: cached?.snapshot ?? null,
    remoteStatus: cached ? 'live' : null,
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
 *   remote live 成功响应需要更新显示缓存；local 成功不写 live 缓存；offline 时保留旧 live 快照；
 *   unsupported/unavailable 不得复用旧缓存。
 *
 * Code Logic（这个函数做什么）:
 *   按 remoteStatus 归并 Map 缓存并返回 display state；stale 请求或错误 projectId 返回 null/空态。
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

  if (snapshot.projectId !== projectId) {
    return {
      snapshot: null,
      remoteStatus: null,
      cachedAt: null,
      loading: false,
      error: null,
    };
  }

  const status = snapshot.remoteStatus;
  if (status === 'live') {
    successCache.set(projectId, { snapshot, cachedAt: receivedAt });
    return {
      snapshot,
      remoteStatus: 'live',
      cachedAt: receivedAt,
      loading: false,
      error: null,
    };
  }

  if (status === 'local') {
    // 本机成功不写入 remote live cache，展示层 remoteStatus 归一为 null。
    return {
      snapshot,
      remoteStatus: null,
      cachedAt: null,
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
 *   传输层失败（非后端四态 DTO）时面板仍要结束 loading；只有网络类失败 + remote live 缓存才标 offline。
 *
 * Code Logic（这个函数做什么）:
 *   stale 请求返回 null；网络失败且有缓存 → offline+cache；其它失败 → unavailable/error，不把任意错误当 offline。
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
  const message = error.message.toLowerCase();
  const networkish =
    message.includes('network') ||
    message.includes('fetch') ||
    message.includes('timeout') ||
    message.includes('timed out') ||
    message.includes('econn') ||
    message.includes('offline') ||
    message.includes('离线') ||
    message.includes('连接') ||
    message.includes('unreachable') ||
    message.includes('failed to fetch');

  if (cached && networkish) {
    return {
      snapshot: cached.snapshot,
      remoteStatus: 'offline',
      cachedAt: cached.cachedAt,
      loading: false,
      error,
    };
  }

  return {
    snapshot: null,
    remoteStatus: cached ? 'unavailable' : null,
    cachedAt: null,
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
