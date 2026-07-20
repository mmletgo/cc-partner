import type {
  TerminalBufferCursor,
  TerminalBufferDelta,
  TerminalBufferSnapshot,
} from '@/hooks/workbenchTerminalBuffer';
import { MAX_WORKBENCH_TERMINAL_BUFFER_CHARS } from '@/hooks/workbenchTerminalBuffer';
import {
  writeTerminalReplay,
  type TerminalReplayGate,
} from './terminalReplay';

/**
 * Business Logic（为什么需要这个接口）:
 *   live writer 只依赖 xterm 的 clear/write，避免把 React 或完整 Terminal 类型拖进热路径模块。
 *
 * Code Logic（这个接口做什么）:
 *   描述可清空屏幕并以可选完成回调异步写入字符串的窄目标。
 */
export interface TerminalLiveWriterTarget {
  clear: () => void;
  write: (data: string, callback?: () => void) => void;
}

/**
 * Business Logic（为什么需要这个接口）:
 *   writer 从既有 buffer store 取 snapshot 与 live/reset 订阅，不得另建第二份事件状态。
 *
 * Code Logic（这个接口做什么）:
 *   暴露 getSnapshot / subscribeLive / subscribeReset 三方法。
 */
export interface TerminalLiveSource {
  getSnapshot: (sessionId: string) => TerminalBufferSnapshot;
  subscribeLive: (
    sessionId: string,
    listener: (delta: TerminalBufferDelta) => void,
  ) => () => void;
  subscribeReset: (sessionId: string, listener: () => void) => () => void;
}

/**
 * Business Logic（为什么需要这个接口）:
 *   pane 在 xterm dispose 前必须取消 live 订阅并作废 in-flight write 回调。
 *
 * Code Logic（这个接口做什么）:
 *   提供 dispose 清理函数。
 */
export interface TerminalLiveWriter {
  dispose: () => void;
}

