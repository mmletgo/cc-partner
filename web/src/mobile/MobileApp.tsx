import { useCallback, useMemo, useState } from 'react';
import type { ReactElement } from 'react';
import { attentionHttpApi } from '@/api/attentionHttp';
import { AttentionProvider } from '@/hooks/useAttention';
import { WorkbenchDependencyProvider } from '@/hooks/useWorkbenchDependency';
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
 *   `/mobile` 是独立于桌面 Tauri shell 的普通浏览器 SPA，需要在根组件启动 HTTP 事件流、
 *   提供终端输出缓存，并挂载共享 Attention Provider（Inbox badge/列表）。
 *
 * Code Logic（这个组件做什么）:
 *   懒初始化 session 级 ring-buffer store（options API，默认 rAF 帧批处理）；
 *   启用 NDJSON HTTP event hook；
 *   用 AttentionProvider(loadSnapshot=attentionHttpApi) 与 WorkbenchDependencyProvider 包裹 MobileWorkbench。
 */
export function MobileApp(): ReactElement {
  const [store] = useState<WorkbenchTerminalBufferStore>(() =>
    createWorkbenchTerminalBufferStore(),
  );

  // terminalStatus/agentRuntime 在 MobileWorkbench 内消费（拥有 sessions 状态）。

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

  /**
   * Business Logic（为什么需要这个函数）:
   *   桌面 Provider 暴露 history sync 失败状态；移动端当前无对等 IPC replay recovery，
   *   仍需满足 Context 合同，避免类型与消费者崩溃（R11 M1）。
   *
   * Code Logic（这个函数做什么）:
   *   恒返回 null（移动端 HTTP replay 路径暂不跟踪永久 history sync failure）。
   */
  const getHistorySyncFailure = useCallback(
    (_sessionId: string) => null,
    [],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   AttentionProvider 需要稳定的 loadSnapshot 引用，避免不必要的 effect 重跑。
   *
   * Code Logic（这个函数做什么）:
   *   委托 attentionHttpApi.listSnapshot（capability-gated HTTP）。
   */
  const loadAttentionSnapshot = useCallback(
    () => attentionHttpApi.listSnapshot(),
    [],
  );

  const contextValue = useMemo<WorkbenchTerminalBuffersContextValue>(
    () => ({
      store,
      resetBuffer,
      removeBuffer,
      getHistorySyncFailure,
    }),
    [getHistorySyncFailure, removeBuffer, resetBuffer, store],
  );

  return (
    <WorkbenchTerminalBuffersContext.Provider value={contextValue}>
      <WorkbenchDependencyProvider>
        <AttentionProvider loadSnapshot={loadAttentionSnapshot}>
          <MobileWorkbench />
        </AttentionProvider>
      </WorkbenchDependencyProvider>
    </WorkbenchTerminalBuffersContext.Provider>
  );
}
