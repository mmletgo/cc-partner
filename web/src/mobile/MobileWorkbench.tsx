import { useCallback, useEffect, useRef, useState } from 'react';
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { httpWorkbenchTransport } from '@/api/workbenchHttp';
import type { WorkbenchProject, WorkbenchSession, WorkbenchWorktree } from '@/lib/types';
import { MobileAutomationPanel } from './components/MobileAutomationPanel';
import { MobileFilesPanel } from './components/MobileFilesPanel';
import { MobileGitPanel } from './components/MobileGitPanel';
import { MobilePromptPanel } from './components/MobilePromptPanel';
import { MobileTerminalPanel } from './components/MobileTerminalPanel';
import { MobileProjectPanel } from './components/MobileProjectPanel';
import { MobileWorkbenchShell } from './components/MobileWorkbenchShell';
import { MobileWorktreeQuickSwitch } from './components/MobileWorktreeQuickSwitch';
import { MobileWorktreePanel } from './components/MobileWorktreePanel';
import {
  canOpenMobileWorktreeSwitcher,
  canSelectMobileProject,
  getInitialMobileWorkbenchPanel,
  selectMobilePanelForProject,
  selectMobileWorktreeWorkspacePanel,
  selectPreferredMobileSession,
  selectPreferredMobileWorktree,
  type MobileWorkbenchPanel,
} from './mobileWorkbenchState';
import {
  getMobileWorktreeMergeAppliedState,
  runMobileWorktreeMergeFlow,
  runMobileWorktreeRefreshFlow,
  shouldConfirmMobileFileDirtyContextSwitch,
  type MobileFileDirtySnapshot,
  type MobileFilePanelContext,
} from './mobilePanelState';
import styles from './MobileWorkbench.module.css';