export interface CreateTerminalLiveWriterOptions {
  terminal: TerminalLiveWriterTarget;
  source: TerminalLiveSource;
  sessionId: string;
  /** 历史 snapshot/resync 写入期间屏蔽 xterm 设备响应写回 PTY */
  gate?: TerminalReplayGate;
  /** pendingChunk 硬上限（UTF-16 code units），默认对齐 200k ring */
  maxPendingChars?: number;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   判断 delta 是否严格新于已应用游标，用于 snapshot 去重与 generation 失效。
 *
 * Code Logic（这个函数做什么）:
 *   generation 更大，或同代 appendId 更大时视为更新。
 */
function isNewerDelta(delta: TerminalBufferCursor, cursor: TerminalBufferCursor): boolean {
  if (delta.generation > cursor.generation) return true;
  if (delta.generation < cursor.generation) return false;
  return delta.appendId > cursor.appendId;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   snapshot/resync 必须沿用 replay gate，防止历史设备查询响应再次进入 PTY。
 *
 * Code Logic（这个函数做什么）:
 *   有 gate 时走 writeTerminalReplay 并在 write 完成回调里继续 handshake；无 gate 时直接 write。
 */
function writeSnapshotWithOptionalGate(
  terminal: TerminalLiveWriterTarget,
  data: string,
  gate: TerminalReplayGate | undefined,
  onComplete: () => void,
): void {
  if (!gate) {
    if (data.length === 0) {
      onComplete();
      return;
    }
    terminal.write(data, onComplete);
    return;
  }

  if (data.length === 0) {
    gate.current = false;
    onComplete();
    return;
  }

  writeTerminalReplay(terminal, data, gate, undefined, onComplete);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   已挂载 xterm 需要先订阅再读 snapshot，把真实 PTY 增量直接写入终端，绕过 rAF/React/KMP。
 *
 * Code Logic（这个函数做什么）:
 *   先 subscribe live/reset，再 snapshot；write in-flight 期间把后续 delta 合并为有界 next-buffer
 *   （单字符串 + 最后 cursor，硬上限 maxPendingChars）；完成回调后再按 generation/appendId 去重 drain；
 *   generation 变化时串行等待当前 write 完成后 clear + replay；pending 超限则 needsSnapshot。
 */
export function createTerminalLiveWriter(
  options: CreateTerminalLiveWriterOptions,
): TerminalLiveWriter {
  const {
    terminal,
    source,
    sessionId,
    gate,
    maxPendingChars = MAX_WORKBENCH_TERMINAL_BUFFER_CHARS,
  } = options;
  let disposed = false;
  let writeEpoch = 0;
  let writing = false;
  let appliedCursor: TerminalBufferCursor = { generation: -1, appendId: -1 };
  /** 有界 pending：合并为单一 next chunk，仅保留最后 cursor（generation/appendId）。 */
  let pendingChunk = '';
  let pendingCursor: TerminalBufferCursor | null = null;
  /** 超限或 generation 变化时，在当前 write 完成后 clear+replay 最新 snapshot。 */
  let needsSnapshot = false;
  let clearBeforeSnapshot = false;

  /**
   * Business Logic（为什么需要这个函数）:
   *   任意异步 write 完成或 dispose 后都必须检查 token，避免旧回调污染新 generation。
   *
   * Code Logic（这个函数做什么）:
   *   disposed 或 epoch 不匹配时返回 false。
   */
  const isCurrent = (epoch: number): boolean => !disposed && epoch === writeEpoch;

  /**
   * Business Logic（为什么需要这个函数）:
   *   snapshot 完成后或 drain 前需要丢弃已包含在 appliedCursor 内的 pending。
   *
   * Code Logic（这个函数做什么）:
   *   若 pendingCursor 不新于 appliedCursor 则清空有界缓冲。
   */
  const dropStalePending = (): void => {
    if (!pendingCursor || !isNewerDelta(pendingCursor, appliedCursor)) {
      pendingChunk = '';
      pendingCursor = null;
    }
  };

  /**
   * Business Logic（为什么需要这个函数）:
   *   首次挂载与 generation 变化后都要用完整 snapshot 重建屏幕，再接着 drain 更新。
   *   Gap/reset 若在 xterm write 回调完成前到达，必须等当前 write 结束后再 clear，
   *   否则旧字节可能在 clear 后继续 paint，与新 snapshot 混合。
   *
   * Code Logic（这个函数做什么）:
   *   若 writing，只标记 needsSnapshot 并串行；否则提升 epoch、可选 clear、写 snapshot。
   */
  const replaySnapshot = (clearFirst: boolean): void => {
    if (disposed) return;
    if (writing) {
      needsSnapshot = true;
      clearBeforeSnapshot = clearBeforeSnapshot || clearFirst;
      return;
    }

    const epoch = ++writeEpoch;
    writing = true;
    pendingChunk = '';
    pendingCursor = null;
    needsSnapshot = false;
    const shouldClear = clearFirst || clearBeforeSnapshot;
    clearBeforeSnapshot = false;
    const snapshot = source.getSnapshot(sessionId);
    // 先抬 appliedCursor，使 snapshot 期间到达的 <=cursor delta 不会进入有界 pending。
    appliedCursor = snapshot.cursor;
    if (shouldClear) {
      terminal.clear();
    }

    writeSnapshotWithOptionalGate(terminal, snapshot.buffer, gate, () => {
      if (!isCurrent(epoch)) return;
      writing = false;
      if (needsSnapshot) {
        const clear = clearBeforeSnapshot;
        needsSnapshot = false;
        clearBeforeSnapshot = false;
        replaySnapshot(clear);
        return;
      }
      dropStalePending();
      drainPending();
    });
  };

  /**
   * Business Logic（为什么需要这个函数）:
   *   live delta 到达时可能正有 write in-flight，需要按到达顺序合并下一批。
   *
   * Code Logic（这个函数做什么）:
   *   过滤 <= appliedCursor 的 pending，把有界 next-buffer 单次 write；完成后再 drain。
   */
  const drainPending = (): void => {
    if (disposed || writing) return;
    if (needsSnapshot) {
      const clear = clearBeforeSnapshot;
      needsSnapshot = false;
      clearBeforeSnapshot = false;
      replaySnapshot(clear);
      return;
    }
    dropStalePending();
    if (!pendingCursor) return;

    const data = pendingChunk;
    const last = pendingCursor;
    pendingChunk = '';
    pendingCursor = null;
    if (data.length === 0) {
      appliedCursor = { generation: last.generation, appendId: last.appendId };
      drainPending();
      return;
    }

    const epoch = writeEpoch;
    writing = true;
    terminal.write(data, () => {
      if (!isCurrent(epoch)) return;
      writing = false;
      appliedCursor = { generation: last.generation, appendId: last.appendId };
      if (needsSnapshot) {
        const clear = clearBeforeSnapshot;
        needsSnapshot = false;
        clearBeforeSnapshot = false;
        replaySnapshot(clear);
        return;
      }
      drainPending();
    });
  };

  /**
   * Business Logic（为什么需要这个函数）:
   *   append 热路径把每个 chunk 同步交给已挂载 writer，但 pending 必须有界，
   *   否则 stalled xterm callback 会让 pendingChunk 无限增长。
   *
   * Code Logic（这个函数做什么）:
   *   忽略已 dispose / 非本 session / 不新于 applied 的 delta；其余合并进有界 next-buffer；
   *   超限标记 needsSnapshot，在当前 write 完成后 clear+replay 最新 snapshot。
   */
  const onLiveDelta = (delta: TerminalBufferDelta): void => {
    if (disposed) return;
    if (delta.sessionId !== sessionId) return;
    if (!isNewerDelta(delta, appliedCursor)) return;
    if (needsSnapshot) {
      // 已决定 snapshot 回填，只推进 cursor 语义到最新，不继续拼接无界字符串。
      pendingCursor = { generation: delta.generation, appendId: delta.appendId };
      return;
    }
    if (pendingCursor && delta.generation < pendingCursor.generation) return;
    if (pendingCursor && delta.generation > pendingCursor.generation) {
      pendingChunk = delta.chunk;
    } else {
      pendingChunk += delta.chunk;
    }
    pendingCursor = { generation: delta.generation, appendId: delta.appendId };
    if (pendingChunk.length > maxPendingChars) {
      pendingChunk = '';
      needsSnapshot = true;
      clearBeforeSnapshot = true;
      if (!writing) {
        const clear = clearBeforeSnapshot;
        needsSnapshot = false;
        clearBeforeSnapshot = false;
        replaySnapshot(clear);
      }
      return;
    }
    drainPending();
  };

  /**
   * Business Logic（为什么需要这个函数）:
   *   resync/remove 会提升 generation，旧队列不得继续 append 到新屏幕；
   *   且必须等当前 xterm write 回调结束后再 clear，避免新旧字节混合。
   *
   * Code Logic（这个函数做什么）:
   *   清空 pending 并请求 clear+replay 最新 snapshot（串行到 writing=false）。
   */
  const onReset = (): void => {
    if (disposed) return;
    pendingChunk = '';
    pendingCursor = null;
    replaySnapshot(true);
  };

  // 必须先订阅再 snapshot，避免 subscribe 与 snapshot 之间丢 delta。
  const unsubscribeLive = source.subscribeLive(sessionId, onLiveDelta);
  const unsubscribeReset = source.subscribeReset(sessionId, onReset);
  replaySnapshot(false);

  return {
    /**
     * Business Logic（为什么需要这个函数）:
     *   pane unmount 时必须停止写 xterm，防止对已 dispose terminal 回调。
     *
     * Code Logic（这个函数做什么）:
     *   置 disposed、提升 epoch、清空队列并取消订阅。
     */
    dispose(): void {
      if (disposed) return;
      disposed = true;
      writeEpoch += 1;
      writing = false;
      pendingChunk = '';
      pendingCursor = null;
      needsSnapshot = false;
      clearBeforeSnapshot = false;
      unsubscribeLive();
      unsubscribeReset();
    },
  };
}
