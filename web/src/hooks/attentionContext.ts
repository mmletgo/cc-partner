/**
 * Attention Context 定义与读取 hook。
 *
 * Business Logic（为什么需要这个模块）:
 *   桌面侧栏 badge、Inbox 页与移动导航需要共享同一份 Attention 状态；
 *   Provider 与 Context 分文件以满足 React Fast Refresh 约定。
 *
 * Code Logic（这个模块做什么）:
 *   定义 AttentionContextValue、创建 Context，并提供 useAttention 读取上下文。
 */

import { createContext, useContext } from 'react';
import type { AttentionCategory, AttentionSnapshot } from '@/lib/types';

/**
 * Attention Provider 对外 value。
 *
 * Business Logic（为什么需要这个类型）:
 *   页面消费 snapshot/loading 与已读写入口，不关心 requestId。
 *
 * Code Logic（字段说明）:
 *   mark* 写本设备 read_state；pendingReadIds 驱动乐观 busy；markError 给 StatusMessage。
 */
export interface AttentionContextValue {
  snapshot: AttentionSnapshot | null;
  loading: boolean;
  refreshing: boolean;
  stale: boolean;
  error: Error | null;
  lastSucceededAt: string | null;
  refresh: () => Promise<void>;
  markRead: (itemIds: string[]) => Promise<void>;
  markUnread: (itemIds: string[]) => Promise<void>;
  markAllRead: () => Promise<void>;
  markCategoryRead: (category: AttentionCategory) => Promise<void>;
  pendingReadIds: ReadonlySet<string>;
  markError: Error | null;
}

export const AttentionContext = createContext<AttentionContextValue | null>(null);

/**
 * Business Logic（为什么需要这个函数）:
 *   多个表面需要读取 Attention 状态，缺少 Provider 时应明确报错。
 *
 * Code Logic（这个函数做什么）:
 *   从 React Context 读取 value；缺失时抛出可诊断错误。
 */
export function useAttention(): AttentionContextValue {
  const value = useContext(AttentionContext);
  if (!value) {
    throw new Error('useAttention must be used inside AttentionProvider');
  }
  return value;
}
