import {
  planTerminalBufferWrite,
  shouldForwardTerminalInput,
  type TerminalReplayGate,
} from '@/pages/Workbench/terminalReplay';

export interface InitialReplayBuffer {
  data: string;
  writtenBuffer: string;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端首次打开终端时会同时拥有 HTTP replay 快照和 NDJSON 增量缓存；当两段 PTY 流无法对齐时，
 *   不能在同一次初始写入里拼接它们，否则可能重复历史输出或污染 xterm 状态。
 *
 * Code Logic（这个函数做什么）:
 *   用桌面端 replay diff helper 判断 live buffer 是否是 replay 的延续；可安全 append 时写 replay+tail，
 *   无法对齐时只写 replay 并把 replay 作为后续 diff 基线，让后续 live buffer 通过 replay 分支清屏重放。
 */
export function prepareInitialReplayBuffer(
  replayBuffer: string,
  liveBuffer: string,
): InitialReplayBuffer {
  if (!liveBuffer) return { data: replayBuffer, writtenBuffer: replayBuffer };
  if (!replayBuffer) return { data: liveBuffer, writtenBuffer: liveBuffer };

  const plan = planTerminalBufferWrite(replayBuffer, liveBuffer);
  if (plan.mode === 'append') {
    return { data: `${replayBuffer}${plan.data}`, writtenBuffer: liveBuffer };
  }
  if (plan.mode === 'none') {
    return { data: replayBuffer, writtenBuffer: replayBuffer };
  }
  return { data: replayBuffer, writtenBuffer: replayBuffer };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端 terminal 在 HTTP replay 完成前不能把用户输入写给后端，否则输入可能先执行再被历史 replay 覆盖。
 *
 * Code Logic（这个函数做什么）:
 *   在桌面 replay gate 判断外额外要求 replayReady=true。
 */
export function shouldForwardMobileTerminalInput(
  gate: TerminalReplayGate,
  replayReady: boolean,
  inputEnabled: boolean,
): boolean {
  return replayReady && shouldForwardTerminalInput(gate, inputEnabled);
}
