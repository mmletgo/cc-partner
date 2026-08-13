/**
 * 工作台跨窗 deep link 解析。
 *
 * Business Logic（为什么需要这个模块）:
 *   Inbox / 执行现场不得把本窗项目切到别人正在看的项目；应聚焦占用窗。
 *
 * Code Logic（这个模块做什么）:
 *   按 occupancy 决定本窗 navigate、聚焦他窗并投递深链，或本窗 claim。
 */

import { buildWorkbenchDeepLink, parseWorkbenchDeepLink, type WorkbenchDeepLink } from './workbenchDeepLink';

export interface WorkbenchWindowOccupancyRow {
  projectId: string;
  windowLabel: string;
}

export type WorkbenchNavigationTarget =
  | { kind: 'local' }
  | { kind: 'other'; label: string }
  | { kind: 'unoccupied' };

/**
 * Business Logic（为什么需要这个函数）:
 *   同一项目只属于一扇窗；导航必须先问 occupancy。
 *
 * Code Logic（这个函数做什么）:
 *   占用者是本窗 → local；他窗 → other；无人占用 → unoccupied。
 */
export function resolveWorkbenchNavigationTarget(
  projectId: string | null,
  currentLabel: string,
  occupancy: WorkbenchWindowOccupancyRow[],
): WorkbenchNavigationTarget {
  if (!projectId) return { kind: 'local' };
  const owner = occupancy.find((row) => row.projectId === projectId);
  if (!owner) return { kind: 'unoccupied' };
  if (owner.windowLabel === currentLabel) return { kind: 'local' };
  return { kind: 'other', label: owner.windowLabel };
}

export interface OpenWorkbenchDeepLinkArgs {
  target: WorkbenchDeepLink;
  currentLabel: string;
  occupancy: WorkbenchWindowOccupancyRow[];
  navigate: (url: string) => void;
  claim: (projectId: string) => Promise<{ action: string; label: string }>;
  focus: (label: string) => Promise<void>;
  applyOnWindow: (label: string, target: WorkbenchDeepLink) => Promise<void>;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Attention 与 Orchestrator 打开执行现场必须共用同一套 occupancy 规则。
 *
 * Code Logic（这个函数做什么）:
 *   local → 本窗 navigate；other → focus+apply；unoccupied → claim 后再 navigate（冲突则转 other）。
 */
export async function openWorkbenchDeepLink(
  args: OpenWorkbenchDeepLinkArgs,
): Promise<'local' | 'focused-other' | 'claimed'> {
  const url = buildWorkbenchDeepLink(args.target);
  const decision = resolveWorkbenchNavigationTarget(
    args.target.projectId,
    args.currentLabel,
    args.occupancy,
  );
  if (decision.kind === 'local') {
    args.navigate(url);
    return 'local';
  }
  if (decision.kind === 'other') {
    await args.focus(decision.label);
    await args.applyOnWindow(decision.label, args.target);
    return 'focused-other';
  }
  if (!args.target.projectId) {
    args.navigate(url);
    return 'local';
  }
  const claim = await args.claim(args.target.projectId);
  if (claim.action === 'occupied') {
    await args.focus(claim.label);
    await args.applyOnWindow(claim.label, args.target);
    return 'focused-other';
  }
  args.navigate(url);
  return 'claimed';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Attention / 执行现场拿到的是完整 URL，需要先拆出 workbench 深链。
 *
 * Code Logic（这个函数做什么）:
 *   `/workbench` 前缀才解析；settings/agent-hub 返回 null。
 */
export function parseWorkbenchUrlAsDeepLink(url: string): WorkbenchDeepLink | null {
  if (!url.startsWith('/workbench')) return null;
  return parseWorkbenchDeepLink(url);
}

export interface RouteAutomationWorkbenchArgs {
  currentLabel: string;
  occupancy: WorkbenchWindowOccupancyRow[];
  navigate: (url: string) => void;
  claim: OpenWorkbenchDeepLinkArgs['claim'];
  focus: OpenWorkbenchDeepLinkArgs['focus'];
  applyOnWindow: OpenWorkbenchDeepLinkArgs['applyOnWindow'];
  fallback: (url: string) => void;
  closeLocalConsole: () => void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   看板「打开执行现场」与 Inbox 共用 occupancy，本窗才关控制台。
 *
 * Code Logic（这个函数做什么）:
 *   非 workbench URL 走 fallback；否则 openWorkbenchDeepLink，本窗成功后关控制台。
 */
export function routeAutomationWorkbenchOpen(
  url: string,
  args: RouteAutomationWorkbenchArgs,
): void {
  const target = parseWorkbenchUrlAsDeepLink(url);
  if (!target) {
    args.fallback(url);
    return;
  }
  void openWorkbenchDeepLink({
    target,
    currentLabel: args.currentLabel,
    occupancy: args.occupancy,
    navigate: args.navigate,
    claim: args.claim,
    focus: args.focus,
    applyOnWindow: args.applyOnWindow,
  }).then((result) => {
    if (result === 'local' || result === 'claimed') args.closeLocalConsole();
  });
}
