import type { WorkbenchProject, WorkbenchSession, WorkbenchWorktree } from '@/lib/types';
import type { WorkbenchPaneSplitDirection } from '@/api/workbench';

export type MobileWorkbenchPanel =
  | 'projects'
  | 'attention'
  | 'terminal'
  | 'browser'
  | 'files'
  | 'git'
  | 'worktrees'
  | 'prompt'
  | 'automation'
  | 'settings';

const MOBILE_WORKBENCH_PANEL_ORDER: readonly MobileWorkbenchPanel[] = [
  'projects',
  'attention',
  'automation',
  'terminal',
  'browser',
  'files',
  'git',
  'worktrees',
  'prompt',
  'settings',
];

export type MobileWorktreeStatusKind = 'clean' | 'dirty' | 'conflict';

/**
 * 移动端项目详情加载状态机。
 *
 * Business Logic（为什么需要这个类型）:
 *   首次详情失败后同项目必须可重试；ready 才允许同项目早退。
 *
 * Code Logic（联合形态）:
 *   idle=未选；loading=在途；ready=权威就绪；error=失败可重试。
 */
export type MobileProjectDetailStatus = 'idle' | 'loading' | 'ready' | 'error';

/**
 * 移动端连接态（online / reconnecting / offline）。
 *
 * Business Logic（为什么需要这个类型）:
 *   弱网下需展示缓存时间与离线原因，恢复后刷新可见 panel。
 *
 * Code Logic（联合形态）:
 *   online 带 lastSucceededAt；reconnecting 带 attempt 与可选 cachedSince；offline 带 lastError/since。
 */
export type MobileConnectionState =
  | { kind: 'online'; lastSucceededAt: number }
  | { kind: 'reconnecting'; attempt: number; cachedSince: number | null }
  | { kind: 'offline'; lastError: string; since: number };

