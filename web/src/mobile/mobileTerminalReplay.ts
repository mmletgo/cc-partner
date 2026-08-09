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
 *   writtenBuffer 必须等于实际写入 xterm 的字节，否则后续 store baseline / buffer effect
 *   会把「只有 live 尾部」当真相，触发 clear 丢掉 scrollback，页面上只能看到打开后的输出。
 *
 * Code Logic（这个函数做什么）:
 *   用桌面端 replay diff helper 判断 live buffer 是否是 replay 的延续；可安全 append 时写 replay+tail，
 *   无法对齐时只写 replay。无论哪条分支，writtenBuffer 一律等于 data（真正写入 xterm 的内容）。
 */
export function prepareInitialReplayBuffer(
  replayBuffer: string,
  liveBuffer: string,
): InitialReplayBuffer {
  if (!liveBuffer) return { data: replayBuffer, writtenBuffer: replayBuffer };
  if (!replayBuffer) return { data: liveBuffer, writtenBuffer: liveBuffer };

  const plan = planTerminalBufferWrite(replayBuffer, liveBuffer);
  if (plan.mode === 'append') {
    // live 可能是 replay 的严格后缀（overlap 后 plan.data 为空）或延续尾部。
    // 无论哪种，xterm 写入的是完整历史（+ 可选新尾），baseline 必须跟它一致，不能退回短 live。
    const data = `${replayBuffer}${plan.data}`;
    return { data, writtenBuffer: data };
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
