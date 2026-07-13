/**
 * Workbench 自动化域 controller —— 自动化控制台开/关意图、staged deep link 应用（project→worktree→session）
 * 与执行现场 takeover（OrchestratorPanel open-workbench 回跳）。
 *
 * Business Logic（为什么需要这个 controller）:
 *   Orchestrator 任务需要把用户带回关联的项目、worktree 与终端；Workbench 通过 staged deep link
 *   （projectId/worktreeId/sessionId 三段式）逐步定位执行现场，期间必须等前一段切换完成（项目被激活、
 *   worktrees 被加载并选中、sessions 被加载）才能应用下一段。同时 deep link 可能被用户后续导航替换，
 *   旧 search 上的未完成应用必须作废。这个 controller 把 staged deep link 的应用 ref、三段式守卫 effect
 *   和 deep link URL 切换逻辑集中持有，让 Workbench.tsx 只负责调度和共享状态。
 *
 *   重要边界（与文件域 controller 一致）：
 *   - `automationConsoleOpen` 与 `workspaceView` 是跨域共享状态（终端全屏、自动化控制台、文件 tab 都会改写），
 *     仍归 Workbench.tsx 所有；controller 通过注入的 `setAutomationConsoleOpen` / `requestWorkspaceView`
 *     回调表达“打开/关闭控制台”或“切回 terminal 视图”的意图。
 *   - controller 不持有项目选择 API、worktree 列表、session 列表或终端字节内容；这些仍归邻接 controller / 页面。
 *   - controller 不负责 task fetching（OrchestratorPanel 内部自管任务列表）。
 *
 * Code Logic（这个 controller 做什么）:
 *   - 持有 `deepLinkApplicationRef`，记录当前 search 已应用到的 projectId/worktreeId/sessionId，
 *     防止同一段 deep link 被重复应用（重入守卫）。
 *   - 注册 locationSearch 变化的 effect：search 切换时把 applied 重置为“当前 search 但三段未应用”，
 *     让旧 search 上未完成的应用自然失效（旧 effect 再跑会发现 search 不匹配而 early return）。
 *   - 注册三段式 deep link 守卫 effect：
 *     - project 段：目标项目存在且非当前 active 时触发 selectProjectFromDeepLink；
 *     - worktree 段：等 project 段对齐后，目标 worktree 存在且非当前 active 时 setActiveWorktreeId；
 *     - session 段：等 project+worktree 段对齐后，目标 session 存在且非当前 active 时 focusSession。
 *   - 暴露 `applyAutomationDeepLink(target)`：命令式应用 deep link（等价于 project 段），返回是否成功选中项目；
 *     页面 deep link effect 主要走 reactive 路径，本方法保留给页面/外部编排按需调用。
 *   - 暴露 `openAutomation` / `closeAutomation` / `openTaskWorkbench`：稳定的意图函数，由页面 handler 委托。
 *
 * 不复制邻接 controller 状态：project / session / worktree / terminal / file 状态仍归 Workbench.tsx
 * 或邻接 controller 所有。
 */
import { useCallback, useEffect, useRef } from 'react';

import type { WorkbenchDeepLink } from '../workbenchDeepLink';
import type { WorkbenchProject, WorkbenchSession, WorkbenchWorktree } from '@/lib/types';
import type { WorkbenchFileWorkspaceView } from '../workbenchFiles';

