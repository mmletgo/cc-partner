/**
 * Attention Provider 可测试状态机（reducer + request sequence 辅助）。
 *
 * Business Logic（为什么需要这个模块）:
 *   桌面与移动端共享同一套 loading/refreshing/stale/error 语义：旧请求不得覆盖新请求，
 *   有快照时刷新失败只标 stale，初次失败不伪造 badge 数字。
 *
 * Code Logic（这个模块做什么）:
 *   定义 AttentionViewState 与纯 reducer；提供 nextRequestId / isCurrentRequest 辅助，
 *   供 Provider 与单元测试共同消费。
 */

import type { AttentionSnapshot } from '@/lib/types';

/**
 * Attention Provider 对外可观察状态。
 *
 * Business Logic（为什么需要这个类型）:
 *   页面与 badge 需要统一的 loading/refreshing/stale/error/snapshot 字段。
 *
 * Code Logic（字段说明）:
 *   snapshot 为最后一次成功快照；loading 仅初次无快照时为 true；
 *   refreshing 表示已有快照时的再请求；stale 表示刷新失败保留旧快照；
 *   lastSucceededAt 为客户端成功写入时刻 ISO 字符串。
 */
export interface AttentionViewState {
  snapshot: AttentionSnapshot | null;
  loading: boolean;
  refreshing: boolean;
  stale: boolean;
  error: Error | null;
  lastSucceededAt: string | null;
}

/**
 * Attention 状态机事件。
 *
 * Business Logic（为什么需要这个类型）:
 *   reducer 用显式事件表达 load 生命周期，便于测试与 Provider 共用。
 *
 * Code Logic（这个类型做什么）:
 *   loadStarted 带 hasSnapshot；loadSucceeded 带 snapshot 与 receivedAt；
 *   loadFailed 带 error 与 hasSnapshot。
 */
export type AttentionStateEvent =
  | { type: 'loadStarted'; hasSnapshot: boolean }
  | { type: 'loadSucceeded'; snapshot: AttentionSnapshot; receivedAt: string }
  | { type: 'loadFailed'; error: Error; hasSnapshot: boolean };

/**
 * Business Logic（为什么需要这个函数）:
 *   统一构造初始空态，避免 Provider/测试手写字段遗漏。
 *
 * Code Logic（这个函数做什么）:
 *   返回无 snapshot、非 loading 的 AttentionViewState。
 */
export function createInitialAttentionState(): AttentionViewState {
  return {
    snapshot: null,
    loading: false,
    refreshing: false,
    stale: false,
    error: null,
    lastSucceededAt: null,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Provider 的所有状态迁移必须可单测、可预测：初次加载、成功、失败、刷新失败保 stale。
 *
 * Code Logic（这个函数做什么）:
 *   纯函数按事件更新 AttentionViewState；不修改入参。
 */
export function attentionReducer(
  state: AttentionViewState,
  event: AttentionStateEvent,
): AttentionViewState {
  switch (event.type) {
    case 'loadStarted':
      if (event.hasSnapshot) {
        return {
          ...state,
          loading: false,
          refreshing: true,
          // 刷新开始时清错误但不清 stale；成功才清 stale。
          error: null,
        };
      }
      return {
        ...state,
        loading: true,
        refreshing: false,
        error: null,
        stale: false,
      };
    case 'loadSucceeded':
      return {
        snapshot: event.snapshot,
        loading: false,
        refreshing: false,
        stale: false,
        error: null,
        lastSucceededAt: event.receivedAt,
      };
    case 'loadFailed':
      if (event.hasSnapshot) {
        return {
          ...state,
          loading: false,
          refreshing: false,
          stale: true,
          error: event.error,
        };
      }
      return {
        ...state,
        snapshot: null,
        loading: false,
        refreshing: false,
        stale: false,
        error: event.error,
        lastSucceededAt: null,
      };
    default: {
      const _exhaustive: never = event;
      return _exhaustive;
    }
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   每次请求需要单调递增 requestId，旧响应不得覆盖新请求。
 *
 * Code Logic（这个函数做什么）:
 *   返回 current + 1。
 */
export function nextAttentionRequestId(current: number): number {
  return current + 1;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   异步回调需要判断响应是否仍属于最新请求。
 *
 * Code Logic（这个函数做什么）:
 *   requestId 与 latestRequestId 严格相等时返回 true。
 */
export function isCurrentAttentionRequest(
  requestId: number,
  latestRequestId: number,
): boolean {
  return requestId === latestRequestId;
}
