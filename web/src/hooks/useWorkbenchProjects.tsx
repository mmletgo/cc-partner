/**
 * Workbench 项目 Provider
 *
 * Business Logic（为什么需要这个模块）:
 *   项目文件夹列表现在是全局侧栏入口，而 Workbench 页面仍需要知道当前项目。
 *   需要一个共享状态源，避免侧栏和页面各自维护选中项目导致不同步。
 *
 * Code Logic（这个模块做什么）:
 *   提供 WorkbenchProjectsProvider，集中管理项目列表加载、系统目录选择并添加、远端项目打开、
 *   选择、移除、terminal window/pane 统计和当前项目持久化。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { workbenchApi } from '@/api/workbench';
import { configApi } from '@/api/config';
import type { WorkbenchProject } from '@/lib/types';
import { MAIN_WINDOW_LABEL } from '@/lib/workbenchWindow';
import { upsertWorkbenchProjectInPlace } from '@/lib/workbenchRemoteProjects';
import {
  projectSessionStats,
  sessionStatsByProject,
  type WorkbenchProjectSessionStats,
} from '@/lib/workbenchProjectStats';
import {
  WorkbenchProjectsContext,
  type WorkbenchProjectsContextValue,
} from './workbenchProjectsContext';
import { useVisibilityPolling } from './useVisibilityPolling';
import { useWorkbenchWindowRole } from './useWorkbenchWindowRole';
import { getWorkbenchAgentHintStore } from './workbenchAgentHintStore';

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
 *   拉取项目列表、持久化当前项目 ID，并提供本机添加、远端打开、选择/移除项目和刷新终端统计的业务动作。
 */
