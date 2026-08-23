/**
 * 移动端工作台 URL 位置。
 *
 * Business Logic（为什么需要这个模块）:
 *   `/mobile` 刷新或从后台被系统杀掉后会丢掉 React 内存里的项目选择，用户必须从列表再点进去。
 *   把当前项目/面板/worktree/session 写进 query，刷新后才能直接回到原来的工作台。
 *
 * Code Logic（这个模块做什么）:
 *   解析与编码 `projectId/panel/worktreeId/sessionId`；项目绑定面板缺 projectId 时回落列表。
 */

import type { WorkbenchSession, WorkbenchWorktree } from '@/lib/types';
import type { MobileWorkbenchPanel } from './mobileWorkbenchState';

const MOBILE_WORKBENCH_PANELS: ReadonlySet<string> = new Set([
  'projects',
  'attention',
  'terminal',
  'browser',
  'files',
  'git',
  'worktrees',
  'transfer',
  'automation',
  'settings',
  'provider',
]);

const PROJECT_BOUND_PANELS: ReadonlySet<string> = new Set([
  'terminal',
  'browser',
  'files',
  'git',
  'worktrees',
  'automation',
]);

export interface MobileWorkbenchLocation {
  projectId: string | null;
  panel: MobileWorkbenchPanel;
  worktreeId: string | null;
  sessionId: string | null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   空串、空白或非法 id 不能当恢复目标，否则会选中不存在的项目。
 *
 * Code Logic（这个函数做什么）:
 *   trim 后空值返回 null。
 */
function readOptionalId(value: string | null): string | null {
  const trimmed = value?.trim() ?? '';
  return trimmed.length > 0 ? trimmed : null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   URL 里的 panel 必须是已知枚举，不能把拼写错误当成一个空白工作台。
 *
 * Code Logic（这个函数做什么）:
 *   命中全集则返回该 panel；否则 null。
 */
function readPanel(value: string | null): MobileWorkbenchPanel | null {
  const trimmed = value?.trim() ?? '';
  if (MOBILE_WORKBENCH_PANELS.has(trimmed)) {
    return trimmed as MobileWorkbenchPanel;
  }
  return null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   刷新后必须从当前 URL 恢复用户离开时的工作台，而不是无条件回到项目列表。
 *
 * Code Logic（这个函数做什么）:
 *   读 projectId/panel/worktreeId/sessionId；未知 panel 回落；
 *   有 projectId 且未写 panel 时默认 terminal；项目绑定面板缺 projectId 时回落 projects。
 */
export function parseMobileWorkbenchLocation(search: string): MobileWorkbenchLocation {
  const params = new URLSearchParams(search.startsWith('?') ? search.slice(1) : search);
  const projectId = readOptionalId(params.get('projectId'));
  const worktreeId = readOptionalId(params.get('worktreeId'));
  const sessionId = readOptionalId(params.get('sessionId'));
  const parsedPanel = readPanel(params.get('panel'));
  const panel = parsedPanel ?? (projectId ? 'terminal' : 'projects');
  if (PROJECT_BOUND_PANELS.has(panel) && !projectId) {
    return {
      projectId: null,
      panel: 'projects',
      worktreeId: null,
      sessionId: null,
    };
  }
  return {
    projectId,
    panel,
    worktreeId: projectId ? worktreeId : null,
    sessionId: projectId ? sessionId : null,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   写回 URL 时不能把已离开的项目工作台上下文留在地址栏，刷新才会停在列表。
 *
 * Code Logic（这个函数做什么）:
 *   projects/provider 清空项目上下文；项目绑定面板缺 project 时回落列表；
 *   无 project 时丢掉 worktree/session。
 */
export function resolveMobileWorkbenchLocation(input: {
  panel: MobileWorkbenchPanel;
  projectId: string | null;
  worktreeId: string | null;
  sessionId: string | null;
}): MobileWorkbenchLocation {
  const leavingWorkbench = input.panel === 'projects' || input.panel === 'provider';
  const projectId = leavingWorkbench ? null : readOptionalId(input.projectId);
  const panel =
    PROJECT_BOUND_PANELS.has(input.panel) && !projectId ? 'projects' : input.panel;
  if (!projectId) {
    return {
      projectId: null,
      panel,
      worktreeId: null,
      sessionId: null,
    };
  }
  return {
    projectId,
    panel,
    worktreeId: readOptionalId(input.worktreeId),
    sessionId: readOptionalId(input.sessionId),
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   地址栏需要一份稳定、可刷新的 query，且远端 id 里的冒号必须编码。
 *
 * Code Logic（这个函数做什么）:
 *   空工作台返回空串；否则按 projectId/panel/worktreeId/sessionId 顺序编码。
 */
export function buildMobileWorkbenchSearch(location: MobileWorkbenchLocation): string {
  const resolved = resolveMobileWorkbenchLocation(location);
  const params = new URLSearchParams();
  if (resolved.projectId) params.set('projectId', resolved.projectId);
  if (resolved.panel !== 'projects') params.set('panel', resolved.panel);
  if (resolved.worktreeId) params.set('worktreeId', resolved.worktreeId);
  if (resolved.sessionId) params.set('sessionId', resolved.sessionId);
  const query = params.toString();
  return query ? `?${query}` : '';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   刷新后请求的 worktree/session 可能已被删除，必须回落到当前项目仍存在的首选窗口。
 *
 * Code Logic（这个函数做什么）:
 *   命中 session 时以该 session 的 worktree 为准；否则按 id 找 worktree，再选首选 session。
 */
export function resolveRestoredMobileWorkspace(
  worktrees: WorkbenchWorktree[],
  sessions: WorkbenchSession[],
  worktreeId: string | null,
  sessionId: string | null,
): { worktree: WorkbenchWorktree | null; session: WorkbenchSession | null } {
  const requestedSession = sessionId
    ? sessions.find((session) => session.id === sessionId) ?? null
    : null;
  const sessionWorktreeId = requestedSession?.worktreeId ?? worktreeId;
  const worktree = sessionWorktreeId
    ? worktrees.find((item) => item.id === sessionWorktreeId) ??
      pickPreferredWorktree(worktrees)
    : pickPreferredWorktree(worktrees);
  const session =
    requestedSession ?? pickPreferredSession(sessions, worktree?.id ?? null);
  return { worktree, session };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   同步地址栏时不得无意义 replaceState，以免打断滚动或触发多余 popstate。
 *
 * Code Logic（这个函数做什么）:
 *   比较当前 search 与目标 search，不同才 replaceState，保留 pathname/hash。
 */
/**
 * Business Logic（为什么需要这个函数）:
 *   恢复目标丢失时仍要打开一个能用的主工作区，规则与列表页首次进入项目一致。
 *
 * Code Logic（这个函数做什么）:
 *   主 worktree 优先，否则首项。
 */
function pickPreferredWorktree(worktrees: WorkbenchWorktree[]): WorkbenchWorktree | null {
  return worktrees.find((worktree) => worktree.isMain) ?? worktrees[0] ?? null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   session id 失效时要落到当前 worktree 上仍在跑的窗口。
 *
 * Code Logic（这个函数做什么）:
 *   matching+running → matching → 任意 running → 首项。
 */
function pickPreferredSession(
  sessions: WorkbenchSession[],
  activeWorktreeId: string | null,
): WorkbenchSession | null {
  const matchingRunning = activeWorktreeId
    ? sessions.find(
        (session) => session.worktreeId === activeWorktreeId && session.status === 'running',
      )
    : undefined;
  const matching = activeWorktreeId
    ? sessions.find((session) => session.worktreeId === activeWorktreeId)
    : undefined;
  return (
    matchingRunning ??
    matching ??
    sessions.find((session) => session.status === 'running') ??
    sessions[0] ??
    null
  );
}

export function syncMobileWorkbenchLocationToHistory(
  location: MobileWorkbenchLocation,
  currentUrl: string,
  replaceState: (url: string) => void,
): void {
  const current = new URL(currentUrl, 'http://localhost');
  const nextSearch = buildMobileWorkbenchSearch(location);
  if (current.search === nextSearch) return;
  current.search = nextSearch.startsWith('?') ? nextSearch.slice(1) : nextSearch;
  replaceState(`${current.pathname}${current.search}${current.hash}`);
}