export interface MobileRefreshWorktreesOptions {
  skipFileContextConfirm?: boolean;
  expectedProjectId?: string;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端 HTTP 请求失败时需要展示可读错误，并兼容非 Error 抛出值。
 *
 * Code Logic（这个函数做什么）:
 *   把 unknown reason 规整为字符串；优先使用 Error.message，空值回退 String(reason)。
 */
function getErrorMessage(reason: unknown): string {
  if (reason instanceof Error && reason.message.trim()) {
    return reason.message;
  }
  return String(reason);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端 Files 面板的 dirty 草稿绑定 project/worktree，父级在切换上下文前需要生成同一种 context 进行比较。
 *
 * Code Logic（这个函数做什么）:
 *   接收当前目标 project/worktree；没有 project 时返回 null，否则返回 MobileFilePanelContext，worktree 缺失时使用 null。
 */
function createMobileFilePanelContext(
  project: WorkbenchProject | null,
  worktree: WorkbenchWorktree | null,
): MobileFilePanelContext | null {
  return project ? { projectId: project.id, worktreeId: worktree?.id ?? null } : null;
}

/**
 * MobileWorkbench（移动端工作台页面）
 *
 * Business Logic（为什么需要这个组件）:
 *   `/mobile` 需要通过 HTTP 加载最近项目；本机与远端快捷方式都应进入完整移动端 Workbench。
 *
 * Code Logic（这个组件做什么）:
 *   管理 active panel/project/worktree/session 状态；local/remote 项目都通过 HTTP transport 拉取 worktree/session，后端按项目类型决定是否代理。
 */
export function MobileWorkbench(): ReactElement {
  const [panel, setPanel] = useState<MobileWorkbenchPanel>(() => getInitialMobileWorkbenchPanel());
  const [projects, setProjects] = useState<WorkbenchProject[]>([]);
  const [activeProject, setActiveProject] = useState<WorkbenchProject | null>(null);
  const [worktrees, setWorktrees] = useState<WorkbenchWorktree[]>([]);
  const [activeWorktree, setActiveWorktree] = useState<WorkbenchWorktree | null>(null);
  const [sessions, setSessions] = useState<WorkbenchSession[]>([]);
  const [activeSession, setActiveSession] = useState<WorkbenchSession | null>(null);
  const [projectsLoading, setProjectsLoading] = useState<boolean>(false);
  const [projectDetailsLoading, setProjectDetailsLoading] = useState<boolean>(false);
  const [worktreeOperationBusy, setWorktreeOperationBusy] = useState<boolean>(false);
  const [worktreeSwitcherOpen, setWorktreeSwitcherOpen] = useState<boolean>(false);
  const [filesDirtySnapshot, setFilesDirtySnapshot] = useState<MobileFileDirtySnapshot>({
    dirty: false,
    context: null,
  });
  const [filesDiscardContextToken, setFilesDiscardContextToken] = useState<number>(0);
  const [error, setError] = useState<string | null>(null);
  const projectsRequestIdRef = useRef<number>(0);
  const projectDetailsRequestIdRef = useRef<number>(0);
  const worktreesRequestIdRef = useRef<number>(0);
  const sessionsRequestIdRef = useRef<number>(0);
  const activeProjectRef = useRef<WorkbenchProject | null>(null);
  const activeWorktreeRef = useRef<WorkbenchWorktree | null>(null);
  const worktreeOperationBusyRef = useRef<boolean>(false);
  const worktreeOperationCountRef = useRef<number>(0);
  const sessionsRef = useRef<WorkbenchSession[]>([]);
  const { t } = useTranslation(['workbench']);

  const panelPlaceholders: Record<MobileWorkbenchPanel, { title: string; label: string }> = {
    projects: {
      title: t('workbench:mobile.placeholders.projects.title'),
      label: t('workbench:mobile.placeholders.projects.label'),
    },
    terminal: {
      title: t('workbench:mobile.placeholders.terminal.title'),
      label: t('workbench:mobile.placeholders.terminal.label'),
    },
    files: {
      title: t('workbench:mobile.placeholders.files.title'),
      label: t('workbench:mobile.placeholders.files.label'),
    },
    git: {
      title: t('workbench:mobile.placeholders.git.title'),
      label: t('workbench:mobile.placeholders.git.label'),
    },
    worktrees: {
      title: t('workbench:mobile.placeholders.worktrees.title'),
      label: t('workbench:mobile.placeholders.worktrees.label'),
    },
    prompt: {
      title: t('workbench:mobile.placeholders.prompt.title'),
      label: t('workbench:mobile.placeholders.prompt.label'),
    },
    automation: {
      title: t('workbench:mobile.placeholders.automation.title'),
      label: t('workbench:mobile.placeholders.automation.label'),
    },
    settings: {
      title: t('workbench:mobile.placeholders.settings.title'),
      label: t('workbench:mobile.placeholders.settings.label'),
    },
  };
  const placeholder = panelPlaceholders[panel];
  const worktreeControlsBusy = projectDetailsLoading || worktreeOperationBusy;

  /* eslint-disable react-hooks/set-state-in-effect -- 项目或可用性变化时需要关闭 transient quick switch sheet */
  useEffect(() => {
    activeProjectRef.current = activeProject;
    setWorktreeSwitcherOpen(false);
  }, [activeProject]);

  useEffect(() => {
    if (!canOpenMobileWorktreeSwitcher(activeProject, worktreeControlsBusy)) {
      setWorktreeSwitcherOpen(false);
    }
  }, [activeProject, worktreeControlsBusy]);
  /* eslint-enable react-hooks/set-state-in-effect */

  useEffect(() => {
    activeWorktreeRef.current = activeWorktree;
  }, [activeWorktree]);

  useEffect(() => {
    sessionsRef.current = sessions;
  }, [sessions]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   destructive worktree 操作需要先询问用户是否允许离开 Files 草稿，但不能在后端成功前清空草稿。
   *
   * Code Logic（这个函数做什么）:
   *   只比较 dirty snapshot 与目标 context 并按需弹确认；不修改任何 React state。
   */
  const confirmFileContextSwitchOnly = useCallback(
    (targetContext: MobileFilePanelContext | null): boolean => {
      if (!shouldConfirmMobileFileDirtyContextSwitch(filesDirtySnapshot, targetContext)) {
        return true;
      }
      return window.confirm(t('workbench:mobile.filesPanel.discardContextConfirm'));
    },
    [filesDirtySnapshot, t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户已确认放弃 Files 草稿后，父级需要让常驻 Files 面板在下一次 context 响应中跳过重复确认。
   *
   * Code Logic（这个函数做什么）:
   *   清空 dirty snapshot 并递增 discard token；调用方负责保证只在确认后或后端 destructive 操作成功后调用。
   */
  const discardConfirmedFileContextSwitch = useCallback((): void => {
    setFilesDirtySnapshot({ dirty: false, context: null });
    setFilesDiscardContextToken((current) => current + 1);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   普通 project/worktree 切换会立即离开当前 Files 上下文，确认通过后可以立刻丢弃草稿。
   *
   * Code Logic（这个函数做什么）:
   *   复用只确认 helper；如果确实需要离开 dirty context，确认通过后清 dirty snapshot 并递增 discard token。
   */
  const confirmFileContextSwitch = useCallback(
    (targetContext: MobileFilePanelContext | null): boolean => {
      const shouldDiscard = shouldConfirmMobileFileDirtyContextSwitch(
        filesDirtySnapshot,
        targetContext,
      );
      if (!confirmFileContextSwitchOnly(targetContext)) return false;
      if (shouldDiscard) {
        discardConfirmedFileContextSwitch();
      }
      return true;
    },
    [confirmFileContextSwitchOnly, discardConfirmedFileContextSwitch, filesDirtySnapshot],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   移动端 worktree 变化会影响状态栏、终端、文件和 Git 面板，必须同步切换到同一 worktree 下的优先 session。
   *
   * Code Logic（这个函数做什么）:
   *   写入 active worktree ref/state，并基于最新 sessions ref 选择匹配 terminal session。
   */
  const setActiveWorktreeWithSession = useCallback((worktree: WorkbenchWorktree | null): void => {
    activeWorktreeRef.current = worktree;
    setActiveWorktree(worktree);
    setActiveSession(selectPreferredMobileSession(sessionsRef.current, worktree?.id ?? null));
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   终端面板更新 session 列表后，父组件要同步 ref，供 worktree 切换时选择最新 session。
   *
   * Code Logic（这个函数做什么）:
   *   同步 sessions ref 和 React state，不改变 active session。
   */
  const handleSessionsChange = useCallback((nextSessions: WorkbenchSession[]): void => {
    sessionsRef.current = nextSessions;
    setSessions(nextSessions);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   worktree 创建、删除、合并和刷新会影响所有移动端面板上下文，期间必须全局禁用 worktree 切换入口防止竞态。
   *
   * Code Logic（这个函数做什么）:
   *   递增全局 worktree 操作计数并同步 ref/state；返回幂等 end 回调，嵌套操作全部结束后才释放 busy。
   */
  const beginWorktreeOperation = useCallback((): (() => void) => {
    worktreeOperationCountRef.current += 1;
    worktreeOperationBusyRef.current = true;
    setWorktreeOperationBusy(true);

    let ended = false;

    /**
     * Business Logic（为什么需要这个函数）:
     *   每个异步 worktree 操作结束时都要释放自己的占用，但不能影响仍在进行的嵌套刷新或 merge。
     *
     * Code Logic（这个函数做什么）:
     *   幂等递减计数；计数归零时同步 ref/state 为非 busy。
     */
    return (): void => {
      if (ended) return;
      ended = true;
      worktreeOperationCountRef.current = Math.max(0, worktreeOperationCountRef.current - 1);
      if (worktreeOperationCountRef.current === 0) {
        worktreeOperationBusyRef.current = false;
        setWorktreeOperationBusy(false);
      }
    };
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   手机端进入 `/mobile` 后需要立即看到最近项目列表，也需要支持用户手动刷新。
   *
   * Code Logic（这个函数做什么）:
   *   调用 HTTP projects.list，使用递增 request id 丢弃旧响应，并更新项目列表加载态与错误态。
   */
  const loadProjects = useCallback(async (): Promise<void> => {
    const requestId = projectsRequestIdRef.current + 1;
    projectsRequestIdRef.current = requestId;
    setProjectsLoading(true);
    setError(null);

    try {
      const nextProjects = await httpWorkbenchTransport.projects.list();
      if (projectsRequestIdRef.current !== requestId) return;
      setProjects(nextProjects);
    } catch (reason) {
      if (projectsRequestIdRef.current !== requestId) return;
      setError(getErrorMessage(reason));
    } finally {
      if (projectsRequestIdRef.current === requestId) {
        setProjectsLoading(false);
      }
    }
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   移动端终端面板在新建 window、分屏或关闭 pane/window 后，需要重新读取后端权威 terminal window 列表。
   *
   * Code Logic（这个函数做什么）:
   *   基于当前 active project 请求 sessions.list，用 request id 防止旧响应覆盖；保留仍存在的 active session，否则按当前 worktree 重新选择。
   */
  const refreshSessions = useCallback(async (): Promise<void> => {
    if (!activeProject) return;
    const projectId = activeProject.id;
    const worktreeId = activeWorktree?.id ?? null;
    const requestId = sessionsRequestIdRef.current + 1;
    sessionsRequestIdRef.current = requestId;

    try {
      const nextSessions = await httpWorkbenchTransport.sessions.list(projectId);
      if (sessionsRequestIdRef.current !== requestId) return;
      if (activeProjectRef.current?.id !== projectId) return;
      sessionsRef.current = nextSessions;
      setSessions(nextSessions);
      setActiveSession((current) => {
        const currentSession = current
          ? nextSessions.find(
              (session) =>
                session.id === current.id && (!worktreeId || session.worktreeId === worktreeId),
            )
          : null;
        return currentSession ?? selectPreferredMobileSession(nextSessions, worktreeId);
      });
    } catch (reason) {
      if (sessionsRequestIdRef.current !== requestId) return;
      setError(getErrorMessage(reason));
    }
  }, [activeProject, activeWorktree?.id]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   Worktree 创建、删除、提交、推送或合并后，移动端所有面板都要重新读取后端权威 worktree 列表。
   *
   * Code Logic（这个函数做什么）:
   *   调用 worktrees.list(projectId)，用 request id 防止旧项目响应覆盖；若当前 active worktree 已不存在则经 dirty guard 选择主工作区或首项。
   */
  const refreshWorktrees = useCallback(async (
    options: MobileRefreshWorktreesOptions = {},
  ): Promise<void> => {
    if (!activeProject) return;
    const projectId = activeProject.id;
    if (options.expectedProjectId && options.expectedProjectId !== projectId) return;
    const endWorktreeOperation = beginWorktreeOperation();
    const requestId = worktreesRequestIdRef.current + 1;
    worktreesRequestIdRef.current = requestId;

    try {
      const nextWorktrees = await httpWorkbenchTransport.worktrees.list(projectId);
      if (worktreesRequestIdRef.current !== requestId) return;
      if (activeProjectRef.current?.id !== projectId) return;
      if (options.expectedProjectId && activeProjectRef.current?.id !== options.expectedProjectId) {
        return;
      }
      const current = activeWorktreeRef.current;
      runMobileWorktreeRefreshFlow({
        nextWorktrees,
        currentActiveWorktreeId: current?.id ?? null,
        skipActivePreflight: options.skipFileContextConfirm,
        confirmActiveWorktreeChange: (nextActive) =>
          confirmFileContextSwitch(createMobileFilePanelContext(activeProject, nextActive)),
        applyRefresh: (plan) => {
          setWorktrees(plan.nextWorktrees);
          setActiveWorktreeWithSession(plan.nextActive);
        },
      });
    } catch (reason) {
      if (worktreesRequestIdRef.current !== requestId) return;
      setError(getErrorMessage(reason));
    } finally {
      endWorktreeOperation();
    }
  }, [activeProject, beginWorktreeOperation, confirmFileContextSwitch, setActiveWorktreeWithSession]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户选择本机或远端项目后，都需要进入可管理终端、worktree、文件、Git 和自动化的移动端工作台。
   *
   * Code Logic（这个函数做什么）:
   *   未支持项目类型直接提示；支持的 local/remote 项目并行请求 worktrees/sessions，选择默认 worktree/session 后切到 terminal。
   */
  const selectProject = useCallback(async (project: WorkbenchProject): Promise<void> => {
    if (!canSelectMobileProject(project)) {
      projectDetailsRequestIdRef.current += 1;
      setProjectDetailsLoading(false);
      setError(t('workbench:mobile.projectPanel.unsupportedProjectKind'));
      return;
    }
    if (activeProject?.id === project.id) {
      setPanel('terminal');
      return;
    }
    if (!confirmFileContextSwitch(createMobileFilePanelContext(project, null))) {
      return;
    }

    worktreesRequestIdRef.current += 1;
    sessionsRequestIdRef.current += 1;
    setError(null);
    activeProjectRef.current = project;
    setActiveProject(project);
    setWorktrees([]);
    activeWorktreeRef.current = null;
    setActiveWorktree(null);
    setSessions([]);
    sessionsRef.current = [];
    setActiveSession(null);

    const requestId = projectDetailsRequestIdRef.current + 1;
    projectDetailsRequestIdRef.current = requestId;
    setProjectDetailsLoading(true);

    try {
      const [nextWorktrees, nextSessions] = await Promise.all([
        httpWorkbenchTransport.worktrees.list(project.id),
        httpWorkbenchTransport.sessions.list(project.id),
      ]);
      if (projectDetailsRequestIdRef.current !== requestId) return;

      const nextActiveWorktree = selectPreferredMobileWorktree(nextWorktrees);
      const nextActiveSession = selectPreferredMobileSession(
        nextSessions,
        nextActiveWorktree?.id ?? null,
      );

      setWorktrees(nextWorktrees);
      activeWorktreeRef.current = nextActiveWorktree;
      setActiveWorktree(nextActiveWorktree);
      sessionsRef.current = nextSessions;
      setSessions(nextSessions);
      setActiveSession(nextActiveSession);
      setPanel('terminal');
    } catch (reason) {
      if (projectDetailsRequestIdRef.current !== requestId) return;
      setError(getErrorMessage(reason));
    } finally {
      if (projectDetailsRequestIdRef.current === requestId) {
        setProjectDetailsLoading(false);
      }
    }
  }, [activeProject?.id, confirmFileContextSwitch, t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   项目列表面板的刷新按钮需要触发异步加载，但按钮事件本身不消费 Promise。
   *
   * Code Logic（这个函数做什么）:
   *   调用 loadProjects 并显式丢弃 Promise，错误由 loadProjects 内部写入状态。
   */
  const handleRefreshProjects = useCallback((): void => {
    void loadProjects();
  }, [loadProjects]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   远端快捷方式在移动端当前只支持自动化代理链路，导航点击本机专属面板时需要回到可用面板。
   *
   * Code Logic（这个函数做什么）:
   *   复用 selectMobilePanelForProject 按当前 activeProject 规整目标面板，然后写入 panel state。
   */
  const handlePanelChange = useCallback(
    (nextPanel: MobileWorkbenchPanel): void => {
      setPanel(selectMobilePanelForProject(activeProject, nextPanel));
    },
    [activeProject],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户点击顶部 worktree pill 时，需要从任意面板打开轻量 quick switch，快速查看和切换本机 worktree。
   *
   * Code Logic（这个函数做什么）:
   *   复用 canOpenMobileWorktreeSwitcher 判断当前项目和加载态；不可打开时忽略点击，可打开时置 open state。
   */
  const handleOpenWorktreeSwitcher = useCallback((): void => {
    if (worktreeOperationBusyRef.current) return;
    if (!canOpenMobileWorktreeSwitcher(activeProject, projectDetailsLoading)) return;
    setWorktreeSwitcherOpen(true);
  }, [activeProject, projectDetailsLoading]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   quick switch sheet 的关闭按钮、遮罩和 Escape 都需要统一关闭入口。
   *
   * Code Logic（这个函数做什么）:
   *   将 worktreeSwitcherOpen 写为 false，不改变当前 panel/worktree/session。
   */
  const handleCloseWorktreeSwitcher = useCallback((): void => {
    setWorktreeSwitcherOpen(false);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   quick switch 的“管理”入口要跳转到完整 Worktrees 面板，但面板状态由父级持有。
   *
   * Code Logic（这个函数做什么）:
   *   接收 MobileWorkbenchPanel 并写入当前 panel state。
   */
  const handleQuickSwitchPanelChange = useCallback((nextPanel: MobileWorkbenchPanel): void => {
    setPanel(selectMobilePanelForProject(activeProjectRef.current, nextPanel));
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   quick switch 内的刷新按钮需要按当前项目刷新 worktree 列表，并避免旧项目响应污染新项目。
   *
   * Code Logic（这个函数做什么）:
   *   从 activeProjectRef 构造 expectedProjectId options；没有当前项目时调用默认刷新让 refreshWorktrees 自行忽略。
   */
  const handleRefreshQuickSwitchWorktrees = useCallback((): Promise<void> | void => {
    const expectedProjectId = activeProjectRef.current?.id;
    if (expectedProjectId) {
      return refreshWorktrees({ expectedProjectId });
    }
    return refreshWorktrees();
  }, [refreshWorktrees]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户切换 worktree 后，移动端状态栏和终端面板应同步到同一 worktree 的优先 session。
   *
   * Code Logic（这个函数做什么）:
   *   写入 active worktree，并从当前 sessions 中选择匹配 session、running session 或首项。
   */
  const handleSelectWorktree = useCallback(
    (worktree: WorkbenchWorktree): boolean => {
      if (worktreeOperationBusyRef.current || projectDetailsLoading) return false;
      if (!confirmFileContextSwitch(createMobileFilePanelContext(activeProject, worktree))) {
        return false;
      }
      setActiveWorktreeWithSession(worktree);
      return true;
    },
    [activeProject, confirmFileContextSwitch, projectDetailsLoading, setActiveWorktreeWithSession],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户在 Worktrees 面板点击工作区卡片后，希望成功切换 active worktree 并直接进入对应终端工作现场。
   *
   * Code Logic（这个函数做什么）:
   *   复用受 dirty guard 保护的 worktree 选择逻辑；只有选择成功时才按 helper 切到 terminal 面板。
   */
  const handleOpenWorktreeWorkspace = useCallback(
    (worktree: WorkbenchWorktree): boolean => {
      const accepted = handleSelectWorktree(worktree);
      const nextPanel = selectMobileWorktreeWorkspacePanel(accepted);
      if (nextPanel) {
        setPanel(nextPanel);
      }
      return accepted;
    },
    [handleSelectWorktree],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   worktree 面板本地增删列表后，父组件必须同步列表；active 切换由受 dirty guard 保护的回调单独处理。
   *
   * Code Logic（这个函数做什么）:
   *   只写入 worktree 列表，避免绕过 Files dirty snapshot 直接改变 active worktree。
   */
  const handleWorktreesChange = useCallback((nextWorktrees: WorkbenchWorktree[]): void => {
    const currentProjectId = activeProjectRef.current?.id ?? null;
    if (
      currentProjectId &&
      nextWorktrees.some((worktree) => worktree.projectId !== currentProjectId)
    ) {
      return;
    }
    setWorktrees(nextWorktrees);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   删除或 merge active worktree 前只需要确认用户愿意离开 Files 草稿，不能提前切换上下文或清空草稿。
   *
   * Code Logic（这个函数做什么）:
   *   为 destructive 操作提供 confirm-only callback；确认取消时调用方不得继续后端操作。
   */
  const handleConfirmActiveWorktreeChange = useCallback(
    (worktree: WorkbenchWorktree | null): boolean =>
      confirmFileContextSwitchOnly(createMobileFilePanelContext(activeProject, worktree)),
    [activeProject, confirmFileContextSwitchOnly],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   destructive worktree 后端操作成功后，移动端才可以丢弃旧 Files 草稿并切到回落 worktree。
   *
   * Code Logic（这个函数做什么）:
   *   清理 Files dirty snapshot/discard token，然后同步 active worktree 与匹配 session。
   */
  const handleApplyActiveWorktreeChange = useCallback(
    (worktree: WorkbenchWorktree | null): void => {
      discardConfirmedFileContextSwitch();
      setActiveWorktreeWithSession(worktree);
    },
    [discardConfirmedFileContextSwitch, setActiveWorktreeWithSession],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   Git 操作返回新的 worktree DTO 后，移动端需要更新列表和当前 active 状态，而不必等待下一轮全量刷新。
   *
   * Code Logic（这个函数做什么）:
   *   用返回的 worktree 替换同 id 项；若它是当前 active worktree，同时更新 active worktree ref/state。
   */
  const handleWorktreeChange = useCallback(
    (updatedWorktree: WorkbenchWorktree): void => {
      if (activeProjectRef.current?.id !== updatedWorktree.projectId) return;
      setWorktrees((current) =>
        current.map((worktree) =>
          worktree.id === updatedWorktree.id ? updatedWorktree : worktree,
        ),
      );
      if (activeWorktreeRef.current?.id === updatedWorktree.id) {
        activeWorktreeRef.current = updatedWorktree;
        setActiveWorktree(updatedWorktree);
      }
    },
    [],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   destructive 操作成功应用时需要用最新 active ref 判断源 worktree 是否在操作期间变成 active。
   *
   * Code Logic（这个函数做什么）:
   *   比较传入 worktree id 与当前 activeWorktreeRef；不依赖渲染时闭包里的旧 activeWorktreeId。
   */
  const isCurrentActiveWorktree = useCallback((worktree: WorkbenchWorktree): boolean => {
    return activeWorktreeRef.current?.id === worktree.id;
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   mobile Git merge 会删除源 worktree，必须先确认用户合并意图；只有合并 active worktree 时才需要处理 Files dirty 草稿切换。
   *
   * Code Logic（这个函数做什么）:
   *   先执行 merge confirm，再用 destructive merge helper 串联可选 dirty guard、HTTP merge、成功后移除源列表并按计划刷新 worktree。
   */
  const handleMergeWorktree = useCallback(
    async (sourceWorktree: WorkbenchWorktree): Promise<boolean> => {
      if (worktreeOperationBusyRef.current || projectDetailsLoading) return false;
      const shouldMerge = window.confirm(
        t('workbench:worktrees.mergeConfirm', { name: sourceWorktree.name }),
      );
      if (!shouldMerge) return false;

      const operationProjectId = activeProjectRef.current?.id ?? null;
      const endWorktreeOperation = beginWorktreeOperation();
      try {
        const result = await runMobileWorktreeMergeFlow({
          worktrees,
          activeWorktreeId: activeWorktreeRef.current?.id ?? null,
          sourceWorktree,
          confirmActiveWorktreeChange: (nextActive) =>
            handleConfirmActiveWorktreeChange(nextActive),
          mergeWorktree: async () => {
            await httpWorkbenchTransport.worktrees.merge(sourceWorktree.id);
          },
          applyMergeSuccess: async (plan) => {
            if (activeProjectRef.current?.id !== operationProjectId) return;
            const appliedState = getMobileWorktreeMergeAppliedState(plan);
            const sourceBecameActive = activeWorktreeRef.current?.id === sourceWorktree.id;
            setWorktrees(appliedState.nextWorktrees);
            if (plan.requiresActivePreflight || sourceBecameActive) {
              discardConfirmedFileContextSwitch();
              setActiveWorktreeWithSession(appliedState.nextActive);
            }
            await refreshWorktrees({
              skipFileContextConfirm: plan.requiresActivePreflight || sourceBecameActive,
              expectedProjectId: operationProjectId ?? undefined,
            });
          },
        });
        return result === 'applied';
      } finally {
        endWorktreeOperation();
      }
    },
    [
      beginWorktreeOperation,
      discardConfirmedFileContextSwitch,
      handleConfirmActiveWorktreeChange,
      projectDetailsLoading,
      refreshWorktrees,
      setActiveWorktreeWithSession,
      t,
      worktrees,
    ],
  );

  /* eslint-disable react-hooks/set-state-in-effect -- 移动端入口挂载时需要加载最近项目列表 */
  useEffect(() => {
    void loadProjects();

    return () => {
      projectsRequestIdRef.current += 1;
      projectDetailsRequestIdRef.current += 1;
      worktreesRequestIdRef.current += 1;
      sessionsRequestIdRef.current += 1;
    };
  }, [loadProjects]);
  /* eslint-enable react-hooks/set-state-in-effect */

  const panelContent =
    panel === 'projects' ? (
      <MobileProjectPanel
        projects={projects}
        activeProjectId={activeProject?.id ?? null}
        loading={projectsLoading}
        error={error}
        onSelect={selectProject}
        onRefresh={handleRefreshProjects}
      />
    ) : panel === 'worktrees' ? (
      <MobileWorktreePanel
        project={activeProject}
        worktrees={worktrees}
        activeWorktreeId={activeWorktree?.id ?? null}
        busy={worktreeControlsBusy}
        onSelect={handleOpenWorktreeWorkspace}
        onWorktreesChange={handleWorktreesChange}
        onConfirmActiveWorktreeChange={handleConfirmActiveWorktreeChange}
        onActiveWorktreeChange={handleApplyActiveWorktreeChange}
        onRefreshWorktrees={refreshWorktrees}
        onMergeWorktree={handleMergeWorktree}
        onBeginWorktreeOperation={beginWorktreeOperation}
        onIsWorktreeActive={isCurrentActiveWorktree}
      />
    ) : panel === 'files' ? null : panel === 'git' ? (
      <MobileGitPanel
        project={activeProject}
        worktree={activeWorktree}
        busy={worktreeControlsBusy}
        onWorktreeChange={handleWorktreeChange}
        onMergeWorktree={handleMergeWorktree}
        onRefreshWorktrees={refreshWorktrees}
      />
    ) : panel === 'prompt' ? (
      <MobilePromptPanel worktree={activeWorktree} session={activeSession} />
    ) : panel === 'automation' ? (
      <MobileAutomationPanel project={activeProject} />
    ) : panel === 'terminal' ? (
      <MobileTerminalPanel
        project={activeProject}
        worktree={activeWorktree}
        sessions={sessions}
        activeSession={activeSession}
        busy={projectDetailsLoading}
        onSessionsChange={handleSessionsChange}
        onActiveSessionChange={setActiveSession}
        onRefreshSessions={refreshSessions}
      />
    ) : (
      <section className={styles.panel} aria-labelledby="mobile-panel-title">
        <div className={styles.panelHeader}>
          <p className={styles.panelKicker}>{t('workbench:mobile.kicker')}</p>
          <h1 id="mobile-panel-title">{placeholder.title}</h1>
        </div>
        {projectDetailsLoading ? (
          <p className={styles.panelState}>{t('workbench:loading')}</p>
        ) : (
          <div className={styles.placeholder}>{placeholder.label}</div>
        )}
        {error ? (
          <p className={styles.panelError}>
            <span>{t('workbench:mobile.projectPanel.error')}</span>
            <span>{error}</span>
          </p>
        ) : null}
      </section>
    );

  return (
    <MobileWorkbenchShell
      panel={panel}
      project={activeProject?.name ?? null}
      worktree={activeWorktree?.name ?? null}
      session={activeSession?.name ?? null}
      worktreeStatusDisabled={!canOpenMobileWorktreeSwitcher(activeProject, worktreeControlsBusy)}
      worktreeStatusExpanded={worktreeSwitcherOpen}
      onWorktreeStatusClick={handleOpenWorktreeSwitcher}
      onPanelChange={handlePanelChange}
    >
      {panelContent}
      <MobileWorktreeQuickSwitch
        open={worktreeSwitcherOpen}
        project={activeProject}
        worktrees={worktrees}
        activeWorktreeId={activeWorktree?.id ?? null}
        busy={worktreeControlsBusy}
        t={t}
        onClose={handleCloseWorktreeSwitcher}
        onSelect={handleSelectWorktree}
        onPanelChange={handleQuickSwitchPanelChange}
        onRefresh={handleRefreshQuickSwitchWorktrees}
      />
      <div hidden={panel !== 'files'}>
        <MobileFilesPanel
          project={activeProject}
          worktree={activeWorktree}
          discardContextToken={filesDiscardContextToken}
          onDirtyContextChange={setFilesDirtySnapshot}
        />
      </div>
    </MobileWorkbenchShell>
  );
}
