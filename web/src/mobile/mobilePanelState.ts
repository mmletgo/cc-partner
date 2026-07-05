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

export type MobileWorktreeDestructiveFlowResult = 'applied' | 'cancelled';

export interface MobileWorktreeRemovalFlowOptions {
  worktrees: WorkbenchWorktree[];
  activeWorktreeId: string | null;
  removingWorktree: WorkbenchWorktree;
  confirmActiveWorktreeChange: (nextActive: WorkbenchWorktree | null) => boolean;
  removeWorktree: (plan: MobileWorktreeRemovalPlan) => Promise<void>;
  applyRemoval: (plan: MobileWorktreeRemovalPlan) => void | Promise<void>;
}

export interface MobileWorktreeMergeFlowOptions {
  worktrees: WorkbenchWorktree[];
  activeWorktreeId: string | null;
  sourceWorktree: WorkbenchWorktree;
  confirmActiveWorktreeChange: (nextActive: WorkbenchWorktree | null) => boolean;
  mergeWorktree: () => Promise<void>;
  applyMergeSuccess: (plan: MobileWorktreeRemovalPlan) => void | Promise<void>;
}

export interface MobileWorktreeRefreshFlowOptions {
  nextWorktrees: WorkbenchWorktree[];
  currentActiveWorktreeId: string | null;
  skipActivePreflight?: boolean;
  confirmActiveWorktreeChange: (nextActive: WorkbenchWorktree | null) => boolean;
  applyRefresh: (plan: Pick<MobileWorktreeRemovalPlan, 'nextWorktrees' | 'nextActive'>) => void;
}

export type MobileGitActionContext = MobileFilePanelContext;

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
 *   删除 active worktree 是破坏性操作，必须先确认是否允许离开 dirty Files 草稿，但只能在后端删除成功后才应用 UI 状态。
 *
 * Code Logic（这个函数做什么）:
 *   计算删除计划；active 删除先执行只读确认，取消时不调用后端；后端成功后才调用 applyRemoval。
 */
export async function runMobileWorktreeRemovalFlow(
  options: MobileWorktreeRemovalFlowOptions,
): Promise<MobileWorktreeDestructiveFlowResult> {
  const plan = getMobileWorktreeRemovalPlan(
    options.worktrees,
    options.activeWorktreeId,
    options.removingWorktree,
  );

  if (
    plan.requiresActivePreflight &&
    !options.confirmActiveWorktreeChange(plan.nextActive)
  ) {
    return 'cancelled';
  }

  await options.removeWorktree(plan);
  await options.applyRemoval(plan);
  return 'applied';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   merge 成功会删除源 worktree；只有源 worktree 正是当前 active 时，移动端才需要在后端 merge 前保护 active Files 草稿。
 *
 * Code Logic（这个函数做什么）:
 *   用真实 active worktree id 复用删除计划；仅 active 源合并执行确认，后端成功后才执行成功应用回调。
 */
export async function runMobileWorktreeMergeFlow(
  options: MobileWorktreeMergeFlowOptions,
): Promise<MobileWorktreeDestructiveFlowResult> {
  const plan = getMobileWorktreeRemovalPlan(
    options.worktrees,
    options.activeWorktreeId,
    options.sourceWorktree,
  );

  if (
    plan.requiresActivePreflight &&
    !options.confirmActiveWorktreeChange(plan.nextActive)
  ) {
    return 'cancelled';
  }

  await options.mergeWorktree();
  await options.applyMergeSuccess(plan);
  return 'applied';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   刷新 worktree 列表时当前 active 可能已被后端删除，移动端不能在用户取消 dirty guard 前先写入新列表。
 *
 * Code Logic（这个函数做什么）:
 *   先计算下一 active；需要离开当前 active 且未显式跳过 guard 时先确认，确认通过后才调用 applyRefresh。
 */
export function runMobileWorktreeRefreshFlow(
  options: MobileWorktreeRefreshFlowOptions,
): MobileWorktreeDestructiveFlowResult {
  const currentStillExists = options.currentActiveWorktreeId
    ? options.nextWorktrees.some((worktree) => worktree.id === options.currentActiveWorktreeId)
    : false;
  const nextActive = currentStillExists
    ? options.nextWorktrees.find((worktree) => worktree.id === options.currentActiveWorktreeId) ??
      null
    : selectPreferredMobileWorktree(options.nextWorktrees);

  if (
    !options.skipActivePreflight &&
    options.currentActiveWorktreeId &&
    !currentStillExists &&
    !options.confirmActiveWorktreeChange(nextActive)
  ) {
    return 'cancelled';
  }

  options.applyRefresh({ nextWorktrees: options.nextWorktrees, nextActive });
  return 'applied';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   merge 成功会删除源 worktree，后续权威刷新即使失败，移动端也应立即离开已删除源 worktree。
 *
 * Code Logic（这个函数做什么）:
 *   从 destructive worktree 计划中提取应先应用到 UI 的 worktree 列表与 active worktree。
 */
export function getMobileWorktreeMergeAppliedState(
  plan: MobileWorktreeRemovalPlan,
): Pick<MobileWorktreeRemovalPlan, 'nextWorktrees' | 'nextActive'> {
  return { nextWorktrees: plan.nextWorktrees, nextActive: plan.nextActive };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   merge 请求返回时用户可能已切到同项目其它 worktree，旧响应不能清空当前 worktree 的提交列表。
 *
 * Code Logic（这个函数做什么）:
 *   比较 merge 请求发起 context 与当前 context 的稳定 key；只有仍停留在请求源 worktree 时才允许回写。
 */
export function isMobileGitMergeResponseCurrent(
  requestContext: MobileGitActionContext,
  currentContext: MobileGitActionContext | null,
): boolean {
  return getMobileFileContextKey(requestContext) === getMobileFileContextKey(currentContext);
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
 *   保存文件响应可能晚于文件切换或 worktree 切换返回，旧响应不能覆盖当前打开文件或清 dirty。
 *
 * Code Logic（这个函数做什么）:
 *   同时校验请求 id、发起 context 与当前 context 的 key，以及发起保存的 path 与当前打开 path。
 */
export function isMobileFileSaveResponseCurrent(
  requestId: number,
  latestRequestId: number,
  requestContext: MobileFilePanelContext,
  loadedContext: MobileFilePanelContext | null,
  requestPath: string,
  openedPath: string | null,
): boolean {
  return (
    requestId === latestRequestId &&
    getMobileFileContextKey(requestContext) === getMobileFileContextKey(loadedContext) &&
    requestPath === openedPath
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

/**
 * Business Logic（为什么需要这个函数）:
 *   Git 长操作返回时用户可能已切换项目或 worktree，旧响应不能污染当前移动端 UI。
 *
 * Code Logic（这个函数做什么）:
 *   比较操作发起 context 与当前 context 的稳定 key；相同才允许回写状态。
 */
export function isMobileGitActionResponseCurrent(
  requestContext: MobileGitActionContext,
  currentContext: MobileGitActionContext | null,
): boolean {
  return getMobileFileContextKey(requestContext) === getMobileFileContextKey(currentContext);
}
