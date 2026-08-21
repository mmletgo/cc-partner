/**
 * 工作台页面 - 本机项目、多终端与项目文件夹检查器
 *
 * Business Logic（为什么需要这个页面）:
 *   指定项目文件夹后管理多个项目终端；右侧检查器切换文件夹、Git 历史与项目笔记。
 *
 * Code Logic（这个页面做什么）:
 *   拉取项目/会话/文件树，用 xterm 渲染当前 session，并嵌入项目级 Orchestrator 控制台。
 *   hooks 全部在 early return 之前，避免 React hooks 调用顺序问题。
 */

import { useCallback, useEffect, useMemo, useRef, useState, type CSSProperties } from 'react';
import { useTranslation } from 'react-i18next';
import { useLocation, useNavigate } from 'react-router-dom';
import '@xterm/xterm/css/xterm.css';
import { tauriWorkbenchTransport } from '@/api/workbenchTransport';
import { WorkbenchBrowserWorkspace } from '@/components/domain/WorkbenchBrowserWorkspace';
import { WorkbenchDependencyNotice } from './views/WorkbenchDependencyNotice';
import { WorkbenchFileWorkspace } from '@/components/domain/WorkbenchFileWorkspace';
import { WorkbenchSessionSearch } from '@/components/domain/WorkbenchSessionSearch';
import type { WorkbenchWorkspaceSwitchValue } from '@/components/domain/WorkbenchWorkspaceSwitch';
import { WorkbenchWorkspaceNav } from '@/components/layout';
import { Button, Dialog, StatusMessage } from '@/components/primitives';
import { useWorkbenchDependency } from '@/hooks/workbenchDependencyContext';
import { useWorkbenchProjects } from '@/hooks/workbenchProjectsContext';
import {
  useStartupBaselineFailure,
  useTerminalHistorySyncFailure,
  useWorkbenchTerminalBuffers,
} from '@/hooks/workbenchTerminalBuffersContext';
import { useAttention, useMarkNeedsInputAttentionOnSessionFocus } from '@/hooks/useAttention';
import { useExperimentalFeatures } from '@/hooks/useExperimentalFeatures';
import {
  MaximizeIcon, MinimizeIcon, RefreshIcon, SearchIcon,
} from '@/lib/icons';
import { WorkbenchPaneTools } from '@/components/domain/WorkbenchPaneTools';
import styles from './Workbench.module.css';
import { WorkbenchPromptTools } from './WorkbenchPromptTools';
import { parseWorkbenchDeepLink, stripWorkbenchProjectAgentSearch, withWorkbenchProjectAgentView } from './workbenchDeepLink';
import { routeAutomationWorkbenchOpen } from './workbenchWindowNavigation';
import * as workbenchModule from '@/api/workbench';
const { workbenchApi, windows } = workbenchModule;
import {
  canListenToTauriEvents,
  deferEffect,
  displayErrorMessage,
  measureInitialTerminalSize,
} from './workbenchPageHelpers';
import { useWorkbenchProjectController } from './controllers/useWorkbenchProjectController';
import { useWorkbenchTerminalController } from './controllers/useWorkbenchTerminalController';
import { useWorkbenchWorktreeGitController } from './controllers/useWorkbenchWorktreeGitController';
import { useWorkbenchFileController } from './controllers/useWorkbenchFileController';
import { useWorkbenchAutomationController } from './controllers/useWorkbenchAutomationController';
import { useWorkbenchPromptOptimizerController } from './controllers/useWorkbenchPromptOptimizerController';
import { useWorkbenchSessionSearchController } from './controllers/useWorkbenchSessionSearchController';
import { useWorkbenchFavoriteQuickInput } from './hooks/useWorkbenchFavoriteQuickInput';
import { useAgentRuntime } from '@/hooks/useAgentRuntime';
import { useAgentLedgerForAgent } from '@/hooks/useAgentLedgerForAgent';
import { WorkbenchTerminalArea } from './WorkbenchTerminalArea';
import { WorkbenchInspector } from './WorkbenchInspector';
import type { WorkbenchInspectorTab } from './WorkbenchInspector';
import { WorkbenchSessionTabs } from './WorkbenchSessionTabs';
import { useWorkbenchPageBridges } from './useWorkbenchPageBridges';
import { WorkbenchStatusCard } from './WorkbenchStatusCard';
import { WorkbenchWorktreeBar } from './WorkbenchWorktreeBar';
import { WorkbenchLaunchSurface } from './WorkbenchLaunchSurface';
import { activeWorktreeRootPath, DEFAULT_WORKTREE_BRANCH_PREFIX } from './workbenchWorktrees';
import type { WorkbenchFileWorkspaceView } from './workbenchFiles';
import { useWorkspaceSafeRestore } from './useWorkspaceSafeRestore';
import { useWorkbenchProjectNotes } from './useWorkbenchProjectNotes';
import { useWorkbenchWindowRole } from '@/hooks/useWorkbenchWindowRole';
import { WorkbenchWorkspaceHeader } from './WorkbenchWorkspaceHeader';
import { WorkbenchWorkspaceSwitchSlot } from './views/WorkbenchWorkspaceSwitchSlot';
import { WorkbenchProjectOverlayLayers } from './WorkbenchProjectOverlayLayers';
import type { WorkbenchProjectAgentConsoleHandle } from '@/pages/AgentHub/WorkbenchProjectAgentConsole';
import { WorkspaceRestoreNotice } from './views/WorkspaceRestoreNotice';

/**
 * Business Logic（为什么需要这个组件）:
 *   工作台是用户进入项目并操作项目终端的主界面。
 *
 * Code Logic（这个组件做什么）:
 *   聚合项目、会话、终端输出 buffer、文件树与文件操作状态，并组合三栏布局；
 *   projectsLoading 时中性 loading；列表就绪后零项目 / 未选中项目 early return 到 LaunchSurface。
 */
