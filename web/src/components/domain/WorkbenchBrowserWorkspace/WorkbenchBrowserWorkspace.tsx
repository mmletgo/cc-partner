import { lazy, Suspense } from 'react';
import type { ReactElement } from 'react';
import type { WorkbenchProject, WorkbenchWorktree } from '@/lib/types';
import type { WorkbenchTransport } from '@/api/workbenchTransport';
import type { WorkbenchBrowserSurface } from './workbenchBrowserHelpers';

export interface WorkbenchBrowserWorkspaceProps {
  surface: WorkbenchBrowserSurface;
  transport: WorkbenchTransport;
  project: WorkbenchProject | null;
  worktree: WorkbenchWorktree | null;
  onReturnToTerminal?: () => void;
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
 * Business Logic（为什么需要这个组件）:
 *   Workbench 用户需要在终端和文件工作区旁快速查看当前项目 dev server 效果，并能自动发现或手动输入 URL。
 *
 * Code Logic（这个组件做什么）:
 *   延迟加载带样式的浏览器预览视图；纯 helper 已拆到 workbenchBrowserHelpers.ts 以满足 Fast Refresh。
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
