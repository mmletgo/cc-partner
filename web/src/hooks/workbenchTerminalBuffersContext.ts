/**
 * Workbench 终端输出缓存 Context 定义与读取 hook。
 *
 * Business Logic（为什么需要这个模块）:
 *   Workbench 页面切换到其他路由后会卸载，但 PTY/tmux 仍在运行；终端输出缓存必须跨路由保留。
 *   R12 M3：historySyncFailure 必须可订阅，Workbench UI 才能在终端产品路径展示永久失败。
 *   R13 M1：启动 sessions.list 永久失败也必须可观察，并可手动重试枚举。
 *
 * Code Logic（这个模块做什么）:
 *   定义 Context value、创建 React Context，并提供 store / snapshot / historySyncFailure /
 *   startupBaselineFailure 读取 hook。
 */

import { createContext, useCallback, useContext, useMemo, useSyncExternalStore } from 'react';
import type {
  TerminalBufferSnapshot,
  TerminalHistorySyncFailure,
  WorkbenchTerminalBufferStore,
} from './workbenchTerminalBuffer';

/**
 * 启动 sessions.list 基线枚举的可观察失败状态（R13 M1）。
 *
 * Business Logic（为什么需要这个类型）:
 *   list 是静默既有 session 的唯一枚举入口；永久失败后 UI 必须可观察并可手动重试。
 *
 * Code Logic（这个类型做什么）:
 *   kind 仅稳定 token；不含 path/body/session 列表。
 */
export type StartupBaselineFailure = {
  kind: 'startup_list_failed';
};

export interface WorkbenchTerminalBuffersContextValue {
  store: WorkbenchTerminalBufferStore;
  resetBuffer: (sessionId: string) => void;
  removeBuffer: (sessionId: string) => void;
  /**
   * Business Logic（为什么需要这个方法）:
   *   永久 replay 错误停止自动重试后，上层需要读取受控的 history sync 失败状态
   *   （R11 M1 / R12 M3），避免无限静默重试且不向用户暴露失败。
   *
   * Code Logic（这个方法做什么）:
   *   返回 session 的 TerminalHistorySyncFailure；无失败时返回 null。
   *   仅稳定 kind token，不含 buffer/body/path。
   */
  getHistorySyncFailure: (sessionId: string) => TerminalHistorySyncFailure | null;
  /**
   * Business Logic（为什么需要这个方法）:
   *   React 组件需订阅 historySyncFailure 变更（R12 M3）。
   *
   * Code Logic（这个方法做什么）:
   *   注册 listener，返回 unsubscribe。
   */
  subscribeHistorySyncFailures: (listener: () => void) => () => void;
  /**
   * Business Logic（为什么需要这个方法）:
   *   useSyncExternalStore 需要 revision 作为 getSnapshot。
   *
   * Code Logic（这个方法做什么）:
   *   返回当前 historySyncFailures revision 号。
   */
  getHistorySyncFailuresRevision: () => number;
  /**
   * Business Logic（为什么需要这个方法）:
   *   用户在终端 UI 看到失败后可显式重试 history 同步（R12 M3）。
   *
   * Code Logic（这个方法做什么）:
   *   clear failure → 抬 epoch needsReplay → requestSessionReplay。
   */
  retryHistorySync: (sessionId: string) => void;
  /**
   * Business Logic（为什么需要这个方法）:
   *   启动 sessions.list 永久失败后 UI 需展示可观察状态（R13 M1）。
   *
   * Code Logic（这个方法做什么）:
   *   返回 StartupBaselineFailure 或 null；仅稳定 kind。
   */
  getStartupBaselineFailure: () => StartupBaselineFailure | null;
  /**
   * Business Logic（为什么需要这个方法）:
   *   订阅启动 list 失败状态变更（R13 M1）。
   *
   * Code Logic（这个方法做什么）:
   *   注册 listener，返回 unsubscribe。
   */
  subscribeStartupBaselineFailure: (listener: () => void) => () => void;
  /**
   * Business Logic（为什么需要这个方法）:
   *   useSyncExternalStore 读取 startup baseline failure revision。
   *
   * Code Logic（这个方法做什么）:
   *   返回 revision 号。
   */
  getStartupBaselineFailureRevision: () => number;
  /**
   * Business Logic（为什么需要这个方法）:
   *   用户可在启动 list 永久失败后手动重试枚举与 baseline schedule（R13 M1）。
   *
   * Code Logic（这个方法做什么）:
   *   清 failure → 重新执行有界 list + schedule 路径。
   */
  retryStartupBaseline: () => void;
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

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 终端区域需要在 history 同步永久失败时展示可观察 StatusMessage（R12 M3）。
 *
 * Code Logic（这个函数做什么）:
 *   用 useSyncExternalStore 订阅 historySyncFailures revision，再读 getHistorySyncFailure。
 *   hooks 全部在 early return 前；sessionId 为 null 时返回 null。
 */
export function useTerminalHistorySyncFailure(
  sessionId: string | null,
): TerminalHistorySyncFailure | null {
  const {
    getHistorySyncFailure,
    subscribeHistorySyncFailures,
    getHistorySyncFailuresRevision,
  } = useWorkbenchTerminalBuffers();

  const revision = useSyncExternalStore(
    subscribeHistorySyncFailures,
    getHistorySyncFailuresRevision,
    () => 0,
  );

  return useMemo(() => {
    if (!sessionId) return null;
    // revision 纳入依赖以强制在 notify 后重读。
    void revision;
    return getHistorySyncFailure(sessionId);
  }, [getHistorySyncFailure, revision, sessionId]);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 在启动 sessions.list 永久失败时需展示可观察 StatusMessage（R13 M1）。
 *
 * Code Logic（这个函数做什么）:
 *   用 useSyncExternalStore 订阅 startupBaselineFailure revision，再读 getStartupBaselineFailure。
 *   hooks 全部在 early return 前。
 */
export function useStartupBaselineFailure(): StartupBaselineFailure | null {
  const {
    getStartupBaselineFailure,
    subscribeStartupBaselineFailure,
    getStartupBaselineFailureRevision,
  } = useWorkbenchTerminalBuffers();

  const revision = useSyncExternalStore(
    subscribeStartupBaselineFailure,
    getStartupBaselineFailureRevision,
    () => 0,
  );

  return useMemo(() => {
    void revision;
    return getStartupBaselineFailure();
  }, [getStartupBaselineFailure, revision]);
}
