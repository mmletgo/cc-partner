/**
 * 速记本 autosave Context 与读取 hook。
 *
 * Business Logic（为什么需要这个模块）:
 *   Scratchpad 路由与 AppShell 关闭路径需要共享同一常驻队列；
 *   Provider 与 Context 分文件以满足 React Fast Refresh 约定。
 *
 * Code Logic（这个模块做什么）:
 *   创建 ScratchpadAutosaveContext，并提供 useScratchpadAutosave 读取队列。
 */

import { createContext, useContext } from 'react';

import type { ScratchpadAutosaveQueue } from './scratchpadAutosave';

export const ScratchpadAutosaveContext = createContext<ScratchpadAutosaveQueue | null>(null);

/**
 * Business Logic（为什么需要这个函数）:
 *   Scratchpad 页与后续关闭 flush 需要访问常驻队列；缺 Provider 时应立即失败。
 *
 * Code Logic（这个函数做什么）:
 *   从 React Context 读取 ScratchpadAutosaveQueue；缺失时抛出可诊断错误。
 */
export function useScratchpadAutosave(): ScratchpadAutosaveQueue {
  const queue = useContext(ScratchpadAutosaveContext);
  if (!queue) {
    throw new Error('useScratchpadAutosave must be used inside ScratchpadAutosaveProvider');
  }
  return queue;
}
