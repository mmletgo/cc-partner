import {
  closeMobileNav,
  openMobileNav,
  selectPreferredMobileSession,
  selectPreferredMobileWorktree,
  selectMobilePanel,
  type MobileWorkbenchPanel,
} from './mobileWorkbenchState';
import type { WorkbenchSession, WorkbenchWorktree } from '@/lib/types';

/**
 * Business Logic（为什么需要这个函数）:
 *   当前 web tsconfig 会编译 src 下测试文件，但未启用 Node 类型；测试断言需要避免依赖 node:assert。
 *
 * Code Logic（这个函数做什么）:
 *   比较 actual 与 expected，不一致时抛出 Error 让 tsx 进程以失败状态退出。
 */
function assertEqual<T>(actual: T, expected: T): void {
  if (actual !== expected) {
    throw new Error(`Expected ${String(expected)}, received ${String(actual)}`);
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Task6 需要在移动端加载项目后自动选中一个 worktree，测试要构造最小合法 DTO。
 *
 * Code Logic（这个函数做什么）:
 *   接收局部字段并补齐 WorkbenchWorktree 的必填字段，返回可复用测试对象。
 */
function createWorktree(
  overrides: Partial<WorkbenchWorktree> & Pick<WorkbenchWorktree, 'id' | 'name'>,
): WorkbenchWorktree {
  return {
    id: overrides.id,
    projectId: overrides.projectId ?? 'project-1',
    name: overrides.name,
    branch: overrides.branch ?? overrides.name,
    baseBranch: overrides.baseBranch ?? null,
    path: overrides.path ?? `/tmp/${overrides.name}`,
    isMain: overrides.isMain ?? false,
    status: overrides.status ?? {
      branch: overrides.branch ?? overrides.name,
      changed: 0,
      ahead: 0,
      behind: 0,
      conflicts: 0,
      clean: true,
      canPush: false,
    },
    createdAt: overrides.createdAt ?? '2026-06-29T00:00:00Z',
    updatedAt: overrides.updatedAt ?? '2026-06-29T00:00:00Z',
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Task6 需要在移动端加载项目后自动选中一个 terminal session，测试要构造最小合法 DTO。
 *
 * Code Logic（这个函数做什么）:
 *   接收局部字段并补齐 WorkbenchSession 的必填字段，返回可复用测试对象。
 */
function createSession(
  overrides: Partial<WorkbenchSession> & Pick<WorkbenchSession, 'id' | 'name'>,
): WorkbenchSession {
  return {
    id: overrides.id,
    projectId: overrides.projectId ?? 'project-1',
    worktreeId: overrides.worktreeId ?? null,
    name: overrides.name,
    command: overrides.command ?? '',
    cwd: overrides.cwd ?? '/tmp/project',
    status: overrides.status ?? 'disconnected',
    cols: overrides.cols ?? 80,
    rows: overrides.rows ?? 24,
    startedAt: overrides.startedAt ?? '2026-06-29T00:00:00Z',
    exitedAt: overrides.exitedAt ?? null,
    exitCode: overrides.exitCode ?? null,
    supportsPanes: overrides.supportsPanes ?? true,
    paneCount: overrides.paneCount ?? 1,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端 Workbench shell 需要稳定切换当前面板，后续业务面板接入时不应破坏默认导航契约。
 *
 * Code Logic（这个函数做什么）:
 *   调用 selectMobilePanel 并断言它总是返回用户下一步选择的面板。
 */
function testSelectMobilePanelReturnsNextPanel(): void {
  const current: MobileWorkbenchPanel = 'projects';
  const next: MobileWorkbenchPanel = 'terminal';

  assertEqual(selectMobilePanel(current, next), next);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   手机竖屏需要通过顶部按钮打开覆盖式导航抽屉，抽屉状态 helper 必须返回打开态。
 *
 * Code Logic（这个函数做什么）:
 *   调用 openMobileNav 并断言返回 true。
 */
function testOpenMobileNavReturnsTrue(): void {
  assertEqual(openMobileNav(), true);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用户选择导航项或点击遮罩后需要关闭移动端抽屉，关闭 helper 必须返回关闭态。
 *
 * Code Logic（这个函数做什么）:
 *   调用 closeMobileNav 并断言返回 false。
 */
function testCloseMobileNavReturnsFalse(): void {
  assertEqual(closeMobileNav(), false);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端选择项目后应优先进入主工作区，避免默认落到随机 feature worktree。
 *
 * Code Logic（这个函数做什么）:
 *   构造 feature/main 顺序的 worktree 列表，断言 helper 仍返回 isMain 的项目。
 */
function testPreferredWorktreeUsesMainBeforeFirst(): void {
  const feature = createWorktree({ id: 'feature', name: 'feature/login' });
  const main = createWorktree({ id: 'main', name: 'main', isMain: true });

  assertEqual(selectPreferredMobileWorktree([feature, main])?.id ?? null, 'main');
}

/**
 * Business Logic（为什么需要这个函数）:
 *   没有主工作区标记时，移动端仍应选择首个 worktree 作为可用上下文。
 *
 * Code Logic（这个函数做什么）:
 *   构造无 main 的列表并断言 helper 返回首项；空列表返回 null。
 */
function testPreferredWorktreeFallsBackToFirstOrNull(): void {
  const feature = createWorktree({ id: 'feature', name: 'feature/login' });

  assertEqual(selectPreferredMobileWorktree([feature])?.id ?? null, 'feature');
  assertEqual(selectPreferredMobileWorktree([]), null);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端切换 active worktree 时应优先显示绑定到该 worktree 的 terminal window。
 *
 * Code Logic（这个函数做什么）:
 *   构造一个 running 但不匹配的 session 和一个匹配 session，断言 helper 优先返回匹配项。
 */
function testPreferredSessionUsesMatchingWorktreeBeforeRunning(): void {
  const runningOther = createSession({
    id: 'running-other',
    name: 'other',
    worktreeId: 'other',
    status: 'running',
  });
  const matching = createSession({
    id: 'matching',
    name: 'matching',
    worktreeId: 'main',
    status: 'disconnected',
  });

  assertEqual(selectPreferredMobileSession([runningOther, matching], 'main')?.id ?? null, 'matching');
}

/**
 * Business Logic（为什么需要这个函数）:
 *   当前 worktree 没有 terminal window 时，移动端应优先展示仍在运行的 session。
 *
 * Code Logic（这个函数做什么）:
 *   构造 disconnected/running 顺序的 session 列表，断言 helper 返回 running；空列表返回 null。
 */
function testPreferredSessionFallsBackToRunningFirstOrNull(): void {
  const stopped = createSession({ id: 'stopped', name: 'stopped', status: 'disconnected' });
  const running = createSession({ id: 'running', name: 'running', status: 'running' });

  assertEqual(selectPreferredMobileSession([stopped, running], 'main')?.id ?? null, 'running');
  assertEqual(selectPreferredMobileSession([stopped], 'main')?.id ?? null, 'stopped');
  assertEqual(selectPreferredMobileSession([], 'main'), null);
}

testSelectMobilePanelReturnsNextPanel();
testOpenMobileNavReturnsTrue();
testCloseMobileNavReturnsFalse();
testPreferredWorktreeUsesMainBeforeFirst();
testPreferredWorktreeFallsBackToFirstOrNull();
testPreferredSessionUsesMatchingWorktreeBeforeRunning();
testPreferredSessionFallsBackToRunningFirstOrNull();
