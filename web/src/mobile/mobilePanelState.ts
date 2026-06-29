import type { WorkbenchWorktree } from '@/lib/types';
import { selectPreferredMobileWorktree } from './mobileWorkbenchState';

export interface MobileFilePanelContext {
  projectId: string;
  worktreeId: string | null;
}

export interface MobileFileDirtySnapshot {
  dirty: boolean;
  context: MobileFilePanelContext | null;
}

export type MobileGitPanelAction = 'commit' | 'push' | 'merge';

export interface MobileWorktreeRemovalPlan {
  nextWorktrees: WorkbenchWorktree[];
  nextActive: WorkbenchWorktree | null;
  requiresActivePreflight: boolean;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   删除 active worktree 会离开当前 Files 草稿上下文，必须先让父级 dirty guard 决定是否允许，再调用后端删除。
 *
 * Code Logic（这个函数做什么）:
 *   根据当前列表、active worktree id 和待删除 worktree，预先计算删除后的列表、目标 active worktree，以及是否需要 active preflight。
 */
export function getMobileWorktreeRemovalPlan(
  worktrees: WorkbenchWorktree[],
  activeWorktreeId: string | null,
  removingWorktree: WorkbenchWorktree,
): MobileWorktreeRemovalPlan {
  const nextWorktrees = worktrees.filter((worktree) => worktree.id !== removingWorktree.id);
  const requiresActivePreflight = activeWorktreeId === removingWorktree.id;
  const nextActive = requiresActivePreflight
    ? selectPreferredMobileWorktree(nextWorktrees)
    : worktrees.find((worktree) => worktree.id === activeWorktreeId) ?? null;

  return { nextWorktrees, nextActive, requiresActivePreflight };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   文件面板需要用稳定 key 判断草稿、目录和异步请求是否仍属于同一个 project/worktree。
 *
 * Code Logic（这个函数做什么）:
 *   将 projectId 与可空 worktreeId 序列化为字符串；null context 返回空字符串。
 */
export function getMobileFileContextKey(context: MobileFilePanelContext | null): string {
  return context ? `${context.projectId}:${context.worktreeId ?? ''}` : '';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用户取消 dirty 草稿的 context 切换后，再切回原 context 时必须保留当前打开文件和草稿。
 *
 * Code Logic（这个函数做什么）:
 *   当 loaded context 与下一次 props context 相同，且当前打开文件没有绑定到其它 context 时，返回 true 表示跳过 reset/reload。
 */
export function shouldSkipMobileFileContextReload(
  loadedContext: MobileFilePanelContext | null,
  nextContext: MobileFilePanelContext | null,
  openedContext: MobileFilePanelContext | null,
): boolean {
  const loadedKey = getMobileFileContextKey(loadedContext);
  const nextKey = getMobileFileContextKey(nextContext);
  if (!loadedKey || loadedKey !== nextKey) return false;
  const openedKey = getMobileFileContextKey(openedContext);
  return !openedKey || openedKey === nextKey;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   dirty 草稿只能阻止真正离开当前 project/worktree，不能阻止同上下文重渲染或用户切回原上下文。
 *
 * Code Logic（这个函数做什么）:
 *   比较 loaded context 与 next context 的 key，并在 dirty 且 key 变化时返回 true。
 */
export function shouldBlockMobileFileContextSwitch(
  loadedContext: MobileFilePanelContext | null,
  nextContext: MobileFilePanelContext | null,
  dirty: boolean,
): boolean {
  const loadedKey = getMobileFileContextKey(loadedContext);
  const nextKey = getMobileFileContextKey(nextContext);
  return Boolean(dirty && loadedKey && nextKey && loadedKey !== nextKey);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Files 面板在移动端导航切到其它面板后仍可能持有未保存草稿，父级切换 project/worktree 前必须先判断是否需要确认。
 *
 * Code Logic（这个函数做什么）:
 *   读取 dirty snapshot 里的 context key，与目标 context key 比较；dirty 且目标不同才返回 true。
 */
export function shouldConfirmMobileFileDirtyContextSwitch(
  snapshot: MobileFileDirtySnapshot,
  targetContext: MobileFilePanelContext | null,
): boolean {
  if (!snapshot.dirty) return false;
  const dirtyKey = getMobileFileContextKey(snapshot.context);
  const targetKey = getMobileFileContextKey(targetContext);
  return Boolean(dirtyKey && targetKey && dirtyKey !== targetKey);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   父级已经确认丢弃 dirty 草稿后，Files 面板响应 context props 变化时不能再次打断用户。
 *
 * Code Logic（这个函数做什么）:
 *   比较上一次和当前 discard token；token 变化表示父级已处理确认，返回 true 让内部 context effect 跳过 confirm。
 */
export function shouldSkipMobileFileContextConfirmForDiscardToken(
  previousToken: number,
  currentToken: number,
): boolean {
  return previousToken !== currentToken;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   files.open 响应可能晚于 project/worktree 切换返回，旧预览不能覆盖当前文件面板。
 *
 * Code Logic（这个函数做什么）:
 *   同时校验请求 id 仍是最新，以及请求发起 context 仍等于当前 loaded context。
 */
export function isMobileFileOpenResponseCurrent(
  requestId: number,
  latestRequestId: number,
  requestContext: MobileFilePanelContext,
  loadedContext: MobileFilePanelContext | null,
): boolean {
  return (
    requestId === latestRequestId &&
    getMobileFileContextKey(requestContext) === getMobileFileContextKey(loadedContext)
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   根目录重载代表文件面板重新建立 context 基线，正在返回的旧 open 请求必须失效。
 *
 * Code Logic（这个函数做什么）:
 *   仅当目录路径为空字符串时返回 true；子目录浏览不触发 context 级失效。
 */
export function shouldInvalidateMobileFileOpenOnDirectoryLoad(path: string): boolean {
  return path === '';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   merge 成功后源 worktree 可能已被删除，不能再用源 worktree id 拉 Git 提交并把成功误报为失败。
 *
 * Code Logic（这个函数做什么）:
 *   commit/push 仍需刷新提交历史；merge 只刷新 worktree 列表，由新的 active worktree 再触发提交加载。
 */
export function shouldReloadMobileGitCommitsAfterAction(action: MobileGitPanelAction): boolean {
  return action !== 'merge';
}
