export type WorkbenchTerminalBuffers = Record<string, string>;

export const MAX_WORKBENCH_TERMINAL_BUFFER_CHARS = 200_000;

type WorkbenchTerminalBufferListener = () => void;

/**
 * Business Logic（为什么需要这个接口）:
 *   终端高频 append 需要合并到同一 animation frame 再通知 React，测试环境又需要确定性调度。
 *
 * Code Logic（这个接口做什么）:
 *   schedule 注册一帧后执行的 callback，并返回 cancel 函数以作废未触发的调度。
 */
export interface TerminalFrameScheduler {
  schedule(callback: () => void): () => void;
}

/**
 * Business Logic（为什么需要这个接口）:
 *   store 创建参数从位置参数迁移为 options，便于注入 initialBuffers / maxChars / frameScheduler。
 *
 * Code Logic（这个接口做什么）:
 *   描述 createWorkbenchTerminalBufferStore 的可选配置项。
 */
export interface TerminalBufferStoreOptions {
  initialBuffers?: WorkbenchTerminalBuffers;
  maxChars?: number;
  frameScheduler?: TerminalFrameScheduler;
}

export interface WorkbenchTerminalBufferStore {
  getBuffer: (sessionId: string | null) => string;
  getRevision: (sessionId: string | null) => number;
  subscribe: (sessionId: string | null, listener: WorkbenchTerminalBufferListener) => () => void;
  append: (sessionId: string, chunk: string) => void;
  reset: (sessionId: string) => void;
  remove: (sessionId: string) => void;
}

/**
 * Business Logic（为什么需要这个类型）:
 *   每个终端 session 需要独立维护 chunk 队列、裁剪偏移、物化缓存与帧调度代数。
 *
 * Code Logic（这个类型做什么）:
 *   描述单 session 的 ring buffer 内部状态，避免每次 append 都拼接完整字符串。
 */