export function WorkbenchProjectsProvider({ children }: WorkbenchProjectsProviderProps) {
  const { t } = useTranslation(['workbench']);
  const { label: currentWindowLabel } = useWorkbenchWindowRole();
  const persistActiveProject = currentWindowLabel === MAIN_WINDOW_LABEL;
  const [projects, setProjects] = useState<WorkbenchProject[]>([]);
  const [activeProjectId, setActiveProjectIdState] = useState<string | null>(() =>
    persistActiveProject ? readStoredActiveProjectId() : null,
  );
  const [projectsLoading, setProjectsLoading] = useState<boolean>(true);
  const [projectBusy, setProjectBusy] = useState<boolean>(false);
  const [projectError, setProjectError] = useState<string | null>(null);
  const [projectSessionStatsMap, setProjectSessionStatsMap] = useState<
    Record<string, WorkbenchProjectSessionStats>
  >({});
  const [occupancy, setOccupancy] = useState<Array<{ projectId: string; windowLabel: string }>>(
    [],
  );
  const projectAddBusyRef = useRef<boolean>(false);

  const desktopUnavailableMessage = t('workbench:errors.desktopUnavailable');
  const activeProject = useMemo(
    () => projects.find((project) => project.id === activeProjectId) ?? null,
    [activeProjectId, projects],
  );

  const setActiveProjectId = useCallback(
    (projectId: string | null) => {
      setActiveProjectIdState(projectId);
      if (persistActiveProject) writeStoredActiveProjectId(projectId);
    },
    [persistActiveProject],
  );

  const refreshOccupancy = useCallback(async () => {
    try {
      const next = await workbenchApi.windows.listOccupancy();
      setOccupancy(Array.isArray(next) ? next : []);
    } catch {
      // occupancy 只辅助 Rail 标记；失败保留上一帧。
    }
  }, []);

  const { runNow: refreshOccupancyNow } = useVisibilityPolling(refreshOccupancy, {
    intervalMs: 15_000,
    enabled: true,
  });

  const refreshProjectSessionStats = useCallback(async (projectId?: string) => {
    try {
      const list = await workbenchApi.sessions.list(projectId);
      getWorkbenchAgentHintStore().reconcileSessionInventory(
        list.map((session) => ({
          sessionId: session.id,
          projectId: session.projectId,
          worktreeId: session.worktreeId,
        })),
        projectId,
      );
      setProjectSessionStatsMap((current) => {
        if (!projectId) return sessionStatsByProject(list);
        return {
          ...current,
          [projectId]: projectSessionStats(list, projectId),
        };
      });
    } catch {
      // 统计只辅助项目卡片展示；失败时保留上一次成功统计，不打断项目列表主流程。
    }
  }, []);

  const loadProjects = useCallback(async () => {
    try {
      setProjectsLoading(true);
      setProjectError(null);
      const list = await workbenchApi.projects.list();
      setProjects(list);
      setActiveProjectIdState((current) => {
        // 仅保留仍存在于列表中的当前选中项；无效/空时保持 null，
        // 让 Workbench 进入「继续工作」启动页（N4），而不是隐式选中 list[0]。
        const next =
          current && list.some((project) => project.id === current)
            ? current
            : null;
        if (persistActiveProject) writeStoredActiveProjectId(next);
        return next;
      });
      void refreshProjectSessionStats();
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
  }, [desktopUnavailableMessage, persistActiveProject, refreshProjectSessionStats, t]);

  const addProjectFromPath = useCallback(
    async (path: string) => {
      const trimmedPath = path.trim();
      if (!trimmedPath) return null;
      const project = await workbenchApi.projects.add(trimmedPath);
      setProjects((current) => upsertWorkbenchProjectInPlace(current, project));
      setActiveProjectId(project.id);
      return project;
    },
    [setActiveProjectId],
  );

  const chooseAndAddProject = useCallback(async () => {
    if (projectAddBusyRef.current || projectBusy) return null;
    projectAddBusyRef.current = true;
    try {
      setProjectBusy(true);
      setProjectError(null);
      let result: { path: string | null };
      try {
        result = await configApi.chooseDir();
      } catch (error) {
        setProjectError(
          displayWorkbenchErrorMessage(
            error,
            t('workbench:errors.chooseDir'),
            desktopUnavailableMessage,
          ),
        );
        return null;
      }
      if (!result.path) return null;
      const project = await addProjectFromPath(result.path);
      if (project) void refreshProjectSessionStats(project.id);
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
      projectAddBusyRef.current = false;
      setProjectBusy(false);
    }
  }, [addProjectFromPath, desktopUnavailableMessage, projectBusy, refreshProjectSessionStats, t]);

  const openRemoteProject = useCallback(
    async (deviceId: string, path: string) => {
      const trimmedPath = path.trim();
      if (!deviceId || !trimmedPath) return null;
      try {
        setProjectBusy(true);
        setProjectError(null);
        const project = await workbenchApi.remote.openProject(deviceId, trimmedPath);
        setProjects((current) => upsertWorkbenchProjectInPlace(current, project));
        setActiveProjectId(project.id);
        void refreshProjectSessionStats(project.id);
        return project;
      } catch (error) {
        const message = displayWorkbenchErrorMessage(
          error,
          t('workbench:errors.openRemoteProject'),
          desktopUnavailableMessage,
        );
        setProjectError(message);
        throw new Error(message, { cause: error });
      } finally {
        setProjectBusy(false);
      }
    },
    [desktopUnavailableMessage, refreshProjectSessionStats, setActiveProjectId, t],
  );

  const selectProject = useCallback(
    async (project: WorkbenchProject) => {
      try {
        const claim = await workbenchApi.windows.claim(project.id);
        if (claim.action === 'occupied') {
          await workbenchApi.windows.focus(claim.label);
          void refreshOccupancyNow({ force: true });
          return project;
        }
      } catch {
        // 无 Tauri / claim 失败时仍允许本窗切换，避免浏览器调试卡死。
      }
      setActiveProjectId(project.id);
      try {
        const touched = await workbenchApi.projects.touch(project.id);
        setProjects((current) => upsertWorkbenchProjectInPlace(current, touched));
        void refreshProjectSessionStats(touched.id);
        void refreshOccupancyNow({ force: true });
        return touched;
      } catch {
        void refreshProjectSessionStats(project.id);
        return project;
      }
    },
    [refreshOccupancyNow, refreshProjectSessionStats, setActiveProjectId],
  );

  const openProjectInNewWindow = useCallback(
    async (project: WorkbenchProject) => {
      try {
        await workbenchApi.windows.open(project.id);
      } catch (error) {
        setProjectError(
          displayWorkbenchErrorMessage(
            error,
            t('workbench:errors.openInNewWindow'),
            desktopUnavailableMessage,
          ),
        );
        return;
      }
      void refreshOccupancyNow({ force: true });
    },
    [desktopUnavailableMessage, refreshOccupancyNow, t],
  );

  const removeProject = useCallback(
    async (projectId: string) => {
      try {
        setProjectBusy(true);
        const occupied = occupancy.find((row) => row.projectId === projectId);
        if (occupied && occupied.windowLabel !== currentWindowLabel) {
          try {
            await workbenchApi.windows.close(occupied.windowLabel);
          } catch {
            // 关窗失败仍尝试删除项目记录；后端会拆 session。
          }
        }
        await workbenchApi.projects.remove(projectId);
        setProjects((current) => current.filter((project) => project.id !== projectId));
        setProjectSessionStatsMap((current) => {
          const next = { ...current };
          delete next[projectId];
          return next;
        });
        if (activeProjectId === projectId) setActiveProjectId(null);
        void refreshOccupancyNow({ force: true });
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
    [activeProjectId, currentWindowLabel, desktopUnavailableMessage, occupancy, refreshOccupancyNow, setActiveProjectId, t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   桌面侧栏拖拽后应立刻反映顺序，并持久化到后端参与跨设备 LWW。
   *
   * Code Logic（这个函数做什么）:
   *   按 orderedIds 乐观重排本地列表；成功后用后端返回列表覆盖；失败回滚并写 projectError。
   */
  const reorderProjects = useCallback(
    async (orderedIds: string[]) => {
      let previous: WorkbenchProject[] = [];
      setProjects((current) => {
        previous = current;
        const byId = new Map(current.map((project) => [project.id, project]));
        const next: WorkbenchProject[] = [];
        for (const id of orderedIds) {
          const project = byId.get(id);
          if (project) next.push(project);
        }
        for (const project of current) {
          if (!orderedIds.includes(project.id)) next.push(project);
        }
        return next;
      });
      try {
        const list = await workbenchApi.projects.reorder(orderedIds);
        setProjects(list);
        setProjectError(null);
      } catch (error) {
        setProjects(previous);
        setProjectError(
          displayWorkbenchErrorMessage(
            error,
            t('workbench:errors.reorderProjects'),
            desktopUnavailableMessage,
          ),
        );
      }
    },
    [desktopUnavailableMessage, t],
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
      projectsLoading,
      projectBusy,
      projectError,
      projectSessionStats: projectSessionStatsMap,
      loadProjects,
      refreshProjectSessionStats,
      chooseAndAddProject,
      openRemoteProject,
      selectProject,
      removeProject,
      reorderProjects,
      currentWindowLabel,
      occupancy,
      openProjectInNewWindow,
    }),
    [
      activeProject,
      activeProjectId,
      currentWindowLabel,
      chooseAndAddProject,
      loadProjects,
      openRemoteProject,
      projectBusy,
      projectError,
      projectSessionStatsMap,
      projects,
      projectsLoading,
      refreshProjectSessionStats,
      occupancy,
      openProjectInNewWindow,
      removeProject,
      reorderProjects,
      selectProject,
    ],
  );

  return (
    <WorkbenchProjectsContext.Provider value={value}>
      {children}
    </WorkbenchProjectsContext.Provider>
  );
}
