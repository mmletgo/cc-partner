/**
 * Workbench 项目 Provider
 *
 * Business Logic（为什么需要这个模块）:
 *   项目文件夹列表现在是全局侧栏入口，而 Workbench 页面仍需要知道当前项目。
 *   需要一个共享状态源，避免侧栏和页面各自维护选中项目导致不同步。
 *
 * Code Logic（这个模块做什么）:
 *   提供 WorkbenchProjectsProvider，集中管理项目列表加载、添加、选择、移除、
 *   系统目录选择器和当前项目持久化。
 */

import { useCallback, useEffect, useMemo, useState } from 'react';
import type { ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { workbenchApi } from '@/api/workbench';
import { configApi } from '@/api/config';
import type { WorkbenchProject } from '@/lib/types';
import {
  WorkbenchProjectsContext,
  type WorkbenchProjectsContextValue,
} from './workbenchProjectsContext';

const ACTIVE_PROJECT_KEY = 'cp-workbench-active-project-id';

/**
 * Business Logic（为什么需要这个函数）:
 *   普通浏览器调试环境没有 Tauri IPC，项目列表加载失败时需要展示用户可理解的状态。
 *
 * Code Logic（这个函数做什么）:
 *   将 Tauri unavailable/invoke 错误映射为桌面端提示，其他错误保留 message。
 */
function displayWorkbenchErrorMessage(
  error: unknown,
  fallback: string,
  desktopUnavailable: string,
): string {
  const message =
    error instanceof Error ? error.message : typeof error === 'string' ? error : String(error);
  const normalized = message.toLowerCase();
  if (
    normalized.includes('invoke') ||
    normalized.includes('__tauri') ||
    normalized.includes('reading \'invoke\'') ||
    normalized.includes('reading "invoke"')
  ) {
    return desktopUnavailable;
  }
  return message && message !== 'undefined' && message !== 'null' ? message : fallback;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   React lint 要求 effect 主体不要同步触发级联 setState；项目列表仍需要页面装载后拉取。
 *
 * Code Logic（这个函数做什么）:
 *   把 effect 内的异步工作延后到下一个 macrotask，并返回清理函数取消尚未执行的任务。
 */
function deferEffect(work: () => void): () => void {
  const timer = window.setTimeout(work, 0);
  return () => window.clearTimeout(timer);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用户重新打开应用后应回到最近选中的工作项目。
 *
 * Code Logic（这个函数做什么）:
 *   从 localStorage 读取项目 ID；普通浏览器隐私限制异常时返回 null。
 */
function readStoredActiveProjectId(): string | null {
  try {
    return window.localStorage.getItem(ACTIVE_PROJECT_KEY);
  } catch {
    return null;
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   当前工作项目需要跨路由和刷新保持一致。
 *
 * Code Logic（这个函数做什么）:
 *   写入或清除 localStorage 中的项目 ID；存储异常时静默降级为内存状态。
 */
function writeStoredActiveProjectId(projectId: string | null): void {
  try {
    if (projectId) {
      window.localStorage.setItem(ACTIVE_PROJECT_KEY, projectId);
    } else {
      window.localStorage.removeItem(ACTIVE_PROJECT_KEY);
    }
  } catch {
    // localStorage 不可用时只保留 React 内存态。
  }
}

export interface WorkbenchProjectsProviderProps {
  children: ReactNode;
}

/**
 * WorkbenchProjectsProvider（工作台项目共享状态）
 *
 * Business Logic（为什么需要这个组件）:
 *   左侧栏项目文件夹列表是进入工作台的全局入口，Workbench 页面需要复用同一份当前项目状态。
 *
 * Code Logic（这个组件做什么）:
 *   拉取项目列表、持久化当前项目 ID，并提供添加/选择/移除项目的业务动作。
 */
export function WorkbenchProjectsProvider({ children }: WorkbenchProjectsProviderProps) {
  const { t } = useTranslation(['workbench']);
  const [projects, setProjects] = useState<WorkbenchProject[]>([]);
  const [activeProjectId, setActiveProjectIdState] = useState<string | null>(() =>
    readStoredActiveProjectId(),
  );
  const [projectPath, setProjectPath] = useState<string>('');
  const [projectFormOpen, setProjectFormOpen] = useState<boolean>(false);
  const [projectsLoading, setProjectsLoading] = useState<boolean>(true);
  const [projectBusy, setProjectBusy] = useState<boolean>(false);
  const [projectError, setProjectError] = useState<string | null>(null);

  const desktopUnavailableMessage = t('workbench:errors.desktopUnavailable');
  const activeProject = useMemo(
    () => projects.find((project) => project.id === activeProjectId) ?? null,
    [activeProjectId, projects],
  );

  const setActiveProjectId = useCallback((projectId: string | null) => {
    setActiveProjectIdState(projectId);
    writeStoredActiveProjectId(projectId);
  }, []);

  const loadProjects = useCallback(async () => {
    try {
      setProjectsLoading(true);
      setProjectError(null);
      const list = await workbenchApi.projects.list();
      setProjects(list);
      setActiveProjectIdState((current) => {
        const next =
          current && list.some((project) => project.id === current)
            ? current
            : list[0]?.id ?? null;
        writeStoredActiveProjectId(next);
        return next;
      });
    } catch (error) {
      setProjectError(
        displayWorkbenchErrorMessage(
          error,
          t('workbench:errors.projects'),
          desktopUnavailableMessage,
        ),
      );
    } finally {
      setProjectsLoading(false);
    }
  }, [desktopUnavailableMessage, t]);

  const chooseProjectDirectory = useCallback(async () => {
    try {
      const result = await configApi.chooseDir();
      if (result.path) setProjectPath(result.path);
    } catch (error) {
      setProjectError(
        displayWorkbenchErrorMessage(
          error,
          t('workbench:errors.chooseDir'),
          desktopUnavailableMessage,
        ),
      );
    }
  }, [desktopUnavailableMessage, t]);

  const addProject = useCallback(async () => {
    const path = projectPath.trim();
    if (!path) return null;
    try {
      setProjectBusy(true);
      setProjectError(null);
      const project = await workbenchApi.projects.add(path);
      setProjects((current) => {
        const withoutDuplicate = current.filter((item) => item.id !== project.id);
        return [project, ...withoutDuplicate];
      });
      setActiveProjectId(project.id);
      setProjectPath('');
      setProjectFormOpen(false);
      return project;
    } catch (error) {
      setProjectError(
        displayWorkbenchErrorMessage(
          error,
          t('workbench:errors.addProject'),
          desktopUnavailableMessage,
        ),
      );
      return null;
    } finally {
      setProjectBusy(false);
    }
  }, [desktopUnavailableMessage, projectPath, setActiveProjectId, t]);

  const selectProject = useCallback(
    async (project: WorkbenchProject) => {
      setActiveProjectId(project.id);
      try {
        const touched = await workbenchApi.projects.touch(project.id);
        setProjects((current) => {
          const withoutCurrent = current.filter((item) => item.id !== touched.id);
          return [touched, ...withoutCurrent];
        });
        return touched;
      } catch {
        // 最近打开时间更新失败不阻断本地切换，下一次刷新会恢复后端状态。
        return project;
      }
    },
    [setActiveProjectId],
  );

  const removeProject = useCallback(
    async (projectId: string) => {
      try {
        setProjectBusy(true);
        await workbenchApi.projects.remove(projectId);
        setProjects((current) => current.filter((project) => project.id !== projectId));
        if (activeProjectId === projectId) setActiveProjectId(null);
      } catch (error) {
        setProjectError(
          displayWorkbenchErrorMessage(
            error,
            t('workbench:errors.removeProject'),
            desktopUnavailableMessage,
          ),
        );
      } finally {
        setProjectBusy(false);
      }
    },
    [activeProjectId, desktopUnavailableMessage, setActiveProjectId, t],
  );

  useEffect(() => {
    return deferEffect(() => {
      void loadProjects();
    });
  }, [loadProjects]);

  const value = useMemo<WorkbenchProjectsContextValue>(
    () => ({
      projects,
      activeProjectId,
      activeProject,
      projectPath,
      projectFormOpen,
      projectsLoading,
      projectBusy,
      projectError,
      setProjectPath,
      setProjectFormOpen,
      loadProjects,
      chooseProjectDirectory,
      addProject,
      selectProject,
      removeProject,
    }),
    [
      activeProject,
      activeProjectId,
      addProject,
      chooseProjectDirectory,
      loadProjects,
      projectBusy,
      projectError,
      projectFormOpen,
      projectPath,
      projects,
      projectsLoading,
      removeProject,
      selectProject,
    ],
  );

  return (
    <WorkbenchProjectsContext.Provider value={value}>
      {children}
    </WorkbenchProjectsContext.Provider>
  );
}
