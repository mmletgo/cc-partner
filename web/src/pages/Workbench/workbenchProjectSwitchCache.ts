import type { WorkbenchSession } from '../../lib/types';
import { mountedTerminalSessions } from './terminalSessionOrder';

export interface WorktreeIdRecord {
  id: string;
}

export interface PickCachedWorktreeIdInput {
  cachedWorktrees: readonly WorktreeIdRecord[] | undefined;
  lastWorktreeId: string | null | undefined;
}

export interface ResolveWorktreeIdAfterProjectSwitchInput {
  nextProjectId: string | null;
  cachedWorktrees: readonly WorktreeIdRecord[] | undefined;
  lastWorktreeId: string | null | undefined;
  cachedSessions: readonly WorkbenchSession[] | undefined;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   切回已访问过的项目时应立刻恢复上次 worktree，不能先清空再等 git status。
 *
 * Code Logic（这个函数做什么）:
 *   缓存非空且 last 仍在列表中则用之，否则回退到第一项；无缓存返回 null。
 */
export function pickCachedWorktreeId(input: PickCachedWorktreeIdInput): string | null {
  const list = input.cachedWorktrees ?? [];
  if (list.length === 0) return null;
  if (input.lastWorktreeId && list.some((item) => item.id === input.lastWorktreeId)) {
    return input.lastWorktreeId;
  }
  return list[0]?.id ?? null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   首次进入项目时 worktrees.list 还在跑 git status，但 sessions.list 往往已经带回 worktreeId。
 *
 * Code Logic（这个函数做什么）:
 *   优先 running 且带 worktreeId 的 session，否则任意带 worktreeId 的 session。
 */
export function suggestWorktreeIdFromSessions(
  sessions: readonly WorkbenchSession[],
): string | null {
  const running = sessions.find(
    (item) => item.status === 'running' && Boolean(item.worktreeId),
  );
  if (running?.worktreeId) return running.worktreeId;
  const any = sessions.find((item) => Boolean(item.worktreeId));
  return any?.worktreeId ?? null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   切项目时 activeWorktreeId 必须在同一轮渲染落到目标项目，避免 B 项目配 A 的 worktree。
 *
 * Code Logic（这个函数做什么）:
 *   离开全部项目返回 null；否则先 pick 缓存 worktree，再回退 session 提示。
 */
export function resolveWorktreeIdAfterProjectSwitch(
  input: ResolveWorktreeIdAfterProjectSwitchInput,
): string | null {
  if (!input.nextProjectId) return null;
  return (
    pickCachedWorktreeId({
      cachedWorktrees: input.cachedWorktrees,
      lastWorktreeId: input.lastWorktreeId,
    }) ?? suggestWorktreeIdFromSessions(input.cachedSessions ?? [])
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Git 历史按 project+worktree 缓存，切回时先展示旧提交再后台刷新。
 *
 * Code Logic（这个函数做什么）:
 *   worktree 为空时用 `__none__`，与既有 git history request seq 键一致。
 */
export function gitHistoryCacheKey(projectId: string, worktreeId: string | null): string {
  return `${projectId}::${worktreeId ?? '__none__'}`;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   只回收当前项目切片里已经关闭的 session，不能把其它项目仍打开的 xterm 拆掉。
 *
 * Code Logic（这个函数做什么）:
 *   返回 previous 中不在 next 的 id；previous 缺失视为空切片。
 */
export function disappearedSessionIds(
  previous: readonly WorkbenchSession[] | undefined,
  next: readonly WorkbenchSession[],
): string[] {
  const nextIds = new Set(next.map((item) => item.id));
  return (previous ?? []).map((item) => item.id).filter((id) => !nextIds.has(id));
}

/**
 * Business Logic（为什么需要这个函数）:
 *   跨项目保留 xterm：mounted 列表必须是本会话访问过的全部 terminal window。
 *
 * Code Logic（这个函数做什么）:
 *   按项目 id 字典序展开后，再按创建时间排序。
 */
export function mountedSessionsFromProjects(
  sessionsByProject: Record<string, WorkbenchSession[]>,
): WorkbenchSession[] {
  const sessions = Object.keys(sessionsByProject)
    .sort()
    .flatMap((projectId) => sessionsByProject[projectId] ?? []);
  return mountedTerminalSessions({ sessions });
}

/**
 * Business Logic（为什么需要这个函数）:
 *   每个项目记住上次 active worktree，切回时才能立刻对齐。
 *
 * Code Logic（这个函数做什么）:
 *   空 id 不写；值未变返回原对象，避免无意义复制。
 */
export function rememberLastWorktree(
  lastByProject: Record<string, string>,
  projectId: string | null,
  worktreeId: string | null,
): Record<string, string> {
  if (!projectId || !worktreeId) return lastByProject;
  if (lastByProject[projectId] === worktreeId) return lastByProject;
  return { ...lastByProject, [projectId]: worktreeId };
}
