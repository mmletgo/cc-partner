import { useCallback, useMemo, useState } from 'react';
import type { ReactElement } from 'react';
import { useWorkbenchHttpEvents } from '@/hooks/useWorkbenchHttpEvents';
import {
  createWorkbenchTerminalBufferStore,
  type WorkbenchTerminalBufferStore,
} from '@/hooks/workbenchTerminalBuffer';
import {
  WorkbenchTerminalBuffersContext,
  type WorkbenchTerminalBuffersContextValue,
} from '@/hooks/workbenchTerminalBuffersContext';
import { MobileWorkbench } from './MobileWorkbench';

/**
 * MobileApp（移动端 Workbench 应用根）
 *
 * Business Logic（为什么需要这个组件）:
 *   `/mobile` 是独立于桌面 Tauri shell 的普通浏览器 SPA，需要在根组件启动 HTTP 事件流并提供终端输出缓存。
 *
 * Code Logic（这个组件做什么）:
 *   懒初始化 session 级 terminal buffer store，启用 NDJSON HTTP event hook，并通过 Context 向移动端工作台提供 store/reset/remove。
 */
export function MobileApp(): ReactElement {
  const [store] = useState<WorkbenchTerminalBufferStore>(() =>
    createWorkbenchTerminalBufferStore(),
  );

  useWorkbenchHttpEvents({ store, enabled: true });

  /**
   * Business Logic（为什么需要这个函数）:
   *   后续移动端创建或重新连接终端 session 时，需要清空指定 session 的旧屏幕缓存。
   *
   * Code Logic（这个函数做什么）:
   *   调用外部 terminal buffer store 的 reset 方法重置目标 session。
   */
  const resetBuffer = useCallback(
    (sessionId: string): void => {
      store.reset(sessionId);
    },
    [store],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   后续移动端关闭终端 session 后，应释放该 session 的输出缓存，避免误 replay 和内存占用。
   *
   * Code Logic（这个函数做什么）:
   *   调用外部 terminal buffer store 的 remove 方法删除目标 session。
   */
  const removeBuffer = useCallback(
    (sessionId: string): void => {
      store.remove(sessionId);
    },
    [store],
  );

  const contextValue = useMemo<WorkbenchTerminalBuffersContextValue>(
    () => ({
      store,
      resetBuffer,
      removeBuffer,
    }),
    [removeBuffer, resetBuffer, store],
  );

  return (
    <WorkbenchTerminalBuffersContext.Provider value={contextValue}>
      <MobileWorkbench />
    </WorkbenchTerminalBuffersContext.Provider>
  );
}
