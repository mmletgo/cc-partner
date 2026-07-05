import {
  canOpenMobileWorktreeSwitcher,
  canSelectMobileProject,
  canRunMobilePaneMutation,
  canRunMobileWorktreeDestructiveAction,
  canSwitchMobilePane,
  closeMobileNav,
  getMobileCreatePaneDirection,
  getInitialMobileNavOpen,
  getMobileWorktreeStatusKind,
  openMobileNav,
  selectMobileWorktreeWorkspacePanel,
  selectPreferredMobileSession,
  selectPreferredMobileWorktree,
  selectMobilePanel,
  type MobileWorkbenchPanel,
} from './mobileWorkbenchState';
import type { WorkbenchProject, WorkbenchSession, WorkbenchWorktree } from '@/lib/types';

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
 *   移动端项目列表需要区分可直接加载的本机项目和暂不支持二级代理的远端快捷方式，测试要构造最小合法 DTO。
 *
 * Code Logic（这个函数做什么）:
 *   接收局部字段并补齐 WorkbenchProject 的必填字段，返回可复用测试对象。
 */
function createProject(
  overrides: Partial<WorkbenchProject> & Pick<WorkbenchProject, 'id' | 'name' | 'kind'>,
): WorkbenchProject {
  return {
    id: overrides.id,
    name: overrides.name,
    kind: overrides.kind,
    deviceId: overrides.deviceId ?? 'device-1',
    deviceName: overrides.deviceName ?? 'This Mac',
    path: overrides.path ?? `/tmp/${overrides.name}`,
    lastOpenedAt: overrides.lastOpenedAt ?? '2026-06-29T00:00:00Z',
    createdAt: overrides.createdAt ?? '2026-06-29T00:00:00Z',
    updatedAt: overrides.updatedAt ?? '2026-06-29T00:00:00Z',
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
 *   用户确认手机竖屏导航默认常开，只有主动收起后才显示顶部展开小按钮。
 *
 * Code Logic（这个函数做什么）:
 *   调用默认导航状态 helper，断言初始状态为打开。
 */
function testInitialMobileNavOpenDefaultsToTrue(): void {
  assertEqual(getInitialMobileNavOpen(), true);
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
 *   `/mobile` 当前只支持直连加载本机项目，远端快捷方式应展示但不可选择，避免用户点入必然失败的二级代理路径。
 *
 * Code Logic（这个函数做什么）:
 *   构造 local 与 remote 两类项目，断言 helper 只允许 local 项目进入详情加载。
 */
function testCanSelectMobileProjectOnlyAllowsLocalProjects(): void {
  const localProject = createProject({ id: 'local', name: 'local-app', kind: 'local' });
  const remoteProject = createProject({ id: 'remote', name: 'remote-app', kind: 'remote' });

  assertEqual(canSelectMobileProject(localProject), true);
  assertEqual(canSelectMobileProject(remoteProject), false);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端 worktree 列表和 Git 面板需要一致区分 clean、dirty、conflict，且冲突必须盖过普通 dirty 状态。
 *
 * Code Logic（这个函数做什么）:
 *   构造 clean/dirty/conflict 样本，断言真实 DTO 的 conflicts 计数大于 0 时优先进入 conflict，clean=false 即使 changed=0 也算 dirty。
 */
function testMobileWorktreeStatusKindPrioritizesConflictThenDirty(): void {
  const clean = createWorktree({ id: 'clean', name: 'main', isMain: true });
  const dirty = createWorktree({
    id: 'dirty',
    name: 'feature/dirty',
    status: {
      branch: 'feature/dirty',
      changed: 2,
      ahead: 0,
      behind: 0,
      conflicts: 0,
      clean: false,
      canPush: false,
    },
  });
  const conflict = createWorktree({
    id: 'conflict',
    name: 'feature/conflict',
    status: {
      branch: 'feature/conflict',
      changed: 2,
      ahead: 0,
      behind: 0,
      conflicts: 1,
      clean: false,
      canPush: false,
    },
  });
  const conflictClean = createWorktree({
    id: 'conflict-clean',
    name: 'feature/conflict-clean',
    status: {
      branch: 'feature/conflict-clean',
      changed: 0,
      ahead: 0,
      behind: 0,
      conflicts: 1,
      clean: true,
      canPush: false,
    },
  });
  const dirtyWithoutChangedCount = createWorktree({
    id: 'dirty-without-changed-count',
    name: 'feature/dirty-without-changed-count',
    status: {
      branch: 'feature/dirty-without-changed-count',
      changed: 0,
      ahead: 0,
      behind: 0,
      conflicts: 0,
      clean: false,
      canPush: false,
    },
  });

  assertEqual(getMobileWorktreeStatusKind(conflict), 'conflict');
  assertEqual(getMobileWorktreeStatusKind(conflictClean), 'conflict');
  assertEqual(getMobileWorktreeStatusKind(dirty), 'dirty');
  assertEqual(getMobileWorktreeStatusKind(dirtyWithoutChangedCount), 'dirty');
  assertEqual(getMobileWorktreeStatusKind(clean), 'clean');
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端 worktree switcher 只能在本机项目且非加载态时打开，避免用户进入远端未支持路径。
 *
 * Code Logic（这个函数做什么）:
 *   构造 null、remote、busy local 和 idle local 场景，断言 switcher 打开条件。
 */
function testMobileWorktreeSwitcherRequiresIdleLocalProject(): void {
  const localProject = createProject({ id: 'local', name: 'local-app', kind: 'local' });
  const remoteProject = createProject({ id: 'remote', name: 'remote-app', kind: 'remote' });

  assertEqual(canOpenMobileWorktreeSwitcher(null, false), false);
  assertEqual(canOpenMobileWorktreeSwitcher(remoteProject, false), false);
  assertEqual(canOpenMobileWorktreeSwitcher(localProject, true), false);
  assertEqual(canOpenMobileWorktreeSwitcher(localProject, false), true);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端删除、合并等破坏性 worktree 操作不能作用于主工作区，也不能在异步操作占用时重复触发。
 *
 * Code Logic（这个函数做什么）:
 *   构造主 worktree、busy feature 和 idle feature，断言破坏性操作可用性。
 */
function testMobileWorktreeDestructiveActionRequiresIdleNonMainWorktree(): void {
  const main = createWorktree({ id: 'main', name: 'main', isMain: true });
  const feature = createWorktree({ id: 'feature', name: 'feature/mobile', isMain: false });

  assertEqual(canRunMobileWorktreeDestructiveAction(main, false), false);
  assertEqual(canRunMobileWorktreeDestructiveAction(feature, true), false);
  assertEqual(canRunMobileWorktreeDestructiveAction(feature, false), true);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用户在移动端 Worktrees 面板点击工作区时，期望成功切换后直接进入对应 Workbench 操作现场。
 *
 * Code Logic（这个函数做什么）:
 *   断言选择成功会跳到 terminal 面板；选择被 dirty guard 拦截时保持当前面板不变。
 */
function testWorktreeWorkspaceClickNavigatesToTerminalOnlyAfterAcceptedSelection(): void {
  assertEqual(selectMobileWorktreeWorkspacePanel(true), 'terminal');
  assertEqual(selectMobileWorktreeWorkspacePanel(false), null);
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
 *   当前 worktree 可能同时有历史断开的 terminal window 和仍在运行的窗口，移动端应优先恢复可交互的 running 窗口。
 *
 * Code Logic（这个函数做什么）:
 *   构造同一 worktree 下 disconnected/running 两个 session，断言 helper 选择 matching 且 running 的窗口。
 */
function testPreferredSessionUsesRunningMatchingBeforeStoppedMatching(): void {
  const stoppedMatching = createSession({
    id: 'stopped-matching',
    name: 'stopped matching',
    worktreeId: 'main',
    status: 'disconnected',
  });
  const runningMatching = createSession({
    id: 'running-matching',
    name: 'running matching',
    worktreeId: 'main',
    status: 'running',
  });

  assertEqual(
    selectPreferredMobileSession([stoppedMatching, runningMatching], 'main')?.id ?? null,
    'running-matching',
  );
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

/**
 * Business Logic（为什么需要这个函数）:
 *   手机端不再让用户选择左右/上下分屏，新增 pane 要有稳定默认方向来适配竖屏操作。
 *
 * Code Logic（这个函数做什么）:
 *   调用移动端 pane 创建方向 helper，断言默认使用 tmux 上下分割方向。
 */
function testMobileCreatePaneDirectionUsesDown(): void {
  assertEqual(getMobileCreatePaneDirection(), 'down');
}

/**
 * Business Logic（为什么需要这个函数）:
 *   pane 新增/关闭必须只在当前 terminal window 支持 tmux pane 且没有异步操作占用时开放。
 *
 * Code Logic（这个函数做什么）:
 *   构造支持/不支持 panes 的 session，断言 mutation 可用性受 supportsPanes 与 busy 共同控制。
 */
function testMobilePaneMutationRequiresSupportedIdleSession(): void {
  const supported = createSession({ id: 'supported', name: 'Supported', supportsPanes: true });
  const unsupported = createSession({
    id: 'unsupported',
    name: 'Unsupported',
    supportsPanes: false,
  });

  assertEqual(canRunMobilePaneMutation(supported, false), true);
  assertEqual(canRunMobilePaneMutation(supported, true), false);
  assertEqual(canRunMobilePaneMutation(unsupported, false), false);
  assertEqual(canRunMobilePaneMutation(null, false), false);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   切换 pane 在单 pane window 内没有可见效果，移动端应避免展示可点击但无意义的操作。
 *
 * Code Logic（这个函数做什么）:
 *   构造单 pane 与多 pane session，断言只有多 pane 且空闲时允许切换。
 */
function testMobilePaneSwitchRequiresMultiplePanes(): void {
  const singlePane = createSession({ id: 'single', name: 'Single', paneCount: 1 });
  const multiPane = createSession({ id: 'multi', name: 'Multi', paneCount: 2 });

  assertEqual(canSwitchMobilePane(singlePane, false), false);
  assertEqual(canSwitchMobilePane(multiPane, false), true);
  assertEqual(canSwitchMobilePane(multiPane, true), false);
}

testSelectMobilePanelReturnsNextPanel();
testOpenMobileNavReturnsTrue();
testInitialMobileNavOpenDefaultsToTrue();
testCloseMobileNavReturnsFalse();
testCanSelectMobileProjectOnlyAllowsLocalProjects();
testMobileWorktreeStatusKindPrioritizesConflictThenDirty();
testMobileWorktreeSwitcherRequiresIdleLocalProject();
testMobileWorktreeDestructiveActionRequiresIdleNonMainWorktree();
testWorktreeWorkspaceClickNavigatesToTerminalOnlyAfterAcceptedSelection();
testPreferredWorktreeUsesMainBeforeFirst();
testPreferredWorktreeFallsBackToFirstOrNull();
testPreferredSessionUsesMatchingWorktreeBeforeRunning();
testPreferredSessionUsesRunningMatchingBeforeStoppedMatching();
testPreferredSessionFallsBackToRunningFirstOrNull();
testMobileCreatePaneDirectionUsesDown();
testMobilePaneMutationRequiresSupportedIdleSession();
testMobilePaneSwitchRequiresMultiplePanes();