/**
 * controller 输入：窄 API + 回调，避免吞并 Projects / Worktrees / Terminal context。
 *
 * 字段说明：
 *   - deepLink：当前 location.search 解析出的 staged deep link；缺失字段为 null。
 *   - locationSearch：原始 query string；用于检测 deep link 是否已被用户导航替换。
 *   - activeProjectId / activeWorktreeId / activeSessionId：当前已激活的项目/worktree/session；用于 staged 守卫。
 *   - projects / worktrees / scopedSessions：可选目标集合；deep link 仅在集合命中时应用对应段。
 *   - automationConsoleOpen：自动化控制台当前开/关状态（页面持有），用于对外暴露 `automationOpen`。
 *   - selectProjectFromDeepLink：项目域 controller 的 deep link 选择 API，触发项目切换。
 *   - setActiveWorktreeId：worktree 选择回写（接受值或 updater 形式，与 React useState setter 一致）。
 *   - focusSession：终端域 controller 的 session focus API。
 *   - setAutomationConsoleOpen：页面共享状态回写（控制台开/关）。
 *   - requestWorkspaceView：页面共享状态回写（切回 terminal 视图）。
 *   - navigate：react-router navigate；用于 openTaskWorkbench 把中心工作区带到 deep link 结果。
 */
export interface WorkbenchAutomationControllerParams {
  deepLink: WorkbenchDeepLink;
  locationSearch: string;
  activeProjectId: string | null;
  activeWorktreeId: string | null;
  activeSessionId: string | null;
  projects: WorkbenchProject[];
  worktrees: WorkbenchWorktree[];
  scopedSessions: WorkbenchSession[];
  automationConsoleOpen: boolean;
  selectProjectFromDeepLink: (projectId: string) => Promise<boolean>;
  setActiveWorktreeId: (
    next: string | null | ((current: string | null) => string | null),
  ) => void;
  focusSession: (sessionId: string) => Promise<boolean>;
  setAutomationConsoleOpen: (open: boolean) => void;
  requestWorkspaceView: (view: WorkbenchFileWorkspaceView) => void;
  navigate: (url: string) => void;
}

/**
 * controller 返回值：自动化域权威状态 + 操作函数。
 *
 * 字段语义：
 *   - automationOpen：当前自动化控制台是否打开（镜像页面 automationConsoleOpen 共享状态）。
 *   - openAutomation：打开控制台并切回 terminal 视图。
 *   - closeAutomation：关闭控制台并切回 terminal 视图。
 *   - applyAutomationDeepLink：命令式应用 staged deep link；返回是否成功命中/选择项目段。
 *   - openTaskWorkbench：执行现场回跳——navigate 到 deep link URL、关闭控制台并切回 terminal 视图。
 */
export interface WorkbenchAutomationControllerResult {
  automationOpen: boolean;
  /** Attention/automation deep link 的 task 焦点（加载后交给 OrchestratorPanel）。 */
  focusTaskId: string | null;
  /** Attention/automation deep link 的 outbox 焦点。 */
  focusOutboxId: string | null;
  openAutomation: () => void;
  closeAutomation: () => void;
  applyAutomationDeepLink: (target: WorkbenchDeepLink) => Promise<boolean>;
  openTaskWorkbench: (url: string) => Promise<void>;
}

/**
 * Business Logic（为什么是默认导出 hook）:
 *   Workbench.tsx 在 early return 之前调用本 hook，与其它 controller 并列组合；保持 React hooks 顺序稳定。
 *
 * Code Logic（这个 hook 做什么）:
 *   1. 持有 deepLinkApplicationRef（与原 Workbench.tsx 内部 ref 行为一致）；
 *   2. 注册 search / project / worktree / session 三段式守卫 effect（保留原 staged ordering）；
 *   3. 暴露稳定的操作函数（useCallback），由页面 handler 委托。
 */
