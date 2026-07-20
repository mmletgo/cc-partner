export type WorkbenchTerminalBuffers = Record<string, string>;

export const MAX_WORKBENCH_TERMINAL_BUFFER_CHARS = 200_000;

/** 废弃 chunk 前缀达到该数量且至少占数组一半时才物理 compact。 */
const COMPACT_HEAD_THRESHOLD = 1_024;

type WorkbenchTerminalBufferListener = () => void;
type WorkbenchTerminalLiveListener = (delta: TerminalBufferDelta) => void;
type WorkbenchTerminalResetListener = () => void;

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
 *   xterm live writer 与 snapshot 握手需要可比较的进程内游标，避免重复写入与漏写。
 *
 * Code Logic（这个接口做什么）:
 *   generation 在 reset/remove 时递增；appendId 在同代每次 append 后单调递增。
 */
export interface TerminalBufferCursor {
  generation: number;
  appendId: number;
  /**
   * Business Logic（为什么需要这个字段）:
   *   桌面 live stream 可能在 resync/baseline 完成前已推送相同 seq 的 chunk；
   *   store 必须按 owner lastSeq 丢弃重复输出，避免 TUI 双写。
   *
   * Code Logic（这个字段做什么）:
   *   可选的后端 PTY seq 上沿；undefined 表示本机 generation/appendId 握手路径。
   */
  lastSeq?: number;
}

/**
 * Business Logic（为什么需要这个接口）:
 *   首次挂载、resync 与 React 非 live 消费者需要完整 buffer 与游标快照。
 *
 * Code Logic（这个接口做什么）:
 *   携带物化 buffer、当前 cursor 与 React revision。
 */
export interface TerminalBufferSnapshot {
  buffer: string;
  cursor: TerminalBufferCursor;
  revision: number;
}

/**
 * Business Logic（为什么需要这个接口）:
 *   已挂载 xterm 只消费增量 chunk，不能每次把完整历史送进 React diff。
 *
 * Code Logic（这个接口做什么）:
 *   描述一次 append 的 session、chunk 与握手游标。
 */
export interface TerminalBufferDelta extends TerminalBufferCursor {
  sessionId: string;
  chunk: string;
}

/**
 * Business Logic（为什么需要这个接口）:
 *   store 创建参数从位置参数迁移为 options，便于注入 initialBuffers / maxChars / frameScheduler。
 *
 * Code Logic（这个接口做什么）:
 *   描述 createWorkbenchTerminalBufferStore 的可选配置项；测试 seam 仅用于观测物化与 compact。
 */
export interface TerminalBufferStoreOptions {
  initialBuffers?: WorkbenchTerminalBuffers;
  maxChars?: number;
  frameScheduler?: TerminalFrameScheduler;
  /** 仅测试：真正 join/slice 物化时回调 */
  onMaterializeForTest?: () => void;
  /** 仅测试：物理 compact 废弃前缀时回调 */
  onCompactForTest?: () => void;
}

/**
 * Business Logic（为什么需要这个接口）:
 *   终端输出缓存既要服务 React revision 订阅，也要服务 mounted xterm 的同步 live delta。
 *
 * Code Logic（这个接口做什么）:
 *   暴露 snapshot/revision/live/reset 订阅与 append/reset/remove 变更 API。
 */
export interface WorkbenchTerminalBufferStore {
  getSnapshot: (sessionId: string | null) => TerminalBufferSnapshot;
  getBuffer: (sessionId: string | null) => string;
  getRevision: (sessionId: string | null) => number;
  /** 读取 session 已应用的 owner lastSeq（无则 0）。 */
  getLastSeq: (sessionId: string | null) => number;
  subscribe: (sessionId: string | null, listener: WorkbenchTerminalBufferListener) => () => void;
  subscribeLive: (
    sessionId: string | null,
    listener: WorkbenchTerminalLiveListener,
  ) => () => void;
  subscribeReset: (sessionId: string, listener: WorkbenchTerminalResetListener) => () => void;
  /**
   * Business Logic（为什么需要这个函数）:
   *   live terminal-output 可能带 seq；<= lastSeq 的事件在 baseline/resync 后必须丢弃。
   *   owner 切换时必须按 authority 重置 lastSeq 基线，否则新 owner 低 seq 会被当重复丢弃。
   *
   * Code Logic（这个函数做什么）:
   *   可选 seq / authorityId；authority 变更时先把 lastSeq 置 0 再比较；
   *   若提供 seq 且 seq<=session.lastSeq 则 no-op；成功 append 后推进 lastSeq。
   */
  append: (
    sessionId: string,
    chunk: string,
    seq?: number,
    authorityId?: string | null,
  ) => void;
  /**
   * Business Logic（为什么需要这个函数）:
   *   resync/baseline 写入完整 buffer 后必须记录 lastSeq 作为 cutover，并绑定 stream authority。
   *
   * Code Logic（这个函数做什么）:
   *   提升 generation、seed buffer，设置 lastSeq（缺省 0）与可选 authorityId。
   */
  reset: (
    sessionId: string,
    buffer?: string,
    lastSeq?: number,
    authorityId?: string | null,
  ) => void;
  remove: (sessionId: string) => void;
}

