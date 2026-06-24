import type { WorkbenchSession } from '../../lib/types';

interface VisibleTerminalSessionsInput {
  sessions: WorkbenchSession[];
  activeSessionId: string | null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   工作台多终端布局中的 pane 位置应当稳定，用户点击终端只改变焦点，不应把终端挪到第一位。
 *
 * Code Logic（这个函数做什么）:
 *   按 startedAt 从早到晚排序；时间相同或无法解析时保持输入顺序，保证排序只由创建时间和原始顺序决定。
 */
function sortSessionsByCreatedOrder(sessions: WorkbenchSession[]): WorkbenchSession[] {
  return sessions
    .map((session, index) => ({
      session,
      index,
      startedAtMs: Date.parse(session.startedAt),
    }))
    .sort((left, right) => {
      const leftTime = Number.isFinite(left.startedAtMs)
        ? left.startedAtMs
        : Number.POSITIVE_INFINITY;
      const rightTime = Number.isFinite(right.startedAtMs)
        ? right.startedAtMs
        : Number.POSITIVE_INFINITY;
      if (leftTime !== rightTime) return leftTime - rightTime;
      return left.index - right.index;
    })
    .map((item) => item.session);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   真实 tmux 映射下，前端每次只 attach 当前 window；pane 布局由 tmux 在该 window 内渲染。
 *
 * Code Logic（这个函数做什么）:
 *   优先返回 activeSessionId 对应 window；缺失时返回最早创建 window。
 */
export function visibleTerminalSessions(input: VisibleTerminalSessionsInput): WorkbenchSession[] {
  const orderedSessions = sortSessionsByCreatedOrder(input.sessions);
  const activeSession =
    input.activeSessionId === null
      ? null
      : input.sessions.find((session) => session.id === input.activeSessionId);
  return (activeSession ? [activeSession] : orderedSessions).slice(0, 1);
}
