import { describe, test } from 'vitest';
import {
  canOpenMobileWorktreeSwitcher,
  canSelectMobileProject,
  canRunMobilePaneMutation,
  canRunMobileWorktreeDestructiveAction,
  canShowMobileTerminalMergeFab,
  canSwitchMobilePane,
  closeMobileNav,
  computeMobileKeyboardInset,
  computeMobileKeyboardShift,
  computeMobileTerminalMinHeight,
  computeMobileViewportLayoutHints,
  isMobileEditableKeyboardTarget,
  isMobileTerminalTypingTarget,
  resolveAppliedMobileKeyboardShift,
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
  isMobileProjectBoundPanel,
  markMobileConnectionOffline,
  markMobileConnectionOnline,
  markMobileConnectionReconnecting,
  openMobileNav,
  reduceMobileSessionRuntime,
  applyKnownMobileSessionUpdatedEvent,
  resolveMobileNavMode,
  seedMobileSessionRuntimeFromSessions,
  selectMobileWorktreeWorkspacePanel,
  selectMobilePanelForProject,
  selectPreferredMobileSession,
  selectPreferredMobileWorktree,
  pruneMobileSessionsForClosedWorktree,
  selectMobilePanel,
  shouldRefreshMobilePanelOnReconnect,
  shouldShowMobileWorktreeStrip,
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
    canCollectMerge: overrides.canCollectMerge ?? false,
    homeBranch: overrides.homeBranch ?? null,
    collectibleBranches: overrides.collectibleBranches ?? [],
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
   *   远端项目在移动端需要与本机项目一样进入终端、文件、Git、worktree 和自动化面板；
   *   传输为全局入口，无项目也可进入；无项目时项目绑定面板必须回落到 projects。
   *
   * Code Logic（这个测试做什么）:
   *   构造 local/remote/null，断言 selectMobilePanelForProject 与 isMobileProjectBoundPanel。
   */
  test('selectMobilePanelForProject keeps requested panel for remote projects', () => {
    const localProject = createProject({ id: 'local', name: 'local-app', kind: 'local' });
    const remoteProject = createProject({ id: 'remote', name: 'remote-app', kind: 'remote' });

    assertEqual(selectMobilePanelForProject(localProject, 'terminal'), 'terminal');
    assertEqual(selectMobilePanelForProject(remoteProject, 'terminal'), 'terminal');
    assertEqual(selectMobilePanelForProject(remoteProject, 'files'), 'files');
    assertEqual(selectMobilePanelForProject(remoteProject, 'git'), 'git');
    assertEqual(selectMobilePanelForProject(remoteProject, 'worktrees'), 'worktrees');
    assertEqual(selectMobilePanelForProject(remoteProject, 'projects'), 'projects');
    assertEqual(selectMobilePanelForProject(remoteProject, 'settings'), 'settings');
    assertEqual(selectMobilePanelForProject(remoteProject, 'automation'), 'automation');
    assertEqual(selectMobilePanelForProject(null, 'terminal'), 'projects');
    assertEqual(selectMobilePanelForProject(null, 'automation'), 'projects');
    assertEqual(selectMobilePanelForProject(null, 'transfer'), 'transfer');
    assertEqual(selectMobilePanelForProject(null, 'attention'), 'attention');
    assertEqual(isMobileProjectBoundPanel('terminal'), true);
    assertEqual(isMobileProjectBoundPanel('transfer'), false);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   双模式导航：全局壳不暴露项目内工具；项目工作台收纳绑定面板并保留全局快捷。
   *
   * Code Logic（这个测试做什么）:
   *   断言 global/project 分组精确 panel 列表，全集无重复，以及 group 反查。
   */
  test('mobile nav groups map every panel exactly once', () => {
    const globalGroups = getMobileWorkbenchNavGroups('global');
    assertArrayEqual(
      globalGroups.map((group) => group.id),
      ['projects', 'inbox', 'tools', 'system'],
    );
    assertArrayEqual(globalGroups[0]?.panels ?? [], ['projects']);
    assertArrayEqual(globalGroups[1]?.panels ?? [], ['attention']);
    assertArrayEqual(globalGroups[2]?.panels ?? [], ['transfer']);
    assertArrayEqual(globalGroups[3]?.panels ?? [], ['settings', 'provider']);

    const projectGroups = getMobileWorkbenchNavGroups('project');
    assertArrayEqual(
      projectGroups.map((group) => group.id),
      ['work', 'shortcuts'],
    );
    assertArrayEqual(projectGroups[0]?.panels ?? [], [
      'terminal',
      'browser',
      'files',
      'git',
      'worktrees',
      'automation',
    ]);
    assertArrayEqual(
      getMobileWorkbenchNavGroups('project', { automationEnabled: false })[0]?.panels ?? [],
      ['terminal', 'browser', 'files', 'git', 'worktrees'],
    );
    assertArrayEqual(
      getMobileWorkbenchNavGroups('project', { browserEnabled: false })[0]?.panels ?? [],
      ['terminal', 'files', 'git', 'worktrees', 'automation'],
    );
    assertArrayEqual(projectGroups[1]?.panels ?? [], ['attention', 'transfer', 'settings']);

    const flat = getMobileWorkbenchPanelOrder();
    assertArrayEqual(flat, [
      'projects',
      'attention',
      'transfer',
      'settings',
      'provider',
      'terminal',
      'browser',
      'files',
      'git',
      'worktrees',
      'automation',
    ]);
    assertEqual(new Set(flat).size, flat.length);
    assertEqual(getMobileNavGroupIdForPanel('worktrees', 'project'), 'work');
    assertEqual(getMobileNavGroupIdForPanel('git', 'project'), 'work');
    assertEqual(getMobileNavGroupIdForPanel('settings', 'global'), 'system');
    assertEqual(getMobileNavGroupIdForPanel('provider', 'global'), 'system');
    assertEqual(getMobileNavGroupIdForPanel('transfer', 'global'), 'tools');
    assertEqual(getMobileNavGroupIdForPanel('transfer', 'project'), 'shortcuts');
    assertEqual(flat.filter((panel) => panel === 'transfer').length, 1);
    assertEqual(getInitialMobileWorkbenchPanel(), 'projects');
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   自动化是项目工作台内一等面板，不能塞进 worktree quick switch 附属入口。
   *
   * Code Logic（这个测试做什么）:
   *   断言 automation 在 project 模式 work 组，且全集顺序中位于 git 之后。
   */
  test('automation panel is a first-class mobile panel after the work group', () => {
    const panels = getMobileWorkbenchPanelOrder();

    assertEqual(panels.includes('automation'), true);
    assertEqual(panels.indexOf('worktrees') < panels.indexOf('automation'), true);
    assertEqual(getMobileNavGroupIdForPanel('automation', 'project'), 'work');
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   全局 Inbox 在全局壳中独立成组；默认首屏仍是 projects。
   *
   * Code Logic（这个测试做什么）:
   *   断言初始面板为 projects，global 第二组为 inbox/attention。
   */
  test('attention is second nav group and never the default panel', () => {
    const groups = getMobileWorkbenchNavGroups('global');

    assertEqual(getInitialMobileWorkbenchPanel(), 'projects');
    assertEqual(groups[1]?.id, 'inbox');
    assertArrayEqual(groups[1]?.panels ?? [], ['attention']);
    assertEqual(getMobileWorkbenchPanelOrder().includes('attention'), true);
    assertEqual(getInitialMobileWorkbenchPanel() === 'attention', false);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   Browser preview 是项目工作台一等面板，固定在 terminal 后、files 前。
   *
   * Code Logic（这个测试做什么）:
   *   读取 project 模式 work 组顺序，断言 browser 位置。
   */
  test('mobile workbench panel order includes browser preview in work group', () => {
    const workPanels = getMobileWorkbenchNavGroups('project')[0]?.panels ?? [];
    assertEqual(workPanels.indexOf('terminal') + 1, workPanels.indexOf('browser'));
    assertEqual(workPanels.indexOf('browser') + 1, workPanels.indexOf('files'));
    assertEqual(getMobileNavGroupIdForPanel('browser', 'project'), 'work');
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   有 active project 时项目绑定面板进入 project 模式；projects/provider 与无项目保持 global。
   *
   * Code Logic（这个测试做什么）:
   *   枚举 resolveMobileNavMode 关键组合。
   */
  test('resolveMobileNavMode switches between global shell and project workbench', () => {
    assertEqual(resolveMobileNavMode('projects', false), 'global');
    assertEqual(resolveMobileNavMode('terminal', false), 'global');
    assertEqual(resolveMobileNavMode('terminal', true), 'project');
    assertEqual(resolveMobileNavMode('attention', true), 'project');
    assertEqual(resolveMobileNavMode('transfer', true), 'project');
    assertEqual(resolveMobileNavMode('settings', true), 'project');
    assertEqual(resolveMobileNavMode('projects', true), 'global');
    assertEqual(resolveMobileNavMode('provider', true), 'global');
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
    // shell 保持全屏高度（不压缩）：keyboardInset 只描述键盘占用，实际上移量由 shift helper 决定。
    // shellHeight 始终 = layoutViewportHeight，terminal 大小不变。
    assertEqual(portrait.shellHeight, 844);
    assertEqual(portrait.landscape, false);
    // shell 全屏，terminalMinHeight 基于 layoutViewportHeight（844）。
    assertEqual(portrait.terminalMinHeight, computeMobileTerminalMinHeight(390, 844, 0));
    assertEqual(portrait.terminalMinHeight, Math.max(160, Math.round(844 * 0.48)));

    // 非键盘状态：vvHeight = layoutHeight，keyboardInset = 0，shell 不上移、不压缩。
    const idle = computeMobileViewportLayoutHints(390, 844, 844, 0);
    assertEqual(idle.shellHeight, 844);
    assertEqual(idle.keyboardInset, 0);

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
   *   终端输入必须整页让出键盘；其它输入不能一律顶满，否则顶部字段会被顶出屏幕。
   *
   * Code Logic（这个测试做什么）:
   *   覆盖 full/focused 模式、顶部原位、居中上移、键盘遮挡最小抬升、无焦点保持 previousShift。
   */
  test('keyboard shift keeps terminal full-lift and centers other focused inputs', () => {
    const layoutHeight = 844;
    const inset = 344;

    assertEqual(
      computeMobileKeyboardShift({
        keyboardInset: inset,
        layoutViewportHeight: layoutHeight,
        focusTop: 40,
        focusHeight: 36,
        mode: 'full',
        previousShift: 0,
      }),
      inset,
    );
    assertEqual(
      computeMobileKeyboardShift({
        keyboardInset: 0,
        layoutViewportHeight: layoutHeight,
        focusTop: 700,
        focusHeight: 36,
        mode: 'full',
        previousShift: 120,
      }),
      0,
    );

    // 原始位置在未遮挡区域上半：不上移。
    assertEqual(
      computeMobileKeyboardShift({
        keyboardInset: inset,
        layoutViewportHeight: layoutHeight,
        focusTop: 48,
        focusHeight: 36,
        mode: 'focused',
        previousShift: 0,
      }),
      0,
    );

    // 焦点中心对准可视区中线：420+20-250=190。
    assertEqual(
      computeMobileKeyboardShift({
        keyboardInset: inset,
        layoutViewportHeight: layoutHeight,
        focusTop: 420,
        focusHeight: 40,
        mode: 'focused',
        previousShift: 0,
      }),
      190,
    );

    // 底部字段：居中需要 550，封顶为键盘高度 344。
    assertEqual(
      computeMobileKeyboardShift({
        keyboardInset: inset,
        layoutViewportHeight: layoutHeight,
        focusTop: 780,
        focusHeight: 40,
        mode: 'focused',
        previousShift: 0,
      }),
      344,
    );

    // 无焦点时保持上次上移，直到键盘收起。
    assertEqual(
      computeMobileKeyboardShift({
        keyboardInset: inset,
        layoutViewportHeight: layoutHeight,
        focusTop: null,
        focusHeight: 0,
        mode: 'focused',
        previousShift: 180,
      }),
      180,
    );

    assertEqual(isMobileTerminalTypingTarget(null), false);
    assertEqual(
      isMobileTerminalTypingTarget({
        classList: { contains: (token: string) => token === 'xterm-helper-textarea' },
      }),
      true,
    );
    assertEqual(
      isMobileTerminalTypingTarget({
        classList: { contains: () => false },
        closest: (selector: string) => (selector === '.xterm-helper-textarea' ? {} : null),
      }),
      true,
    );
    assertEqual(
      isMobileEditableKeyboardTarget({ tagName: 'TEXTAREA' }),
      true,
    );
    assertEqual(
      isMobileEditableKeyboardTarget({ tagName: 'BUTTON' }),
      false,
    );
    assertEqual(
      resolveAppliedMobileKeyboardShift({
        dialogTransform: 'translateY(-180px)',
        insideShell: false,
        shellShift: 344,
      }),
      180,
    );
    assertEqual(
      resolveAppliedMobileKeyboardShift({
        dialogTransform: null,
        insideShell: true,
        shellShift: 344,
      }),
      344,
    );
    assertEqual(
      resolveAppliedMobileKeyboardShift({
        dialogTransform: null,
        insideShell: false,
        shellShift: 344,
      }),
      0,
    );
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
   *   终端右下角合并 FAB 只应在可合并场景出现：非主工作区、非主分支，或主工作区可 collect-merge。
   *
   * Code Logic（这个测试做什么）:
   *   覆盖 null、主工作区主分支、主工作区非主分支、主工作区 canCollectMerge、功能 worktree。
   */
  test('terminal merge FAB appears on non-main worktree or non-home branch', () => {
    const mainHome = createWorktree({
      id: 'main',
      name: 'main',
      isMain: true,
      branch: 'main',
      homeBranch: 'main',
    });
    const mainFeatureBranch = createWorktree({
      id: 'main',
      name: 'main',
      isMain: true,
      branch: 'feature/local',
      homeBranch: 'main',
    });
    const mainCollect = createWorktree({
      id: 'main',
      name: 'main',
      isMain: true,
      branch: 'main',
      homeBranch: 'main',
      canCollectMerge: true,
    });
    const feature = createWorktree({ id: 'feature', name: 'feature/mobile', isMain: false });

    assertEqual(canShowMobileTerminalMergeFab(null), false);
    assertEqual(canShowMobileTerminalMergeFab(mainHome), false);
    assertEqual(canShowMobileTerminalMergeFab(mainFeatureBranch), true);
    assertEqual(canShowMobileTerminalMergeFab(mainCollect), true);
    assertEqual(canShowMobileTerminalMergeFab(feature), true);
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
   *   merge 关闭源 worktree 终端后，切到 main 前必须摘掉已关闭 session，避免回落到僵尸窗口。
   *
   * Code Logic（这个测试做什么）:
   *   功能 worktree running session + main disconnected session；prune 后断言只剩 main，且 preferred 不再是功能窗口。
   */
  test('pruneMobileSessionsForClosedWorktree drops source sessions before preferred select', () => {
    const featureRunning = createSession({
      id: 'feature-running',
      name: 'feature',
      worktreeId: 'wt-feature',
      status: 'running',
    });
    const mainStopped = createSession({
      id: 'main-stopped',
      name: 'main',
      worktreeId: 'main',
      status: 'disconnected',
    });

    const remaining = pruneMobileSessionsForClosedWorktree(
      [featureRunning, mainStopped],
      'wt-feature',
    );
    assertEqual(remaining.length, 1);
    assertEqual(remaining[0]?.id ?? null, 'main-stopped');
    // 主工作区尚无窗口时，未 prune 会回落到已关闭的功能 worktree running 窗口。
    assertEqual(
      selectPreferredMobileSession([featureRunning], 'main')?.id ?? null,
      'feature-running',
    );
    assertEqual(selectPreferredMobileSession(remaining, 'main')?.id ?? null, 'main-stopped');
    assertEqual(selectPreferredMobileSession(
      pruneMobileSessionsForClosedWorktree([featureRunning], 'wt-feature'),
      'main',
    ), null);
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

    assertEqual(normalChrome.windowTabs, true);
    assertEqual(normalChrome.paneActions, true);
    assertEqual(normalChrome.terminalSurface, true);
    assertEqual(normalChrome.exitFullscreen, false);
    assertEqual(normalChrome.worktreeStrip, true);

    assertEqual(fullscreenChrome.windowTabs, false);
    assertEqual(fullscreenChrome.paneActions, true);
    assertEqual(fullscreenChrome.terminalSurface, true);
    assertEqual(fullscreenChrome.exitFullscreen, true);
    assertEqual(fullscreenChrome.worktreeStrip, false);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   worktree 条必须出现在 terminal/files/browser/git，不能只挂在终端面板。
   *
   * Code Logic（这个测试做什么）:
   *   断言工作区四面板 true，Worktrees/自动化/全局面板 false。
   */
  test('shouldShowMobileWorktreeStrip covers non-terminal workspace panels', () => {
    assertEqual(shouldShowMobileWorktreeStrip('terminal'), false);
    assertEqual(shouldShowMobileWorktreeStrip('files'), true);
    assertEqual(shouldShowMobileWorktreeStrip('browser'), true);
    assertEqual(shouldShowMobileWorktreeStrip('git'), true);
    assertEqual(shouldShowMobileWorktreeStrip('worktrees'), false);
    assertEqual(shouldShowMobileWorktreeStrip('automation'), false);
    assertEqual(shouldShowMobileWorktreeStrip('projects'), false);
    assertEqual(shouldShowMobileWorktreeStrip('settings'), false);
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

  /**
   * Business Logic（为什么需要这个测试）:
   *   同一 terminal 启动新 Agent 时 version 会从 1 重置；旧 Agent 的高版本不能让移动端
   *   永久停在已停止状态。
   *
   * Code Logic（这个测试做什么）:
   *   先写入 version=8 的旧 Agent，再写入不同 id、version=1 的 needsInput，断言新投影生效。
   */
  test('accepts a new agent with reset version on the same mobile terminal', () => {
    const seeded = seedMobileSessionRuntimeFromSessions(
      [createSession({ id: 's1', name: 'one', status: 'running' })],
      emptyMobileSessionRuntimeState(),
    );
    const oldAgent = reduceMobileSessionRuntime(seeded, {
      kind: 'agentRuntime',
      agentSession: {
        id: 'agent-old',
        projectId: 'project-1',
        worktreeId: null,
        terminalSessionId: 's1',
        orchestratorTaskId: null,
        orchestratorAttempt: null,
        providerId: 'claudeCodeVisible',
        phase: 'disconnected',
        version: 8,
        startedAt: '2026-07-15T00:00:00.000Z',
        lastActivityAt: '2026-07-15T00:01:00.000Z',
        endedAt: '2026-07-15T00:01:00.000Z',
        outcomeCode: 'provider_session_exited',
        resumedFromAgentSessionId: null,
        isActive: false,
      },
    });
    const newAgent = reduceMobileSessionRuntime(oldAgent, {
      kind: 'agentRuntime',
      agentSession: {
        id: 'agent-new',
        projectId: 'project-1',
        worktreeId: null,
        terminalSessionId: 's1',
        orchestratorTaskId: null,
        orchestratorAttempt: null,
        providerId: 'claudeCodeVisible',
        phase: 'needsInput',
        version: 1,
        startedAt: '2026-07-15T00:02:00.000Z',
        lastActivityAt: '2026-07-15T00:02:01.000Z',
        endedAt: null,
        outcomeCode: null,
        resumedFromAgentSessionId: null,
        isActive: true,
      },
    });

    assertEqual(newAgent.sessions.s1?.agent?.id, 'agent-new');
    assertEqual(newAgent.sessions.s1?.agent?.phase, 'needsInput');
    assertEqual(newAgent.sessions.s1?.agent?.version, 1);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   Mobile 收到标题更新后只应修改当前页面已知 session，并同步 activeSession；
   *   其他项目或尚未加载的未知 session 事件必须 fail-closed。
   *
   * Code Logic（这个测试做什么）:
   *   对已知 active session 应用完整 DTO，断言列表和 active 引用都获得新标题；
   *   再对未知 id 应用事件，断言状态引用和值完全不变。
   */
  test('merges known sessionUpdated into sessions and activeSession but ignores unknown ids', () => {
    const known = createSession({ id: 's1', name: '旧标题', status: 'running' });
    const sibling = createSession({ id: 's2', name: '另一个会话', status: 'running' });
    const updated: WorkbenchSession = {
      ...known,
      name: '自动标题',
      nameSource: 'auto',
      paneCount: 2,
    };

    const applied = applyKnownMobileSessionUpdatedEvent([known, sibling], known, updated);
    assertEqual(applied.applied, true);
    assertEqual(applied.sessions[0]?.name, '自动标题');
    assertEqual(applied.sessions[0]?.nameSource, 'auto');
    assertEqual(applied.sessions[0]?.paneCount, 2);
    assertEqual(applied.sessions[1], sibling);
    assertEqual(applied.activeSession, applied.sessions[0] ?? null);

    const missing = createSession({ id: 'missing', name: '不得加入' });
    const ignored = applyKnownMobileSessionUpdatedEvent(
      applied.sessions,
      applied.activeSession,
      missing,
    );
    assertEqual(ignored.applied, false);
    assertEqual(ignored.sessions, applied.sessions);
    assertEqual(ignored.activeSession, applied.activeSession);
    assertEqual(ignored.sessions.length, 2);
  });
});