interface SessionRingBuffer {
  chunks: string[];
  /** 第一个 chunk 内已裁剪掉的前缀字符数 */
  startOffset: number;
  /** 有效 UTF-16 code unit 总数（扣除 startOffset） */
  length: number;
  /** getBuffer 物化后的缓存；null 表示需要重新 join */
  materialized: string | null;
  revision: number;
  scheduledCancel: (() => void) | null;
  /** 每次 reset/remove/cancel 递增，防止 stale frame 误通知 */
  generation: number;
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
 *   浏览器/Tauri webview 默认用 animation frame 合并同帧多次 append 通知。
 *
 * Code Logic（这个函数做什么）:
 *   包装 requestAnimationFrame / cancelAnimationFrame，返回 TerminalFrameScheduler。
 */
function createDefaultFrameScheduler(): TerminalFrameScheduler {
  return {
    /**
     * Business Logic（为什么需要这个函数）:
     *   生产路径需要把多个 PTY chunk 的 React 通知合并到下一帧。
     *
     * Code Logic（这个函数做什么）:
     *   requestAnimationFrame 调度 callback，返回 cancelAnimationFrame 清理函数。
     */
    schedule(callback: () => void): () => void {
      const frameId = globalThis.requestAnimationFrame(callback);
      return () => {
        globalThis.cancelAnimationFrame(frameId);
      };
    },
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   新建或首次写入 session 时需要统一的空 ring buffer 结构。
 *
 * Code Logic（这个函数做什么）:
 *   返回 chunks 为空、revision/generation 为 0 的 SessionRingBuffer。
 */
function createEmptySessionBuffer(): SessionRingBuffer {
  return {
    chunks: [],
    startOffset: 0,
    length: 0,
    materialized: null,
    revision: 0,
    scheduledCancel: null,
    generation: 0,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   initialBuffers 以完整字符串 seed 时，需要转成 chunk deque 才能继续高频 append。
 *
 * Code Logic（这个函数做什么）:
 *   把字符串包装为单 chunk session，length 为字符串长度，materialized 直接缓存该字符串。
 */
function createSessionBufferFromString(content: string): SessionRingBuffer {
  if (content.length === 0) {
    return createEmptySessionBuffer();
  }
  return {
    chunks: [content],
    startOffset: 0,
    length: content.length,
    materialized: content,
    revision: 0,
    scheduledCancel: null,
    generation: 0,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   超过 maxChars 时必须丢掉最旧输出，否则内存无限增长且 xterm replay 也会过大。
 *
 * Code Logic（这个函数做什么）:
 *   从头推进 startOffset / 丢弃完整 chunk；只在边界 chunk 上 slice 一次；清空 materialized。
 */
function trimSessionBuffer(session: SessionRingBuffer, maxChars: number): void {
  if (session.length <= maxChars) return;

  let overflow = session.length - maxChars;
  while (overflow > 0 && session.chunks.length > 0) {
    const head = session.chunks[0] ?? '';
    const available = head.length - session.startOffset;
    if (overflow >= available) {
      session.chunks.shift();
      session.startOffset = 0;
      session.length -= available;
      overflow -= available;
    } else {
      session.startOffset += overflow;
      session.length -= overflow;
      overflow = 0;
    }
  }

  if (session.chunks.length === 0) {
    session.startOffset = 0;
    session.length = 0;
  }

  session.materialized = null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   xterm / React 订阅层需要完整字符串 snapshot，但 append 路径不能每次 join。
 *
 * Code Logic（这个函数做什么）:
 *   若 materialized 为空则 join（首 chunk 应用 startOffset），缓存后返回；否则直接返回缓存。
 */
function materializeSessionBuffer(session: SessionRingBuffer): string {
  if (session.materialized !== null) {
    return session.materialized;
  }

  if (session.chunks.length === 0 || session.length === 0) {
    session.materialized = '';
    return '';
  }

  let joined: string;
  if (session.chunks.length === 1) {
    const only = session.chunks[0] ?? '';
    joined = session.startOffset > 0 ? only.slice(session.startOffset) : only;
  } else {
    const parts: string[] = new Array(session.chunks.length);
    const first = session.chunks[0] ?? '';
    parts[0] = session.startOffset > 0 ? first.slice(session.startOffset) : first;
    for (let index = 1; index < session.chunks.length; index += 1) {
      parts[index] = session.chunks[index] ?? '';
    }
    joined = parts.join('');
  }

  session.materialized = joined;
  return joined;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   reset/remove 或重新 schedule 前必须作废未触发的帧，避免旧 generation 误 bump revision。
 *
 * Code Logic（这个函数做什么）:
 *   调用 scheduledCancel（若有）并清空引用；generation 由调用方在需要时递增。
 */
function cancelScheduledFrame(session: SessionRingBuffer): void {
  if (session.scheduledCancel) {
    session.scheduledCancel();
    session.scheduledCancel = null;
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   终端输出可能非常高频，不能让 React Context 每个 chunk 都唤醒整个应用和 Workbench 页面。
 *
 * Code Logic（这个函数做什么）:
 *   创建外部可变 ring-buffer store：按 session 维护 chunk deque + revision；append 仅 push 并
 *   在 animation frame 合并 notify；getBuffer 惰性物化并缓存；reset/remove 立即生效并取消 stale frame。
 */
export function createWorkbenchTerminalBufferStore(
  options: TerminalBufferStoreOptions = {},
): WorkbenchTerminalBufferStore {
  const maxChars = options.maxChars ?? MAX_WORKBENCH_TERMINAL_BUFFER_CHARS;
  const frameScheduler = options.frameScheduler ?? createDefaultFrameScheduler();
  const sessions = new Map<string, SessionRingBuffer>();
  const listenersBySession = new Map<string, Set<WorkbenchTerminalBufferListener>>();

  for (const [sessionId, content] of Object.entries(options.initialBuffers ?? {})) {
    sessions.set(sessionId, createSessionBufferFromString(content));
  }

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
   *   首次 append 前需要拿到可写的 session 状态，避免调用方重复判空。
   *
   * Code Logic（这个函数做什么）:
   *   若 map 中无该 session 则创建空 ring buffer 并登记后返回。
   */
  const ensureSession = (sessionId: string): SessionRingBuffer => {
    let session = sessions.get(sessionId);
    if (!session) {
      session = createEmptySessionBuffer();
      sessions.set(sessionId, session);
    }
    return session;
  };

  /**
   * Business Logic（为什么需要这个函数）:
   *   同帧多次 append 只需一次 React 重渲染；生产用 rAF，测试注入 scheduler。
   *
   * Code Logic（这个函数做什么）:
   *   若尚无 pending frame 则 schedule。先登记 wrappedCancel，再调用 schedule，避免同步
   *   scheduler（callback 在返回前已执行）把已清空的 cancel 句柄再次写回并卡住后续通知。
   *   回调入口清掉本轮 pending token；generation 校验后 bump revision 并 notify 一次。
   */
  const scheduleNotify = (sessionId: string, session: SessionRingBuffer): void => {
    if (session.scheduledCancel) return;

    const scheduledGeneration = session.generation;
    let active = true;
    let cancelFromScheduler: () => void = () => {};

    const wrappedCancel = (): void => {
      if (!active) return;
      active = false;
      cancelFromScheduler();
      if (session.scheduledCancel === wrappedCancel) {
        session.scheduledCancel = null;
      }
    };

    // 先绑定 pending token，同步 callback 才能正确识别并清空
    session.scheduledCancel = wrappedCancel;

    cancelFromScheduler = frameScheduler.schedule(() => {
      const current = sessions.get(sessionId);
      // 回调入口先清 pending，防止同步/异步路径留下 stale cancel
      if (current && current.scheduledCancel === wrappedCancel) {
        current.scheduledCancel = null;
      }
      if (!active) return;
      active = false;
      if (!current || current.generation !== scheduledGeneration) return;
      current.revision += 1;
      notify(sessionId);
    });

    // 同步 scheduler 时 callback 已跑完并清空；若仍挂着本轮 token 则强制清除
    if (!active && session.scheduledCancel === wrappedCancel) {
      session.scheduledCancel = null;
    }
  };

  /**
   * Business Logic（为什么需要这个函数）:
   *   reset/remove 是显式生命周期事件，订阅方需要立刻看到空/删除态，不能等下一帧。
   *
   * Code Logic（这个函数做什么）:
   *   取消 pending frame、递增 generation、bump revision 并立即 notify。
   */
  const notifyImmediately = (sessionId: string, session: SessionRingBuffer): void => {
    cancelScheduledFrame(session);
    session.generation += 1;
    session.revision += 1;
    notify(sessionId);
  };

  return {
    getBuffer(sessionId) {
      if (!sessionId) return '';
      const session = sessions.get(sessionId);
      if (!session) return '';
      return materializeSessionBuffer(session);
    },
    getRevision(sessionId) {
      if (!sessionId) return 0;
      return sessions.get(sessionId)?.revision ?? 0;
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
      // 空 chunk 不改内容，也不调度 notify，避免无意义 revision+1 / re-render
      if (chunk.length === 0) return;
      const session = ensureSession(sessionId);
      session.chunks.push(chunk);
      session.length += chunk.length;
      session.materialized = null;
      trimSessionBuffer(session, maxChars);
      scheduleNotify(sessionId, session);
    },
    reset(sessionId) {
      const session = ensureSession(sessionId);
      session.chunks = [];
      session.startOffset = 0;
      session.length = 0;
      session.materialized = '';
      notifyImmediately(sessionId, session);
    },
    remove(sessionId) {
      // remove 后仍保留 tombstone revision：与旧实现 revisions map 一致，避免 getRevision 回落到 0
      // 导致 useSyncExternalStore 认为 snapshot 未变。
      const existing = sessions.get(sessionId) ?? createEmptySessionBuffer();
      cancelScheduledFrame(existing);
      existing.generation += 1;
      sessions.set(sessionId, {
        chunks: [],
        startOffset: 0,
        length: 0,
        materialized: '',
        revision: existing.revision + 1,
        scheduledCancel: null,
        generation: existing.generation,
      });
      notify(sessionId);
    },
  };
}
