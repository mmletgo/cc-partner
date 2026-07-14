/**
 * ScratchpadAutosaveProvider — AppShell 常驻速记本 autosave 队列。
 *
 * Business Logic（为什么需要这个模块）:
 *   速记本路由可卸载，但未保存正文必须在 GUI 关闭前仍可 flush；
 *   队列生命周期应等于 AppShell，而非 Scratchpad 页面。
 *
 * Code Logic（这个模块做什么）:
 *   创建一次 createScratchpadAutosaveQueue（真实 save 走 scratchpadApi）；
 *   将 queue.flushAll 登记到 pendingWrites；通过 Context 下发队列。
 */

import { useEffect, useState } from 'react';
import type { ReactNode } from 'react';

import { scratchpadApi } from '@/api/scratchpad';
import { pendingWrites } from '@/lib/pendingWrites';

import {
  SCRATCHPAD_AUTOSAVE_DELAY_MS,
  createScratchpadAutosaveQueue,
  type ScratchpadAutosaveQueue,
} from './scratchpadAutosave';
import { ScratchpadAutosaveContext } from './scratchpadAutosaveContext';

/** pendingWrites 中速记本队列的固定登记 id。 */
export const SCRATCHPAD_PENDING_WRITE_ID = 'scratchpad-autosave';

/**
 * Provider props。
 *
 * Business Logic（为什么需要这个类型）:
 *   AppShell 只传入 children；保存实现固定走后端 API。
 *
 * Code Logic（字段说明）:
 *   children 为子树。
 */
export interface ScratchpadAutosaveProviderProps {
  children: ReactNode;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Provider 需要稳定的队列实例，且 save 固定走后端 API。
 *
 * Code Logic（这个函数做什么）:
 *   创建绑定 scratchpadApi.updatePageContent 的 autosave 队列。
 */
function createDefaultScratchpadAutosaveQueue(): ScratchpadAutosaveQueue {
  return createScratchpadAutosaveQueue(
    async (pageId, content) => {
      await scratchpadApi.updatePageContent(pageId, content);
    },
    { delayMs: SCRATCHPAD_AUTOSAVE_DELAY_MS },
  );
}

/**
 * Business Logic（为什么需要这个组件）:
 *   保证整个桌面会话只有一个速记本 autosave 队列，并在关闭前可被 pendingWrites 发现。
 *
 * Code Logic（这个组件做什么）:
 *   useState 惰性创建队列（稳定引用，避免 render 期读 ref）；
 *   mount 时 register flushAll，unmount 时 unregister。
 *   所有 hooks 在 return 之前调用。
 */
export function ScratchpadAutosaveProvider({ children }: ScratchpadAutosaveProviderProps) {
  const [queue] = useState(createDefaultScratchpadAutosaveQueue);

  useEffect(() => {
    return pendingWrites.register(SCRATCHPAD_PENDING_WRITE_ID, () => queue.flushAll());
  }, [queue]);

  return (
    <ScratchpadAutosaveContext.Provider value={queue}>{children}</ScratchpadAutosaveContext.Provider>
  );
}

// eslint-disable-next-line react-refresh/only-export-components -- 与其它 Provider 入口一致，re-export 读取 hook
export { useScratchpadAutosave } from './scratchpadAutosaveContext';