/**
 * Business Logic（为什么需要这个类型）:
 *   每个终端 session 需要独立维护 chunk 队列、裁剪偏移、物化缓存与帧调度代数。
 *
 * Code Logic（这个类型做什么）:
 *   描述单 session 的 ring buffer 内部状态；headIndex 推进废弃前缀，避免每次 shift。
 */
interface SessionRingBuffer {
  chunks: string[];
  /** 逻辑读头：小于 headIndex 的 chunk 已废弃，不得再 shift */
  headIndex: number;
  /** 活动首 chunk 内已裁剪掉的前缀字符数 */
  startOffset: number;
  /** 有效 UTF-16 code unit 总数（扣除 startOffset） */
  length: number;
  /** getSnapshot 物化后的缓存；null 表示需要重新 join */
  materialized: string | null;
  revision: number;
  scheduledCancel: (() => void) | null;
  /** reset/remove 递增；append 帧通知也用它作废 stale frame */
  generation: number;
  /** 同 generation 内每次成功 append 后递增 */
  appendId: number;
  /** owner replay/output 的 lastSeq 上沿（用于 stream cutover） */
  lastSeq: number;
  /** 当前 ring 绑定的 ownerInstanceId（null 表示尚未绑定） */
  authorityId: string | null;
}

const EMPTY_SNAPSHOT: TerminalBufferSnapshot = {
  buffer: '',
  cursor: { generation: 0, appendId: 0, lastSeq: 0 },
  revision: 0,
};

/**
 * Business Logic（为什么需要这个类型）:
 *   baseline/resync Promise 完成前，live terminal-output 可能已带着更新的 seq 到达；
 *   Provider 需要暂存这些事件，避免 cutover 后永久丢失 relay 不会重发的 chunk。
 *
 * Code Logic（这个类型做什么）:
 *   描述一条带 owner seq 的 live 输出；chunk 为原始字符串，seq 为有限 number。
 */
export interface HeldLiveTerminalEvent {
  chunk: string;
  seq: number;
}

/** Provider 侧每个 session 最多暂存的带 seq live 事件条数（有界，防内存膨胀）。 */
export const MAX_HELD_LIVE_TERMINAL_EVENTS = 256;

/**
 * Business Logic（为什么需要这个接口）:
 *   乱序 baseline/resync 与 held 溢出 re-baseline 并发时，必须按 session 单调拒绝
 *   更旧的 cutover，否则已提交的较新 baseline 会被晚到的旧 A 再次 reset 抹掉；
 *   溢出后还要记住 high-water seq，避免无 epoch 的旧 launch baseline 误清 needsReplay。
 *   owner 重启后 reader seq 从 0 起算，必须按 ownerInstanceId 分代，否则新低 seq 被当重复丢弃。
 *
 * Code Logic（这个接口做什么）:
 *   记录已提交 baseline lastSeq、cutover 代数 epoch、needsReplay、replayInFlight、
 *   overflowHighWaterSeq（0 表示无溢出水位），以及 stream authority（ownerInstanceId）。
 */
