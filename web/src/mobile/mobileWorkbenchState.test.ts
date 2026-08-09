import { describe, test } from 'vitest';
import {
  canOpenMobileWorktreeSwitcher,
  canSelectMobileProject,
  canRunMobilePaneMutation,
  canRunMobileWorktreeDestructiveAction,
  canSwitchMobilePane,
  closeMobileNav,
  computeMobileKeyboardInset,
  computeMobileTerminalMinHeight,
  computeMobileViewportLayoutHints,
  emptyMobileSessionRuntimeState,
  getMobileConnectionCachedAt,
  getMobileNavGroupIdForPanel,
  getMobileWorkbenchNavGroups,
  getMobileWorkbenchPanelOrder,
  getMobileTerminalChromeVisibility,
  getMobileCreatePaneDirection,
  getInitialMobileWorkbenchPanel,
  getInitialMobileNavOpen,
  getMobileWorktreeStatusKind,
  markMobileConnectionOffline,
  markMobileConnectionOnline,
  markMobileConnectionReconnecting,
  openMobileNav,
  reduceMobileSessionRuntime,
  seedMobileSessionRuntimeFromSessions,
  selectMobileWorktreeWorkspacePanel,
  selectMobilePanelForProject,
  selectPreferredMobileSession,
  selectPreferredMobileWorktree,
  selectMobilePanel,
  shouldRefreshMobilePanelOnReconnect,
  shouldSkipMobileProjectReload,
  type MobileWorkbenchPanel,
} from './mobileWorkbenchState';
import type { WorkbenchProject, WorkbenchSession, WorkbenchWorktree } from '@/lib/types';

/**
 * Business Logic（为什么需要这个函数）:
 *   当前 web tsconfig 会编译 src 下测试文件，但未启用 Node 类型；测试断言需要避免依赖 node:assert。
 *
 * Code Logic（这个函数做什么）:
 *   比较 actual 与 expected，不一致时抛出 Error 让用例失败。
 */