export function useWorkbenchAutomationController(
  params: WorkbenchAutomationControllerParams,
): WorkbenchAutomationControllerResult {
  const {
    deepLink,
    locationSearch,
    activeProjectId,
    activeWorktreeId,
    activeSessionId,
    projects,
    worktrees,
    scopedSessions,
    automationConsoleOpen,
    selectProjectFromDeepLink,
    setActiveWorktreeId,
    focusSession,
    setAutomationConsoleOpen,
    requestWorkspaceView,
    navigate,
  } = params;

  const deepLinkProjectId = deepLink.projectId;
  const deepLinkWorktreeId = deepLink.worktreeId;
  const deepLinkSessionId = deepLink.sessionId;

  // Business Logic: 与原 Workbench.tsx 行为一致——用 ref 记录当前 search 已应用到的
  // projectId/worktreeId/sessionId，防止同一段被重复应用；search 切换时整体重置三段为 null。
  const deepLinkApplicationRef = useRef<{
    search: string;
    projectId: string | null;
    worktreeId: string | null;
    sessionId: string | null;
  }>({
    search: locationSearch,
    projectId: null,
    worktreeId: null,
    sessionId: null,
  });

  // Business Logic: locationSearch 变化时把 applied 重置为“当前 search 但三段未应用”，
  // 让旧 search 上未完成的 staged effect 自然失效（它们会在 search 不匹配时 early return）。
  useEffect(() => {
    deepLinkApplicationRef.current = {
      search: locationSearch,
      projectId: null,
      worktreeId: null,
      sessionId: null,
    };
  }, [locationSearch]);

  const deepLinkView = deepLink.view ?? null;
  const deepLinkTaskId = deepLink.taskId ?? null;
  const deepLinkOutboxId = deepLink.outboxId ?? null;
  const isAutomationDeepLink = deepLinkView === 'automation';

  // Business Logic: staged deep link —— project 段。命中后 fire-and-forget 触发 selectProjectFromDeepLink，
  // 与原 Workbench.tsx 行为一致（不等待切换完成，让后续 worktree/session 段 effect 自行守卫）。
  // Code Logic: 先在 effect 主体里同步确认目标项目存在并标记 applied.projectId 防止重入，再交给
  // selectProjectFromDeepLink 执行实际切换。
  useEffect(() => {
    if (!deepLinkProjectId) return;
    const applied = deepLinkApplicationRef.current;
    if (applied.search !== locationSearch || applied.projectId === deepLinkProjectId) return;
    if (activeProjectId === deepLinkProjectId) {
      applied.projectId = deepLinkProjectId;
      return;
    }
    if (!projects.some((project) => project.id === deepLinkProjectId)) return;
    applied.projectId = deepLinkProjectId;
    void selectProjectFromDeepLink(deepLinkProjectId);
  }, [activeProjectId, deepLinkProjectId, locationSearch, projects, selectProjectFromDeepLink]);

  /**
   * Business Logic（为什么需要这个 effect）:
   *   Attention task/outbox deep link 必须先打开 automation 控制台，不能直接进终端。
   *
   * Code Logic（这个 effect 做什么）:
   *   view=automation 且 project 已对齐时 openAutomation；无 project 约束时也打开。
   */
  useEffect(() => {
    if (!isAutomationDeepLink) return;
    if (deepLinkProjectId && activeProjectId !== deepLinkProjectId) return;
    if (!automationConsoleOpen) {
      setAutomationConsoleOpen(true);
      requestWorkspaceView('terminal');
    }
  }, [
    activeProjectId,
    automationConsoleOpen,
    deepLinkProjectId,
    isAutomationDeepLink,
    requestWorkspaceView,
    setAutomationConsoleOpen,
  ]);

  // Business Logic: staged deep link —— worktree 段。等 project 段对齐后，命中目标 worktree 则选中。
  // Attention automation 链接不消费 worktree/session，避免误开终端。
  useEffect(() => {
    if (isAutomationDeepLink) return;
    if (!deepLinkWorktreeId) return;
    if (deepLinkProjectId && activeProjectId !== deepLinkProjectId) return;
    const applied = deepLinkApplicationRef.current;
    if (applied.search !== locationSearch || applied.worktreeId === deepLinkWorktreeId) return;
    if (!worktrees.some((worktree) => worktree.id === deepLinkWorktreeId)) return;
    applied.worktreeId = deepLinkWorktreeId;
    queueMicrotask(() => {
      setActiveWorktreeId((current) =>
        current === deepLinkWorktreeId ? current : deepLinkWorktreeId,
      );
    });
  }, [
    activeProjectId,
    deepLinkProjectId,
    deepLinkWorktreeId,
    isAutomationDeepLink,
    locationSearch,
    worktrees,
    setActiveWorktreeId,
  ]);

  // Business Logic: staged deep link —— session 段。等 project+worktree 段对齐后，命中目标 session 则 focus。
  useEffect(() => {
    if (isAutomationDeepLink) return;
    if (!deepLinkSessionId) return;
    if (deepLinkProjectId && activeProjectId !== deepLinkProjectId) return;
    if (deepLinkWorktreeId && activeWorktreeId !== deepLinkWorktreeId) return;
    const applied = deepLinkApplicationRef.current;
    if (applied.search !== locationSearch || applied.sessionId === deepLinkSessionId) return;
    if (!scopedSessions.some((session) => session.id === deepLinkSessionId)) return;
    applied.sessionId = deepLinkSessionId;
    if (activeSessionId !== deepLinkSessionId) {
      queueMicrotask(() => {
        focusSession(deepLinkSessionId);
      });
    }
  }, [
    activeProjectId,
    activeSessionId,
    activeWorktreeId,
    deepLinkProjectId,
    deepLinkSessionId,
    deepLinkWorktreeId,
    focusSession,
    isAutomationDeepLink,
    locationSearch,
    scopedSessions,
  ]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   页面层 handler 需要一个稳定的“打开自动化控制台”入口，委托给页面共享状态。
   *
   * Code Logic（这个函数做什么）:
   *   标记控制台为开，并请求把中心工作区切回 terminal（保证 Orchestrator 面板可见且不被文件/浏览器层遮挡）。
   */
  const openAutomation = useCallback((): void => {
    setAutomationConsoleOpen(true);
    requestWorkspaceView('terminal');
  }, [setAutomationConsoleOpen, requestWorkspaceView]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   页面层 handler / 文件域 controller 需要一个稳定的“关闭自动化控制台”入口，委托给页面共享状态。
   *
   * Code Logic（这个函数做什么）:
   *   标记控制台为关，并请求把中心工作区切回 terminal（执行现场回跳后让终端结果可见）。
   */
  const closeAutomation = useCallback((): void => {
    setAutomationConsoleOpen(false);
    requestWorkspaceView('terminal');
  }, [setAutomationConsoleOpen, requestWorkspaceView]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   命令式应用 staged deep link 的 project 段；页面/外部编排按需调用，返回是否成功命中目标项目。
   *
   * Code Logic（这个函数做什么）:
   *   若 target.projectId 缺失则视为 no-op 成功；若已是当前 active project 则直接返回 true；
   *   若目标项目存在则调用 selectProjectFromDeepLink 并返回其结果；否则返回 false。
   */
  const applyAutomationDeepLink = useCallback(
    async (target: WorkbenchDeepLink): Promise<boolean> => {
      const targetProjectId = target.projectId;
      if (!targetProjectId) return true;
      if (activeProjectId === targetProjectId) return true;
      if (!projects.some((project) => project.id === targetProjectId)) return false;
      return selectProjectFromDeepLink(targetProjectId);
    },
    [activeProjectId, projects, selectProjectFromDeepLink],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户在 Orchestrator 看板中点击 blocked 任务的执行现场入口时，需要回到对应 Workbench 项目/worktree/终端。
   *
   * Code Logic（这个函数做什么）:
   *   应用 Orchestrator 构造出的 deep link URL（navigate），关闭自动化控制台并把中心工作区切回 terminal，
   *   让 deep link 聚焦结果可见。
   */
  const openTaskWorkbench = useCallback(
    async (url: string): Promise<void> => {
      navigate(url);
      closeAutomation();
    },
    [navigate, closeAutomation],
  );

  return {
    automationOpen: automationConsoleOpen,
    focusTaskId: isAutomationDeepLink ? deepLinkTaskId : null,
    focusOutboxId: isAutomationDeepLink ? deepLinkOutboxId : null,
    openAutomation,
    closeAutomation,
    applyAutomationDeepLink,
    openTaskWorkbench,
  };
}
