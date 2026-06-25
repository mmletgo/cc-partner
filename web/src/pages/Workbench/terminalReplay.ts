interface TerminalReplayWriter {
  write(data: string, callback?: () => void): void;
}

type TerminalReplayReleaseScheduler = (release: () => void) => void;

export interface TerminalReplayGate {
  current: boolean;
  releaseId?: number;
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
): void {
  if (data.length === 0) {
    gate.current = false;
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
  });
}
