import { lazy, Suspense, useCallback, useEffect, useRef, useState } from 'react';
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import {
  createHttpOrchestratorClientRequestId,
  httpWorkbenchTransport,
  workbenchHttp,
} from '@/api/workbenchHttp';
import {
  isMutationSucceeded,
  isMutationUnknown,
  WorkbenchMutationUnknownError,
} from '@/lib/asyncState/mutationOutcome';
import { useAttention } from '@/hooks/attentionContext';
import { useWorkbenchHttpEvents } from '@/hooks/useWorkbenchHttpEvents';
import { useWorkbenchTerminalBuffers } from '@/hooks/workbenchTerminalBuffersContext';
import type {
  AttentionItem,
  MutationIntent,
  WorkbenchProject,
  WorkbenchSession,
  WorkbenchSessionStatus,
  WorkbenchWorktree,
} from '@/lib/types';
import type { AgentSessionRuntimeDto } from '@/lib/types/agentRuntime';
import {
  buildMergeRemoveAuthority,
  reconcileWorkbenchMutation,
} from '@/lib/workbenchMutationReconciliation';
import type { MobileAutomationExecutionContext } from './components/MobileAutomationPanel';
import { MobileAttentionPanel } from './components/MobileAttentionPanel';
import { MobileProjectPanel } from './components/MobileProjectPanel';
import { MobileWorkbenchShell } from './components/MobileWorkbenchShell';
import { MobileWorktreeTabs, type MobileWorktreeTabsProps } from './components/MobileWorktreeTabs';
import { useMobileWorktreeBarController } from './controllers/useMobileWorktreeBarController';
import {
  mapMobileAttentionTarget,
  resolveMobileAttentionMissingTargetPanel,
  type MobileAttentionNavigation,
} from './mobileAttentionTarget';
import {
  applyMobileAgentRuntimeEvent,
  applyKnownMobileSessionUpdatedEvent,
  applyMobileTerminalStatusEvent,
  canOpenMobileWorktreeSwitcher,
  canSelectMobileProject,
  emptyMobileSessionRuntimeState,
  getInitialMobileWorkbenchPanel,
  getMobileConnectionCachedAt,
  markMobileConnectionOffline,
  markMobileConnectionOnline,
  mergeMobileSessionsWithRuntime,
  seedMobileSessionRuntimeFromSessions,
  selectMobilePanelForProject,
  selectMobileWorktreeWorkspacePanel,
  selectPreferredMobileSession,
  selectPreferredMobileWorktree,
  shouldRefreshMobilePanelOnReconnect,
  shouldShowMobileWorktreeStrip,
  shouldSkipMobileProjectReload,
  type MobileConnectionState,
  type MobileProjectDetailStatus,
  type MobileSessionRuntimeState,
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

/**
 * Business Logic（为什么需要这些 lazy 边界）:
 *   `/mobile` 默认只展示 Projects shell；terminal/files/automation/browser 等重面板
 *   若同步 import 会把 xterm 等打入 mobile initial graph，违反 280 KiB 与 forbidden 合同。
 *
 * Code Logic（这些常量做什么）:
 *   对重面板使用 React.lazy + 动态 import 适配 named export；
 *   轻量 Project/Attention/Shell/QuickSwitch 保持同步。
 */
const MobileAutomationPanel = lazy(() =>
  import('./components/MobileAutomationPanel').then((module) => ({
    default: module.MobileAutomationPanel,
  })),
);
const MobileBrowserPanel = lazy(() =>
  import('./components/MobileBrowserPanel').then((module) => ({
    default: module.MobileBrowserPanel,
  })),
);
const MobileFilesPanel = lazy(() =>
  import('./components/MobileFilesPanel').then((module) => ({
    default: module.MobileFilesPanel,
  })),
);
const MobileGitPanel = lazy(() =>
  import('./components/MobileGitPanel').then((module) => ({
    default: module.MobileGitPanel,
  })),
);
const MobilePromptPanel = lazy(() =>
  import('./components/MobilePromptPanel').then((module) => ({
    default: module.MobilePromptPanel,
  })),
);
const MobileSettingsPanel = lazy(() =>
  import('./components/MobileSettingsPanel').then((module) => ({
    default: module.MobileSettingsPanel,
  })),
);
const MobileProviderPanel = lazy(() =>
  import('./components/MobileProviderPanel').then((module) => ({
    default: module.MobileProviderPanel,
  })),
);
const MobileTerminalPanel = lazy(() =>
  import('./components/MobileTerminalPanel').then((module) => ({
    default: module.MobileTerminalPanel,
  })),
);
const MobileWorktreePanel = lazy(() =>
  import('./components/MobileWorktreePanel').then((module) => ({
    default: module.MobileWorktreePanel,
  })),
);

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
  const { store: terminalBufferStore } = useWorkbenchTerminalBuffers();
  const [panel, setPanel] = useState<MobileWorkbenchPanel>(() => getInitialMobileWorkbenchPanel());
  const [projects, setProjects] = useState<WorkbenchProject[]>([]);
  const [activeProject, setActiveProject] = useState<WorkbenchProject | null>(null);
  const [worktrees, setWorktrees] = useState<WorkbenchWorktree[]>([]);
  const [activeWorktree, setActiveWorktree] = useState<WorkbenchWorktree | null>(null);
  const [sessions, setSessions] = useState<WorkbenchSession[]>([]);
  const [sessionRuntime, setSessionRuntime] = useState<MobileSessionRuntimeState>(() =>
    emptyMobileSessionRuntimeState(),
  );
  const [activeSession, setActiveSession] = useState<WorkbenchSession | null>(null);
  const [projectsLoading, setProjectsLoading] = useState<boolean>(false);
  const [projectDetailStatus, setProjectDetailStatus] =
    useState<MobileProjectDetailStatus>('idle');
  const [connectionState, setConnectionState] = useState<MobileConnectionState | null>(null);
  const [worktreeOperationBusy, setWorktreeOperationBusy] = useState<boolean>(false);
  /**
   * @deprecated quick switch sheet 暂未挂载（移动端 worktree 切换已迁移到 shell 固定 `MobileWorktreeTabs`），
   *   本 state 仅保留以备未来 panel 内快捷刷新入口接入；当前始终为 false。grep
   *   `MobileWorktreeQuickSwitch` 可见全部调用点。
   */
  const [worktreeSwitcherOpen, setWorktreeSwitcherOpen] = useState<boolean>(false);
  void worktreeSwitcherOpen;
  void setWorktreeSwitcherOpen;
  // files 首次打开后保持挂载（hidden），以便 dirty snapshot 在切走后仍可用于 context guard
  const [filesPanelMounted, setFilesPanelMounted] = useState<boolean>(
    () => getInitialMobileWorkbenchPanel() === 'files',
  );
  const [filesDirtySnapshot, setFilesDirtySnapshot] = useState<MobileFileDirtySnapshot>({
    dirty: false,
    context: null,
  });
  const [filesDiscardContextToken, setFilesDiscardContextToken] = useState<number>(0);
  const [error, setError] = useState<string | null>(null);
  const [attentionNotice, setAttentionNotice] = useState<string | null>(null);
  const [attentionFocusTaskId, setAttentionFocusTaskId] = useState<string | null>(null);
  const [attentionFocusOutboxId, setAttentionFocusOutboxId] = useState<string | null>(null);
  const projectsRequestIdRef = useRef<number>(0);
  const projectDetailsRequestIdRef = useRef<number>(0);
  const projectDetailsAbortRef = useRef<AbortController | null>(null);
  const worktreesRequestIdRef = useRef<number>(0);
  const sessionsRequestIdRef = useRef<number>(0);
  const sessionsRef = useRef<WorkbenchSession[]>(sessions);
  const activeProjectIdRef = useRef<string | null>(null);

  useEffect(() => {
    activeProjectIdRef.current = activeProject?.id ?? null;
  }, [activeProject?.id]);

  // sessions 列表变化时在 render 阶段播种 runtime（保留已有 agent 投影），避免 setState-in-effect
  const [seededSessions, setSeededSessions] = useState(sessions);
  if (seededSessions !== sessions) {
    setSeededSessions(sessions);
    setSessionRuntime((prev) => seedMobileSessionRuntimeFromSessions(sessions, prev));
  }

  /**
   * Business Logic（为什么需要这个函数）:
   *   HTTP terminalStatus 需更新当前已知 session 的 status，并同步 activeSession。
   *
   * Code Logic（这个函数做什么）:
   *   applyMobileTerminalStatusEvent；合并 sessions 列表 status。
   */
  const handleTerminalStatusEvent = useCallback(
    (payload: { sessionId: string; status: string }): void => {
      const status = payload.status as WorkbenchSessionStatus;
      setSessionRuntime((prev) =>
        applyMobileTerminalStatusEvent(prev, payload.sessionId, status),
      );
      setSessions((prev) =>
        prev.map((session) =>
          session.id === payload.sessionId ? { ...session, status } : session,
        ),
      );
      setActiveSession((prev) =>
        prev && prev.id === payload.sessionId ? { ...prev, status } : prev,
      );
    },
    [],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   agentRuntime 事件只投影当前项目已知 session 的 Agent phase。
   *
   * Code Logic（这个函数做什么）:
   *   过滤 projectId；applyMobileAgentRuntimeEvent。
   */
  const handleAgentRuntimeEvent = useCallback(
    (payload: { agentSession: AgentSessionRuntimeDto }): void => {
      const dto = payload.agentSession;
      const projectId = activeProjectIdRef.current;
      if (projectId && dto.projectId !== projectId) return;
      setSessionRuntime((prev) => applyMobileAgentRuntimeEvent(prev, dto, 'live'));
    },
    [],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   agent 自动标题或手动 rename 后，Mobile 应立即刷新已加载 session 及当前标题，且不重连事件流。
   *
   * Code Logic（这个函数做什么）:
   *   以 sessionsRef 做已知 id fail-closed；命中后整体替换 DTO、同步 ref/state，
   *   activeSession 同 id 时切到列表中的新对象。
   */
  const handleSessionUpdatedEvent = useCallback((payload: WorkbenchSession): void => {
    const result = applyKnownMobileSessionUpdatedEvent(sessionsRef.current, null, payload);
    if (!result.applied) return;
    sessionsRef.current = result.sessions;
    setSessions(result.sessions);
    setActiveSession((current) =>
      current?.id === payload.id
        ? result.sessions.find((session) => session.id === payload.id) ?? current
        : current,
    );
  }, []);

  useWorkbenchHttpEvents({
    store: terminalBufferStore,
    enabled: true,
    terminalSessionId: activeSession?.status === 'running' ? activeSession.id : null,
    onTerminalStatus: handleTerminalStatusEvent,
    onSessionUpdated: handleSessionUpdatedEvent,
    onAgentRuntime: handleAgentRuntimeEvent,
  });
  useEffect(() => {
    const sessionId = activeSession?.status === 'running' ? activeSession.id : null;
    if (!sessionId) return undefined;
    return () => {
      // compare-and-clear：切换窗口或离开 Mobile Workbench 后停止旧远端窗口正文流。
      void httpWorkbenchTransport.sessions.focus(sessionId, false).catch(() => {
        // cleanup best-effort；下一次 focus 会以当前窗口重新建立过滤目标。
      });
    };
  }, [activeSession?.id, activeSession?.status]);
  const activeProjectRef = useRef<WorkbenchProject | null>(null);
  const activeWorktreeRef = useRef<WorkbenchWorktree | null>(null);
  const worktreeOperationBusyRef = useRef<boolean>(false);
  const worktreeOperationCountRef = useRef<number>(0);
  const projectsRef = useRef<WorkbenchProject[]>([]);
  const panelRef = useRef<MobileWorkbenchPanel>(getInitialMobileWorkbenchPanel());
  const connectionStateRef = useRef<MobileConnectionState | null>(null);
  const { t } = useTranslation(['workbench', 'attention']);
  const { snapshot: attentionSnapshot, refresh: refreshAttention } = useAttention();
  const attentionTotal = attentionSnapshot?.counts.total ?? null;
  const projectDetailsLoading = projectDetailStatus === 'loading';

  const panelPlaceholders: Record<MobileWorkbenchPanel, { title: string; label: string }> = {
    projects: {
      title: t('workbench:mobile.placeholders.projects.title'),
      label: t('workbench:mobile.placeholders.projects.label'),
    },
    attention: {
      title: t('workbench:mobile.placeholders.attention.title'),
      label: t('workbench:mobile.placeholders.attention.label'),
    },
    terminal: {
      title: t('workbench:mobile.placeholders.terminal.title'),
      label: t('workbench:mobile.placeholders.terminal.label'),
    },
    browser: {
      title: t('workbench:mobile.browser.title'),
      label: t('workbench:mobile.browser.title'),
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
    provider: {
      title: t('workbench:mobile.placeholders.provider.title'),
      label: t('workbench:mobile.placeholders.provider.label'),
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

  useEffect(() => {
    projectsRef.current = projects;
  }, [projects]);

  useEffect(() => {
    panelRef.current = panel;
  }, [panel]);

  useEffect(() => {
    connectionStateRef.current = connectionState;
  }, [connectionState]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   读成功时推进 online；失败时标 offline，供状态栏与恢复刷新判定。
   *
   * Code Logic（这个函数做什么）:
   *   success=true 写 online；否则写 offline（保留既有 since）。
   */
  const noteConnectionOutcome = useCallback((success: boolean, lastError?: string): void => {
    const now = Date.now();
    const prev = connectionStateRef.current;
    if (success) {
      const next = markMobileConnectionOnline(now);
      connectionStateRef.current = next;
      setConnectionState(next);
      return;
    }
    const next = markMobileConnectionOffline(lastError ?? 'offline', now, prev);
    connectionStateRef.current = next;
    setConnectionState(next);
  }, []);

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
      noteConnectionOutcome(true);
    } catch (reason) {
      if (projectsRequestIdRef.current !== requestId) return;
      const message = getErrorMessage(reason);
      setError(message);
      noteConnectionOutcome(false, message);
    } finally {
      if (projectsRequestIdRef.current === requestId) {
        setProjectsLoading(false);
      }
    }
  }, [noteConnectionOutcome]);

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
  const selectProject = useCallback(async (
    project: WorkbenchProject,
    options: { nextPanel?: MobileWorkbenchPanel; forceReload?: boolean } = {},
  ): Promise<boolean> => {
    const nextPanel = options.nextPanel ?? 'terminal';
    if (!canSelectMobileProject(project)) {
      projectDetailsRequestIdRef.current += 1;
      projectDetailsAbortRef.current?.abort();
      setProjectDetailStatus('idle');
      setError(t('workbench:mobile.projectPanel.unsupportedProjectKind'));
      return false;
    }
    // 同项目早退仅 ready；error/loading 必须允许重试
    if (
      !options.forceReload &&
      shouldSkipMobileProjectReload(activeProject?.id ?? null, project.id, projectDetailStatus)
    ) {
      setPanel(nextPanel);
      return true;
    }
    if (
      activeProject?.id !== project.id &&
      !confirmFileContextSwitch(createMobileFilePanelContext(project, null))
    ) {
      return false;
    }

    worktreesRequestIdRef.current += 1;
    sessionsRequestIdRef.current += 1;
    setError(null);
    activeProjectRef.current = project;
    setActiveProject(project);
    if (activeProject?.id !== project.id) {
      setWorktrees([]);
      activeWorktreeRef.current = null;
      setActiveWorktree(null);
      setSessions([]);
      sessionsRef.current = [];
      setActiveSession(null);
    }

    projectDetailsAbortRef.current?.abort();
    const abortController = new AbortController();
    projectDetailsAbortRef.current = abortController;

    const requestId = projectDetailsRequestIdRef.current + 1;
    projectDetailsRequestIdRef.current = requestId;
    setProjectDetailStatus('loading');

    try {
      const [nextWorktrees, nextSessions] = await Promise.all([
        httpWorkbenchTransport.worktrees.list(project.id),
        httpWorkbenchTransport.sessions.list(project.id),
      ]);
      if (abortController.signal.aborted) return false;
      if (projectDetailsRequestIdRef.current !== requestId) return false;

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
      setPanel(nextPanel);
      setProjectDetailStatus('ready');
      noteConnectionOutcome(true);
      return true;
    } catch (reason) {
      if (abortController.signal.aborted) return false;
      if (projectDetailsRequestIdRef.current !== requestId) return false;
      const message = getErrorMessage(reason);
      setError(message || t('workbench:mobile.projectPanel.detailError'));
      setProjectDetailStatus('error');
      noteConnectionOutcome(false, message);
      return false;
    }
  }, [
    activeProject?.id,
    confirmFileContextSwitch,
    noteConnectionOutcome,
    projectDetailStatus,
    t,
  ]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   详情 error 后用户必须能显式重试当前项目，而不是同项目 early return。
   *
   * Code Logic（这个函数做什么）:
   *   对 activeProject forceReload selectProject。
   */
  const handleReloadProjectDetails = useCallback((): void => {
    const project = activeProjectRef.current;
    if (!project) return;
    void selectProject(project, { forceReload: true, nextPanel: panelRef.current });
  }, [selectProject]);

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
   *   项目绑定面板在无 active project 时回落到项目列表；有项目时进入对应工作台面板。
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
   *   项目工作台导航需要一键回到全局项目列表，与桌面离开当前工作台上下文对齐。
   *
   * Code Logic（这个函数做什么）:
   *   将 panel 置为 projects（导航模式随之切回 global）。
   */
  const handleBackToProjects = useCallback((): void => {
    setPanel('projects');
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户点击顶部 worktree pill 时，需要从任意面板打开轻量 quick switch，快速查看和切换本机 worktree。
   *
   * Code Logic（这个函数做什么）:
   *   复用 canOpenMobileWorktreeSwitcher 判断当前项目和加载态；不可打开时忽略点击，可打开时置 open state。
   *
   * @deprecated 移动端 worktree 切换已迁移到 `MobileWorktreeTabs`（终端窗口 tab 上方 + files/browser/git shell chrome）；
   *   当前 quick switch sheet 暂未挂载，本 handler 仅保留以备未来 panel 内快捷刷新入口接入。grep
   *   `MobileWorktreeQuickSwitch` 可见全部调用点。
   */
  const handleOpenWorktreeSwitcher = useCallback((): void => {
    if (worktreeOperationBusyRef.current) return;
    if (!canOpenMobileWorktreeSwitcher(activeProject, projectDetailsLoading)) return;
    setWorktreeSwitcherOpen(true);
  }, [activeProject, projectDetailsLoading]);
  void handleOpenWorktreeSwitcher;

  /**
   * Business Logic（为什么需要这个函数）:
   *   quick switch sheet 的关闭按钮、遮罩和 Escape 都需要统一关闭入口。
   *
   * Code Logic（这个函数做什么）:
   *   将 worktreeSwitcherOpen 写为 false，不改变当前 panel/worktree/session。
   *
   * @deprecated 与 `handleOpenWorktreeSwitcher` 同步保留；sheet 重新挂载时复用。
   */
  const handleCloseWorktreeSwitcher = useCallback((): void => {
    setWorktreeSwitcherOpen(false);
  }, []);
  void handleCloseWorktreeSwitcher;

  /**
   * Business Logic（为什么需要这个函数）:
   *   quick switch 的“管理”入口要跳转到完整 Worktrees 面板，但面板状态由父级持有。
   *
   * Code Logic（这个函数做什么）:
   *   接收 MobileWorkbenchPanel 并写入当前 panel state。
   *
   * @deprecated 与 `handleOpenWorktreeSwitcher` 同步保留。
   */
  const handleQuickSwitchPanelChange = useCallback((nextPanel: MobileWorkbenchPanel): void => {
    setPanel(selectMobilePanelForProject(activeProjectRef.current, nextPanel));
  }, []);
  void handleQuickSwitchPanelChange;

  /**
   * Business Logic（为什么需要这个函数）:
   *   quick switch 内的刷新按钮需要按当前项目刷新 worktree 列表，并避免旧项目响应污染新项目。
   *
   * Code Logic（这个函数做什么）:
   *   从 activeProjectRef 构造 expectedProjectId options；没有当前项目时调用默认刷新让 refreshWorktrees 自行忽略。
   *
   * @deprecated 与 `handleOpenWorktreeSwitcher` 同步保留。
   */
  const handleRefreshQuickSwitchWorktrees = useCallback((): Promise<void> | void => {
    const expectedProjectId = activeProjectRef.current?.id;
    if (expectedProjectId) {
      return refreshWorktrees({ expectedProjectId });
    }
    return refreshWorktrees();
  }, [refreshWorktrees]);
  void handleRefreshQuickSwitchWorktrees;

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

  const applyWorktreeBarCreated = useCallback(
    (
      nextWorktrees: WorkbenchWorktree[],
      nextActive: WorkbenchWorktree | null,
      session: WorkbenchSession | null,
    ): void => {
      handleWorktreesChange(nextWorktrees);
      if (session) {
        handleSessionsChange([
          ...sessionsRef.current.filter((item) => item.id !== session.id),
          session,
        ]);
      }
      setActiveWorktreeWithSession(nextActive);
    },
    [handleSessionsChange, handleWorktreesChange, setActiveWorktreeWithSession],
  );

  const applyWorktreeBarRemoval = useCallback(
    (plan: {
      nextWorktrees: WorkbenchWorktree[];
      nextActive: WorkbenchWorktree | null;
      requiresActivePreflight: boolean;
    }): void => {
      handleWorktreesChange(plan.nextWorktrees);
      if (plan.requiresActivePreflight) {
        handleApplyActiveWorktreeChange(plan.nextActive);
      }
    },
    [handleApplyActiveWorktreeChange, handleWorktreesChange],
  );

  const worktreeBar = useMobileWorktreeBarController({
    project: activeProject,
    worktrees,
    activeWorktreeId: activeWorktree?.id ?? null,
    controlsBusy: worktreeControlsBusy,
    confirmSwitchToWorktree: (nextWorktree) =>
      confirmFileContextSwitch(createMobileFilePanelContext(activeProject, nextWorktree)),
    confirmActiveWorktreeChange: handleConfirmActiveWorktreeChange,
    applyCreated: applyWorktreeBarCreated,
    applyRemoval: applyWorktreeBarRemoval,
    beginWorktreeOperation,
    refreshWorktrees,
    refreshSessions,
  });

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
   *   mobile Git merge 会删除功能 worktree，或把主工作区可收集分支合入 home；必须先确认用户意图。
   *
   * Code Logic（这个函数做什么）:
   *   主工作区用 collect-merge 确认文案；再串联可选 dirty guard、HTTP merge、按 merge plan 刷新列表。
   */
  const handleMergeWorktree = useCallback(
    async (sourceWorktree: WorkbenchWorktree): Promise<boolean> => {
      if (worktreeOperationBusyRef.current || projectDetailsLoading) return false;
      const shouldMerge = window.confirm(
        sourceWorktree.isMain
          ? t('workbench:worktrees.mergeCollectConfirm', {
              home: sourceWorktree.homeBranch ?? 'main',
              names: sourceWorktree.collectibleBranches.join(', '),
              count: sourceWorktree.collectibleBranches.length,
            })
          : t('workbench:worktrees.mergeConfirm', { name: sourceWorktree.name }),
      );
      if (!shouldMerge) return false;

      const operationProjectId = activeProjectRef.current?.id ?? null;
      const clientOperationId = createHttpOrchestratorClientRequestId();
      const endWorktreeOperation = beginWorktreeOperation();
      try {
        const result = await runMobileWorktreeMergeFlow({
          worktrees,
          activeWorktreeId: activeWorktreeRef.current?.id ?? null,
          sourceWorktree,
          confirmActiveWorktreeChange: (nextActive) =>
            handleConfirmActiveWorktreeChange(nextActive),
          mergeWorktree: async () => {
            const envelope = await workbenchHttp.git.merge({
              worktreeId: sourceWorktree.id,
              clientOperationId,
            });
            if (isMutationSucceeded(envelope)) return;
            if (isMutationUnknown(envelope)) {
              const ledger = await workbenchHttp.git
                .getMutationOperation(envelope.clientOperationId)
                .catch(() => null);
              await refreshWorktrees({
                expectedProjectId: operationProjectId ?? undefined,
              });
              const intent: MutationIntent | null = ledger?.intent ?? null;
              if (ledger?.state === 'succeeded') return;
              if (ledger?.state === 'failed') {
                throw new Error(t('workbench:errors.mergeWorktree'));
              }
              if (
                (intent?.kind === 'merge' || intent?.kind === 'collectMerge')
                && operationProjectId
              ) {
                try {
                  const latest = await httpWorkbenchTransport.worktrees.list(operationProjectId);
                  let mainCommitHashes: string[] | undefined;
                  const main = latest.find((item) => item.isMain) ?? null;
                  if (main) {
                    try {
                      const mainCommits = await httpWorkbenchTransport.git.listCommits(
                        operationProjectId,
                        main.id,
                        100,
                      );
                      mainCommitHashes = mainCommits.map((commit) => commit.hash);
                    } catch {
                      mainCommitHashes = undefined;
                    }
                  }
                  const authority = buildMergeRemoveAuthority(intent, latest, {
                    mainCommitHashes,
                  });
                  const confirmed = reconcileWorkbenchMutation(intent, ledger, authority);
                  if (confirmed === 'confirmedSucceeded') return;
                  if (confirmed === 'confirmedFailed') {
                    throw new Error(t('workbench:errors.mergeWorktree'));
                  }
                } catch (inner) {
                  if (inner instanceof Error && inner.message === t('workbench:errors.mergeWorktree')) {
                    throw inner;
                  }
                  // keep unknown
                }
              } else if (intent) {
                const confirmed = reconcileWorkbenchMutation(intent, ledger, {});
                if (confirmed === 'confirmedSucceeded') return;
                if (confirmed === 'confirmedFailed') {
                  throw new Error(t('workbench:errors.mergeWorktree'));
                }
              }
              throw new WorkbenchMutationUnknownError(
                envelope.clientOperationId,
                t('workbench:errors.mutationUnknown'),
              );
            }
          },
          applyMergeSuccess: async (plan) => {
            if (activeProjectRef.current?.id !== operationProjectId) return;
            const appliedState = getMobileWorktreeMergeAppliedState(plan);
            const sourceBecameActive = activeWorktreeRef.current?.id === sourceWorktree.id;
            setWorktrees(appliedState.nextWorktrees);
            // collect-merge 留在主工作区，不能当成“源 worktree 被删”清掉 Files 草稿。
            if (
              !sourceWorktree.isMain
              && (plan.requiresActivePreflight || sourceBecameActive)
            ) {
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

  /**
   * Business Logic（为什么需要这个函数）:
   *   自动化任务详情里的“打开执行现场”需要切换到现有 Mobile terminal 面板，并聚焦任务绑定的 worktree/session。
   *
   * Code Logic（这个函数做什么）:
   *   校验任务仍属于当前项目；按 task worktreeId/sessionId 从父级权威列表中选择 active worktree/session，然后进入 terminal。
   */
  const handleOpenAutomationExecutionContext = useCallback(
    (context: MobileAutomationExecutionContext): void => {
      if (activeProjectRef.current?.id !== context.projectId) return;
      const nextSession = context.sessionId
        ? sessionsRef.current.find((session) => session.id === context.sessionId) ?? null
        : null;
      const nextWorktree = context.worktreeId
        ? worktrees.find((worktree) => worktree.id === context.worktreeId) ??
          activeWorktreeRef.current
        : nextSession?.worktreeId
          ? worktrees.find((worktree) => worktree.id === nextSession.worktreeId) ??
            activeWorktreeRef.current
          : activeWorktreeRef.current;

      activeWorktreeRef.current = nextWorktree;
      setActiveWorktree(nextWorktree);
      setActiveSession(
        nextSession ?? selectPreferredMobileSession(sessionsRef.current, nextWorktree?.id ?? null),
      );
      setPanel('terminal');
    },
    [worktrees],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   Attention 条目只导航到现有 Automation/Settings/Terminal，不在列表内执行副作用动作。
   *
   * Code Logic（这个函数做什么）:
   *   mapMobileAttentionTarget 后：settings 切 settings；agent → terminal；task/outbox/experiment → automation。
   */
  const handleOpenAttentionItem = useCallback(
    async (item: AttentionItem): Promise<void> => {
      const navigation: MobileAttentionNavigation = mapMobileAttentionTarget(item.target);
      setAttentionNotice(null);

      if (navigation.kind === 'settingsDependencies') {
        setAttentionFocusTaskId(null);
        setAttentionFocusOutboxId(null);
        setPanel('settings');
        return;
      }

      if (navigation.kind === 'agentHubAsset') {
        // Agent Hub 权威界面在桌面 `/agent-hub`；mobile 仅回到 Attention 列表，不执行动作。
        setAttentionFocusTaskId(null);
        setAttentionFocusOutboxId(null);
        setPanel('attention');
        return;
      }

      const project =
        projectsRef.current.find((entry) => entry.id === navigation.projectId) ?? null;
      if (!project) {
        setAttentionNotice(t('attention:resolvedOrChanged'));
        void refreshAttention();
        setPanel('attention');
        return;
      }

      if (navigation.kind === 'terminalSession') {
        setAttentionFocusTaskId(null);
        setAttentionFocusOutboxId(null);
        const selected = await selectProject(project, { nextPanel: 'terminal' });
        if (!selected) {
          setAttentionNotice(t('attention:resolvedOrChanged'));
          void refreshAttention();
          setPanel('attention');
          return;
        }
        // 选择项目后若 session 列表已有目标，聚焦对应 terminal window。
        const targetSession =
          sessionsRef.current.find((session) => session.id === navigation.sessionId) ?? null;
        if (targetSession) {
          setActiveSession(targetSession);
        }
        return;
      }

      setAttentionFocusTaskId(
        navigation.kind === 'automationTask' ? navigation.taskId : null,
      );
      setAttentionFocusOutboxId(
        navigation.kind === 'automationOutbox' ? navigation.outboxId : null,
      );
      const selected = await selectProject(project, { nextPanel: 'automation' });
      if (!selected) {
        setAttentionNotice(t('attention:resolvedOrChanged'));
        void refreshAttention();
        setPanel('attention');
      }
    },
    [refreshAttention, selectProject, t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   Automation 聚焦 task/outbox 失败（已解决）时必须刷新 Inbox 并回到 Attention。
   *
   * Code Logic（这个函数做什么）:
   *   missing 时清理 focus id、提示文案、refreshAttention、setPanel(attention)。
   */
  const handleAutomationFocusResult = useCallback(
    (result: {
      status: 'found' | 'missing';
      entity: 'task' | 'outbox';
      id: string;
    }): void => {
      if (result.status === 'found') return;
      setAttentionFocusTaskId(null);
      setAttentionFocusOutboxId(null);
      setAttentionNotice(t('attention:resolvedOrChanged'));
      void refreshAttention();
      setPanel(resolveMobileAttentionMissingTargetPanel({ status: 'missing', entity: result.entity }));
    },
    [refreshAttention, t],
  );

  /* eslint-disable react-hooks/set-state-in-effect -- 移动端入口挂载时需要加载最近项目列表 */
  useEffect(() => {
    void loadProjects();

    return () => {
      projectsRequestIdRef.current += 1;
      projectDetailsRequestIdRef.current += 1;
      projectDetailsAbortRef.current?.abort();
      worktreesRequestIdRef.current += 1;
      sessionsRequestIdRef.current += 1;
    };
  }, [loadProjects]);
  /* eslint-enable react-hooks/set-state-in-effect */

  /**
   * Business Logic（为什么需要这个 effect）:
   *   从 offline/reconnecting 恢复 online 后应刷新当前可见 panel 权威数据。
   *
   * Code Logic（这个 effect 做什么）:
   *   比较 prev connection 与 online；按 panel 刷新 projects/worktrees/sessions。
   */
  const previousConnectionKindRef = useRef<MobileConnectionState['kind'] | null>(null);
  useEffect(() => {
    const prevKind = previousConnectionKindRef.current;
    const nextKind = connectionState?.kind ?? null;
    previousConnectionKindRef.current = nextKind;
    if (!connectionState || connectionState.kind !== 'online') return;
    if (prevKind !== 'offline' && prevKind !== 'reconnecting') return;
    if (
      !shouldRefreshMobilePanelOnReconnect(
        prevKind === 'offline'
          ? { kind: 'offline', lastError: '', since: 0 }
          : { kind: 'reconnecting', attempt: 0, cachedSince: null },
        connectionState,
      )
    ) {
      return;
    }
    const currentPanel = panelRef.current;
    const projectId = activeProjectRef.current?.id;
    if (!projectId) {
      void loadProjects();
      return;
    }
    if (
      currentPanel === 'projects' ||
      currentPanel === 'settings' ||
      currentPanel === 'provider'
    )
      return;
    void refreshWorktrees({ expectedProjectId: projectId });
    if (currentPanel === 'terminal') {
      void refreshSessions();
    }
  }, [connectionState, loadProjects, refreshSessions, refreshWorktrees]);

  /* eslint-disable react-hooks/set-state-in-effect -- files 首次激活后保持挂载以保留 dirty 草稿 */
  useEffect(() => {
    if (panel === 'files') {
      setFilesPanelMounted(true);
    }
  }, [panel]);
  /* eslint-enable react-hooks/set-state-in-effect */

  const worktreeTabsProps: MobileWorktreeTabsProps = {
    worktrees,
    activeWorktreeId: activeWorktree?.id ?? null,
    projectId: activeProject?.id ?? null,
    controlsBusy: worktreeControlsBusy,
    createOpen: worktreeBar.createOpen,
    createPrefix: worktreeBar.createPrefix,
    createSuffix: worktreeBar.createSuffix,
    creating: worktreeBar.creating,
    removing: worktreeBar.removing,
    pendingRemoval: worktreeBar.pendingRemoval,
    error: worktreeBar.error,
    mutationPhase: worktreeBar.mutationPhase,
    onSelect: handleSelectWorktree,
    onOpenCreate: worktreeBar.openCreate,
    onCancelCreate: worktreeBar.cancelCreate,
    onPrefixChange: worktreeBar.setCreatePrefix,
    onSuffixChange: worktreeBar.setCreateSuffix,
    onCreate: () => {
      void worktreeBar.createWorktree();
    },
    onRequestRemove: worktreeBar.requestRemove,
    onCancelRemove: worktreeBar.cancelRemove,
    onConfirmRemove: () => {
      void worktreeBar.confirmRemove();
    },
    onRetryReconcile: () => {
      void worktreeBar.retryReconcile();
    },
  };

  const heavyPanelFallback = (
    <p className={styles.panelState} role="status">
      {t('workbench:loading')}
    </p>
  );

  const projectDetailRetry =
    projectDetailStatus === 'error' && activeProject ? (
      <button
        type="button"
        className={styles.secondaryButton}
        onClick={handleReloadProjectDetails}
      >
        {t('workbench:mobile.projectPanel.reload')}
      </button>
    ) : null;

  const panelContent =
    panel === 'projects' ? (
      <>
        <MobileProjectPanel
          projects={projects}
          activeProjectId={activeProject?.id ?? null}
          loading={projectsLoading || projectDetailStatus === 'loading'}
          error={error}
          onSelect={(project) => {
            void selectProject(project);
          }}
          onRefresh={handleRefreshProjects}
        />
        {projectDetailRetry}
      </>
    ) : panel === 'attention' ? (
      <MobileAttentionPanel
        onOpenItem={(item) => {
          void handleOpenAttentionItem(item);
        }}
        notice={attentionNotice}
      />
    ) : panel === 'worktrees' ? (
      <Suspense fallback={heavyPanelFallback}>
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
          onRefreshSessions={refreshSessions}
          onCreatedSession={(session) => {
            handleSessionsChange([
              ...sessionsRef.current.filter((item) => item.id !== session.id),
              session,
            ]);
          }}
          onMergeWorktree={handleMergeWorktree}
          onBeginWorktreeOperation={beginWorktreeOperation}
          onIsWorktreeActive={isCurrentActiveWorktree}
        />
      </Suspense>
    ) : panel === 'files' ? null : panel === 'git' ? (
      <Suspense fallback={heavyPanelFallback}>
        <MobileGitPanel
          project={activeProject}
          worktree={activeWorktree}
          busy={worktreeControlsBusy}
          onWorktreeChange={handleWorktreeChange}
          onMergeWorktree={handleMergeWorktree}
          onRefreshWorktrees={refreshWorktrees}
        />
      </Suspense>
    ) : panel === 'prompt' ? (
      <Suspense fallback={heavyPanelFallback}>
        <MobilePromptPanel worktree={activeWorktree} session={activeSession} />
      </Suspense>
    ) : panel === 'automation' ? (
      <Suspense fallback={heavyPanelFallback}>
        <MobileAutomationPanel
          project={activeProject}
          onOpenExecutionContext={handleOpenAutomationExecutionContext}
          focusTaskId={attentionFocusTaskId}
          focusOutboxId={attentionFocusOutboxId}
          onFocusResult={handleAutomationFocusResult}
        />
      </Suspense>
    ) : panel === 'settings' ? (
      <Suspense fallback={heavyPanelFallback}>
        <MobileSettingsPanel />
      </Suspense>
    ) : panel === 'provider' ? (
      <Suspense fallback={heavyPanelFallback}>
        <MobileProviderPanel />
      </Suspense>
    ) : panel === 'browser' ? (
      <Suspense fallback={heavyPanelFallback}>
        <MobileBrowserPanel
          transport={httpWorkbenchTransport}
          project={activeProject}
          worktree={activeWorktree}
        />
      </Suspense>
    ) : panel === 'terminal' ? (
      <Suspense fallback={heavyPanelFallback}>
        <MobileTerminalPanel
          project={activeProject}
          worktree={activeWorktree}
          worktreeBar={worktreeTabsProps}
          sessions={mergeMobileSessionsWithRuntime(sessions, sessionRuntime)}
          activeSession={activeSession}
          busy={projectDetailsLoading}
          sessionRuntime={sessionRuntime}
          onSessionsChange={handleSessionsChange}
          onActiveSessionChange={setActiveSession}
          onRefreshSessions={refreshSessions}
        />
      </Suspense>
    ) : (
      <section className={styles.panel} aria-labelledby="mobile-panel-title">
        <div className={styles.panelHeader}>
          <h1 id="mobile-panel-title">{placeholder.title}</h1>
        </div>
        {projectDetailsLoading ? (
          <p className={styles.panelState}>{t('workbench:loading')}</p>
        ) : (
          <div className={styles.placeholder}>{placeholder.label}</div>
        )}
        {error ? (
          <p className={styles.panelError} role="alert">
            <span>{t('workbench:mobile.projectPanel.error')}</span>
            <span>{error}</span>
          </p>
        ) : null}
        {projectDetailRetry}
      </section>
    );

  const connectionCachedAt = getMobileConnectionCachedAt(connectionState);

  return (
    <MobileWorkbenchShell
      panel={panel}
      project={activeProject?.name ?? null}
      worktree={activeWorktree?.name ?? null}
      session={activeSession?.name ?? null}
      hasActiveProject={activeProject != null && canSelectMobileProject(activeProject)}
      onPanelChange={handlePanelChange}
      onBackToProjects={handleBackToProjects}
      attentionTotal={attentionTotal}
      connectionState={connectionState}
      connectionCachedAt={connectionCachedAt}
      worktreeStrip={
        shouldShowMobileWorktreeStrip(panel) ? (
          <MobileWorktreeTabs {...worktreeTabsProps} />
        ) : null
      }
    >
      {panelContent}
      {filesPanelMounted ? (
        <div hidden={panel !== 'files'}>
          <Suspense fallback={heavyPanelFallback}>
            <MobileFilesPanel
              project={activeProject}
              worktree={activeWorktree}
              discardContextToken={filesDiscardContextToken}
              onDirtyContextChange={setFilesDirtySnapshot}
            />
          </Suspense>
        </div>
      ) : null}
    </MobileWorkbenchShell>
  );
}
