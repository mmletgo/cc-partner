interface TerminalReplayWriter {
  write(data: string, callback?: () => void): void;
}

type TerminalReplayReleaseScheduler = (release: () => void) => void;

export interface TerminalReplayGate {
  current: boolean;
  releaseId?: number;
}

export type TerminalBufferWriteMode = 'none' | 'append' | 'replay';

export interface TerminalBufferWritePlan {
  mode: TerminalBufferWriteMode;
  data: string;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   xterm replay 写入完成回调后，同一轮事件循环中仍可能冒出设备能力响应，不能立刻恢复输入转发。
 *
 * Code Logic（这个函数做什么）:
 *   把 gate 释放推迟到下一轮 macrotask，让 replay 引发的 terminal-generated data 先被屏蔽。
 */
function scheduleTerminalReplayGateRelease(release: () => void): void {
  globalThis.setTimeout(release, 0);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 终端输出缓存达到上限后会裁掉前缀，但活跃 xterm 仍需要继续追加新输出。
 *
 * Code Logic（这个函数做什么）:
 *   用 KMP 前缀表计算 previous 后缀与 next 前缀的最长重叠长度，避免长度相同的滑动缓存被误判为无变化。
 */
function longestSuffixPrefixOverlap(previous: string, next: string): number {
  const maxLength = Math.min(previous.length, next.length);
  if (maxLength === 0) return 0;

  const pattern = next.slice(0, maxLength);
  const prefixTable = new Array<number>(pattern.length).fill(0);

  for (let index = 1, matched = 0; index < pattern.length; index += 1) {
    while (matched > 0 && pattern[index] !== pattern[matched]) {
      matched = prefixTable[matched - 1] ?? 0;
    }
    if (pattern[index] === pattern[matched]) {
      matched += 1;
      prefixTable[index] = matched;
    }
  }

  let matched = 0;
  const start = previous.length - maxLength;
  for (let index = start; index < previous.length; index += 1) {
    while (matched > 0 && previous[index] !== pattern[matched]) {
      matched = prefixTable[matched - 1] ?? 0;
    }
    if (previous[index] === pattern[matched]) {
      matched += 1;
      if (matched === pattern.length && index < previous.length - 1) {
        matched = prefixTable[matched - 1] ?? 0;
      }
    }
  }

  return matched;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   终端输出缓存截断后，当前 buffer 长度可能保持不变，但用户仍需要看到新产生的终端输出。
 *
 * Code Logic（这个函数做什么）:
 *   对比上次已写入 xterm 的 buffer 与最新缓存：前缀扩展走 append，滑动截断走重叠 append，无法对齐时要求 clear + replay。
 */
export function planTerminalBufferWrite(
  previousBuffer: string,
  nextBuffer: string,
): TerminalBufferWritePlan {
  if (previousBuffer === nextBuffer) return { mode: 'none', data: '' };
  if (nextBuffer.startsWith(previousBuffer)) {
    return { mode: 'append', data: nextBuffer.slice(previousBuffer.length) };
  }

  const overlap = longestSuffixPrefixOverlap(previousBuffer, nextBuffer);
  if (overlap > 0) {
    return { mode: 'append', data: nextBuffer.slice(overlap) };
  }

  return { mode: 'replay', data: nextBuffer };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   工作台 terminal 首次挂载或 buffer 截断后会重放历史 PTY 输出，历史输出中的 tmux 查询序列不应再次写回后端。
 *
 * Code Logic（这个函数做什么）:
 *   同时检查 terminal 是否为 active 可交互实例，以及 replay gate 是否正在屏蔽历史输出副作用。
 */
export function shouldForwardTerminalInput(
  gate: TerminalReplayGate,
  inputEnabled = true,
): boolean {
  return inputEnabled && !gate.current;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   xterm 在处理 tmux 的设备能力查询序列时会通过 onData 产生响应；历史 buffer replay 期间这些响应是旧输出副作用。
 *
 * Code Logic（这个函数做什么）:
 *   写入 replay buffer 前打开 gate，等 xterm write callback 触发后关闭 gate；空 buffer 立即保持可输入。
 */
export function writeTerminalReplay(
  terminal: TerminalReplayWriter,
  data: string,
  gate: TerminalReplayGate,
  scheduleRelease: TerminalReplayReleaseScheduler = scheduleTerminalReplayGateRelease,
  onWriteComplete?: () => void,
): void {
  if (data.length === 0) {
    gate.current = false;
    onWriteComplete?.();
    return;
  }

  gate.current = true;
  const releaseId = (gate.releaseId ?? 0) + 1;
  gate.releaseId = releaseId;
  terminal.write(data, () => {
    scheduleRelease(() => {
      if (gate.releaseId !== releaseId) return;
      gate.current = false;
    });
    onWriteComplete?.();
  });
}
