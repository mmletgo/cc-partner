import type { WorkbenchProject, WorkbenchSession, WorkbenchSessionStatus, WorkbenchWorktree } from '@/lib/types';
import type { WorkbenchPaneSplitDirection } from '@/api/workbench';
import type { AgentPhase, AgentSessionProjection } from '@/lib/types/agentRuntime';
import { toAgentSessionProjection } from '@/lib/agentRuntimeState';
import type { AgentSessionRuntimeDto } from '@/lib/types/agentRuntime';

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
  | 'settings'
  | 'provider';

/**
 * 移动端主导航任务分组 id。
 *
 * Business Logic（为什么需要这个类型）:
 *   十个扁平 panel 增加导航负担；按任务分组后仍映射既有 MobileWorkbenchPanel，不引入第二套路由。
 *
 * Code Logic（联合形态）:
 *   projects/attention/work/automation/more 五个稳定分组 id。
 */
export type MobileWorkbenchNavGroupId =
  | 'projects'
  | 'attention'
  | 'work'
  | 'automation'
  | 'more';

/**
 * 移动端主导航分组。
 *
 * Business Logic（为什么需要这个接口）:
 *   Drawer/rail 需要按组渲染 section + 组内 panel 入口，并保证每个 panel 恰好出现一次。
 *
 * Code Logic（字段说明）:
 *   id 为分组；panels 为该组包含的 MobileWorkbenchPanel 只读列表。
 */
export interface MobileWorkbenchNavGroup {
  id: MobileWorkbenchNavGroupId;
  panels: readonly MobileWorkbenchPanel[];
}

/**
 * 软键盘/viewport 派生的 shell 尺寸提示。
 *
 * Business Logic（为什么需要这个接口）:
 *   软键盘弹出时 visualViewport 变矮，shell 与终端需压缩高度并保留顶部菜单可见。
 *
 * Code Logic（字段说明）:
 *   shellHeight/keyboardInset 为 CSS 像素；landscape 表示宽>高。
 */
export interface MobileViewportLayoutHints {
  shellHeight: number;
  keyboardInset: number;
  landscape: boolean;
  terminalMinHeight: number;
}

/** 设计合同：Projects / Attention / Work / Automation / More 映射，每个 panel 恰好一次。 */
const MOBILE_WORKBENCH_NAV_GROUPS: readonly MobileWorkbenchNavGroup[] = [
  { id: 'projects', panels: ['projects', 'worktrees'] },
  { id: 'attention', panels: ['attention'] },
  { id: 'work', panels: ['terminal', 'browser', 'files', 'git', 'prompt'] },
  { id: 'automation', panels: ['automation'] },
  { id: 'more', panels: ['settings', 'provider'] },
];

/** 由分组扁平化得到的 panel 顺序，保证与分组合同一致。 */
const MOBILE_WORKBENCH_PANEL_ORDER: readonly MobileWorkbenchPanel[] =
  MOBILE_WORKBENCH_NAV_GROUPS.flatMap((group) => group.panels);

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
 *   返回由导航分组扁平化得到的只读主面板顺序；每个 panel 恰好出现一次。
 */
