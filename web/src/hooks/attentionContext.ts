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
import type { AttentionSnapshot } from '@/lib/types';

/**
 * Attention Provider 对外 value。
 *
 * Business Logic（为什么需要这个类型）:
 *   页面只消费 snapshot/loading/refreshing/stale/error/refresh，不关心 requestId。
 *
 * Code Logic（字段说明）:
 *   与计划 Shared Interfaces 中的 AttentionContextValue 对齐。
 */
export interface AttentionContextValue {
  snapshot: AttentionSnapshot | null;
  loading: boolean;
  refreshing: boolean;
  stale: boolean;
  error: Error | null;
  lastSucceededAt: string | null;
  refresh: () => Promise<void>;
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