function assertEqual<T>(actual: T, expected: T): void {
  if (actual !== expected) {
    throw new Error(`Expected ${String(expected)}, received ${String(actual)}`);
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端主导航顺序是手机端功能发现的核心契约，测试需要断言完整数组而不是只比较相对位置。
 *
 * Code Logic（这个函数做什么）:
 *   比较两个只读字符串数组的长度与逐项值，不一致时抛出包含完整数组的错误。
 */
function assertArrayEqual(actual: readonly string[], expected: readonly string[]): void {
  const matches =
    actual.length === expected.length && actual.every((value, index) => value === expected[index]);
  if (!matches) {
    throw new Error(`Expected ${JSON.stringify(expected)}, received ${JSON.stringify(actual)}`);
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

describe('mobileWorkbenchState', () => {
  /**
   * Business Logic（为什么需要这个测试）:
   *   移动端 Workbench shell 需要稳定切换当前面板，后续业务面板接入时不应破坏默认导航契约。
   *
   * Code Logic（这个测试做什么）:
   *   调用 selectMobilePanel 并断言它总是返回用户下一步选择的面板。
   */
  test('selectMobilePanel returns the next panel', () => {
    const current: MobileWorkbenchPanel = 'projects';
    const next: MobileWorkbenchPanel = 'terminal';

    assertEqual(selectMobilePanel(current, next), next);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   远端项目在移动端需要与本机项目一样进入终端、文件、Git、worktree、Prompt 和自动化面板。
   *
   * Code Logic（这个测试做什么）:
   *   构造 local 与 remote 项目，断言面板选择 helper 不再把 remote 项目的本机专属面板回落到 automation。
   */
  test('selectMobilePanelForProject keeps requested panel for remote projects', () => {
    const localProject = createProject({ id: 'local', name: 'local-app', kind: 'local' });
    const remoteProject = createProject({ id: 'remote', name: 'remote-app', kind: 'remote' });

    assertEqual(selectMobilePanelForProject(localProject, 'terminal'), 'terminal');
    assertEqual(selectMobilePanelForProject(remoteProject, 'terminal'), 'terminal');
    assertEqual(selectMobilePanelForProject(remoteProject, 'files'), 'files');
    assertEqual(selectMobilePanelForProject(remoteProject, 'git'), 'git');
    assertEqual(selectMobilePanelForProject(remoteProject, 'worktrees'), 'worktrees');
    assertEqual(selectMobilePanelForProject(remoteProject, 'prompt'), 'prompt');
    assertEqual(selectMobilePanelForProject(remoteProject, 'projects'), 'projects');
    assertEqual(selectMobilePanelForProject(remoteProject, 'settings'), 'settings');
    assertEqual(selectMobilePanelForProject(remoteProject, 'automation'), 'automation');
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   设计合同要求固定五组映射，且每个既有 panel 恰好出现一次，避免第二套路由或重复入口。
   *
   * Code Logic（这个测试做什么）:
   *   断言 Projects/Attention/Work/Automation/More 精确 panel 列表，并校验扁平顺序无重复覆盖全部 panel。
   */
  test('mobile nav groups map every panel exactly once', () => {
    const groups = getMobileWorkbenchNavGroups();
    assertArrayEqual(
      groups.map((group) => group.id),
      ['projects', 'attention', 'work', 'automation', 'more'],
    );
    assertArrayEqual(groups[0]?.panels ?? [], ['projects', 'worktrees']);
    assertArrayEqual(groups[1]?.panels ?? [], ['attention']);
    assertArrayEqual(groups[2]?.panels ?? [], [
      'terminal',
      'browser',
      'files',
      'git',
      'prompt',
    ]);
    assertArrayEqual(groups[3]?.panels ?? [], ['automation']);
    assertArrayEqual(groups[4]?.panels ?? [], ['settings', 'provider']);

    const flat = getMobileWorkbenchPanelOrder();
    assertArrayEqual(flat, [
      'projects',
      'worktrees',
      'attention',
      'terminal',
      'browser',
      'files',
      'git',
      'prompt',
      'automation',
      'settings',
      'provider',
    ]);
    assertEqual(new Set(flat).size, flat.length);
    assertEqual(getMobileNavGroupIdForPanel('worktrees'), 'projects');
    assertEqual(getMobileNavGroupIdForPanel('git'), 'work');
    assertEqual(getMobileNavGroupIdForPanel('settings'), 'more');
    assertEqual(getMobileNavGroupIdForPanel('provider'), 'more');
    assertEqual(getInitialMobileWorkbenchPanel(), 'projects');
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   自动化是移动端 Workbench 的项目级同级面板，不能被塞进 worktree quick switch 这类 worktree 附属入口。
   *
   * Code Logic（这个测试做什么）:
   *   调用移动端面板顺序 helper，断言 automation 出现在主导航顺序中，且位于 Work 组之后的独立 Automation 组。
   */
  test('automation panel is a first-class mobile panel after the work group', () => {
    const panels = getMobileWorkbenchPanelOrder();

    assertEqual(panels.includes('automation'), true);
    assertEqual(panels.indexOf('prompt') < panels.indexOf('automation'), true);
    assertEqual(panels.indexOf('automation') < panels.indexOf('settings'), true);
    assertEqual(getMobileNavGroupIdForPanel('automation'), 'automation');
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   全局 Inbox 在移动端必须是独立 Attention 分组，默认首屏仍是 projects。
   *
   * Code Logic（这个测试做什么）:
   *   断言初始面板为 projects，Attention 分组仅含 attention，且 attention 不是默认面板。
   */
  test('attention is second nav group and never the default panel', () => {
    const groups = getMobileWorkbenchNavGroups();

    assertEqual(getInitialMobileWorkbenchPanel(), 'projects');
    assertEqual(groups[1]?.id, 'attention');
    assertArrayEqual(groups[1]?.panels ?? [], ['attention']);
    assertEqual(getMobileWorkbenchPanelOrder().includes('attention'), true);
    assertEqual(getInitialMobileWorkbenchPanel() === 'attention', false);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   Browser preview 是移动端 Workbench 的一等面板，必须固定在 Work 组内 terminal 后、files 前。
   *
   * Code Logic（这个测试做什么）:
   *   读取分组扁平顺序，断言 browser 位于 terminal 与 files 之间。
   */
  test('mobile workbench panel order includes browser preview in work group', () => {
    const panels = getMobileWorkbenchPanelOrder();
    assertEqual(panels.indexOf('terminal') + 1, panels.indexOf('browser'));
    assertEqual(panels.indexOf('browser') + 1, panels.indexOf('files'));
    assertEqual(getMobileNavGroupIdForPanel('browser'), 'work');
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   软键盘与横屏需要可测的 viewport 派生值，驱动 shell 高度与终端优先高度。
   *
   * Code Logic（这个测试做什么）:
   *   断言 keyboardInset/shellHeight/landscape/terminalMinHeight 的计算合同。
   */
  test('viewport helpers derive keyboard inset and landscape terminal height', () => {
    assertEqual(computeMobileKeyboardInset(844, 500, 0), 344);
    assertEqual(computeMobileKeyboardInset(844, 500, 40), 304);
    assertEqual(computeMobileKeyboardInset(844, 900, 0), 0);

    const portrait = computeMobileViewportLayoutHints(390, 844, 500, 0);
    assertEqual(portrait.keyboardInset, 344);
    assertEqual(portrait.shellHeight, 500);
    assertEqual(portrait.landscape, false);
    // shellHeight 取 layout/visual 较小值，terminalMinHeight 不得再扣 keyboardInset。
    assertEqual(portrait.terminalMinHeight, computeMobileTerminalMinHeight(390, 500, 0));
    assertEqual(portrait.terminalMinHeight, Math.max(160, Math.round(500 * 0.48)));

    // Android Chrome interactive-widget=resizes-content：键盘弹出时 layout viewport 也缩小，
    // shellHeight 取 layout/visual 较小值，两者都缩小到键盘上方。
    const resizesContent = computeMobileViewportLayoutHints(390, 500, 500, 0);
    assertEqual(resizesContent.shellHeight, 500);
    // 兜底：layout 比 visual 更小时（layout 已缩、visual 尚未更新），取 layout。
    const layoutSmaller = computeMobileViewportLayoutHints(390, 400, 500, 0);
    assertEqual(layoutSmaller.shellHeight, 400);

    const landscape = computeMobileViewportLayoutHints(844, 390, 390, 0);
    assertEqual(landscape.landscape, true);
    assertEqual(landscape.keyboardInset, 0);
    // 同高度下横屏 ratio 更高，优先终端可视高度
    assertEqual(
      landscape.terminalMinHeight > computeMobileTerminalMinHeight(390, 390, 0),
      true,
    );
    assertEqual(landscape.terminalMinHeight, Math.max(160, Math.round(390 * 0.72)));
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   手机竖屏需要通过顶部按钮打开覆盖式导航抽屉，抽屉状态 helper 必须返回打开态。
   *
   * Code Logic（这个测试做什么）:
   *   调用 openMobileNav 并断言返回 true。
   */
  test('openMobileNav returns true', () => {
    assertEqual(openMobileNav(), true);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   移动端打开 `/mobile` 时应直接展示项目列表，让用户先选择要操作的项目。
   *
   * Code Logic（这个测试做什么）:
   *   调用默认面板 helper，断言初始面板为 projects。
   */
  test('initial mobile workbench panel defaults to projects', () => {
    assertEqual(getInitialMobileWorkbenchPanel(), 'projects');
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   移动端打开项目列表时不应被默认展开的侧边栏遮挡，用户需要时再主动打开导航。
   *
   * Code Logic（这个测试做什么）:
   *   调用默认导航状态 helper，断言初始状态为关闭。
   */
  test('initial mobile nav open defaults to false', () => {
    assertEqual(getInitialMobileNavOpen(), false);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   用户选择导航项或点击遮罩后需要关闭移动端抽屉，关闭 helper 必须返回关闭态。
   *
   * Code Logic（这个测试做什么）:
   *   调用 closeMobileNav 并断言返回 false。
   */
  test('closeMobileNav returns false', () => {
    assertEqual(closeMobileNav(), false);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   `/mobile` 自动化面板需要支持手机通过本机代理到远端设备，因此本机项目和远端快捷方式都应可进入项目上下文。
   *
   * Code Logic（这个测试做什么）:
   *   构造 local、remote 与未知类型项目，断言 helper 允许已支持项目类型并拒绝未知类型。
   */
  test('canSelectMobileProject allows local and remote projects', () => {
    const localProject = createProject({ id: 'local', name: 'local-app', kind: 'local' });
    const remoteProject = createProject({ id: 'remote', name: 'remote-app', kind: 'remote' });
    const unknownProject = createProject({
      id: 'unknown',
      name: 'unknown-app',
      kind: 'other',
    });

    assertEqual(canSelectMobileProject(localProject), true);
    assertEqual(canSelectMobileProject(remoteProject), true);
    assertEqual(canSelectMobileProject(unknownProject), false);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   移动端 worktree 列表和 Git 面板需要一致区分 clean、dirty、conflict，且冲突必须盖过普通 dirty 状态。
   *
   * Code Logic（这个测试做什么）:
   *   构造 clean/dirty/conflict 样本，断言真实 DTO 的 conflicts 计数大于 0 时优先进入 conflict，clean=false 即使 changed=0 也算 dirty。
   */
  test('mobile worktree status kind prioritizes conflict then dirty', () => {
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
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   移动端 worktree switcher 需要同时支持本机和远端项目，让手机端能切换远端设备项目 worktree。
   *
   * Code Logic（这个测试做什么）:
   *   构造 null、remote、busy local 和 idle local 场景，断言 local/remote 都在非 busy 时可打开。
   */
  test('mobile worktree switcher allows idle local and remote project', () => {
    const localProject = createProject({ id: 'local', name: 'local-app', kind: 'local' });
    const remoteProject = createProject({ id: 'remote', name: 'remote-app', kind: 'remote' });

    assertEqual(canOpenMobileWorktreeSwitcher(null, false), false);
    assertEqual(canOpenMobileWorktreeSwitcher(remoteProject, false), true);
    assertEqual(canOpenMobileWorktreeSwitcher(remoteProject, true), false);
    assertEqual(canOpenMobileWorktreeSwitcher(localProject, true), false);
    assertEqual(canOpenMobileWorktreeSwitcher(localProject, false), true);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   移动端删除、合并等破坏性 worktree 操作不能作用于主工作区，也不能在异步操作占用时重复触发。
   *
   * Code Logic（这个测试做什么）:
   *   构造主 worktree、busy feature 和 idle feature，断言破坏性操作可用性。
   */
  test('mobile worktree destructive action requires idle non-main worktree', () => {
    const main = createWorktree({ id: 'main', name: 'main', isMain: true });
    const feature = createWorktree({ id: 'feature', name: 'feature/mobile', isMain: false });

    assertEqual(canRunMobileWorktreeDestructiveAction(main, false), false);
    assertEqual(canRunMobileWorktreeDestructiveAction(feature, true), false);
    assertEqual(canRunMobileWorktreeDestructiveAction(feature, false), true);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   用户在移动端 Worktrees 面板点击工作区时，期望成功切换后直接进入对应 Workbench 操作现场。
   *
   * Code Logic（这个测试做什么）:
   *   断言选择成功会跳到 terminal 面板；选择被 dirty guard 拦截时保持当前面板不变。
   */
  test('worktree workspace click navigates to terminal only after accepted selection', () => {
    assertEqual(selectMobileWorktreeWorkspacePanel(true), 'terminal');
    assertEqual(selectMobileWorktreeWorkspacePanel(false), null);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   移动端选择项目后应优先进入主工作区，避免默认落到随机 feature worktree。
   *
   * Code Logic（这个测试做什么）:
   *   构造 feature/main 顺序的 worktree 列表，断言 helper 仍返回 isMain 的项目。
   */
  test('preferred worktree uses main before first', () => {
    const feature = createWorktree({ id: 'feature', name: 'feature/login' });
    const main = createWorktree({ id: 'main', name: 'main', isMain: true });

    assertEqual(selectPreferredMobileWorktree([feature, main])?.id ?? null, 'main');
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   没有主工作区标记时，移动端仍应选择首个 worktree 作为可用上下文。
   *
   * Code Logic（这个测试做什么）:
   *   构造无 main 的列表并断言 helper 返回首项；空列表返回 null。
   */
  test('preferred worktree falls back to first or null', () => {
    const feature = createWorktree({ id: 'feature', name: 'feature/login' });

    assertEqual(selectPreferredMobileWorktree([feature])?.id ?? null, 'feature');
    assertEqual(selectPreferredMobileWorktree([]), null);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   移动端切换 active worktree 时应优先显示绑定到该 worktree 的 terminal window。
   *
   * Code Logic（这个测试做什么）:
   *   构造一个 running 但不匹配的 session 和一个匹配 session，断言 helper 优先返回匹配项。
   */
  test('preferred session uses matching worktree before running', () => {
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
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   当前 worktree 可能同时有历史断开的 terminal window 和仍在运行的窗口，移动端应优先恢复可交互的 running 窗口。
   *
   * Code Logic（这个测试做什么）:
   *   构造同一 worktree 下 disconnected/running 两个 session，断言 helper 选择 matching 且 running 的窗口。
   */
  test('preferred session uses running matching before stopped matching', () => {
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
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   当前 worktree 没有 terminal window 时，移动端应优先展示仍在运行的 session。
   *
   * Code Logic（这个测试做什么）:
   *   构造 disconnected/running 顺序的 session 列表，断言 helper 返回 running；空列表返回 null。
   */
  test('preferred session falls back to running first or null', () => {
    const stopped = createSession({ id: 'stopped', name: 'stopped', status: 'disconnected' });
    const running = createSession({ id: 'running', name: 'running', status: 'running' });

    assertEqual(selectPreferredMobileSession([stopped, running], 'main')?.id ?? null, 'running');
    assertEqual(selectPreferredMobileSession([stopped], 'main')?.id ?? null, 'stopped');
    assertEqual(selectPreferredMobileSession([], 'main'), null);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   手机端不再让用户选择左右/上下分屏，新增 pane 要有稳定默认方向来适配竖屏操作。
   *
   * Code Logic（这个测试做什么）:
   *   调用移动端 pane 创建方向 helper，断言默认使用 tmux 上下分割方向。
   */
  test('mobile create pane direction uses down', () => {
    assertEqual(getMobileCreatePaneDirection(), 'down');
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   pane 新增/关闭必须只在当前 terminal window 支持 tmux pane 且没有异步操作占用时开放。
   *
   * Code Logic（这个测试做什么）:
   *   构造支持/不支持 panes 的 session，断言 mutation 可用性受 supportsPanes 与 busy 共同控制。
   */
  test('mobile pane mutation requires supported idle session', () => {
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
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   切换 pane 在单 pane window 内没有可见效果，移动端应避免展示可点击但无意义的操作。
   *
   * Code Logic（这个测试做什么）:
   *   构造单 pane 与多 pane session，断言只有多 pane 且空闲时允许切换。
   */
  test('mobile pane switch requires multiple panes', () => {
    const singlePane = createSession({ id: 'single', name: 'Single', paneCount: 1 });
    const multiPane = createSession({ id: 'multi', name: 'Multi', paneCount: 2 });

    assertEqual(canSwitchMobilePane(singlePane, false), false);
    assertEqual(canSwitchMobilePane(multiPane, false), true);
    assertEqual(canSwitchMobilePane(multiPane, true), false);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   移动端终端全屏后应隐藏项目标题、window tabs 等外围内容，只保留 pane 功能行、终端输出和退出全屏入口。
   *
   * Code Logic（这个测试做什么）:
   *   分别断言普通模式与全屏模式的 chrome 可见性，确保组件渲染不会误保留无关区域。
   */
  test('mobile terminal fullscreen chrome only keeps pane actions and exit', () => {
    const normalChrome = getMobileTerminalChromeVisibility(false);
    const fullscreenChrome = getMobileTerminalChromeVisibility(true);

    assertEqual(normalChrome.panelHeader, true);
    assertEqual(normalChrome.windowTabs, true);
    assertEqual(normalChrome.paneActions, true);
    assertEqual(normalChrome.terminalSurface, true);
    assertEqual(normalChrome.exitFullscreen, false);

    assertEqual(fullscreenChrome.panelHeader, false);
    assertEqual(fullscreenChrome.windowTabs, false);
    assertEqual(fullscreenChrome.paneActions, true);
    assertEqual(fullscreenChrome.terminalSurface, true);
    assertEqual(fullscreenChrome.exitFullscreen, true);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   同项目早退只允许 ready；error/loading 必须允许重试详情。
   *
   * Code Logic（这个测试做什么）:
   *   断言 ready 早退 true；error/loading/idle 为 false。
   */
  test('shouldSkipMobileProjectReload only when same project is ready', () => {
    assertEqual(shouldSkipMobileProjectReload('p1', 'p1', 'ready'), true);
    assertEqual(shouldSkipMobileProjectReload('p1', 'p1', 'error'), false);
    assertEqual(shouldSkipMobileProjectReload('p1', 'p1', 'loading'), false);
    assertEqual(shouldSkipMobileProjectReload('p1', 'p1', 'idle'), false);
    assertEqual(shouldSkipMobileProjectReload('p1', 'p2', 'ready'), false);
    assertEqual(shouldSkipMobileProjectReload(null, 'p1', 'ready'), false);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   连接态切换要保留缓存起点，并在 offline→online 触发可见 panel 刷新。
   *
   * Code Logic（这个测试做什么）:
   *   覆盖 online/reconnecting/offline 转换与 shouldRefresh 判定。
   */
  test('mobile connection state preserves cache and detects reconnect', () => {
    const online = markMobileConnectionOnline(1_000);
    assertEqual(online.kind, 'online');
    assertEqual(getMobileConnectionCachedAt(online), 1_000);

    const reconnecting = markMobileConnectionReconnecting(2, online);
    assertEqual(reconnecting.kind, 'reconnecting');
    if (reconnecting.kind === 'reconnecting') {
      assertEqual(reconnecting.attempt, 2);
      assertEqual(reconnecting.cachedSince, 1_000);
    }

    const offline = markMobileConnectionOffline('timeout', 2_000, reconnecting);
    assertEqual(offline.kind, 'offline');
    if (offline.kind === 'offline') {
      assertEqual(offline.since, 2_000);
      assertEqual(offline.lastError, 'timeout');
    }

    const stillOffline = markMobileConnectionOffline('network', 3_000, offline);
    if (stillOffline.kind === 'offline') {
      assertEqual(stillOffline.since, 2_000);
      assertEqual(stillOffline.lastError, 'network');
    }

    assertEqual(shouldRefreshMobilePanelOnReconnect(offline, markMobileConnectionOnline(4_000)), true);
    assertEqual(shouldRefreshMobilePanelOnReconnect(online, markMobileConnectionOnline(4_000)), false);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   terminalStatus 与 agentRuntime 必须实时反映到已知 session，未知 id 忽略。
   *
   * Code Logic（这个测试做什么）:
   *   seed s1 → status disconnected → agent needsInput。
   */
  test('applies terminalStatus and agentRuntime to the selected mobile session', () => {
    const seeded = seedMobileSessionRuntimeFromSessions(
      [createSession({ id: 's1', name: 'one', status: 'running' })],
      emptyMobileSessionRuntimeState(),
    );
    const afterStatus = reduceMobileSessionRuntime(seeded, {
      kind: 'terminalStatus',
      sessionId: 's1',
      status: 'disconnected',
    });
    const afterAgent = reduceMobileSessionRuntime(afterStatus, {
      kind: 'agentRuntime',
      agentSession: {
        id: 'a1',
        projectId: 'project-1',
        worktreeId: null,
        terminalSessionId: 's1',
        orchestratorTaskId: null,
        orchestratorAttempt: null,
        providerId: 'claudeCodeVisible',
        phase: 'needsInput',
        version: 2,
        startedAt: '2026-07-15T00:00:00.000Z',
        lastActivityAt: '2026-07-15T00:01:00.000Z',
        endedAt: null,
        outcomeCode: null,
        resumedFromAgentSessionId: null,
        isActive: true,
      },
    });
    assertEqual(afterAgent.sessions.s1?.status, 'disconnected');
    assertEqual(afterAgent.sessions.s1?.agent?.phase, 'needsInput');

    // 未知 session 忽略
    const ignored = reduceMobileSessionRuntime(afterAgent, {
      kind: 'terminalStatus',
      sessionId: 'missing',
      status: 'running',
    });
    assertEqual(Object.keys(ignored.sessions).length, 1);
  });
});
