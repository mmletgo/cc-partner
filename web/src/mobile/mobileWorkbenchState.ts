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
  | 'transfer'
  | 'automation'
  | 'settings'
  | 'provider';

/**
 * 移动端主导航模式。
 *
 * Business Logic（为什么需要这个类型）:
 *   项目绑定入口（终端/文件/Git 等）应像桌面一样收进「某项目工作台」；
 *   无项目时顶层只保留全局入口，进入项目后再切换为项目内导航。
 *
 * Code Logic（联合形态）:
 *   global=项目列表与全局工具；project=当前项目工作台 + 全局快捷入口。
 */
export type MobileWorkbenchNavMode = 'global' | 'project';

/**
 * 移动端主导航任务分组 id。
 *
 * Business Logic（为什么需要这个类型）:
 *   双模式导航按任务分组渲染；global 与 project 使用不同分组集合。
 *
 * Code Logic（联合形态）:
 *   global: projects/inbox/tools/system；project: work/shortcuts。
 */
export type MobileWorkbenchNavGroupId =
  | 'projects'
  | 'inbox'
  | 'tools'
  | 'system'
  | 'work'
  | 'shortcuts';

/**
 * 移动端主导航分组。
 *
 * Business Logic（为什么需要这个接口）:
 *   Drawer/rail 需要按组渲染 section + 组内 panel 入口。
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
 *   软键盘弹出时 visualViewport 变矮；shell 保持全屏高度，由 shift 决定是否上移让出键盘。
 *
 * Code Logic（字段说明）:
 *   shellHeight/keyboardInset 为 CSS 像素；landscape 表示宽>高。
 *   keyboardInset 是键盘占用高度，不等于页面上移量；上移量由 computeMobileKeyboardShift 另算。
 */
export interface MobileViewportLayoutHints {
  shellHeight: number;
  keyboardInset: number;
  landscape: boolean;
  terminalMinHeight: number;
}

/**
 * 移动端软键盘页面上移模式。
 *
 * Business Logic（为什么需要这个类型）:
 *   终端输入不能一律顶满：只有点击位置顶页后仍可见才整页让出键盘；其它输入只需要把焦点
 *   放到未遮挡可视区。
 *
 * Code Logic（联合形态）:
 *   full=点击锚点顶页后仍可见才按键盘高度上移，否则 overlay；focused=按焦点几何把输入放到
 *   可视区纵向中线附近。
 */
export type MobileKeyboardShiftMode = 'full' | 'focused';

/** 终端点击进入输入态时，把未上移坐标系的锚点写在 helper textarea 上。 */
export const MOBILE_KEYBOARD_ANCHOR_TOP_ATTR = 'data-mobile-keyboard-anchor-top';

/** 点击锚点变化后通知 shell 重算 shift；键盘已打开时没有 resize/focusin。 */
export const MOBILE_KEYBOARD_ANCHOR_CHANGE_EVENT = 'cp-mobile-keyboard-anchor-change';

/**
 * 计算软键盘上移量所需的焦点几何。
 *
 * Business Logic（为什么需要这个接口）:
 *   非终端输入要把字段尽量放进未遮挡区域的纵向中间，几何必须可单测、不绑 DOM。
 *
 * Code Logic（字段说明）:
 *   focusTop 是未上移坐标系中焦点顶边（layout viewport）；null 表示当前没有可编辑焦点。
 */
export interface MobileKeyboardShiftInput {
  keyboardInset: number;
  layoutViewportHeight: number;
  focusTop: number | null;
  focusHeight: number;
  mode: MobileKeyboardShiftMode;
  previousShift: number;
}

/** 必须绑定 active project 才有意义的面板（进入项目工作台后才出现在主导航）。 */
const MOBILE_PROJECT_BOUND_PANELS: readonly MobileWorkbenchPanel[] = [
  'terminal',
  'browser',
  'files',
  'git',
  'worktrees',
  'automation',
];

