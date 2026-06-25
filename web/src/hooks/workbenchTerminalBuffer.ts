export type WorkbenchTerminalBuffers = Record<string, string>;

export const MAX_WORKBENCH_TERMINAL_BUFFER_CHARS = 200_000;

type WorkbenchTerminalBufferListener = () => void;

export interface WorkbenchTerminalBufferStore {
  getBuffer: (sessionId: string | null) => string;
  getRevision: (sessionId: string | null) => number;
  subscribe: (sessionId: string | null, listener: WorkbenchTerminalBufferListener) => () => void;
  append: (sessionId: string, chunk: string) => void;
  reset: (sessionId: string) => void;
  remove: (sessionId: string) => void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 页面切出后，常驻终端 Provider 仍要持续缓存 PTY/tmux 输出，切回时 xterm 可 replay。
 *
 * Code Logic（这个函数做什么）:
 *   将指定 session 的输出追加到 buffer，并只保留末尾 maxChars 个字符，避免内存无限增长。
 */
export function appendWorkbenchTerminalOutput(
  buffers: WorkbenchTerminalBuffers,
  sessionId: string,
  chunk: string,
  maxChars = MAX_WORKBENCH_TERMINAL_BUFFER_CHARS,
): WorkbenchTerminalBuffers {
  const nextBuffer = `${buffers[sessionId] ?? ''}${chunk}`;
  return {
    ...buffers,
    [sessionId]: nextBuffer.length > maxChars ? nextBuffer.slice(-maxChars) : nextBuffer,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   新建 terminal window 后应从空屏幕开始 replay，避免复用同 id 的旧输出残留。
 *
 * Code Logic（这个函数做什么）:
 *   返回浅拷贝对象，并把指定 session buffer 置为空字符串。
 */
export function resetWorkbenchTerminalBuffer(
  buffers: WorkbenchTerminalBuffers,
  sessionId: string,
): WorkbenchTerminalBuffers {
  return {
    ...buffers,
    [sessionId]: '',
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用户关闭 terminal window 后，对应输出缓存不应继续占用内存或在未来误 replay。
 *
 * Code Logic（这个函数做什么）:
 *   从浅拷贝对象中删除指定 session buffer。
 */
export function removeWorkbenchTerminalBuffer(
  buffers: WorkbenchTerminalBuffers,
  sessionId: string,
): WorkbenchTerminalBuffers {
  const next = { ...buffers };
  delete next[sessionId];
  return next;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   终端输出可能非常高频，不能让 React Context 每个 chunk 都唤醒整个应用和 Workbench 页面。
 *
 * Code Logic（这个函数做什么）:
 *   创建一个外部可变缓存 store；按 sessionId 维护 buffer/revision，并只通知该 session 的订阅者。
 */
export function createWorkbenchTerminalBufferStore(
  initialBuffers: WorkbenchTerminalBuffers = {},
  maxChars = MAX_WORKBENCH_TERMINAL_BUFFER_CHARS,
): WorkbenchTerminalBufferStore {
  let buffers = { ...initialBuffers };
  const revisions: Record<string, number> = {};
  const listenersBySession = new Map<string, Set<WorkbenchTerminalBufferListener>>();

  /**
   * Business Logic（为什么需要这个函数）:
   *   某个终端 session 输出变化后，只需要唤醒该 session 的 xterm pane。
   *
   * Code Logic（这个函数做什么）:
   *   查找 sessionId 对应 listener 集合并逐个执行；没有订阅者时直接返回。
   */
  const notify = (sessionId: string): void => {
    const listeners = listenersBySession.get(sessionId);
    if (!listeners) return;
    listeners.forEach((listener) => listener());
  };

  /**
   * Business Logic（为什么需要这个函数）:
   *   React 订阅层需要稳定的数字 snapshot 判断某个 session 的 buffer 是否变化。
   *
   * Code Logic（这个函数做什么）:
   *   自增指定 session 的 revision 后通知该 session 的订阅者。
   */
  const bumpRevision = (sessionId: string): void => {
    revisions[sessionId] = (revisions[sessionId] ?? 0) + 1;
    notify(sessionId);
  };

  return {
    getBuffer(sessionId) {
      if (!sessionId) return '';
      return buffers[sessionId] ?? '';
    },
    getRevision(sessionId) {
      if (!sessionId) return 0;
      return revisions[sessionId] ?? 0;
    },
    subscribe(sessionId, listener) {
      if (!sessionId) return () => {};
      const listeners = listenersBySession.get(sessionId) ?? new Set();
      listeners.add(listener);
      listenersBySession.set(sessionId, listeners);
      return () => {
        listeners.delete(listener);
        if (listeners.size === 0) {
          listenersBySession.delete(sessionId);
        }
      };
    },
    append(sessionId, chunk) {
      buffers = appendWorkbenchTerminalOutput(buffers, sessionId, chunk, maxChars);
      bumpRevision(sessionId);
    },
    reset(sessionId) {
      buffers = resetWorkbenchTerminalBuffer(buffers, sessionId);
      bumpRevision(sessionId);
    },
    remove(sessionId) {
      buffers = removeWorkbenchTerminalBuffer(buffers, sessionId);
      bumpRevision(sessionId);
    },
  };
}