export function getMobileWorkbenchPanelOrder(): readonly MobileWorkbenchPanel[] {
  return MOBILE_WORKBENCH_PANEL_ORDER;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Drawer/rail 需要按任务分组渲染导航，降低十个扁平入口的扫描成本。
 *
 * Code Logic（这个函数做什么）:
 *   返回固定的 Projects/Attention/Work/Automation/More 分组合同（只读）。
 */
export function getMobileWorkbenchNavGroups(): readonly MobileWorkbenchNavGroup[] {
  return MOBILE_WORKBENCH_NAV_GROUPS;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   选中 panel 时要高亮所属分组，且深链/测试需要从 panel 反查 group。
 *
 * Code Logic（这个函数做什么）:
 *   遍历分组表，返回包含该 panel 的 group id；未命中时抛错（合同完整性守卫）。
 */
export function getMobileNavGroupIdForPanel(
  panel: MobileWorkbenchPanel,
): MobileWorkbenchNavGroupId {
  for (const group of MOBILE_WORKBENCH_NAV_GROUPS) {
    if (group.panels.includes(panel)) {
      return group.id;
    }
  }
  throw new Error(`Panel ${panel} is not mapped to any mobile nav group`);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   软键盘占用 visualViewport 下方时，shell 需要知道 inset，避免菜单被顶走或覆盖终端输入。
 *
 * Code Logic（这个函数做什么）:
 *   keyboardInset = max(0, layoutViewportHeight - visualViewportHeight - visualViewportOffsetTop)。
 */
export function computeMobileKeyboardInset(
  layoutViewportHeight: number,
  visualViewportHeight: number,
  visualViewportOffsetTop: number,
): number {
  const inset =
    layoutViewportHeight - visualViewportHeight - Math.max(0, visualViewportOffsetTop);
  return Math.max(0, Math.round(inset));
}

/**
 * Business Logic（为什么需要这个函数）:
 *   横屏时终端应优先占可视高度；竖屏保留常规比例；软键盘时 shell 高度已是 visualViewport，
 *   不得再二次扣减 keyboardInset。
 *
 * Code Logic（这个函数做什么）:
 *   基于 availableHeight（调用方传入的已是可见高度）与 landscape 计算 terminalMinHeight。
 *   keyboardInset 参数保留兼容；仅在 availableHeight 仍为 layout 高度时由调用方扣减。
 */
export function computeMobileTerminalMinHeight(
  viewportWidth: number,
  viewportHeight: number,
  keyboardInset: number,
): number {
  const available = Math.max(0, viewportHeight - Math.max(0, keyboardInset));
  const landscape = viewportWidth > viewportHeight;
  const ratio = landscape ? 0.72 : 0.48;
  return Math.max(160, Math.round(available * ratio));
}

/**
 * Business Logic（为什么需要这个函数）:
 *   shell CSS 变量需要一次计算 keyboard inset、shell 高度与终端优先高度，避免组件内散落公式。
 *
 * Code Logic（这个函数做什么）:
 *   shellHeight = min(layoutViewportHeight, visualViewportHeight)：layout 与 visual 取较小值，
 *   兼容 Android Chrome 两种键盘模式（interactive-widget=resizes-content 缩 layout、默认 resizes-visual
 *   缩 visual），任一缩小都能让 shell 压缩到键盘上方；keyboardInset 仅作 data-keyboard-open 检测，
 *   不从 shellHeight 扣减；terminalMinHeight 基于 shellHeight 直接计算。
 */
export function computeMobileViewportLayoutHints(
  layoutViewportWidth: number,
  layoutViewportHeight: number,
  visualViewportHeight: number | null,
  visualViewportOffsetTop: number,
): MobileViewportLayoutHints {
  const vvHeight =
    visualViewportHeight != null && Number.isFinite(visualViewportHeight)
      ? visualViewportHeight
      : layoutViewportHeight;
  const keyboardInset = computeMobileKeyboardInset(
    layoutViewportHeight,
    vvHeight,
    visualViewportOffsetTop,
  );
  // layout 与 visual 取较小值：兼容 resizes-content（layout 缩）与 resizes-visual（visual 缩）。
  const shellHeight = Math.max(0, Math.round(Math.min(layoutViewportHeight, vvHeight)));
  const landscape = layoutViewportWidth > layoutViewportHeight;
  // shellHeight 已排除键盘占用，terminalMinHeight 不得二次扣 inset。
  const terminalMinHeight = computeMobileTerminalMinHeight(
    layoutViewportWidth,
    shellHeight,
    0,
  );
  return { shellHeight, keyboardInset, landscape, terminalMinHeight };
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

/**
 * 移动端单 session 的运行时投影（terminal status + Agent）。
 *
 * Business Logic（为什么需要这个类型）:
 *   HTTP terminalStatus/agentRuntime 需合并进当前项目会话，且 Agent 不得写回 WorkbenchSession DTO。
 *
 * Code Logic（字段说明）:
 *   status 为 terminal 生命周期；agent 为可选 projection。
 */
export interface MobileSessionRuntimeEntry {
  status: WorkbenchSessionStatus;
  agent: AgentSessionProjection | null;
}

/**
 * 移动端 session 运行时聚合（按 sessionId）。
 *
 * Business Logic（为什么需要这个类型）:
 *   pure reducer 测试与 MobileWorkbench 共用同一形状。
 *
 * Code Logic（字段说明）:
 *   sessions 为 Record；knownSessionIds 限制只更新已知会话。
 */
export interface MobileSessionRuntimeState {
  sessions: Record<string, MobileSessionRuntimeEntry>;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   测试与初始化需要确定性空态。
 *
 * Code Logic（这个函数做什么）:
 *   返回 sessions={}。
 */
export function emptyMobileSessionRuntimeState(): MobileSessionRuntimeState {
  return { sessions: {} };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   从当前 list sessions 播种运行时条目，保留已有 agent 投影。
 *
 * Code Logic（这个函数做什么）:
 *   以 session list 为权威重建 keys；保留旧 agent。
 */
export function seedMobileSessionRuntimeFromSessions(
  sessions: WorkbenchSession[],
  prev: MobileSessionRuntimeState,
): MobileSessionRuntimeState {
  const next: Record<string, MobileSessionRuntimeEntry> = {};
  for (const session of sessions) {
    const previous = prev.sessions[session.id];
    next[session.id] = {
      status: session.status,
      agent: previous?.agent ?? null,
    };
  }
  return { sessions: next };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   terminalStatus 事件应更新已知 session 的 status，未知 id 忽略。
 *
 * Code Logic（这个函数做什么）:
 *   若 sessions[id] 存在则更新 status；否则返回原 state。
 */
export function applyMobileTerminalStatusEvent(
  state: MobileSessionRuntimeState,
  sessionId: string,
  status: WorkbenchSessionStatus,
): MobileSessionRuntimeState {
  const existing = state.sessions[sessionId];
  if (!existing) return state;
  if (existing.status === status) return state;
  return {
    sessions: {
      ...state.sessions,
      [sessionId]: { ...existing, status },
    },
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   agentRuntime 事件更新已知 session 的 Agent phase；version 乱序拒绝。
 *
 * Code Logic（这个函数做什么）:
 *   映射 DTO→projection；仅当 version 更大或无 agent 时写入。
 */
export function applyMobileAgentRuntimeEvent(
  state: MobileSessionRuntimeState,
  dto: AgentSessionRuntimeDto,
  freshness: AgentSessionProjection['freshness'] = 'live',
): MobileSessionRuntimeState {
  const sessionId = dto.terminalSessionId;
  const existing = state.sessions[sessionId];
  if (!existing) return state;
  const incoming = toAgentSessionProjection(dto, freshness);
  if (existing.agent && existing.agent.version >= incoming.version) {
    return state;
  }
  return {
    sessions: {
      ...state.sessions,
      [sessionId]: { ...existing, agent: incoming },
    },
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   测试与页面可用统一 reduce 入口消费 terminalStatus/agentRuntime。
 *
 * Code Logic（这个函数做什么）:
 *   按 kind 分发到 apply* 函数。
 */
export type MobileSessionRuntimeEvent =
  | { kind: 'terminalStatus'; sessionId: string; status: WorkbenchSessionStatus }
  | { kind: 'agentRuntime'; agentSession: AgentSessionRuntimeDto };

/**
 * Business Logic（为什么需要这个函数）:
 *   pure reducer 便于单测 terminalStatus + agent 合并。
 *
 * Code Logic（这个函数做什么）:
 *   switch event.kind 调用对应 apply。
 */
export function reduceMobileSessionRuntime(
  state: MobileSessionRuntimeState,
  event: MobileSessionRuntimeEvent,
): MobileSessionRuntimeState {
  if (event.kind === 'terminalStatus') {
    return applyMobileTerminalStatusEvent(state, event.sessionId, event.status);
  }
  return applyMobileAgentRuntimeEvent(state, event.agentSession);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   将运行时 status 合并回 WorkbenchSession 列表供 tab 展示。
 *
 * Code Logic（这个函数做什么）:
 *   若 runtime 有该 id 则覆盖 status。
 */
export function mergeMobileSessionsWithRuntime(
  sessions: WorkbenchSession[],
  runtime: MobileSessionRuntimeState,
): WorkbenchSession[] {
  return sessions.map((session) => {
    const entry = runtime.sessions[session.id];
    if (!entry || entry.status === session.status) return session;
    return { ...session, status: entry.status };
  });
}

/**
 * Business Logic（为什么需要这个函数）:
 *   取某 session 的 Agent phase 供 MobileTerminalPanel 展示。
 *
 * Code Logic（这个函数做什么）:
 *   返回 agent 或 null。
 */
export function mobileAgentForSession(
  runtime: MobileSessionRuntimeState,
  sessionId: string,
): AgentSessionProjection | null {
  return runtime.sessions[sessionId]?.agent ?? null;
}

/** 导出 AgentPhase 供测试字面量（避免循环 import 噪音）。 */
export type { AgentPhase };