export interface MobileTerminalChromeVisibility {
  panelHeader: boolean;
  windowTabs: boolean;
  paneActions: boolean;
  terminalSurface: boolean;
  exitFullscreen: boolean;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   同项目早退只允许 ready，避免 error/loading 时点击项目无法重试详情。
 *
 * Code Logic（这个函数做什么）:
 *   active 与 next 同 id 且 status===ready 时返回 true。
 */
export function shouldSkipMobileProjectReload(
  activeProjectId: string | null,
  nextProjectId: string,
  detailStatus: MobileProjectDetailStatus,
): boolean {
  return activeProjectId === nextProjectId && detailStatus === 'ready';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   读成功需要把连接态收敛到 online，供状态栏显示最近成功时间。
 *
 * Code Logic（这个函数做什么）:
 *   返回 { kind:'online', lastSucceededAt: now }。
 */
export function markMobileConnectionOnline(now: number): MobileConnectionState {
  return { kind: 'online', lastSucceededAt: now };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   自动重连过程中要保留最后成功缓存时间，便于展示“缓存于 …”。
 *
 * Code Logic（这个函数做什么）:
 *   从 prev 提取 cachedSince（online→lastSucceededAt；reconnecting→cachedSince；offline→null）。
 */
export function markMobileConnectionReconnecting(
  attempt: number,
  prev: MobileConnectionState | null,
): MobileConnectionState {
  const cachedSince =
    prev?.kind === 'online'
      ? prev.lastSucceededAt
      : prev?.kind === 'reconnecting'
        ? prev.cachedSince
        : null;
  return { kind: 'reconnecting', attempt, cachedSince };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   明确 offline 后状态栏展示错误与 since；若已是 offline 则保留原始 since。
 *
 * Code Logic（这个函数做什么）:
 *   prev 已是 offline 时只更新 lastError；否则写入 since=now。
 */
export function markMobileConnectionOffline(
  lastError: string,
  now: number,
  prev: MobileConnectionState | null,
): MobileConnectionState {
  if (prev?.kind === 'offline') {
    return { kind: 'offline', lastError, since: prev.since };
  }
  return { kind: 'offline', lastError, since: now };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   从 reconnecting/offline 回到 online 时，需要刷新当前可见 panel 的权威数据。
 *
 * Code Logic（这个函数做什么）:
 *   prev 非 online 且 next 为 online 时返回 true。
 */
export function shouldRefreshMobilePanelOnReconnect(
  prev: MobileConnectionState | null,
  next: MobileConnectionState,
): boolean {
  if (next.kind !== 'online') return false;
  if (!prev || prev.kind === 'online') return false;
  return true;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   状态栏需要稳定取缓存起点：online 用 lastSucceededAt，reconnecting 用 cachedSince。
 *
 * Code Logic（这个函数做什么）:
 *   返回可展示的 epoch ms 或 null。
 */
export function getMobileConnectionCachedAt(
  connection: MobileConnectionState | null,
): number | null {
  if (!connection) return null;
  if (connection.kind === 'online') return connection.lastSucceededAt;
  if (connection.kind === 'reconnecting') return connection.cachedSince;
  return null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端打开 `/mobile` 时应先展示最近项目列表，让用户明确选择要操作的项目。
 *
 * Code Logic（这个函数做什么）:
 *   返回移动端 Workbench 的初始面板，当前固定为 projects。
 */
export function getInitialMobileWorkbenchPanel(): MobileWorkbenchPanel {
  return 'projects';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端 Workbench 的 shell 导航、测试和后续面板扩展需要共享同一份项目级面板顺序，避免自动化入口被误放到 worktree 快捷切换器。
 *
 * Code Logic（这个函数做什么）:
 *   返回只读的移动端主面板顺序；attention 固定为 projects 后第二项，automation 为项目级同级面板。
 */
export function getMobileWorkbenchPanelOrder(): readonly MobileWorkbenchPanel[] {
  return MOBILE_WORKBENCH_PANEL_ORDER;
}

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
 *   手机端终端全屏时，用户希望隐藏项目标题、window tabs 和导航等外围内容，专注当前 pane 操作与终端输出。
 *
 * Code Logic（这个函数做什么）:
 *   根据 fullscreen 状态返回终端面板各 chrome 区域的可见性，组件据此决定渲染 header、window tabs、pane 功能行和退出入口。
 */
export function getMobileTerminalChromeVisibility(
  fullscreen: boolean,
): MobileTerminalChromeVisibility {
  return {
    panelHeader: !fullscreen,
    windowTabs: !fullscreen,
    paneActions: true,
    terminalSurface: true,
    exitFullscreen: fullscreen,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端默认展示项目列表内容，导航抽屉不应在首屏遮挡项目卡片。
 *
 * Code Logic（这个函数做什么）:
 *   返回移动端 shell 初始抽屉状态；当前规范为默认关闭。
 */
export function getInitialMobileNavOpen(): boolean {
  return false;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端 Workbench 自动化面板支持本机项目与远端快捷方式；未知项目类型仍不能点入，避免进入未定义链路。
 *
 * Code Logic（这个函数做什么）:
 *   接收 WorkbenchProject DTO，kind 为 local 或 remote 时返回 true，其它项目类型返回 false。
 */
export function canSelectMobileProject(project: WorkbenchProject): boolean {
  return project.kind === 'local' || project.kind === 'remote';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   远端快捷方式在移动端已复用 Workbench 二级代理链路，导航行为需要与本机项目保持一致。
 *
 * Code Logic（这个函数做什么）:
 *   接收当前项目和目标面板；当前 local/remote 都直接返回目标面板，project 参数保留给后续扩展。
 */
export function selectMobilePanelForProject(
  project: WorkbenchProject | null,
  next: MobileWorkbenchPanel,
): MobileWorkbenchPanel {
  void project;
  return next;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端 worktree 列表和 Git 面板需要共享同一套状态分类，让 clean、dirty、conflict 展示保持一致。
 *
 * Code Logic（这个函数做什么）:
 *   先判断真实 DTO 的 conflicts 计数，其次判断 clean 布尔值；返回 conflict、dirty 或 clean 三态。
 */
export function getMobileWorktreeStatusKind(
  worktree: WorkbenchWorktree,
): MobileWorktreeStatusKind {
  if (worktree.status.conflicts > 0) return 'conflict';
  if (!worktree.status.clean) return 'dirty';
  return 'clean';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端 worktree switcher 需要支持本机项目和远端项目，方便手机端切换不同工作区。
 *
 * Code Logic（这个函数做什么）:
 *   project 存在、kind 为 local 或 remote 且 busy 为 false 时返回 true。
 */
export function canOpenMobileWorktreeSwitcher(
  project: WorkbenchProject | null,
  busy: boolean,
): boolean {
  return (project?.kind === 'local' || project?.kind === 'remote') && !busy;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   删除、合并等移动端 worktree 破坏性动作不能作用于主工作区，也不能在已有操作占用时重复触发。
 *
 * Code Logic（这个函数做什么）:
 *   worktree 非主工作区且 busy 为 false 时返回 true。
 */
export function canRunMobileWorktreeDestructiveAction(
  worktree: WorkbenchWorktree,
  busy: boolean,
): boolean {
  return !worktree.isMain && !busy;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用户在移动端 Worktrees 面板点击工作区时，期望成功切换后直接进入对应 Workbench 操作现场。
 *
 * Code Logic（这个函数做什么）:
 *   接收 worktree 选择是否被父级 dirty guard 接受；成功时返回 terminal 面板，失败时返回 null 表示保持当前面板。
 */
export function selectMobileWorktreeWorkspacePanel(
  accepted: boolean,
): MobileWorkbenchPanel | null {
  return accepted ? 'terminal' : null;
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
 *   依次选择 matching worktree 且 running、任意 matching worktree、任意 running、首项；空列表返回 null。
 */
export function selectPreferredMobileSession(
  sessions: WorkbenchSession[],
  activeWorktreeId: string | null,
): WorkbenchSession | null {
  const matchingRunningSession = activeWorktreeId
    ? sessions.find(
        (session) => session.worktreeId === activeWorktreeId && session.status === 'running',
      )
    : undefined;
  const matchingSession = activeWorktreeId
    ? sessions.find((session) => session.worktreeId === activeWorktreeId)
    : undefined;
  return (
    matchingRunningSession ??
    matchingSession ??
    sessions.find((session) => session.status === 'running') ??
    sessions[0] ??
    null
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   手机端不需要暴露左右/上下分屏选择，但新增 pane 仍要映射到真实 tmux split-pane 能力。
 *
 * Code Logic（这个函数做什么）:
 *   返回移动端新增 pane 的固定 split 方向；竖屏默认使用 down，让 pane 在视觉上更符合上下堆叠。
 */
export function getMobileCreatePaneDirection(): WorkbenchPaneSplitDirection {
  return 'down';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端 pane 新增/关闭按钮需要统一判断当前 terminal window 是否支持真实 tmux pane 操作。
 *
 * Code Logic（这个函数做什么）:
 *   接收当前 session 与操作占用态，只有存在支持 panes 的 session 且未 busy 时返回 true。
 */
export function canRunMobilePaneMutation(
  session: WorkbenchSession | null,
  busy: boolean,
): boolean {
  return Boolean(session?.supportsPanes && !busy);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   单 pane window 中切换 pane 没有可见效果，移动端应禁用该操作避免误导用户。
 *
 * Code Logic（这个函数做什么）:
 *   在通用 pane mutation 可用性基础上要求 paneCount 大于 1。
 */
export function canSwitchMobilePane(session: WorkbenchSession | null, busy: boolean): boolean {
  return canRunMobilePaneMutation(session, busy) && (session?.paneCount ?? 0) > 1;
}