/** 全局模式：与桌面侧栏一致，不暴露项目内工具。 */
const MOBILE_GLOBAL_NAV_GROUPS: readonly MobileWorkbenchNavGroup[] = [
  { id: 'projects', panels: ['projects'] },
  { id: 'inbox', panels: ['attention'] },
  { id: 'tools', panels: ['transfer'] },
  { id: 'system', panels: ['settings', 'provider'] },
];

/**
 * 项目模式：项目内工具 + 底部全局快捷（待处理/传输/设置）。
 * Provider 仅全局模式可达，避免项目工作台噪音。
 * Prompt 优化不进主导航，只留在终端内浮层。
 */
const MOBILE_PROJECT_NAV_GROUPS: readonly MobileWorkbenchNavGroup[] = [
  {
    id: 'work',
    panels: ['terminal', 'browser', 'files', 'git', 'worktrees', 'automation'],
  },
  { id: 'shortcuts', panels: ['attention', 'transfer', 'settings'] },
];

/** 全量 panel 枚举（去重后），供测试与「存在性」合同使用。 */
const MOBILE_WORKBENCH_PANEL_ORDER: readonly MobileWorkbenchPanel[] = [
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
  windowTabs: boolean;
  paneActions: boolean;
  terminalSurface: boolean;
  exitFullscreen: boolean;
  /**
   * 终端面板内、窗口 tab 上方的 worktree 条。
   * 全屏 overlay 覆盖 shell，必须在面板内自行隐藏。
   */
  worktreeStrip: boolean;
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
 *   终端/文件/Git 等入口必须绑定具体项目；导航与深链需要统一判定。
 *
 * Code Logic（这个函数做什么）:
 *   panel 属于 MOBILE_PROJECT_BOUND_PANELS 时返回 true。
 */
export function isMobileProjectBoundPanel(panel: MobileWorkbenchPanel): boolean {
  return (MOBILE_PROJECT_BOUND_PANELS as readonly string[]).includes(panel);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Drawer/rail 在「全局壳」与「项目工作台」之间切换展示，需要稳定可测的 mode 解析。
 *
 * Code Logic（这个函数做什么）:
 *   无 active project → global；panel 为 projects/provider → global；
 *   其余（含 attention/transfer/settings 与项目绑定面板）在有项目时 → project。
 */
export function resolveMobileNavMode(
  panel: MobileWorkbenchPanel,
  hasActiveProject: boolean,
): MobileWorkbenchNavMode {
  if (!hasActiveProject) return 'global';
  if (panel === 'projects' || panel === 'provider') return 'global';
  return 'project';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端 Workbench 的 shell 导航、测试和后续面板扩展需要共享同一份 panel 全集顺序。
 *
 * Code Logic（这个函数做什么）:
 *   返回只读 panel 全集；每个 panel 恰好出现一次（跨两种导航模式的并集）。
 */
export function getMobileWorkbenchPanelOrder(): readonly MobileWorkbenchPanel[] {
  return MOBILE_WORKBENCH_PANEL_ORDER;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Drawer/rail 按当前导航模式渲染分组，避免无项目时展示终端/文件等无意义入口。
 *
 * Code Logic（这个函数做什么）:
 *   mode=global 返回全局四组；mode=project 返回 work + shortcuts。
 */
export function getMobileWorkbenchNavGroups(
  mode: MobileWorkbenchNavMode = 'global',
  options?: { automationEnabled?: boolean; browserEnabled?: boolean },
): readonly MobileWorkbenchNavGroup[] {
  const groups = mode === 'project' ? MOBILE_PROJECT_NAV_GROUPS : MOBILE_GLOBAL_NAV_GROUPS;
  if (options?.automationEnabled !== false && options?.browserEnabled !== false) {
    return groups;
  }
  return groups.map((group) => ({
    ...group,
    panels: group.panels.filter((panel) => {
      if (panel === 'automation' && options.automationEnabled === false) return false;
      if (panel === 'browser' && options.browserEnabled === false) return false;
      return true;
    }),
  }));
}

/**
 * Business Logic（为什么需要这个函数）:
 *   选中 panel 时要高亮所属分组，且深链/测试需要从 panel 反查 group。
 *
 * Code Logic（这个函数做什么）:
 *   在指定 mode 的分组表中查找 panel；未命中时抛错（合同完整性守卫）。
 */
export function getMobileNavGroupIdForPanel(
  panel: MobileWorkbenchPanel,
  mode: MobileWorkbenchNavMode = 'global',
): MobileWorkbenchNavGroupId {
  for (const group of getMobileWorkbenchNavGroups(mode)) {
    if (group.panels.includes(panel)) {
      return group.id;
    }
  }
  throw new Error(`Panel ${panel} is not mapped to mobile nav mode ${mode}`);
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
 *   shellHeight = layoutViewportHeight：shell 保持全屏高度不压缩；keyboardInset =
 *   layoutViewportHeight - visualViewportHeight（依赖 visualViewport.resize），供
 *   data-keyboard-open 与 computeMobileKeyboardShift 使用；实际 CSS top 上移量由
 *   shift helper 按终端/焦点模式另算；terminalMinHeight 基于 layoutViewportHeight。
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
  // shell 保持全屏高度：软键盘弹出时由 --mobile-keyboard-shift 整体上移而非压缩高度。
  const shellHeight = Math.max(0, Math.round(layoutViewportHeight));
  const landscape = layoutViewportWidth > layoutViewportHeight;
  // shell 全屏不压缩，terminalMinHeight 基于 layoutViewportHeight。
  const terminalMinHeight = computeMobileTerminalMinHeight(
    layoutViewportWidth,
    shellHeight,
    0,
  );
  return { shellHeight, keyboardInset, landscape, terminalMinHeight };
}

/** 焦点与键盘边缘之间保留的最小间距，避免字段贴住键盘上沿。 */
const MOBILE_KEYBOARD_FOCUS_MARGIN_PX = 8;

/**
 * Business Logic（为什么需要这个函数）:
 *   xterm helper textarea 才是「终端输入态」；Prompt 优化 / 收藏搜索等弹层虽在终端页，
 *   也必须走焦点居中，不能把整页顶满。
 *
 * Code Logic（这个函数做什么）:
 *   class 含 xterm-helper-textarea，或 closest 命中该 class / .mobileTerminalViewport 内的
 *   textarea 时返回 true。
 */
export function isMobileTerminalTypingTarget(
  target: {
    classList?: { contains(token: string): boolean };
    closest?: (selector: string) => unknown;
  } | null | undefined,
): boolean {
  if (!target) return false;
  if (target.classList?.contains('xterm-helper-textarea')) return true;
  if (typeof target.closest !== 'function') return false;
  if (target.closest('.xterm-helper-textarea')) return true;
  if (target.closest('textarea.xterm-helper-textarea')) return true;
  return false;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   只有会唤起系统键盘的可编辑控件才需要按焦点计算上移；按钮/只读节点不应带动页面。
 *
 * Code Logic（这个函数做什么）:
 *   INPUT/TEXTAREA/contentEditable，或 xterm helper textarea class，返回 true。
 */
export function isMobileEditableKeyboardTarget(
  target: {
    tagName?: string;
    isContentEditable?: boolean;
    classList?: { contains(token: string): boolean };
  } | null | undefined,
): boolean {
  if (!target) return false;
  const tag = (target.tagName ?? '').toUpperCase();
  if (tag === 'TEXTAREA' || tag === 'INPUT') return true;
  if (target.isContentEditable) return true;
  return Boolean(target.classList?.contains('xterm-helper-textarea'));
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Dialog portal 不走 shell 的 `top` 上移；反推未上移坐标时必须用「作用在该焦点上」的位移，
 *   不能把 shell 的 previousShift 加到尚未平移的弹层输入上。
 *
 * Code Logic（这个函数做什么）:
 *   若焦点在 dialog 内，从 inline transform 解析 translateY 像素；否则若在 shell 内返回 shellShift；
 *   都不是则 0。
 */
export function resolveAppliedMobileKeyboardShift(input: {
  dialogTransform: string | null;
  insideShell: boolean;
  shellShift: number;
}): number {
  if (input.dialogTransform) {
    const match = /translateY\(\s*-?([\d.]+)px\s*\)/.exec(input.dialogTransform);
    if (match) return Math.max(0, Number(match[1]));
    return 0;
  }
  if (input.insideShell) return Math.max(0, Math.round(input.shellShift));
  return 0;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   判断「整页按键盘高度上移后，进入输入态的位置是否还在未遮挡可视区内」。
 *
 * Code Logic（这个函数做什么）:
 *   锚点平移 inset 后，顶底边都落在 [0, layoutHeight - inset] 内则可见。
 */
export function isMobileKeyboardAnchorVisibleAfterLift(input: {
  keyboardInset: number;
  layoutViewportHeight: number;
  anchorTop: number;
  anchorHeight: number;
}): boolean {
  const inset = Math.max(0, Math.round(input.keyboardInset));
  const layoutHeight = Math.max(0, input.layoutViewportHeight);
  const visibleHeight = Math.max(0, layoutHeight - inset);
  if (visibleHeight <= 0) return false;
  const shiftedTop = input.anchorTop - inset;
  const shiftedBottom = shiftedTop + Math.max(0, input.anchorHeight);
  return shiftedTop >= 0 && shiftedBottom <= visibleHeight;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   点击发生在可能已上移的视口里，计算 shift 必须用未上移坐标系。
 *
 * Code Logic（这个函数做什么）:
 *   clientY + 当前已应用 shift，四舍五入为 CSS 像素。
 */
export function resolveUnshiftedMobileKeyboardAnchorTop(
  clientY: number,
  appliedShift: number,
): number {
  return Math.round(clientY + Math.max(0, appliedShift));
}

/**
 * Business Logic（为什么需要这个函数）:
 *   终端面板读取 shell 写在 documentElement 上的上移量，才能把点击坐标还原成未上移锚点。
 *
 * Code Logic（这个函数做什么）:
 *   解析 CSS 像素字符串；非法值当 0。
 */
export function readDocumentMobileKeyboardShiftPx(raw: string): number {
  const value = Number.parseFloat(raw);
  return Number.isFinite(value) ? Math.max(0, Math.round(value)) : 0;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   xterm helper textarea 在光标处，不能代表用户点击进入输入态的位置。
 *
 * Code Logic（这个函数做什么）:
 *   把未上移锚点写入 data-mobile-keyboard-anchor-top。
 */
export function stampMobileKeyboardAnchorTop(
  target: { setAttribute(name: string, value: string): void } | null | undefined,
  unshiftedTop: number,
): void {
  if (!target) return;
  target.setAttribute(MOBILE_KEYBOARD_ANCHOR_TOP_ATTR, String(Math.round(unshiftedTop)));
}

/**
 * Business Logic（为什么需要这个函数）:
 *   shell 计算 shift 时优先用点击锚点，而不是 helper textarea 的光标矩形。
 *
 * Code Logic（这个函数做什么）:
 *   读 data-mobile-keyboard-anchor-top；缺失或非数字返回 null。
 */
export function readStampedMobileKeyboardAnchorTop(
  target: {
    getAttribute?: (name: string) => string | null;
    dataset?: { mobileKeyboardAnchorTop?: string };
  } | null | undefined,
): number | null {
  if (!target) return null;
  const raw =
    target.getAttribute?.(MOBILE_KEYBOARD_ANCHOR_TOP_ATTR) ??
    target.dataset?.mobileKeyboardAnchorTop ??
    null;
  if (raw == null || raw === '') return null;
  const value = Number(raw);
  return Number.isFinite(value) ? value : null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   离开输入态后点击锚点失效，避免下次焦点误用旧坐标。
 *
 * Code Logic（这个函数做什么）:
 *   移除 data-mobile-keyboard-anchor-top。
 */
export function clearMobileKeyboardAnchorTop(
  target: { removeAttribute(name: string): void } | null | undefined,
): void {
  target?.removeAttribute(MOBILE_KEYBOARD_ANCHOR_TOP_ATTR);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   键盘已经打开时再点终端，visualViewport 与 focus 都不变，shell 不会重算上移量。
 *
 * Code Logic（这个函数做什么）:
 *   向 window 派发 cp-mobile-keyboard-anchor-change。
 */
export function notifyMobileKeyboardAnchorChanged(
  dispatch: (event: Event) => void = (event) => window.dispatchEvent(event),
): void {
  dispatch(new Event(MOBILE_KEYBOARD_ANCHOR_CHANGE_EVENT));
}

/**
 * Business Logic（为什么需要这个函数）:
 *   终端输入只有点击位置在整页上移后仍可见才抬升；其它输入只把焦点放到未遮挡可视区
 *   的纵向中间。原始位置已在可视区上半时不得再往中间拽。
 *
 * Code Logic（这个函数做什么）:
 *   full 模式：锚点顶页后仍完全落在未遮挡区则返回 keyboardInset，否则 0（键盘遮底）；
 *   focused 模式按未上移坐标系的焦点中心对准可视区中心，并保证焦点底边不被键盘盖住；
 *   上移量夹在 [0, keyboardInset]；无焦点时保持 previousShift 直到键盘收起，避免 blur
 *   与键盘动画不同步时整页回落。
 */
export function computeMobileKeyboardShift(input: MobileKeyboardShiftInput): number {
  const inset = Math.max(0, Math.round(input.keyboardInset));
  if (inset <= 0) return 0;
  if (input.mode === 'full') {
    const previous = Math.min(inset, Math.max(0, Math.round(input.previousShift)));
    if (input.focusTop == null) return previous;
    if (
      isMobileKeyboardAnchorVisibleAfterLift({
        keyboardInset: inset,
        layoutViewportHeight: input.layoutViewportHeight,
        anchorTop: input.focusTop,
        anchorHeight: input.focusHeight,
      })
    ) {
      return inset;
    }
    return 0;
  }

  const previous = Math.min(inset, Math.max(0, Math.round(input.previousShift)));
  if (input.focusTop == null) return previous;

  const layoutHeight = Math.max(0, input.layoutViewportHeight);
  const visibleHeight = Math.max(0, layoutHeight - inset);
  if (visibleHeight <= 0) return inset;

  const focusTop = input.focusTop;
  const focusHeight = Math.max(0, input.focusHeight);
  const focusBottom = focusTop + focusHeight;
  const focusCenter = focusTop + focusHeight / 2;
  const visibleCenter = visibleHeight / 2;
  const uncover = Math.max(0, focusBottom + MOBILE_KEYBOARD_FOCUS_MARGIN_PX - visibleHeight);

  if (focusHeight >= visibleHeight) {
    return Math.min(inset, Math.max(0, Math.round(focusTop)));
  }

  // 原始位置已在未遮挡区域上半：保持原位，只在会被键盘盖住时抬到刚好露出。
  if (focusCenter <= visibleCenter) {
    return Math.min(inset, Math.round(uncover));
  }

  const toCenter = focusCenter - visibleCenter;
  return Math.min(inset, Math.max(uncover, Math.round(toCenter)));
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
 *   手机端终端全屏时，用户希望隐藏项目标题和 window tabs 等外围内容，专注当前 pane 操作与终端输出。
 *
 * Code Logic（这个函数做什么）:
 *   根据 fullscreen 状态返回终端面板内 chrome 的可见性。终端页的 worktree 条
 *   挂在窗口 tab 上方，全屏时与 window tabs 一同隐藏。
 */
export function getMobileTerminalChromeVisibility(
  fullscreen: boolean,
): MobileTerminalChromeVisibility {
  return {
    windowTabs: !fullscreen,
    paneActions: true,
    terminalSurface: true,
    exitFullscreen: fullscreen,
    worktreeStrip: !fullscreen,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   文件/浏览器/Git 没有窗口 tab，需要在 shell chrome 里放同一条 worktree 条。
 *   终端页改回挂在窗口 tab 上方：终端面板 min-height≈100dvh 会把 shell 里的条裁出视口。
 *
 * Code Logic（这个函数做什么）:
 *   files / browser / git 返回 true；terminal 由面板自己渲染，其它面板返回 false。
 */
export function shouldShowMobileWorktreeStrip(panel: MobileWorkbenchPanel): boolean {
  return panel === 'files' || panel === 'browser' || panel === 'git';
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
 *   项目绑定面板在无 active project 时不可进入，应回到项目列表引导用户选择。
 *   本机与远端快捷方式共享同一套导航规则。
 *
 * Code Logic（这个函数做什么）:
 *   若 next 为项目绑定面板且 project 为空，返回 projects；否则返回 next。
 */
export function selectMobilePanelForProject(
  project: WorkbenchProject | null,
  next: MobileWorkbenchPanel,
): MobileWorkbenchPanel {
  if (isMobileProjectBoundPanel(next) && !project) {
    return 'projects';
  }
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
 *   移动端终端右下角合并 FAB 只应在非主工作区、非主分支或主工作区可 collect-merge 时出现，
 *   避免在默认主分支主工作区误露出与桌面 Git 历史相同的合并入口。
 *
 * Code Logic（这个函数做什么）:
 *   null 隐藏；非主 worktree 显示；主工作区在 canCollectMerge 或当前分支不等于 homeBranch 时显示。
 */
export function canShowMobileTerminalMergeFab(worktree: WorkbenchWorktree | null): boolean {
  if (worktree == null) return false;
  if (!worktree.isMain) return true;
  if (worktree.canCollectMerge) return true;
  const currentBranch = worktree.branch ?? worktree.status.branch;
  const homeBranch = worktree.homeBranch;
  return Boolean(currentBranch && homeBranch && currentBranch !== homeBranch);
}

/**
 * 移动端终端 FAB 环形展开位姿。
 *
 * Business Logic（为什么需要这个接口）:
 *   右下角折叠钮点开后，动作要沿左上象限散开，避免再叠成一列挡住终端。
 *
 * Code Logic（字段说明）:
 *   angleDeg 为 CSS 角（180=左，270=上）；delayOpenMs / delayCloseMs 供错开进出场。
 */
export interface MobileTerminalFabArcPose {
  angleDeg: number;
  delayOpenMs: number;
  delayCloseMs: number;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   4 个常驻动作与可选 Merge 共用一条左上弧，数量变化时仍要保持按钮间距。
 *
 * Code Logic（这个函数做什么）:
 *   index 0 在正上方，沿顺时针扫向左（4 个扫 90°，5 个扫 120°）；进出场 delay 互为倒序。
 */
export function computeMobileTerminalFabArc(
  index: number,
  count: number,
): MobileTerminalFabArcPose {
  const safeCount = Math.max(1, count);
  const clampedIndex = Math.min(Math.max(0, index), safeCount - 1);
  const sweepDeg = safeCount <= 4 ? 90 : 120;
  const t = safeCount === 1 ? 0 : clampedIndex / (safeCount - 1);
  const staggerMs = 40;
  return {
    angleDeg: 270 - sweepDeg * t,
    delayOpenMs: clampedIndex * staggerMs,
    delayCloseMs: (safeCount - 1 - clampedIndex) * staggerMs,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   环形轨道 SVG 需要与按钮同一条弧，4/5 个动作对应不同终点。
 *
 * Code Logic（这个函数做什么）:
 *   返回单位圆 path：从 (0,-1) 扫到终点；sweep-flag=1 为屏幕坐标系顺时针。
 */
export function computeMobileTerminalFabArcPath(count: number): string {
  const sweepDeg = count <= 4 ? 90 : 120;
  const endDeg = 270 - sweepDeg;
  const endRad = (endDeg * Math.PI) / 180;
  return `M 0 -1 A 1 1 0 0 1 ${Math.cos(endRad).toFixed(4)} ${Math.sin(endRad).toFixed(4)}`;
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
 *   功能 worktree merge/remove 会关闭该 worktree 下的终端。切到 main 前必须从本地列表摘掉这些 session，
 *   否则 selectPreferredMobileSession 会回落到已关闭的功能分支窗口，后续 resize 打到不存在的会话。
 *
 * Code Logic（这个函数做什么）:
 *   过滤掉 worktreeId 等于已关闭 worktree 的 session；其它项保持原顺序。
 */
export function pruneMobileSessionsForClosedWorktree(
  sessions: WorkbenchSession[],
  closedWorktreeId: string,
): WorkbenchSession[] {
  return sessions.filter((session) => session.worktreeId !== closedWorktreeId);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   worktree 列表更新后，已不在列表里的 worktree 窗口必须从本机 session 列表摘掉，
 *   否则切到回落工作区时会选中已关闭窗口。
 *
 * Code Logic（这个函数做什么）:
 *   保留 worktreeId 为空或仍属于当前 worktree 列表的 session。
 */
export function pruneMobileSessionsNotInWorktrees(
  sessions: WorkbenchSession[],
  worktrees: WorkbenchWorktree[],
): WorkbenchSession[] {
  const worktreeIds = new Set(worktrees.map((worktree) => worktree.id));
  return sessions.filter(
    (session) => session.worktreeId == null || worktreeIds.has(session.worktreeId),
  );
}

/** 移动端已知 session 元数据更新结果。 */
export interface MobileKnownSessionUpdateResult {
  sessions: WorkbenchSession[];
  activeSession: WorkbenchSession | null;
  applied: boolean;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   sessionUpdated 是全局低带宽事件，但 Mobile 页面只能接受当前已加载列表中的会话；
 *   未知 session 不能借事件跨项目插入，同时 activeSession 必须与列表中的完整 DTO 同步。
 *
 * Code Logic（这个函数做什么）:
 *   按 id 查找已知 session；未命中返回原引用与 applied=false；命中则替换完整 DTO，
 *   activeSession 同 id 时指向替换后的同一对象，否则保持原引用。
 */
export function applyKnownMobileSessionUpdatedEvent(
  sessions: WorkbenchSession[],
  activeSession: WorkbenchSession | null,
  updated: WorkbenchSession,
): MobileKnownSessionUpdateResult {
  const index = sessions.findIndex((session) => session.id === updated.id);
  if (index < 0) {
    return { sessions, activeSession, applied: false };
  }
  const nextSessions = sessions.slice();
  nextSessions[index] = updated;
  return {
    sessions: nextSessions,
    activeSession: activeSession?.id === updated.id ? updated : activeSession,
    applied: true,
  };
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
 *   agentRuntime 事件更新已知 session 的 Agent phase；同一 Agent 内的 version 乱序拒绝。
 *
 * Code Logic（这个函数做什么）:
 *   映射 DTO→projection；同 id 时仅接受更大 version，不同 id 代表 terminal 上的新 Agent，
 *   version 会从 1 重新开始，必须替换旧投影。
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
  if (
    existing.agent &&
    existing.agent.id === incoming.id &&
    existing.agent.version >= incoming.version
  ) {
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
