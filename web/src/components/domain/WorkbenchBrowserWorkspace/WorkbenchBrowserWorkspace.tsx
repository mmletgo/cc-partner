import { lazy, Suspense } from 'react';
import type { ReactElement } from 'react';
import type {
  WorkbenchBrowserPreview,
  WorkbenchProject,
  WorkbenchWorktree,
} from '@/lib/types';
import type { WorkbenchTransport } from '@/api/workbenchTransport';

export type WorkbenchBrowserSurface = 'desktop' | 'mobile';

export interface WorkbenchBrowserWorkspaceProps {
  surface: WorkbenchBrowserSurface;
  transport: WorkbenchTransport;
  project: WorkbenchProject | null;
  worktree: WorkbenchWorktree | null;
  onReturnToTerminal?: () => void;
}

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

/**
 * Business Logic（为什么需要这个函数）:
 *   纯 helper 测试需要在 Node/tsx 中导入本模块，而完整视图会加载 CSS Modules 与 primitives。
 *
 * Code Logic（这个函数做什么）:
 *   动态导入带样式的浏览器预览视图，并转换成 React.lazy 需要的 default export 形状。
 */
async function loadWorkbenchBrowserWorkspaceView() {
  const module = await import('./WorkbenchBrowserWorkspaceView');
  return { default: module.WorkbenchBrowserWorkspaceView };
}

const LazyWorkbenchBrowserWorkspaceView = lazy(loadWorkbenchBrowserWorkspaceView);

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

/**
 * Business Logic（为什么需要这个组件）:
 *   Workbench 用户需要在终端和文件工作区旁快速查看当前项目 dev server 效果，并能自动发现或手动输入 URL。
 *
 * Code Logic（这个组件做什么）:
 *   延迟加载带样式的浏览器预览视图，让纯 helper 测试可直接导入本模块，同时运行时渲染完整工作区。
 */
export function WorkbenchBrowserWorkspace(
  props: WorkbenchBrowserWorkspaceProps,
): ReactElement {
  return (
    <Suspense fallback={null}>
      <LazyWorkbenchBrowserWorkspaceView {...props} />
    </Suspense>
  );
}
