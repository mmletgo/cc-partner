/**
 * Workbench 终端输出缓存 Context 定义与读取 hook。
 *
 * Business Logic（为什么需要这个模块）:
 *   Workbench 页面切换到其他路由后会卸载，但 PTY/tmux 仍在运行；终端输出缓存必须跨路由保留。
 *
 * Code Logic（这个模块做什么）:
 *   定义 Context value、创建 React Context，并提供 store / snapshot 读取 hook。
 */

import { createContext, useCallback, useContext, useMemo, useSyncExternalStore } from 'react';
import type {
  TerminalBufferSnapshot,
  TerminalHistorySyncFailure,
  WorkbenchTerminalBufferStore,
} from './workbenchTerminalBuffer';

export interface WorkbenchTerminalBuffersContextValue {
  store: WorkbenchTerminalBufferStore;
  resetBuffer: (sessionId: string) => void;
  removeBuffer: (sessionId: string) => void;
  /**
   * Business Logic（为什么需要这个方法）:
   *   永久 replay 错误停止自动重试后，上层需要读取受控的 history sync 失败状态
   *   （R11 M1），避免无限静默重试且不向用户暴露失败。
   *
   * Code Logic（这个方法做什么）:
   *   返回 session 的 TerminalHistorySyncFailure；无失败时返回 null。
   *   仅稳定 kind token，不含 buffer/body/path。
   */
  getHistorySyncFailure: (sessionId: string) => TerminalHistorySyncFailure | null;
}

/**
 * Business Logic（为什么需要这个接口）:
 *   React 非 xterm 消费者仍按 revision 读取 buffer snapshot。
 *
 * Code Logic（这个接口做什么）:
 *   暴露 buffer 字符串与 revision 号（cursor 可选供诊断）。
 */
export interface WorkbenchTerminalBufferSnapshot {
  buffer: string;
  revision: number;
}

export const WorkbenchTerminalBuffersContext =
  createContext<WorkbenchTerminalBuffersContextValue | null>(null);

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 页面需要读取跨路由保留的终端输出缓存。
 *
 * Code Logic（这个函数做什么）:
 *   从 React Context 读取 value；缺少 Provider 时抛出明确错误。
 */
export function useWorkbenchTerminalBuffers(): WorkbenchTerminalBuffersContextValue {
  const value = useContext(WorkbenchTerminalBuffersContext);
  if (!value) {
    throw new Error(
      'useWorkbenchTerminalBuffers must be used inside WorkbenchTerminalBuffersProvider',
    );
  }
  return value;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   TerminalPane 的 live writer 需要稳定 store 引用做 imperative 订阅，不能经 React revision 热路径。
 *
 * Code Logic（这个函数做什么）:
 *   返回 Context 中的 store；缺少 Provider 时抛错。
 */
export function useWorkbenchTerminalBufferStore(): WorkbenchTerminalBufferStore {
  return useWorkbenchTerminalBuffers().store;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   每个非 xterm 消费者只需要响应自身 session 的输出 revision，不能让某个终端输出导致整个 Workbench 重渲染。
 *
 * Code Logic（这个函数做什么）:
 *   使用 useSyncExternalStore 订阅指定 session 的 revision，并按 revision 读取当前 snapshot.buffer。
 */
export function useWorkbenchTerminalBuffer(
  sessionId: string | null,
): WorkbenchTerminalBufferSnapshot {
  const { store } = useWorkbenchTerminalBuffers();
  const subscribe = useCallback(
    (listener: () => void) => store.subscribe(sessionId, listener),
    [sessionId, store],
  );
  const getRevision = useCallback(() => store.getRevision(sessionId), [sessionId, store]);
  const revision = useSyncExternalStore(subscribe, getRevision, () => 0);

  return useMemo(() => {
    const snapshot: TerminalBufferSnapshot = store.getSnapshot(sessionId);
    return {
      buffer: snapshot.buffer,
      revision,
    };
  }, [revision, sessionId, store]);
}
