import type { WorkbenchSession } from '../../lib/types';
import type { TerminalLayoutMode } from './terminalSizing';

export const TERMINAL_LAYOUT_LIMIT: Record<TerminalLayoutMode, number> = {
  single: 1,
  double: 2,
  quad: 4,
};

interface VisibleTerminalSessionsInput {
  sessions: WorkbenchSession[];
  activeSessionId: string | null;
  layout: TerminalLayoutMode;
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
 *   单终端布局应继续显示当前焦点终端；双列/四宫格多终端布局则需要固定显示创建时间最早的终端集合。
 *
 * Code Logic（这个函数做什么）:
 *   single 优先返回 activeSessionId 对应会话，缺失时返回最早创建会话；double/quad 返回按创建时间排序后的前 N 个会话。
 */
export function visibleTerminalSessions(input: VisibleTerminalSessionsInput): WorkbenchSession[] {
  const orderedSessions = sortSessionsByCreatedOrder(input.sessions);
  const limit = TERMINAL_LAYOUT_LIMIT[input.layout];
  if (input.layout === 'single') {
    const activeSession =
      input.activeSessionId === null
        ? null
        : input.sessions.find((session) => session.id === input.activeSessionId);
    return (activeSession ? [activeSession] : orderedSessions).slice(0, limit);
  }
  return orderedSessions.slice(0, limit);
}
