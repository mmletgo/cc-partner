/**
 * Workbench 浏览器预览纯 helper（与组件文件分离以满足 Fast Refresh）。
 *
 * Business Logic（为什么需要这个模块）:
 *   桌面/移动端共享 preview URL 选择、source i18n key 映射与请求 stale guard；
 *   这些纯函数若与 React 组件同文件导出会触发 react-refresh/only-export-components。
 *
 * Code Logic（这个模块做什么）:
 *   导出 surface/props 无关的类型与纯函数，不包含任何 React 组件。
 */

import type {
  WorkbenchBrowserPreview,
  WorkbenchBrowserTargetSource,
} from '@/lib/types';

export type WorkbenchBrowserSurface = 'desktop' | 'mobile';

export interface WorkbenchBrowserRequestState {
  sequence: number;
  projectId: string | null;
  worktreeId: string | null;
}

export interface WorkbenchBrowserRequestSnapshot {
  sequence: number;
  projectId: string;
  worktreeId: string | null;
}

export type WorkbenchBrowserSourceLabelKey =
  | 'workbench:browserPreview.sources.remembered'
  | 'workbench:browserPreview.sources.terminalOutput'
  | 'workbench:browserPreview.sources.projectConfig'
  | 'workbench:browserPreview.sources.portProbe'
  | 'workbench:browserPreview.sources.manual';

export const WORKBENCH_BROWSER_IFRAME_SANDBOX =
  'allow-scripts allow-forms allow-popups allow-downloads allow-modals';

/**
 * Business Logic（为什么需要这个函数）:
 *   桌面 Tauri 和移动端浏览器访问 preview proxy 的 URL 不同，组件需要稳定选择 iframe src。
 *
 * Code Logic（这个函数做什么）:
 *   desktop 返回后端提供的绝对 loopback URL，mobile 返回同源 path。
 */
export function getWorkbenchBrowserFrameSrc(
  preview: WorkbenchBrowserPreview | null,
  surface: WorkbenchBrowserSurface,
): string | null {
  if (!preview) return null;
  return surface === 'desktop' ? preview.desktopProxyUrl : preview.mobileProxyPath;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   浏览器候选来源文案必须跟随当前前端语言，不能直接展示后端 DTO 的 label 字段。
 *
 * Code Logic（这个函数做什么）:
 *   将稳定 source 枚举映射到 workbench namespace 下的 i18n key，供视图组件交给 t() 渲染。
 */
export function getWorkbenchBrowserTargetSourceLabelKey(
  source: WorkbenchBrowserTargetSource,
): WorkbenchBrowserSourceLabelKey {
  switch (source) {
    case 'remembered':
      return 'workbench:browserPreview.sources.remembered';
    case 'terminalOutput':
      return 'workbench:browserPreview.sources.terminalOutput';
    case 'projectConfig':
      return 'workbench:browserPreview.sources.projectConfig';
    case 'portProbe':
      return 'workbench:browserPreview.sources.portProbe';
    case 'manual':
      return 'workbench:browserPreview.sources.manual';
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   浏览器预览的发现与打开请求可能乱序返回，旧项目/worktree 或旧点击结果不能覆盖当前预览。
 *
 * Code Logic（这个函数做什么）:
 *   同时比较请求序号、projectId 和 worktreeId，三者完全匹配才允许异步结果写入 UI 状态。
 */
export function canApplyWorkbenchBrowserRequest(
  current: WorkbenchBrowserRequestState,
  request: WorkbenchBrowserRequestSnapshot,
): boolean {
  return (
    current.sequence === request.sequence &&
    current.projectId === request.projectId &&
    current.worktreeId === request.worktreeId
  );
}
