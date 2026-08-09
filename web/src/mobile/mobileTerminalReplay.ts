import {
  shouldForwardTerminalInput,
  type TerminalReplayGate,
} from '@/pages/Workbench/terminalReplay';
import type { TerminalBufferDelta } from '@/hooks/workbenchTerminalBuffer';

export interface ReplayWithHeldLive {
  buffer: string;
  lastSeq: number;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   HTTP replay 在网络往返期间，NDJSON 可能已经收到更高 seq 的终端输出。若 cutover 只 reset
 *   replay 快照，这些已经推进 event cursor 的字节会永久丢失；若按字符串模糊拼接又会重复 TUI 帧。
 *
 * Code Logic（这个函数做什么）:
 *   从 replay 的 owner/lastSeq 起步，只按到达顺序追加同 owner 且 seq 更高的 exact live delta，
 *   重复/旧 seq no-op，返回真实纳入后的 buffer 与 lastSeq baseline。
 */
export function appendHeldLiveAfterReplay(
  replayBuffer: string,
  replayLastSeq: number,
  replayOwnerInstanceId: string | null | undefined,
  heldLive: readonly TerminalBufferDelta[],
): ReplayWithHeldLive {
  const owner = replayOwnerInstanceId ?? null;
  let buffer = replayBuffer;
  let lastSeq = replayLastSeq;

  for (const delta of heldLive) {
    if ((delta.authorityId ?? null) !== owner) continue;
    const seq = delta.lastSeq;
    if (typeof seq !== 'number' || !Number.isFinite(seq) || seq <= lastSeq) continue;
    buffer += delta.chunk;
    lastSeq = seq;
  }

  return { buffer, lastSeq };
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
