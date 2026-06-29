import {
  getMobileFileContextKey,
  isMobileFileOpenResponseCurrent,
  shouldBlockMobileFileContextSwitch,
  shouldConfirmMobileFileDirtyContextSwitch,
  shouldSkipMobileFileContextConfirmForDiscardToken,
  shouldInvalidateMobileFileOpenOnDirectoryLoad,
  shouldReloadMobileGitCommitsAfterAction,
  shouldSkipMobileFileContextReload,
  type MobileFilePanelContext,
  type MobileFileDirtySnapshot,
} from './mobilePanelState';

/**
 * Business Logic（为什么需要这个函数）:
 *   当前 web tsconfig 会编译 src 下测试文件，但未启用 Node 类型；测试断言需要避免依赖 node:assert。
 *
 * Code Logic（这个函数做什么）:
 *   比较 actual 与 expected，不一致时抛出 Error 让 tsx 进程以失败状态退出。
 */
function assertEqual<T>(actual: T, expected: T, message: string): void {
  if (actual !== expected) {
    throw new Error(`${message}: expected ${String(expected)}, received ${String(actual)}`);
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端文件面板的测试需要构造 project/worktree 上下文，避免在断言中重复字面量。
 *
 * Code Logic（这个函数做什么）:
 *   接收项目与 worktree id，返回 MobileFilePanelContext；空 worktree 统一为 null。
 */
function createContext(projectId: string, worktreeId: string | null): MobileFilePanelContext {
  return { projectId, worktreeId };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用户取消切换后再回到原 project/worktree 时，未保存草稿必须保留，不能重新加载根目录覆盖当前文件状态。
 *
 * Code Logic（这个函数做什么）:
 *   构造 loaded/next/opened 都相同的上下文，断言上下文同步应跳过 reset 与 reload。
 */
function testReturningToLoadedContextSkipsReload(): void {
  const context = createContext('project-1', 'worktree-1');

  assertEqual(
    shouldSkipMobileFileContextReload(context, context, context),
    true,
    'returning to loaded dirty context should skip reload',
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   dirty 草稿只应阻止真正切换到另一个 project/worktree，同上下文重渲染不应进入阻塞态。
 *
 * Code Logic（这个函数做什么）:
 *   分别断言 dirty+不同上下文会阻塞，dirty+同上下文或 clean+不同上下文不会阻塞。
 */
function testDirtyContextSwitchBlockBoundary(): void {
  const current = createContext('project-1', 'worktree-1');
  const next = createContext('project-1', 'worktree-2');

  assertEqual(
    shouldBlockMobileFileContextSwitch(current, next, true),
    true,
    'dirty changed context should block until user confirms',
  );
  assertEqual(
    shouldBlockMobileFileContextSwitch(current, current, true),
    false,
    'dirty same context should not block',
  );
  assertEqual(
    shouldBlockMobileFileContextSwitch(current, next, false),
    false,
    'clean changed context should not block',
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Files 面板被隐藏但仍挂载时，父级在切换项目或 worktree 前必须能用 dirty snapshot 判断是否需要确认。
 *
 * Code Logic（这个函数做什么）:
 *   构造 dirty snapshot 与不同目标上下文，断言父级切换守卫需要弹出确认。
 */
function testDirtySnapshotDifferentTargetRequiresParentConfirm(): void {
  const snapshot: MobileFileDirtySnapshot = {
    dirty: true,
    context: createContext('project-1', 'worktree-1'),
  };
  const next = createContext('project-1', 'worktree-2');

  assertEqual(
    shouldConfirmMobileFileDirtyContextSwitch(snapshot, next),
    true,
    'dirty snapshot should require parent confirm when target context differs',
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用户重新选择当前 project/worktree 时不能误弹确认或清空草稿，否则会破坏 Task8 的“切回原上下文保留草稿”要求。
 *
 * Code Logic（这个函数做什么）:
 *   构造 dirty snapshot 与相同目标上下文，断言父级切换守卫不需要确认。
 */
function testDirtySnapshotSameTargetSkipsParentConfirm(): void {
  const context = createContext('project-1', 'worktree-1');
  const snapshot: MobileFileDirtySnapshot = {
    dirty: true,
    context,
  };

  assertEqual(
    shouldConfirmMobileFileDirtyContextSwitch(snapshot, context),
    false,
    'dirty snapshot should not require parent confirm when target context is unchanged',
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   父级已经确认丢弃 dirty 草稿后，Files 面板响应 props context 变化时不能再次弹出同一条确认。
 *
 * Code Logic（这个函数做什么）:
 *   比较上一次与当前 discard token，断言 token 变化时内部 context confirm 应被跳过，token 不变时不跳过。
 */
function testDiscardTokenChangeSkipsInternalContextConfirm(): void {
  assertEqual(
    shouldSkipMobileFileContextConfirmForDiscardToken(1, 2),
    true,
    'changed discard token should skip internal context confirm',
  );
  assertEqual(
    shouldSkipMobileFileContextConfirmForDiscardToken(2, 2),
    false,
    'unchanged discard token should keep normal internal confirm behavior',
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   旧 files.open 响应不能在 project/worktree 切换或根目录重载后写入旧文件预览。
 *
 * Code Logic（这个函数做什么）:
 *   同时校验 request id 和发起请求时的 context；任一不匹配都应判定为 stale。
 */
function testOpenResponseRequiresLatestRequestAndCurrentContext(): void {
  const context = createContext('project-1', 'worktree-1');
  const otherContext = createContext('project-1', 'worktree-2');

  assertEqual(
    isMobileFileOpenResponseCurrent(2, 2, context, context),
    true,
    'latest open response in loaded context should be current',
  );
  assertEqual(
    isMobileFileOpenResponseCurrent(1, 2, context, context),
    false,
    'older open response should be stale',
  );
  assertEqual(
    isMobileFileOpenResponseCurrent(2, 2, context, otherContext),
    false,
    'open response from previous context should be stale',
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   重新加载根目录代表文件面板回到新的上下文基线，未完成的 open 请求必须失效。
 *
 * Code Logic（这个函数做什么）:
 *   断言空路径根目录需要失效 open 请求，子目录加载不触发该边界规则。
 */
function testRootDirectoryLoadInvalidatesOpenRequests(): void {
  assertEqual(
    shouldInvalidateMobileFileOpenOnDirectoryLoad(''),
    true,
    'root directory load should invalidate pending open requests',
  );
  assertEqual(
    shouldInvalidateMobileFileOpenOnDirectoryLoad('src'),
    false,
    'child directory load should not be treated as root context reload',
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   merge 成功后源 worktree 可能已被后端删除，移动端不能再用旧源 worktree id 拉 commits 并误报失败。
 *
 * Code Logic（这个函数做什么）:
 *   断言 commit/push 成功后仍刷新 commits，但 merge 成功后只刷新 worktrees。
 */
function testMergeRefreshSkipsCommitReload(): void {
  assertEqual(
    shouldReloadMobileGitCommitsAfterAction('commit'),
    true,
    'commit should reload commits',
  );
  assertEqual(
    shouldReloadMobileGitCommitsAfterAction('push'),
    true,
    'push should reload commits',
  );
  assertEqual(
    shouldReloadMobileGitCommitsAfterAction('merge'),
    false,
    'merge should not reload commits from deleted source worktree',
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   context key 是文件面板 stale guard 的基础，null worktree 必须有稳定表示。
 *
 * Code Logic（这个函数做什么）:
 *   断言同一 project + null worktree 生成稳定 key，null context 返回空 key。
 */
function testContextKeyIsStable(): void {
  assertEqual(
    getMobileFileContextKey(createContext('project-1', null)),
    'project-1:',
    'null worktree should produce stable key',
  );
  assertEqual(getMobileFileContextKey(null), '', 'null context should produce empty key');
}

testReturningToLoadedContextSkipsReload();
testDirtyContextSwitchBlockBoundary();
testDirtySnapshotDifferentTargetRequiresParentConfirm();
testDirtySnapshotSameTargetSkipsParentConfirm();
testDiscardTokenChangeSkipsInternalContextConfirm();
testOpenResponseRequiresLatestRequestAndCurrentContext();
testRootDirectoryLoadInvalidatesOpenRequests();
testMergeRefreshSkipsCommitReload();
testContextKeyIsStable();

console.log('mobilePanelState.test.ts passed');
