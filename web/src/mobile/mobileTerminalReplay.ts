import {
  shouldForwardTerminalInput,
  type TerminalReplayGate,
} from '@/pages/Workbench/terminalReplay';

export interface InitialReplayBuffer {
  data: string;
  writtenBuffer: string;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端首次打开终端时会同时拥有 HTTP replay 快照和 NDJSON 增量缓存。
 *   两段流必须严格按「同一 session 的前后缀关系」对齐；不能用模糊 KMP 重叠拼接，
 *   否则另一 terminal window 的 live 缓存一旦误入 bufferRef，会被拼进当前 xterm/store，
 *   表现为「两个窗口历史混在一起」。
 *   writtenBuffer 必须等于实际写入 xterm 的字节，作为后续 store baseline。
 *
 * Code Logic（这个函数做什么）:
 *   - live 空：写 replay
 *   - replay 空：写 live
 *   - live 以 replay 为严格前缀：写 live（真延续）
 *   - replay 以 live 为严格后缀：写完整 replay（NDJSON 仅有近期尾）
 *   - 其它：只写 replay，绝不拼接（含跨 session / 无关字符串的部分重叠）
 */
export function prepareInitialReplayBuffer(
  replayBuffer: string,
  liveBuffer: string,
): InitialReplayBuffer {
  if (!liveBuffer) return { data: replayBuffer, writtenBuffer: replayBuffer };
  if (!replayBuffer) return { data: liveBuffer, writtenBuffer: liveBuffer };

  // 真延续：live 已包含完整 replay 并可能更长。
  if (liveBuffer.startsWith(replayBuffer)) {
    return { data: liveBuffer, writtenBuffer: liveBuffer };
  }

  // NDJSON 重建后往往只有近期后缀；完整历史在 HTTP replay。
  if (replayBuffer.endsWith(liveBuffer)) {
    return { data: replayBuffer, writtenBuffer: replayBuffer };
  }

  // 无法严格对齐：禁止模糊拼接（避免跨 window 历史混合）。
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
