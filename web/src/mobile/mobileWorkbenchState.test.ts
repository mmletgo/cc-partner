import { describe, test } from 'vitest';
import {
  canOpenMobileWorktreeSwitcher,
  canSelectMobileProject,
  canRunMobilePaneMutation,
  canRunMobileWorktreeDestructiveAction,
  canSwitchMobilePane,
  closeMobileNav,
  getMobileWorkbenchPanelOrder,
  getMobileTerminalChromeVisibility,
  getMobileCreatePaneDirection,
  getInitialMobileWorkbenchPanel,
  getInitialMobileNavOpen,
  getMobileWorktreeStatusKind,
  openMobileNav,
  selectMobileWorktreeWorkspacePanel,
  selectMobilePanelForProject,
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
   *   自动化是移动端 Workbench 的项目级同级面板，不能被塞进 worktree quick switch 这类 worktree 附属入口。
   *
   * Code Logic（这个测试做什么）:
   *   调用移动端面板顺序 helper，断言 automation 出现在主导航顺序中，且位于 projects 之后、terminal 之前。
   */
  test('automation panel is a first-class mobile panel between projects and terminal', () => {
    const panels = getMobileWorkbenchPanelOrder();

    assertEqual(panels.includes('automation'), true);
    assertEqual(panels.indexOf('projects') < panels.indexOf('automation'), true);
    assertEqual(panels.indexOf('automation') < panels.indexOf('terminal'), true);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   Browser preview 是移动端 Workbench 的一等面板，必须固定在 terminal 后、files 前，避免后续导航调整破坏功能入口。
   *
   * Code Logic（这个测试做什么）:
   *   读取移动端面板顺序 helper，并逐项断言完整顺序包含 browser。
   */
  test('mobile workbench panel order includes browser preview', () => {
    assertArrayEqual(getMobileWorkbenchPanelOrder(), [
      'projects',
      'automation',
      'terminal',
      'browser',
      'files',
      'git',
      'worktrees',
      'prompt',
      'settings',
    ]);
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
});