export function Workbench() {
  const { t } = useTranslation(['workbench', 'common', 'promptOptimizer', 'attention']);
  const location = useLocation();
  const navigate = useNavigate();
  const { status: dependencyStatus, check: checkDependency } = useWorkbenchDependency();
  const {
    projects,
    activeProjectId,
    activeProject,
    projectsLoading,
    selectProject,
    refreshProjectSessionStats,
    chooseAndAddProject,
    currentWindowLabel,
    occupancy,
  } = useWorkbenchProjects();
  const { layoutSlotKey } = useWorkbenchWindowRole();
  const restoreSlotKey = layoutSlotKey ?? 'desktop:auto';
  const {
    resetBuffer: resetTerminalBuffer,
    removeBuffer: removeTerminalBuffer,
    retryHistorySync,
    retryStartupBaseline,
  } = useWorkbenchTerminalBuffers();
  const locationSearch = location.search;
  const workbenchDeepLink = useMemo(
    () => parseWorkbenchDeepLink(locationSearch),
    [locationSearch],
  );
  // Business Logic: 项目域由独立 controller 持有，不复制邻接域 state。
  const projectCtrl = useWorkbenchProjectController({
    activeProject,
    activeProjectId,
    projects,
    selectProject,
  });
  const {
    remoteProjectOffline,
    remoteWriteDisabled,
    isCurrentProject,
    markRequestFailure,
    markRequestSuccess,
    selectProjectFromDeepLink,
    launchSummary,
    refreshLaunchSummary,
  } = projectCtrl;
  const [activeWorktreeId, setActiveWorktreeId] = useState<string | null>(null);
  // Business Logic: workspaceView / automationConsoleOpen 是跨域共享状态（终端全屏、自动化控制台、文件 tab 都会改写），
  // 仍由 Workbench.tsx 持有；文件域 controller 通过 requestWorkspaceView / requestHideAutomationConsole 回调表达意图。
  const [workspaceView, setWorkspaceView] = useState<WorkbenchFileWorkspaceView>('terminal');
  const { features: experimentalFeatures } = useExperimentalFeatures();
  const [automationConsoleOpen, setAutomationConsoleOpen] = useState<boolean>(false);
  const [projectAgentConsoleOpen, setProjectAgentConsoleOpen] = useState<boolean>(false);
  const projectOverlayOpen = automationConsoleOpen || projectAgentConsoleOpen;
  // Business Logic: 右侧栏默认 Git 历史（用户偏好）；持久化 layout 恢复仍优先生效，无记录时才落到此默认。
  const [inspectorTab, setInspectorTab] = useState<WorkbenchInspectorTab>('history');
  // Business Logic: workspace layout autosave 需要真实 browser target；由 BrowserWorkspace 回写。
  const [browserTargetUrl, setBrowserTargetUrl] = useState<string | null>(null);
  // Business Logic: 非主 worktree chip 的 x 按钮按下后进入 pending，由共享 Dialog 二次确认；
  // 确认后才走 controller 的 removeWorkbenchWorktree 路径；取消则清空。
  const [pendingRemovalWorktreeId, setPendingRemovalWorktreeId] = useState<string | null>(null);
  const activeProjectIdRef = useRef<string | null>(null);
  const activeWorktreeIdRef = useRef<string | null>(null);
  const terminalPanelRef = useRef<HTMLElement | null>(null);
  const terminalAreaRef = useRef<HTMLDivElement | null>(null);
  const worktreeBranchInputRef = useRef<HTMLInputElement | null>(null);
  const promptInputRef = useRef<HTMLTextAreaElement | null>(null);
  const projectAgentConsoleRef = useRef<WorkbenchProjectAgentConsoleHandle | null>(null);
  const workspaceViewBeforeOverlayRef = useRef<WorkbenchFileWorkspaceView>('terminal');

  // Business Logic: 终端域（session 生命周期 + focus 同步 + pane 操作 + terminal-status 事件）由独立 controller
  // 持有，避免在 Workbench.tsx 里散落多处 state/effect/handler；controller 接收窄 API/回调，不复制邻接域 state，
  // 也绝不持有终端字节内容（字节内容仍归 WorkbenchTerminalBuffersProvider 所有）。
  const desktopUnavailableMessage = t('workbench:errors.desktopUnavailable');
  // 错误/消息文案 + Prompt 优化配置/流式写入集中为稳定回调（useWorkbenchPageBridges），避免页面膨胀超 1200 行；
  // 每个回调 useCallback 稳定，防止下游 controller 依赖抖动反复触发 loadSessions / loadDir 等效应。
  const {
    translateTerminalError,
    translateWorktreeError,
    translateWorktreeMessage,
    translateFileError,
    translateFileMessage,
    translatePromptFillFailed,
    translatePromptOptimizeFailed,
    translatePromptRemoteOffline,
    loadPromptOptimizerConfig,
    streamPromptToTerminal,
  } = useWorkbenchPageBridges(t, activeProject?.deviceName);
  const terminalController = useWorkbenchTerminalController({
    api: workbenchApi.sessions,
    activeProjectId,
    activeWorktreeId,
    remoteWriteDisabled,
    terminalPanelRef,
    resetBuffer: resetTerminalBuffer,
    removeBuffer: removeTerminalBuffer,
    refreshProjectSessionStats,
    markRequestFailure,
    markRequestSuccess,
    isCurrentProject,
    measureInitialTerminalSize,
    displayErrorMessage,
    translateError: translateTerminalError,
    desktopUnavailableMessage,
    canListenToTauriEvents,
  });
  // A2 Agent 投影；snapshot 失败 phase=error 须暴露 refresh（禁永久 pending）。
  const agentRuntime = useAgentRuntime(activeProjectId);
  const {
    scopedSessions,
    activeSession,
    activeSessionId,
    visibleSessions,
    mountedSessions,
    renderedActiveSessionId,
    sessionNameDraft,
    sessionBusy,
    sessionError,
    terminalFullscreen,
    terminalResizeRequestKey,
    canUsePanes,
    canSwitchPane,
    canRefreshCurrentTerminalSize,
    setSessionNameDraft,
    setSessionError,
    setActiveSessionId,
    setSessions,
    loadSessions,
    focusSession,
    handleCreateSession,
    handleSplitPane,
    handleSwitchPane,
    handleClosePane,
    handleCloseSession,
    handleRenameSession,
    renameSessionById,
    handleInput,
    handlePasteImage,
    isWriteBlocked,
    handleResize,
    handleRefreshTerminalSize,
    handleSelectPaneAt,
  } = terminalController;
  // R12 M3：history sync 永久失败可订阅状态（hooks 必须在 early return 前）。
  const historySyncFailure = useTerminalHistorySyncFailure(activeSessionId);
  const startupBaselineFailure = useStartupBaselineFailure();
  // 状态卡速率 / 上下文 % 优先 live usage；ledger 仅终态回退（hook 仍只在终态拉取）。
  const activeAgentForStatusCard = activeSessionId
    ? agentRuntime.latestAgentForTerminal(activeSessionId)
    : null;
  const agentLedgerForStatusCard = useAgentLedgerForAgent(
    activeProjectId,
    activeAgentForStatusCard?.id ?? null,
    activeAgentForStatusCard?.phase ?? null,
  );
  // Business Logic: worktree/Git 域（worktree 生命周期 + 创建表单/busy/error + Git 提交刷新 + merge 阶段）
  // 由独立 controller 持有，避免在 Workbench.tsx 里散落多处 state/effect/handler；controller 接收窄 API/回调 +
  // terminalBridge，不复制邻接域 state。activeWorktreeId 仍由页面持有（终端域 controller / 文件 effect /
  // deep link effect 都读取同一值），controller 通过 setActiveWorktreeId 回写。
  const confirmWorktreeAction = useCallback(
    (message: string): boolean => window.confirm(message),
    [],
  );
  const worktreeGitController = useWorkbenchWorktreeGitController({
    api: { worktrees: workbenchApi.worktrees, git: workbenchApi.git },
    activeProjectId,
    activeWorktreeId,
    setActiveWorktreeId,
    remoteWriteDisabled,
    inspectorTab,
    isCurrentProject,
    markRequestFailure,
    markRequestSuccess,
    refreshProjectSessionStats,
    terminalBridge: terminalController,
    displayErrorMessage,
    desktopUnavailableMessage,
    translateError: translateWorktreeError,
    translateWorktreeMessage,
    confirmAction: confirmWorktreeAction,
    canListenToTauriEvents,
  });
  const {
    worktrees,
    setWorktrees,
    worktreeBusy,
    unknownMutationLock,
    worktreeError,
    hookRepair,
    createWorktreeOpen,
    setCreateWorktreeOpen,
    createWorktreeBranchPrefix,
    createWorktreeBranchSuffixDraft,
    setCreateWorktreeBranchPrefix,
    setCreateWorktreeBranchSuffixDraft,
    gitCommits,
    setGitCommits,
    gitHistoryLoading,
    gitHistoryError,
    setGitHistoryError,
    mergeStages,
    loadWorktrees,
    loadGitHistory,
    handleOpenCreateWorktree,
    handleCancelCreateWorktree,
    handleCreateWorktree,
    handleCommitWorktree,
    handlePushWorktree,
    handleMergeWorktree,
    handleRemoveWorktree,
    handleRepairHookFailure,
    handleDismissHookFailure,
    handleRetryAfterRepair,
    clearMergeStagePanel,
  } = worktreeGitController;
  // 业务逻辑：文件工作区域（目录树 + tab + dirty/save/format + create/rename/delete/copy）由独立 controller
  // 持有，避免在 Workbench.tsx 里散落多处 state/handler/effect；controller 接收窄 API/回调，不复制邻接域 state。
  // workspaceView / automationConsoleOpen 仍由页面持有（跨域共享），controller 通过 request* 回调表达切换意图。
  // Code Logic: translateFileError / translateFileMessage 必须稳定（useCallback），否则 controller 内 useCallback
  const requestWorkspaceView = useCallback(
    (view: WorkbenchFileWorkspaceView) => {
      setWorkspaceView(view);
    },
    [],
  );
  const requestHideAutomationConsole = useCallback(() => {
    setAutomationConsoleOpen(false);
    setProjectAgentConsoleOpen(false);
  }, []);
  const fileController = useWorkbenchFileController({
    api: workbenchApi.files,
    activeProjectId,
    activeWorktreeId,
    remoteWriteDisabled,
    isCurrentProject,
    markRequestFailure,
    markRequestSuccess,
    requestWorkspaceView,
    requestHideAutomationConsole,
    displayErrorMessage,
    desktopUnavailableMessage,
    translateFileError,
    translateFileMessage,
  });
  const {
    rootNodes,
    childrenByPath,
    expandedPaths,
    selectedPath,
    selectedInfo,
    fileLoadingPath,
    fileError,
    fileNotice,
    fileTabs,
    activeFileTabId,
    fileSaving,
    newEntryName,
    renameName,
    setNewEntryName,
    setRenameName,
    loadDir,
    handleToggleNode,
    handleSelectNode,
    handleActivateFileTab,
    handleCloseFileTab,
    handleFileContentChange,
    handleFileModeChange,
    handleSaveFileTab,
    handleFormatFileTab,
    handleSelectSqliteTable,
    handleLoadHtmlAsset,
    handleCreateEntry,
    handleRenamePath,
    handleDeletePath,
    handleCopySelectedPath,
    openFileByPath,
    resetForContext: resetFileForContext,
  } = fileController;
  // Business Logic: 自动化域（自动化控制台开/关 + staged deep link 应用 + 执行现场回跳）由独立 controller 持有，
  // 避免在 Workbench.tsx 里散落 deepLinkApplicationRef 与三段式 deep link effect。
  // automationConsoleOpen / workspaceView 仍是跨域共享状态（终端全屏、自动化控制台、文件 tab 都会改写），
  // 由页面持有；controller 通过 setAutomationConsoleOpen / requestWorkspaceView 回调表达意图，
  // 并通过 selectProjectFromDeepLink / setActiveWorktreeId / focusSession 触发跨域编排。
  // controller 不持有 task fetching、worktree 列表、session 列表或终端字节内容。
  const automationController = useWorkbenchAutomationController({
    deepLink: workbenchDeepLink,
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
    setProjectAgentConsoleOpen,
    requestWorkspaceView,
    openFileByPath,
    navigate,
  });
  const {
    closeAutomation: closeAutomationConsole,
    openTaskWorkbench,
    focusTaskId: automationFocusTaskId,
    focusOutboxId: automationFocusOutboxId,
  } = automationController;
  const { refresh: refreshAttention } = useAttention();
  useMarkNeedsInputAttentionOnSessionFocus(activeSessionId, workspaceView === 'terminal' && !projectOverlayOpen);
  /**
   * Business Logic（为什么需要这个函数）:
   *   Attention deep link 目标已解决时不能打开空白详情/终端，应提示并回到 Inbox。
   *
   * Code Logic（这个函数做什么）:
   *   显示提示文案、refresh Attention、navigate `/attention`。
   */
  const handleAttentionTargetNotFound = useCallback(() => {
    window.alert(t('attention:targetResolved'));
    void refreshAttention();
    navigate('/attention');
  }, [navigate, refreshAttention, t]);

  const activeWorktree = useMemo(
    () => worktrees.find((worktree) => worktree.id === activeWorktreeId) ?? worktrees[0] ?? null,
    [activeWorktreeId, worktrees],
  );
  const emptyValue = t('workbench:emptyValue');
  const activeRootPath = activeWorktreeRootPath(activeProject?.path ?? '', activeWorktree);
  // Business Logic: 终端全屏时 inspector 被 fixed overlay 盖住，运行时长无需 1 Hz 刷新；
  // 由既有 terminalFullscreen 状态派生 visible，不使用 IntersectionObserver。
  const runtimeVisible = !terminalFullscreen;
  const TerminalFullscreenIcon = terminalFullscreen ? MinimizeIcon : MaximizeIcon;
  const terminalFullscreenLabel = terminalFullscreen
    ? t('workbench:terminalExitFullscreen')
    : t('workbench:terminalEnterFullscreen');
  const promptWorkingDirectory = activeRootPath || undefined;
  // Business Logic: Prompt 优化浮层域（配置加载 + 打开/输入/定位 + Control 单键快捷键 + IME 安全 +
  // 流式写入终端 + 焦点回归 + 重新打开清空）由独立 controller 持有，避免在 Workbench.tsx 里散落
  // 多处 state/effect/handler；controller 接收窄 API/回调 + 共享 refs，不复制邻接域 state。
  const promptOptimizerController = useWorkbenchPromptOptimizerController({
    activeSession,
    activeProjectId,
    promptWorkingDirectory,
    remoteWriteDisabled,
    automationConsoleOpen: projectOverlayOpen,
    workspaceView,
    terminalAreaRef,
    promptInputRef,
    loadConfig: loadPromptOptimizerConfig,
    streamToTerminal: streamPromptToTerminal,
    markRequestFailure,
    setSessionError,
    displayErrorMessage,
    desktopUnavailableMessage,
    translateFillFailed: translatePromptFillFailed,
    translateOptimizeFailed: translatePromptOptimizeFailed,
    translateRemoteOffline: translatePromptRemoteOffline,
  });
  const {
    promptPanelOpen,
    promptInput,
    promptOptimizing,
    promptPanelPosition,
    setPromptInput,
    closePromptPanel,
    togglePromptOptimizerPanel,
    handleCursorAnchorChange,
    handlePromptInputKeyDown,
  } = promptOptimizerController;
  const favoriteQuickInput = useWorkbenchFavoriteQuickInput({ activeSessionId, terminalPanelRef, handleInput });
  // Business Logic: Session 搜索浮层域（⌘K/Ctrl+K 开/关 + resume 成功刷新 sessions/focus 新 session/关闭浮层）
  // 由独立 controller 持有，避免在 Workbench.tsx 里散落 open state 与 keydown 监听；controller 只持有 open 状态
  // 与 resume 编排，搜索结果数据（query/hits/preview）仍归 WorkbenchSessionSearch 组件所有。
  const {
    sessionSearchOpen,
    openSessionSearch,
    closeSessionSearch,
    handleResumed: handleSessionSearchResumed,
  } = useWorkbenchSessionSearchController({
    workspaceView,
    activeProjectId,
    loadSessions,
    focusSession,
  });

  // A8: 必须在 project/worktree effect 之前挂载，以便 suppressContextResetRef 可供那些 effect 读取。
  // 命名 snapshot Dialog 的 UI 入口已在 2026-08 下线；hook 仍保留命名 snapshot 数据层
  // （storage / IPC / schema / 单元测试），便于后续按需重新暴露 UI。
  const {
    restoreSummary,
    dismissRestoreNotice,
    suppressContextResetRef,
  } = useWorkspaceSafeRestore({
    projectsLoading,
    projectsLength: projects.length,
    activeProjectId,
    activeWorktreeId,
    activeSessionId,
    workspaceView,
    inspectorTab,
    browserTargetUrl,
    dirtyEditor: fileTabs.some((tab) => tab.dirty),
    activeProjectIdRef,
    activeWorktreeIdRef,
    selectProjectFromDeepLink,
    setActiveWorktreeId,
    focusSession,
    setWorkspaceView,
    setInspectorTab,
    setBrowserTargetUrl,
    layoutSlotKey: restoreSlotKey,
    urlProjectId: workbenchDeepLink.projectId,
    browserEnabled: experimentalFeatures.browser,
  });

  const notes = useWorkbenchProjectNotes({
    activeProjectId, inspectorTab, desktopUnavailableMessage, remoteWriteDisabled,
    loadFailedFallback: t('workbench:notesLoadFailed'),
  });

  useEffect(() => {
    activeProjectIdRef.current = activeProjectId;
  }, [activeProjectId]);

  useEffect(() => {
    activeWorktreeIdRef.current = activeWorktreeId;
  }, [activeWorktreeId]);

  useEffect(() => {
    if (!createWorktreeOpen) return undefined;
    const frame = window.requestAnimationFrame(() => {
      worktreeBranchInputRef.current?.focus();
    });
    return () => window.cancelAnimationFrame(frame);
  }, [createWorktreeOpen]);

  useEffect(() => {
    return deferEffect(() => {
      // Business Logic: merge 在后端后台运行；切换项目时保留 controller 的按项目阶段快照，返回后继续展示。
      // 成功快照由 controller 自动隐藏，失败快照由后续同项目操作覆盖，不能在这里无条件清空。
      // Business Logic: 文件域（含 open/save/format/sqlite/dir stale 守卫）由 fileController.resetForContext 统一重置。
      // Code Logic: resetForContext 忽略入参（仅作语义占位），不依赖当前 activeWorktreeId，因此本 effect 不订阅
      // activeWorktreeId 变化——避免 worktree 切换时重跑 loadSessions 把 terminal-status 事件更新覆盖回 running。
      // A8: restore apply 窗口内 suppressContextResetRef 为 true 时，只加载 worktrees/sessions，
      // 不清 worktree / 不强制 terminal，避免与 restore 顺序竞态。
      const suppressReset = suppressContextResetRef.current;
      if (!suppressReset) {
        resetFileForContext(activeProjectId, null);
      }
      if (!activeProjectId) {
        if (!suppressReset) {
          setSessions([]);
          setActiveSessionId(null);
          setWorktrees([]);
          setActiveWorktreeId(null);
          setCreateWorktreeOpen(false);
          setCreateWorktreeBranchPrefix(DEFAULT_WORKTREE_BRANCH_PREFIX);
          setCreateWorktreeBranchSuffixDraft('');
          setWorkspaceView('terminal');
          setGitCommits([]);
          setGitHistoryError(null);
        }
        return;
      }
      if (!suppressReset) {
        setWorktrees([]);
        setActiveWorktreeId(null);
        setCreateWorktreeOpen(false);
        setCreateWorktreeBranchPrefix(DEFAULT_WORKTREE_BRANCH_PREFIX);
        setCreateWorktreeBranchSuffixDraft('');
        setWorkspaceView('terminal');
        setGitCommits([]);
        setGitHistoryError(null);
      }
      void loadWorktrees(activeProjectId);
      void loadSessions(activeProjectId);
    });
  }, [activeProjectId, loadSessions, loadWorktrees, resetFileForContext, setActiveSessionId, setSessions, setWorktrees, setCreateWorktreeOpen, setCreateWorktreeBranchPrefix, setCreateWorktreeBranchSuffixDraft, setGitCommits, setGitHistoryError]);

  useEffect(() => {
    return deferEffect(() => {
      // Business Logic: worktree 切换时文件域需要彻底重置（含 stale 守卫），随后按新 worktree 重新加载根目录。
      // A8: restore apply 期间不强制 terminal，保留 plan 中的 workspaceView。
      if (suppressContextResetRef.current) {
        if (activeProjectId && activeWorktreeId) {
          void loadDir('');
        }
        return;
      }
      resetFileForContext(activeProjectId, activeWorktreeId);
      setWorkspaceView('terminal');
      setGitCommits([]);
      setGitHistoryError(null);
      if (activeProjectId && activeWorktreeId) {
        void loadDir('');
      }
    });
  }, [activeProjectId, activeWorktreeId, loadDir, resetFileForContext, setGitCommits, setGitHistoryError]);

  useEffect(() => {
    if (inspectorTab !== 'history') return undefined;
    return deferEffect(() => {
      void loadGitHistory();
    });
  }, [activeProjectId, activeWorktreeId, inspectorTab, loadGitHistory]);

  // Business Logic: 桌面端用户需要把当前终端临时铺满屏幕，隐藏项目标题、worktree 管理层、文件层和右侧检查器。
  // Code Logic: 关闭 Prompt 优化浮层，切回 terminal 工作区，并通过 controller 打开 terminalLayer fixed overlay。
  const handleEnterTerminalFullscreen = useCallback((): void => {
    closePromptPanel();
    setWorkspaceView('terminal');
    terminalController.handleEnterTerminalFullscreen();
  }, [closePromptPanel, terminalController]);

  // Business Logic: 进入终端全屏后必须有明确出口，恢复完整 Workbench 布局。
  // Code Logic: 通过 controller 关闭 terminalLayer 的 fixed overlay 状态；不改变 session/worktree 或文件 tab。
  const handleExitTerminalFullscreen = useCallback((): void => {
    terminalController.handleExitTerminalFullscreen();
  }, [terminalController]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   文件浏览是独立工作区，即使尚未打开任何文件也应可进入空白页；同时把右侧栏切到
   *   项目文件夹，让用户立刻看到可点开的文件树。离开终端全屏切到浏览层时必须退出 overlay，
   *   否则 modal 终端层会盖住文件/浏览器工作区和右侧栏。
   *
   * Code Logic（这个函数做什么）:
   *   切到非 terminal 时若处于全屏则先退出；切到 files 时写入 inspectorTab='files'；最后写入 workspaceView。
   */
  const handleWorkspaceViewChange = useCallback(
    (next: WorkbenchWorkspaceSwitchValue): void => {
      if (next === 'browser' && !experimentalFeatures.browser) {
        return;
      }
      if (next !== 'terminal' && terminalFullscreen) {
        terminalController.handleExitTerminalFullscreen();
      }
      if (next === 'files') {
        setInspectorTab('files');
      }
      setWorkspaceView(next);
    },
    [experimentalFeatures.browser, terminalController, terminalFullscreen],
  );

  // Business Logic: 文件域操作函数（toggle/select/open/activate/close/content-change/mode-change/
  // save/format/sqlite/html-asset/create/rename/delete/copy）已迁移到 useWorkbenchFileController；
  // 页面不再保留 handleReturnToTerminal / handleReturnToFiles 跨域回调——三工作区切换由标题行
  // <WorkbenchWorkspaceSwitch> 统一承担，文件 / 浏览器工作区不再自带返回按钮。

  // Business Logic: worktree chip 切换前先调用 fileController.guardDirtyContextChange（Plan 2 新增保护：原
  //   Workbench.tsx eae5bef chip/其余 context 切换均无 dirty guard；当前已更严格，Finding 1 确认非回归）。用户取消则中止。
  const handleSelectWorktree = useCallback(
    async (nextWorktreeId: string): Promise<void> => {
      const ok = await fileController.guardDirtyContextChange();
      if (!ok) return;
      setActiveWorktreeId(nextWorktreeId);
    },
    [fileController, setActiveWorktreeId],
  );

  const handleToggleProjectAutomation = useCallback(() => {
    closePromptPanel();
    if (automationConsoleOpen) {
      closeAutomationConsole();
      return;
    }
    void (async () => {
      if (projectAgentConsoleOpen) {
        const ok = (await projectAgentConsoleRef.current?.confirmClose()) ?? true;
        if (!ok) return;
        setProjectAgentConsoleOpen(false);
        navigate(stripWorkbenchProjectAgentSearch(locationSearch), { replace: true });
      }
      setWorkspaceView('terminal');
      setAutomationConsoleOpen(true);
    })();
  }, [automationConsoleOpen, closeAutomationConsole, closePromptPanel, locationSearch, navigate, projectAgentConsoleOpen]);

  const handleToggleProjectAgent = useCallback(() => {
    closePromptPanel();
    if (projectAgentConsoleOpen) {
      void (async () => {
        const ok = (await projectAgentConsoleRef.current?.confirmClose()) ?? true;
        if (!ok) return;
        setProjectAgentConsoleOpen(false);
        setWorkspaceView(workspaceViewBeforeOverlayRef.current);
        navigate(stripWorkbenchProjectAgentSearch(locationSearch), { replace: true });
      })();
      return;
    }
    if (automationConsoleOpen) closeAutomationConsole();
    workspaceViewBeforeOverlayRef.current = workspaceView;
    if (activeProjectId) {
      navigate(withWorkbenchProjectAgentView(locationSearch, activeProjectId), { replace: true });
    }
    setProjectAgentConsoleOpen(true);
  }, [activeProjectId, automationConsoleOpen, closeAutomationConsole, closePromptPanel, locationSearch, navigate, projectAgentConsoleOpen, workspaceView]);

  const handleOpenAutomationTaskWorkbench = useCallback(
    (url: string): void => {
      routeAutomationWorkbenchOpen(url, {
        currentLabel: currentWindowLabel,
        occupancy,
        navigate,
        claim: windows.claim,
        focus: windows.focus,
        applyOnWindow: windows.applyDeepLink,
        fallback: (next) => {
          void openTaskWorkbench(next);
        },
        closeLocalConsole: closeAutomationConsole,
      });
    },
    [closeAutomationConsole, currentWindowLabel, navigate, occupancy, openTaskWorkbench],
  );

  const workspaceLine = activeProject
    ? `${activeProject.deviceName} · ${activeProject.path}`
    : t('workbench:noProjectHint');
  const promptPanelStyle = {
    '--prompt-panel-left': `${promptPanelPosition.left}px`,
    '--prompt-panel-top': `${promptPanelPosition.top}px`,
  } as CSSProperties;

  const handleEmptyAddLocal = useCallback(() => {
    void chooseAndAddProject();
  }, [chooseAndAddProject]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   零项目次级动作「检查 tmux 依赖」应 recheck 并跳到 Settings 依赖 tab。
   *
   * Code Logic（这个函数做什么）:
   *   void checkDependency() 后 navigate `/settings?tab=dependencies`。
   */
  const handleEmptyCheckTmux = useCallback(() => {
    void checkDependency();
    navigate('/settings?tab=dependencies');
  }, [checkDependency, navigate]);

  // 三模式 + loading：项目列表未就绪时中性 loading，禁止把 projects=[] 当成真·零项目。
  // empty 仅 !projectsLoading && length===0；continue 仅 !projectsLoading && length>0 && !active。
  // 全部 hooks 已在上方无条件调用，early return 不破坏 hooks 顺序。
  if (projectsLoading) {
    return (
      <div className={styles.launchPage} data-testid="workbench-projects-loading">
        <main className={styles.launchEmptyMain}>
          <p className={styles.launchMuted}>{t('workbench:loading')}</p>
        </main>
      </div>
    );
  }

  if (projects.length === 0) {
    return (
      <WorkbenchLaunchSurface
        mode="empty"
        launchSummary={launchSummary}
        onRefreshLaunchSummary={() => {
          void refreshLaunchSummary();
        }}
        emptyActions={{
          onAddLocal: handleEmptyAddLocal,
          onCheckTmux: handleEmptyCheckTmux,
        }}
      />
    );
  }

  if (!activeProjectId) {
    return (
      <WorkbenchLaunchSurface
        mode="continue"
        launchSummary={launchSummary}
        onRefreshLaunchSummary={() => {
          void refreshLaunchSummary();
        }}
      />
    );
  }

  return (
    <div className={styles.page} data-automation-open={projectOverlayOpen || undefined}>
      <main className={styles.centerPane}>
        <WorkspaceRestoreNotice
          summary={restoreSummary}
          onDismiss={dismissRestoreNotice}
        />
        {workbenchDeepLink.view === 'projectAgent' &&
        workbenchDeepLink.projectId &&
        !projects.some((project) => project.id === workbenchDeepLink.projectId) ? (
          <StatusMessage tone="warn" data-testid="workbench-project-agent-missing">
            {t('workbench:projectAgent.missingProject')}
          </StatusMessage>
        ) : null}
        <WorkbenchWorkspaceHeader
          workspaceLine={workspaceLine}
          terminalFullscreen={terminalFullscreen}
          activeProjectId={activeProjectId}
          projectCtrl={projectCtrl}
          projectAgentOpen={projectAgentConsoleOpen}
          automationOpen={automationConsoleOpen}
          onToggleProjectAgent={handleToggleProjectAgent}
          onToggleAutomation={handleToggleProjectAutomation}
        />

        <section
          className={styles.worktreeBar}
          aria-label={t('workbench:worktrees.label')}
          hidden={projectOverlayOpen}
        >
          <WorkbenchWorktreeBar
            worktrees={worktrees}
            activeWorktree={activeWorktree}
            activeProjectId={activeProjectId}
            remoteWriteDisabled={remoteWriteDisabled}
            worktreeBusy={worktreeBusy}
            unknownMutationLock={unknownMutationLock}
            createWorktreeOpen={createWorktreeOpen}
            createWorktreeBranchPrefix={createWorktreeBranchPrefix}
            createWorktreeBranchSuffixDraft={createWorktreeBranchSuffixDraft}
            worktreeBranchInputRef={worktreeBranchInputRef}
            onSelectWorktree={(id) => {
              void handleSelectWorktree(id);
            }}
            setCreateWorktreeBranchPrefix={setCreateWorktreeBranchPrefix}
            setCreateWorktreeBranchSuffixDraft={setCreateWorktreeBranchSuffixDraft}
            handleOpenCreateWorktree={handleOpenCreateWorktree}
            handleCancelCreateWorktree={handleCancelCreateWorktree}
            handleCreateWorktree={handleCreateWorktree}
            onRequestRemoveWorktree={(worktreeId) => {
              setPendingRemovalWorktreeId(worktreeId);
            }}
            workspaceSwitch={
              <WorkbenchWorkspaceSwitchSlot
                value={workspaceView}
                onChange={handleWorkspaceViewChange}
                browserEnabled={experimentalFeatures.browser}
                canOpenBrowser={Boolean(activeProject && activeWorktree)}
              />
            }
          />
        </section>
        <Dialog
          open={pendingRemovalWorktreeId !== null}
          titleId="workbench-remove-worktree-confirm-title"
          onClose={() => {
            setPendingRemovalWorktreeId(null);
          }}
        >
          {(() => {
            const pendingWorktree = pendingRemovalWorktreeId
              ? worktrees.find((worktree) => worktree.id === pendingRemovalWorktreeId) ?? null
              : null;
            return (
              <>
                <h2
                  id="workbench-remove-worktree-confirm-title"
                  className={styles.removeDialogTitle}
                >
                  {t('workbench:worktrees.removeConfirmDialog.title')}
                </h2>
                <p className={styles.removeDialogBody}>
                  {t('workbench:worktrees.removeConfirmDialog.body', {
                    name: pendingWorktree?.branch ?? pendingWorktree?.name ?? '',
                  })}
                </p>
                <div className={styles.removeDialogActions}>
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => {
                      setPendingRemovalWorktreeId(null);
                    }}
                  >
                    {t('workbench:worktrees.removeConfirmDialog.cancel')}
                  </Button>
                  <Button
                    variant="danger"
                    size="sm"
                    loading={worktreeBusy === 'remove'}
                    disabled={worktreeBusy === 'remove' || !pendingWorktree}
                    onClick={() => {
                      const worktreeId = pendingRemovalWorktreeId;
                      setPendingRemovalWorktreeId(null);
                      if (worktreeId) {
                        void handleRemoveWorktree(worktreeId);
                      }
                    }}
                  >
                    {t('workbench:worktrees.removeConfirmDialog.confirm')}
                  </Button>
                </div>
              </>
            );
          })()}
        </Dialog>

        <div className={styles.noticeStack}>
          {remoteProjectOffline ? (
            <div className={styles.noticeBox}>
              {t('workbench:remoteOfflineNotice', {
                device: activeProject?.deviceName ?? emptyValue,
              })}
            </div>
          ) : null}
          {sessionError ? <StatusMessage tone="danger" className={styles.errorBox} action={terminalController.hasWriteBlockedSessions ? <Button variant="secondary" size="sm" onClick={() => { void terminalController.retryWriteBlockRecovery(); }}>{t('workbench:recheckWriteBlock')}</Button> : undefined}>{sessionError}</StatusMessage> : null}
          {historySyncFailure ? (
            <StatusMessage
              tone="warn"
              className={styles.errorBox}
              data-testid="terminal-history-sync-failure"
              action={
                activeSessionId ? (
                  <Button
                    variant="secondary"
                    size="sm"
                    onClick={() => {
                      retryHistorySync(activeSessionId);
                    }}
                  >
                    {t('workbench:historySync.retry')}
                  </Button>
                ) : undefined
              }
            >
              {historySyncFailure.kind === 'not_found'
                ? t('workbench:historySync.notFound')
                : t('workbench:historySync.failed')}
            </StatusMessage>
          ) : null}
          {startupBaselineFailure ? (
            <StatusMessage
              tone="warn"
              className={styles.errorBox}
              data-testid="terminal-startup-baseline-failure"
              action={
                <Button
                  variant="secondary"
                  size="sm"
                  onClick={() => {
                    retryStartupBaseline();
                  }}
                >
                  {t('workbench:startupBaseline.retry')}
                </Button>
              }
            >
              {t('workbench:startupBaseline.listFailed')}
            </StatusMessage>
          ) : null}
          {worktreeError ? <StatusMessage tone="danger" className={styles.errorBox}>{worktreeError}</StatusMessage> : null}
          {agentRuntime.phase === 'error' && agentRuntime.error ? <StatusMessage tone="warn" className={styles.errorBox} action={<Button variant="secondary" size="sm" onClick={() => { void agentRuntime.refresh(); }}>{t('workbench:refresh')}</Button>}>{agentRuntime.error.message}</StatusMessage> : null}
          {activeProject ? <WorkbenchDependencyNotice compact className={styles.dependencyNotice} project={activeProject} localStatus={dependencyStatus.status} remoteWriteDisabled={remoteWriteDisabled} /> : null}
        </div>

        <div className={styles.mainWorkspace}>
          <div
            className={styles.terminalLayer}
            data-hidden={(!terminalFullscreen && (projectOverlayOpen || workspaceView !== 'terminal')) || undefined}
            data-fullscreen={terminalFullscreen || undefined}
          >
            <WorkbenchWorkspaceNav
              ariaLabel={t('workbench:terminalTabs')}
              actionsAriaLabel={t('workbench:paneActions')}
              tabs={
                <WorkbenchSessionTabs
                  sessions={scopedSessions}
                  activeSessionId={activeSessionId}
                  sessionBusy={sessionBusy}
                  canCreate={Boolean(activeProjectId && activeWorktree && !remoteWriteDisabled)}
                  onFocusSession={(sessionId) => {
                    void focusSession(sessionId);
                  }}
                  onCloseSession={handleCloseSession}
                  onCreateSession={() => {
                    void handleCreateSession();
                  }}
                  onRenameSession={renameSessionById}
                  canRename={!remoteWriteDisabled}
                />
              }
              actions={
                <>
                  {/* 会话搜索按钮全屏态下保留,与窗格菜单同级(离开全屏切到浏览器/文件仍可走 WorkbenchWorkspaceSwitch) */}
                  <Button
                    className={styles.terminalActionButton}
                    variant="secondary"
                    size="sm"
                    icon={<SearchIcon />}
                    title={t('workbench:sessionSearch.open')}
                    aria-label={t('workbench:sessionSearch.open')}
                    data-workbench-responsive-action="true"
                    disabled={!activeProjectId || !activeWorktree}
                    onClick={openSessionSearch}
                  >
                    <span data-workbench-responsive-label="true">{t('workbench:sessionSearch.open')}</span>
                  </Button>
                  {/* Prompt 工具组(Prompt 优化 + 收藏快捷输入)全屏态下保留,与既有规范对齐 */}
                  <WorkbenchPromptTools
                    hasActiveSession={!!activeSession}
                    remoteWriteDisabled={remoteWriteDisabled}
                    promptPanelOpen={promptPanelOpen}
                    onTogglePromptOptimizer={togglePromptOptimizerPanel}
                    favoriteOpen={favoriteQuickInput.open}
                    onToggleFavorite={favoriteQuickInput.onToggle}
                  />
                  <Button
                    className={styles.terminalActionButton}
                    variant="secondary"
                    size="sm"
                    icon={<RefreshIcon />}
                    title={t('workbench:fitTerminalSize')}
                    aria-label={t('workbench:fitTerminalSize')}
                    data-workbench-responsive-action="true"
                    disabled={!canRefreshCurrentTerminalSize}
                    onClick={handleRefreshTerminalSize}
                  >
                    <span data-workbench-responsive-label="true">{t('workbench:fitTerminalSize')}</span>
                  </Button>
                  {/* 窗格四操作（分屏右/下、切换、关闭）收纳进「窗格」菜单；全屏时仍可用 */}
                  <WorkbenchPaneTools
                    canUsePanes={canUsePanes}
                    canSwitchPane={canSwitchPane}
                    remoteWriteDisabled={remoteWriteDisabled}
                    onSplitPane={(direction) => {
                      void handleSplitPane(direction);
                    }}
                    onSwitchPane={() => {
                      void handleSwitchPane();
                    }}
                    onClosePane={() => {
                      void handleClosePane();
                    }}
                  />
                  <Button
                    className={styles.terminalActionButton}
                    variant="secondary"
                    size="sm"
                    icon={<TerminalFullscreenIcon />}
                    title={terminalFullscreenLabel}
                    aria-label={terminalFullscreenLabel}
                    data-workbench-responsive-action="true"
                    disabled={!terminalFullscreen && !activeSession}
                    onClick={terminalFullscreen ? handleExitTerminalFullscreen : handleEnterTerminalFullscreen}
                  >
                    <span data-workbench-responsive-label="true">{terminalFullscreenLabel}</span>
                  </Button>
                </>
              }
            />
            <WorkbenchTerminalArea
              terminalAreaRef={terminalAreaRef}
              terminalPanelRef={terminalPanelRef}
              promptPanelOpen={promptPanelOpen}
              terminalFullscreen={terminalFullscreen}
              promptPanelStyle={promptPanelStyle}
              promptInputRef={promptInputRef}
              promptInput={promptInput}
              setPromptInput={setPromptInput}
              handlePromptInputKeyDown={handlePromptInputKeyDown}
              promptOptimizing={promptOptimizing}
              remoteWriteDisabled={remoteWriteDisabled}
              automationConsoleOpen={projectOverlayOpen}
              workspaceView={workspaceView}
              activeProject={activeProject}
              visibleSessions={visibleSessions}
              mountedSessions={mountedSessions}
              renderedActiveSessionId={renderedActiveSessionId}
              terminalResizeRequestKey={terminalResizeRequestKey}
              handleInput={handleInput}
              handlePasteImage={handlePasteImage}
              isWriteBlocked={isWriteBlocked}
              resolveAgent={agentRuntime.latestAgentForTerminal}
              handleResize={handleResize}
              handleCursorAnchorChange={handleCursorAnchorChange}
              handleSelectPaneAt={handleSelectPaneAt}
              focusSession={focusSession}
              favoriteQuickInput={favoriteQuickInput}
            />
          </div>

          <div
            className={styles.browserLayer}
            data-hidden={(projectOverlayOpen || workspaceView !== 'browser') || undefined}
          >
            <WorkbenchBrowserWorkspace
              surface="desktop"
              transport={tauriWorkbenchTransport}
              project={activeProject}
              worktree={activeWorktree}
              onBrowserTargetUrlChange={setBrowserTargetUrl}
            />
          </div>

          <div
            className={styles.fileLayer}
            data-hidden={(projectOverlayOpen || workspaceView !== 'files') || undefined}
          >
            <WorkbenchFileWorkspace
              tabs={fileTabs}
              activeTabId={activeFileTabId}
              saving={fileSaving}
              writeDisabled={remoteWriteDisabled}
              onActivate={handleActivateFileTab}
              onClose={handleCloseFileTab}
              onContentChange={handleFileContentChange}
              onModeChange={handleFileModeChange}
              loadHtmlAsset={handleLoadHtmlAsset}
              onSave={handleSaveFileTab}
              onFormat={handleFormatFileTab}
              onSelectSqliteTable={handleSelectSqliteTable}
            />
          </div>

          {automationConsoleOpen || projectAgentConsoleOpen ? (
            <WorkbenchProjectOverlayLayers
              automationOpen={automationConsoleOpen}
              projectAgentOpen={projectAgentConsoleOpen}
              project={activeProject}
              unsavedFiles={fileTabs.some((tab) => tab.dirty)}
              projectAgentRef={projectAgentConsoleRef}
              automationFocusTaskId={automationFocusTaskId}
              automationFocusOutboxId={automationFocusOutboxId}
              onOpenAutomationTaskWorkbench={handleOpenAutomationTaskWorkbench}
              onFocusTargetNotFound={handleAttentionTargetNotFound}
            />
          ) : null}
        </div>
      </main>

      <aside className={styles.inspectorPane} data-testid="workbench-inspector">
        <WorkbenchStatusCard
          activeProject={activeProject}
          activeWorktree={activeWorktree}
          activeSession={activeSession}
          activeRootPath={activeRootPath}
          remoteWriteDisabled={remoteWriteDisabled}
          sessionNameDraft={sessionNameDraft}
          setSessionNameDraft={setSessionNameDraft}
          handleRenameSession={handleRenameSession}
          handleCloseSession={handleCloseSession}
          runtimeVisible={runtimeVisible}
          activeAgent={activeAgentForStatusCard}
          ledgerEntry={agentLedgerForStatusCard.ledgerEntry}
        />
        <WorkbenchInspector
          inspectorTab={inspectorTab}
          setInspectorTab={setInspectorTab}
          fileInspector={{
            activeProjectId,
            remoteWriteDisabled,
            rootNodes,
            childrenByPath,
            expandedPaths,
            selectedPath,
            selectedInfo,
            fileLoadingPath,
            fileError,
            fileNotice,
            newEntryName,
            renameName,
            setNewEntryName,
            setRenameName,
            loadDir,
            handleToggleNode,
            handleSelectNode,
            handleCreateEntry,
            handleRenamePath,
            handleDeletePath,
            handleCopySelectedPath,
          }}
          gitInspector={{
            activeProjectId,
            activeWorktree,
            remoteWriteDisabled,
            gitCommits,
            gitHistoryLoading,
            gitHistoryError, worktreeBusy, unknownMutationLock,
            hookRepair, handleRepairHookFailure, handleDismissHookFailure, handleRetryAfterRepair,
            mergeStages, clearMergeStagePanel, loadGitHistory,
            handleCommitWorktree, handlePushWorktree, handleMergeWorktree,
          }}
          notesInspector={{ activeProjectId, ...notes }}
        />
      </aside>
      <WorkbenchSessionSearch
        open={sessionSearchOpen}
        onClose={closeSessionSearch}
        projectId={activeProjectId}
        worktreeId={activeWorktreeId}
        offline={remoteProjectOffline}
        worktreeName={activeWorktree?.name}
        onResumed={handleSessionSearchResumed}
      />
    </div>
  );
}
