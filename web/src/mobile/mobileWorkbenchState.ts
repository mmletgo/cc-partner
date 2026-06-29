import type { WorkbenchSession, WorkbenchWorktree } from '@/lib/types';

export type MobileWorkbenchPanel =
  | 'projects'
  | 'terminal'
  | 'files'
  | 'git'
  | 'worktrees'
  | 'prompt'
  | 'settings';

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端 Workbench shell 的导航点击需要以纯函数方式选择下一个面板，便于组件和测试共享契约。
 *
 * Code Logic（这个函数做什么）:
 *   接收当前面板和目标面板，返回目标面板；current 保留在签名中用于后续按当前态扩展切换规则。
 */
export function selectMobilePanel(
  current: MobileWorkbenchPanel,
  next: MobileWorkbenchPanel,
): MobileWorkbenchPanel {
  void current;
  return next;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   手机竖屏通过顶部小按钮打开覆盖式导航抽屉，组件需要复用统一的打开态值。
 *
 * Code Logic（这个函数做什么）:
 *   返回 true，表示移动端导航抽屉处于打开状态。
 */
export function openMobileNav(): boolean {
  return true;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用户点击导航项、遮罩或关闭按钮后需要收起移动端导航抽屉，组件需要复用统一的关闭态值。
 *
 * Code Logic（这个函数做什么）:
 *   返回 false，表示移动端导航抽屉处于关闭状态。
 */
export function closeMobileNav(): boolean {
  return false;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端选择项目后需要自动落到最合理的 worktree，状态栏、终端和文件面板都依赖这个上下文。
 *
 * Code Logic（这个函数做什么）:
 *   优先返回 isMain 的 worktree；没有主工作区标记时返回首项；空列表返回 null。
 */
export function selectPreferredMobileWorktree(
  worktrees: WorkbenchWorktree[],
): WorkbenchWorktree | null {
  return worktrees.find((worktree) => worktree.isMain) ?? worktrees[0] ?? null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端切换项目或 worktree 后需要自动选择一个 terminal window，减少用户进入终端面板后的空态。
 *
 * Code Logic（这个函数做什么）:
 *   优先返回绑定 active worktree 的 session；没有匹配时返回首个 running session；仍没有则返回首项或 null。
 */
export function selectPreferredMobileSession(
  sessions: WorkbenchSession[],
  activeWorktreeId: string | null,
): WorkbenchSession | null {
  const matchingSession = activeWorktreeId
    ? sessions.find((session) => session.worktreeId === activeWorktreeId)
    : undefined;
  return (
    matchingSession ??
    sessions.find((session) => session.status === 'running') ??
    sessions[0] ??
    null
  );
}
