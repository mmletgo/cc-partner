/**
 * Workbench 项目 Context 定义与读取 hook
 *
 * Business Logic（为什么需要这个模块）:
 *   侧栏项目文件夹入口和 Workbench 页面需要共享当前项目、项目列表和项目操作。
 *
 * Code Logic（这个模块做什么）:
 *   定义 WorkbenchProjectsContextValue、创建 React Context，并提供本机/远端项目操作与 useWorkbenchProjects 读取上下文。
 */

import { createContext, useContext } from 'react';
import type { WorkbenchProject } from '@/lib/types';
import type { WorkbenchProjectSessionStats } from '@/lib/workbenchProjectStats';

export interface WorkbenchProjectsContextValue {
  projects: WorkbenchProject[];
  activeProjectId: string | null;
  activeProject: WorkbenchProject | null;
  projectsLoading: boolean;
  projectBusy: boolean;
  projectError: string | null;
  projectSessionStats: Record<string, WorkbenchProjectSessionStats>;
  loadProjects: () => Promise<void>;
  refreshProjectSessionStats: (projectId?: string) => Promise<void>;
  chooseAndAddProject: () => Promise<WorkbenchProject | null>;
  openRemoteProject: (deviceId: string, path: string) => Promise<WorkbenchProject | null>;
  selectProject: (project: WorkbenchProject) => Promise<WorkbenchProject>;
  removeProject: (projectId: string) => Promise<void>;
}

export const WorkbenchProjectsContext = createContext<WorkbenchProjectsContextValue | null>(null);

/**
 * Business Logic（为什么需要这个函数）:
 *   侧栏和工作台页面都需要访问当前项目状态。
 *
 * Code Logic（这个函数做什么）:
 *   从 React Context 读取 Workbench 项目状态；缺少 Provider 时抛出明确错误。
 */
export function useWorkbenchProjects(): WorkbenchProjectsContextValue {
  const value = useContext(WorkbenchProjectsContext);
  if (!value) {
    throw new Error('useWorkbenchProjects must be used inside WorkbenchProjectsProvider');
  }
  return value;
}