export interface SessionCutoverState {
  committedBaselineLastSeq: number;
  cutoverEpoch: number;
  needsReplay: boolean;
  replayInFlight: boolean;
  /**
   * Business Logic（为什么需要这个字段）:
   *   held 溢出清空后，无 epoch 的 launch/resync cutover 若 lastSeq 未盖住溢出水位，
   *   不得清 needsReplay，否则当前 epoch 的 replay 失败会留下永久终端缺口。
   *
   * Code Logic（这个字段做什么）:
   *   溢出时取 max(已有水位, 被清空 held 的最大有限 seq, 0)；0 表示无溢出保护水位。
   */
  overflowHighWaterSeq: number;
  /**
   * Business Logic（为什么需要这个字段）:
   *   持久 tmux session 在新 owner 恢复时 output seq 从 0 重启；只比 lastSeq 会永久冻结终端。
   *
   * Code Logic（这个字段做什么）:
   *   最近一次已提交 cutover/live 的 ownerInstanceId；null 表示尚未绑定 authority。
   */
  authorityId: string | null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   每个终端 session 首次遇到 live/baseline 时需要统一的 cutover 初始态。
 *
 * Code Logic（这个函数做什么）:
 *   返回 committedBaselineLastSeq/cutoverEpoch/overflowHighWaterSeq=0、
 *   needsReplay/replayInFlight=false 的新状态。
 */
export function createEmptySessionCutoverState(): SessionCutoverState {
  return {
    committedBaselineLastSeq: 0,
    cutoverEpoch: 0,
    needsReplay: false,
    replayInFlight: false,
    overflowHighWaterSeq: 0,
    authorityId: null,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   判断 cutover/live 是否切换到新的 owner stream authority。
 *
 * Code Logic（这个函数做什么）:
 *   非空 authorityId 且与 state.authorityId 不同（含 state 尚为 null）→ true。
 */
export function isTerminalAuthorityChange(
  state: SessionCutoverState,
  authorityId?: string | null,
): boolean {
  if (typeof authorityId !== 'string' || authorityId.length === 0) {
    return false;
  }
  return state.authorityId !== authorityId;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   晚到的旧 baseline A 或 held 溢出后作废的 in-flight replay 不得 clobber 已提交较新 B；
 *   但 owner 切换时新 authority 的较低 lastSeq 必须接受，否则终端永久冻结。
 *
 * Code Logic（这个函数做什么）:
 *   lastSeq 非有限 → reject；authority 变更 → accept；
 *   lastSeq < committedBaselineLastSeq → reject；
 *   若提供 requestEpoch 且 requestEpoch < cutoverEpoch → reject；否则 accept。
 */
export function shouldAcceptTerminalCutover(
  state: SessionCutoverState,
  lastSeq: number,
  requestEpoch?: number,
  authorityId?: string | null,
): boolean {
  if (typeof lastSeq !== 'number' || !Number.isFinite(lastSeq)) {
    return false;
  }
  if (isTerminalAuthorityChange(state, authorityId)) {
    return true;
  }
  if (lastSeq < state.committedBaselineLastSeq) {
    return false;
  }
  if (
    typeof requestEpoch === 'number' &&
    Number.isFinite(requestEpoch) &&
    requestEpoch < state.cutoverEpoch
  ) {
    return false;
  }
  return true;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   steady-state live 不得无限写入 held；仅异步 baseline/replay 窗口需要暂存。
 *
 * Code Logic（这个函数做什么）:
 *   baseline 未 settle → true；needsReplay 或 replayInFlight → true；否则 false。
 */
export function shouldCollectHeldLiveTerminalEvent(
  state: SessionCutoverState,
  baselineSettled: boolean,
): boolean {
  if (!baselineSettled) {
    return true;
  }
  return state.needsReplay || state.replayInFlight;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   overflow 后 needsReplay 是防永久缺口的闸门；无 epoch 的旧 baseline 只要 lastSeq
 *   未盖住 overflow high-water，就绝不能清掉 needsReplay。
 *
 * Code Logic（这个函数做什么）:
 *   !needsReplay → true；requestEpoch 有限且等于 cutoverEpoch → true；
 *   overflowHighWaterSeq>0 且 lastSeq>=highWater → true；否则 false。
 */
export function shouldClearTerminalNeedsReplay(
  state: SessionCutoverState,
  lastSeq: number,
  requestEpoch?: number,
): boolean {
  if (!state.needsReplay) {
    return true;
  }
  if (
    typeof requestEpoch === 'number' &&
    Number.isFinite(requestEpoch) &&
    requestEpoch === state.cutoverEpoch
  ) {
    return true;
  }
  if (state.overflowHighWaterSeq > 0 && lastSeq >= state.overflowHighWaterSeq) {
    return true;
  }
  return false;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   成功 apply baseline/resync 后必须抬高已提交 lastSeq；但只有在当前 epoch 匹配
 *   或 lastSeq 盖住 overflow high-water 时才清 needsReplay，避免 stale baseline 误关闸。
 *   owner 切换时同时绑定新 authorityId，作为后续 seq 比较基线。
 *
 * Code Logic（这个函数做什么）:
 *   总是更新 committedBaselineLastSeq=lastSeq；非空 authorityId 写入 state；
 *   仅当 shouldClearTerminalNeedsReplay 为真时置 needsReplay=false 且 overflowHighWaterSeq=0；
 *   保留 cutoverEpoch/replayInFlight。
 */
export function commitTerminalCutover(
  state: SessionCutoverState,
  lastSeq: number,
  requestEpoch?: number,
  authorityId?: string | null,
): SessionCutoverState {
  const clearNeedsReplay = shouldClearTerminalNeedsReplay(
    state,
    lastSeq,
    requestEpoch,
  );
  const nextAuthority =
    typeof authorityId === 'string' && authorityId.length > 0
      ? authorityId
      : state.authorityId;
  return {
    ...state,
    committedBaselineLastSeq: lastSeq,
    needsReplay: clearNeedsReplay ? false : state.needsReplay,
    overflowHighWaterSeq: clearNeedsReplay ? 0 : state.overflowHighWaterSeq,
    authorityId: nextAuthority,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   held live 超过有界上限时，静默 drop-oldest 会在 cutover 后留下永久缺口（relay 不重发）；
 *   必须抬高 cutover epoch、标记 needsReplay，并记录溢出 high-water，让无 epoch 的旧
 *   baseline 无法误清 needsReplay，同时让 in-flight 旧结果失效并触发 re-baseline。
 *
 * Code Logic（这个函数做什么）:
 *   cutoverEpoch+=1、needsReplay=true；overflowHighWaterSeq=max(已有, 入参 high water, 0)；
 *   返回新 state 与供 in-flight 请求绑定的 requestEpoch。
 */
export function beginHeldOverflowReplay(
  state: SessionCutoverState,
  overflowHighWaterSeq?: number,
): { state: SessionCutoverState; requestEpoch: number } {
  const cutoverEpoch = state.cutoverEpoch + 1;
  const provided =
    typeof overflowHighWaterSeq === 'number' && Number.isFinite(overflowHighWaterSeq)
      ? overflowHighWaterSeq
      : 0;
  const highWater = Math.max(state.overflowHighWaterSeq, provided, 0);
  return {
    state: {
      ...state,
      cutoverEpoch,
      needsReplay: true,
      overflowHighWaterSeq: highWater,
    },
    requestEpoch: cutoverEpoch,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Provider 在启动/结束 sessions.replay 时需要更新 in-flight 门闩，避免同 session 狂刷。
 *
 * Code Logic（这个函数做什么）:
 *   返回浅拷贝，仅覆写 replayInFlight。
 */
export function setTerminalCutoverReplayInFlight(
  state: SessionCutoverState,
  replayInFlight: boolean,
): SessionCutoverState {
  return {
    ...state,
    replayInFlight,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   过期 baseline/resync 的 reset 会抹掉 reset 前已 append 的更新 live chunk，
 *   而 owner relay 不会重发这些 seq；cutover 必须在 reset 后把 held 中更新的事件写回。
 *   owner 切换时用新 authority 重置 seq 基线，旧 authority 的 held 不得写回。
 *
 * Code Logic（这个函数做什么）:
 *   先 store.reset(sessionId, buffer, lastSeq, authorityId)；
 *   authority 变更时丢弃全部 held 返回 []；
 *   否则按序对 held 中 seq > lastSeq 的事件 store.append(..., authorityId)；
 *   返回仍应保留跟踪的 pruned held 列表。
 *   调用方必须先 shouldAcceptTerminalCutover 再调用本函数。
 */
export function applyTerminalBaselineCutover(
  store: WorkbenchTerminalBufferStore,
  sessionId: string,
  buffer: string,
  lastSeq: number,
  heldLiveEvents: readonly HeldLiveTerminalEvent[],
  authorityId?: string | null,
  authorityChanged = false,
): HeldLiveTerminalEvent[] {
  store.reset(sessionId, buffer, lastSeq, authorityId);
  if (authorityChanged) {
    // 旧 owner 的 held seq 与新 stream 不可比，全部丢弃。
    return [];
  }
  const pruned: HeldLiveTerminalEvent[] = [];
  for (const event of heldLiveEvents) {
    if (typeof event.seq !== 'number' || !Number.isFinite(event.seq)) {
      continue;
    }
    if (event.seq <= lastSeq) {
      continue;
    }
    store.append(sessionId, event.chunk, event.seq, authorityId);
    pruned.push(event);
  }
  return pruned;
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
 *   返回 chunks 为空、revision/generation/appendId 为 0 的 SessionRingBuffer。
 */
function createEmptySessionBuffer(): SessionRingBuffer {
  return {
    chunks: [],
    headIndex: 0,
    startOffset: 0,
    length: 0,
    materialized: null,
    revision: 0,
    scheduledCancel: null,
    generation: 0,
    appendId: 0,
    lastSeq: 0,
    authorityId: null,
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
    headIndex: 0,
    startOffset: 0,
    length: content.length,
    materialized: content,
    revision: 0,
    scheduledCancel: null,
    generation: 0,
    appendId: 0,
    lastSeq: 0,
    authorityId: null,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   满容量后若每轮 Array.shift，小 chunk 热路径会变成 O(n)；需要摊销 compact。
 *
 * Code Logic（这个函数做什么）:
 *   headIndex 足够大且废弃前缀至少占一半时，slice 掉前缀并归零 headIndex。
 */
function maybeCompactSessionBuffer(
  session: SessionRingBuffer,
  onCompact?: () => void,
): void {
  const discarded = session.headIndex;
  const total = session.chunks.length;
  if (discarded < COMPACT_HEAD_THRESHOLD) return;
  if (discarded * 2 < total) return;
  session.chunks = session.chunks.slice(session.headIndex);
  session.headIndex = 0;
  onCompact?.();
}

/**
 * Business Logic（为什么需要这个函数）:
 *   超过 maxChars 时必须丢掉最旧输出，否则内存无限增长且 xterm replay 也会过大。
 *
 * Code Logic（这个函数做什么）:
 *   只推进 headIndex / startOffset 丢弃前缀；禁止 Array.shift；必要时摊销 compact。
 */
function trimSessionBuffer(
  session: SessionRingBuffer,
  maxChars: number,
  onCompact?: () => void,
): void {
  if (session.length <= maxChars) return;

  let overflow = session.length - maxChars;
  while (overflow > 0 && session.headIndex < session.chunks.length) {
    const head = session.chunks[session.headIndex] ?? '';
    const available = head.length - session.startOffset;
    if (overflow >= available) {
      session.headIndex += 1;
      session.startOffset = 0;
      session.length -= available;
      overflow -= available;
    } else {
      session.startOffset += overflow;
      session.length -= overflow;
      overflow = 0;
    }
  }

  if (session.headIndex >= session.chunks.length) {
    session.chunks = [];
    session.headIndex = 0;
    session.startOffset = 0;
    session.length = 0;
  } else {
    maybeCompactSessionBuffer(session, onCompact);
  }

  session.materialized = null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   xterm / React 订阅层需要完整字符串 snapshot，但 append 路径不能每次 join。
 *
 * Code Logic（这个函数做什么）:
 *   若 materialized 为空则从 headIndex 起 join（活动首 chunk 应用 startOffset），缓存后返回。
 */
function materializeSessionBuffer(
  session: SessionRingBuffer,
  onMaterialize?: () => void,
): string {
  if (session.materialized !== null) {
    return session.materialized;
  }

  onMaterialize?.();

  if (session.headIndex >= session.chunks.length || session.length === 0) {
    session.materialized = '';
    return '';
  }

  const activeCount = session.chunks.length - session.headIndex;
  let joined: string;
  if (activeCount === 1) {
    const only = session.chunks[session.headIndex] ?? '';
    joined = session.startOffset > 0 ? only.slice(session.startOffset) : only;
  } else {
    const parts: string[] = new Array(activeCount);
    const first = session.chunks[session.headIndex] ?? '';
    parts[0] = session.startOffset > 0 ? first.slice(session.startOffset) : first;
    for (let index = 1; index < activeCount; index += 1) {
      parts[index] = session.chunks[session.headIndex + index] ?? '';
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
 *   创建外部可变 ring-buffer store：按 session 维护 chunk deque + cursor；append 只 push/trim 并
 *   同步发布 live delta，React revision 仍经 animation frame 合并；snapshot 惰性物化。
 */
export function createWorkbenchTerminalBufferStore(
  options: TerminalBufferStoreOptions = {},
): WorkbenchTerminalBufferStore {
  const maxChars = options.maxChars ?? MAX_WORKBENCH_TERMINAL_BUFFER_CHARS;
  const frameScheduler = options.frameScheduler ?? createDefaultFrameScheduler();
  const onMaterializeForTest = options.onMaterializeForTest;
  const onCompactForTest = options.onCompactForTest;
  const sessions = new Map<string, SessionRingBuffer>();
  const listenersBySession = new Map<string, Set<WorkbenchTerminalBufferListener>>();
  const liveListenersBySession = new Map<string, Set<WorkbenchTerminalLiveListener>>();
  const resetListenersBySession = new Map<string, Set<WorkbenchTerminalResetListener>>();

  for (const [sessionId, content] of Object.entries(options.initialBuffers ?? {})) {
    sessions.set(sessionId, createSessionBufferFromString(content));
  }

  /**
   * Business Logic（为什么需要这个函数）:
   *   某个终端 session 输出变化后，只需要唤醒该 session 的 React 订阅方。
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
   *   mounted xterm 需要同步收到每个 chunk，不能等 rAF/React。
   *
   * Code Logic（这个函数做什么）:
   *   同步调用该 session 的 live listeners。
   */
  const notifyLive = (sessionId: string, delta: TerminalBufferDelta): void => {
    const listeners = liveListenersBySession.get(sessionId);
    if (!listeners) return;
    listeners.forEach((listener) => listener(delta));
  };

  /**
   * Business Logic（为什么需要这个函数）:
   *   resync/remove 后旧 generation 的 writer 队列必须立刻失效并重放。
   *
   * Code Logic（这个函数做什么）:
   *   同步调用该 session 的 reset listeners。
   */
  const notifyReset = (sessionId: string): void => {
    const listeners = resetListenersBySession.get(sessionId);
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
   *   seed/reset 字符串可能超过 maxChars，需要与 append 相同的裁剪语义。
   *
   * Code Logic（这个函数做什么）:
   *   用单 chunk 写入 session，必要时 trim；empty 清空。
   */
  const seedSessionBuffer = (session: SessionRingBuffer, buffer: string): void => {
    if (buffer.length === 0) {
      session.chunks = [];
      session.headIndex = 0;
      session.startOffset = 0;
      session.length = 0;
      session.materialized = '';
      return;
    }
    session.chunks = [buffer];
    session.headIndex = 0;
    session.startOffset = 0;
    session.length = buffer.length;
    session.materialized = buffer;
    trimSessionBuffer(session, maxChars, onCompactForTest);
  };

  /**
   * Business Logic（为什么需要这个函数）:
   *   同帧多次 append 只需一次 React 重渲染；生产用 rAF，测试注入 scheduler。
   *
   * Code Logic（这个函数做什么）:
   *   若尚无 pending frame 则 schedule。先登记 wrappedCancel，再调用 schedule，避免同步
   *   scheduler（callback 在返回前已执行）把已清空的 cancel 句柄再次写回并卡住后续通知。
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

    session.scheduledCancel = wrappedCancel;

    cancelFromScheduler = frameScheduler.schedule(() => {
      const current = sessions.get(sessionId);
      if (current && current.scheduledCancel === wrappedCancel) {
        current.scheduledCancel = null;
      }
      if (!active) return;
      active = false;
      if (!current || current.generation !== scheduledGeneration) return;
      current.revision += 1;
      notify(sessionId);
    });

    if (!active && session.scheduledCancel === wrappedCancel) {
      session.scheduledCancel = null;
    }
  };

  /**
   * Business Logic（为什么需要这个函数）:
   *   首次挂载、resync 与测试读取需要完整 buffer 与握手 cursor。
   *
   * Code Logic（这个函数做什么）:
   *   惰性物化 session buffer，并返回 generation/appendId/revision 快照。
   */
  const getSnapshot = (sessionId: string | null): TerminalBufferSnapshot => {
    if (!sessionId) return EMPTY_SNAPSHOT;
    const session = sessions.get(sessionId);
    if (!session) return EMPTY_SNAPSHOT;
    return {
      buffer: materializeSessionBuffer(session, onMaterializeForTest),
      cursor: {
        generation: session.generation,
        appendId: session.appendId,
        lastSeq: session.lastSeq,
      },
      revision: session.revision,
    };
  };

  return {
    getSnapshot,
    getBuffer(sessionId) {
      return getSnapshot(sessionId).buffer;
    },
    getRevision(sessionId) {
      if (!sessionId) return 0;
      return sessions.get(sessionId)?.revision ?? 0;
    },
    getLastSeq(sessionId) {
      if (!sessionId) return 0;
      return sessions.get(sessionId)?.lastSeq ?? 0;
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
    subscribeLive(sessionId, listener) {
      if (!sessionId) return () => {};
      const listeners = liveListenersBySession.get(sessionId) ?? new Set();
      listeners.add(listener);
      liveListenersBySession.set(sessionId, listeners);
      return () => {
        listeners.delete(listener);
        if (listeners.size === 0) {
          liveListenersBySession.delete(sessionId);
        }
      };
    },
    subscribeReset(sessionId, listener) {
      const listeners = resetListenersBySession.get(sessionId) ?? new Set();
      listeners.add(listener);
      resetListenersBySession.set(sessionId, listeners);
      return () => {
        listeners.delete(listener);
        if (listeners.size === 0) {
          resetListenersBySession.delete(sessionId);
        }
      };
    },
    append(sessionId, chunk, seq, authorityId) {
      // 空 chunk 不改内容，也不发布 delta / schedule notify
      if (chunk.length === 0) return;
      const session = ensureSession(sessionId);
      if (typeof authorityId === 'string' && authorityId.length > 0) {
        if (session.authorityId !== authorityId) {
          // 新 owner stream：seq 从 0 起算，必须重置比较基线。
          session.authorityId = authorityId;
          session.lastSeq = 0;
        }
      }
      if (typeof seq === 'number' && Number.isFinite(seq) && seq <= session.lastSeq) {
        // baseline/resync 已包含该 seq；丢弃重复 live 事件
        return;
      }
      session.chunks.push(chunk);
      session.length += chunk.length;
      session.materialized = null;
      trimSessionBuffer(session, maxChars, onCompactForTest);
      session.appendId += 1;
      if (typeof seq === 'number' && Number.isFinite(seq) && seq > session.lastSeq) {
        session.lastSeq = seq;
      }
      notifyLive(sessionId, {
        sessionId,
        chunk,
        generation: session.generation,
        appendId: session.appendId,
        lastSeq: session.lastSeq,
      });
      scheduleNotify(sessionId, session);
    },
    reset(sessionId, buffer = '', lastSeq = 0, authorityId) {
      const session = ensureSession(sessionId);
      cancelScheduledFrame(session);
      session.generation += 1;
      session.appendId = 0;
      session.lastSeq =
        typeof lastSeq === 'number' && Number.isFinite(lastSeq) && lastSeq > 0 ? lastSeq : 0;
      if (typeof authorityId === 'string' && authorityId.length > 0) {
        session.authorityId = authorityId;
      }
      seedSessionBuffer(session, buffer);
      // reset 本身不伪造 live delta；只同步通知 reset listeners 再立即 bump revision
      notifyReset(sessionId);
      session.revision += 1;
      notify(sessionId);
    },
    remove(sessionId) {
      // remove 后仍保留 tombstone revision，避免 getRevision 回落到 0 导致 useSyncExternalStore 失真。
      const existing = sessions.get(sessionId) ?? createEmptySessionBuffer();
      cancelScheduledFrame(existing);
      existing.generation += 1;
      existing.appendId = 0;
      existing.lastSeq = 0;
      existing.authorityId = null;
      notifyReset(sessionId);
      sessions.set(sessionId, {
        chunks: [],
        headIndex: 0,
        startOffset: 0,
        length: 0,
        materialized: '',
        revision: existing.revision + 1,
        scheduledCancel: null,
        generation: existing.generation,
        appendId: 0,
        lastSeq: 0,
        authorityId: null,
      });
      notify(sessionId);
    },
  };
}
