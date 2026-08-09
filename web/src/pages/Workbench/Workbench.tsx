/**
 * 工作台页面 - 本机项目、多终端与项目文件夹检查器
 *
 * Business Logic（为什么需要这个页面）:
 *   用户需要指定一个项目文件夹，并在 cc-partner 内为该项目同时管理多个项目终端；
 *   右侧检查器展示当前会话状态，并可在项目文件夹与 Git 提交历史之间切换。
 *
 * Code Logic（这个页面做什么）:
 *   - 拉取/添加/移除工作台项目，并按当前项目加载会话与根目录文件树
 *   - 用 xterm 渲染当前 session，监听后端 terminal output/status 事件同步 UI
 *   - 提供文件夹展开、选中、创建、重命名、删除和 Git 提交历史查看等检查器交互
 *   - 在项目层级嵌入 Orchestrator 自动化控制台，任务运行后再 deep link 到具体执行现场
 *   - hooks 全部在 early return 之前，避免 React hooks 调用顺序问题
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { CSSProperties } from 'react';
import { useTranslation } from 'react-i18next';
import { useLocation, useNavigate } from 'react-router-dom';
import '@xterm/xterm/css/xterm.css';
import { tauriWorkbenchTransport } from '@/api/workbenchTransport';
import { OrchestratorPanel } from '@/pages/Orchestrator';
import { WorkbenchBrowserWorkspace } from '@/components/domain/WorkbenchBrowserWorkspace';
import { WorkbenchDependencyCard } from '@/components/domain/WorkbenchDependencyCard';
import { WorkbenchFileWorkspace } from '@/components/domain/WorkbenchFileWorkspace';
import { WorkbenchSessionSearch } from '@/components/domain/WorkbenchSessionSearch';
import { WorkbenchWorkspaceNav } from '@/components/layout';
import { Button, StatusMessage } from '@/components/primitives';
import { useWorkbenchDependency } from '@/hooks/workbenchDependencyContext';
import { useWorkbenchProjects } from '@/hooks/workbenchProjectsContext';
import {
  useStartupBaselineFailure,
  useTerminalHistorySyncFailure,
  useWorkbenchTerminalBuffers,
} from '@/hooks/workbenchTerminalBuffersContext';
import { useAttention } from '@/hooks/useAttention';
import {
  ArrowRightIcon, BrowserIcon, FileIcon, MaximizeIcon, MinimizeIcon, OrchestratorIcon,
  RefreshIcon, SearchIcon, SplitDownIcon, SplitRightIcon, XIcon,
} from '@/lib/icons';
import styles from './Workbench.module.css';
import { WorkbenchPromptTools } from './WorkbenchPromptTools';
import { parseWorkbenchDeepLink } from './workbenchDeepLink';
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
import { AgentLedgerWorkbenchChrome } from './views/AgentLedgerWorkbenchChrome';
import { WorkspaceRestoreNotice } from './views/WorkspaceRestoreNotice';
import { WorkspaceSnapshotDialog } from './views/WorkspaceSnapshotDialog';

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
  } = useWorkbenchProjects();
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
  // Business Logic: 项目域（远端离线状态机 + 跨项目请求守卫 + 项目级 deep link）由独立 controller 持有，
  // 避免在 Workbench.tsx 里散落多处 state/effect；controller 接收窄 API/回调，不复制邻接域 state。
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
  const [automationConsoleOpen, setAutomationConsoleOpen] = useState<boolean>(false);
  // Business Logic: 右侧栏默认 Git 历史（用户偏好）；持久化 layout 恢复仍优先生效，无记录时才落到此默认。
  const [inspectorTab, setInspectorTab] = useState<WorkbenchInspectorTab>('history');
  // Business Logic: workspace layout autosave 需要真实 browser target；由 BrowserWorkspace 回写。
  const [browserTargetUrl, setBrowserTargetUrl] = useState<string | null>(null);
  const activeProjectIdRef = useRef<string | null>(null);
  const activeWorktreeIdRef = useRef<string | null>(null);
  const terminalPanelRef = useRef<HTMLElement | null>(null);
  const terminalAreaRef = useRef<HTMLDivElement | null>(null);
  const worktreeBranchInputRef = useRef<HTMLInputElement | null>(null);
  const promptInputRef = useRef<HTMLTextAreaElement | null>(null);

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
    isWriteBlocked,
    handleResize,
    handleRefreshTerminalSize,
    handleSelectPaneAt,
  } = terminalController;
  // R12 M3：history sync 永久失败可订阅状态（hooks 必须在 early return 前）。
  const historySyncFailure = useTerminalHistorySyncFailure(activeSessionId);
  const startupBaselineFailure = useStartupBaselineFailure();
  // Business Logic: worktree/Git 域（worktree 生命周期 + 创建表单/busy/error + Git 提交刷新 + merge 阶段）
  // 由独立 controller 持有，避免在 Workbench.tsx 里散落多处 state/effect/handler；controller 接收窄 API/回调 +
  // terminalBridge，不复制邻接域 state。activeWorktreeId 仍由页面持有（终端域 controller / 文件 effect /
  // deep link effect 都读取同一值），controller 通过 setActiveWorktreeId 回写。
  const confirmWorktreeAction = useCallback(
    (message: string): boolean => window.confirm(message),
    [],
  );
  const worktreeGitController = useWorkbenchWorktreeGitController({
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
  }, []);
  const fileController = useWorkbenchFileController({
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
    automationConsoleOpen,
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
  const {
    restoreSummary,
    dismissRestoreNotice,
    snapshotOpen,
    setSnapshotOpen,
    namedSnapshots,
    openSnapshotDialog,
    saveNamedSnapshot,
    applyNamedSnapshot,
    deleteNamedSnapshot,
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
      clearMergeStagePanel();
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
  }, [activeProjectId, clearMergeStagePanel, loadSessions, loadWorktrees, resetFileForContext, setActiveSessionId, setSessions, setWorktrees, setCreateWorktreeOpen, setCreateWorktreeBranchPrefix, setCreateWorktreeBranchSuffixDraft, setGitCommits, setGitHistoryError]);

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

  // Business Logic: 文件域操作函数（toggle/select/open/activate/close/content-change/mode-change/
  // save/format/sqlite/html-asset/create/rename/delete/copy）已迁移到 useWorkbenchFileController；
  // 页面只保留 handleReturnToTerminal / handleReturnToFiles 两个跨域导航回调（它们同时影响 workspaceView
  // 与 automationConsoleOpen 共享状态，且 workbenchWorkspaceSwitch 静态测试需要这两个名字留在页面源码里）。
  const handleReturnToTerminal = useCallback(() => {
    fileController.handleReturnToTerminal();
  }, [fileController]);

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

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户从文件预览返回终端后，仍需要从终端工具栏一键回到已打开的文件工作区，形成对称导航。
   *
   * Code Logic（这个函数做什么）:
   *   委托给文件域 controller 的 handleReturnToFiles（恢复 active tab、隐藏自动化控制台并切回 files 视图）。
   *   保留页面层 useCallback 包装以稳定引用并满足 workbenchWorkspaceSwitch 静态契约。
   */
  const handleReturnToFiles = useCallback(() => {
    setWorkspaceView('files');
    fileController.handleReturnToFiles();
  }, [fileController]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户需要从 Workbench 项目层级进入或退出自动化任务队列，避免再通过自动化层内部的返回按钮理解页面层级。
   *
   * Code Logic（这个函数做什么）:
   *   当前已打开时通过 automation controller 的 closeAutomation 关闭控制台并切回 terminal；
   *   未打开时关闭 Prompt 浮层并打开项目级自动化控制台。
   *   保留 setAutomationConsoleOpen(true) / setWorkspaceView('terminal') 字面调用以稳定共享状态写入顺序，
   *   并满足 workbenchAutomationView 静态契约。
   */
  const handleToggleProjectAutomation = useCallback(() => {
    closePromptPanel();
    if (automationConsoleOpen) {
      closeAutomationConsole();
      return;
    }
    setWorkspaceView('terminal');
    setAutomationConsoleOpen(true);
  }, [automationConsoleOpen, closeAutomationConsole, closePromptPanel]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户在自动化看板中点击 blocked 任务的现场入口时，需要回到对应 Workbench 项目、worktree 和终端。
   *
   * Code Logic（这个函数做什么）:
   *   委托给 automation controller 的 openTaskWorkbench：navigate 到 deep link、关闭控制台并把中心工作区
   *   切回 terminal，让 deep link 聚焦结果可见。
   */
  const handleOpenAutomationTaskWorkbench = useCallback(
    (url: string): void => {
      void openTaskWorkbench(url);
    },
    [openTaskWorkbench],
  );

  const workspaceLine = activeProject
    ? `${activeProject.deviceName} · ${activeProject.path}`
    : t('workbench:noProjectHint');
  const promptPanelStyle = {
    '--prompt-panel-left': `${promptPanelPosition.left}px`,
    '--prompt-panel-top': `${promptPanelPosition.top}px`,
  } as CSSProperties;

  /**
   * Business Logic（为什么需要这个函数）:
   *   零项目空态主 CTA 复用侧栏/rail 同一套「添加本机项目」流程，避免另起一套。
   *
   * Code Logic（这个函数做什么）:
   *   调用 projects context 的 chooseAndAddProject。
   */
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
    <div className={styles.page}>
      <main className={styles.centerPane}>
        <WorkspaceRestoreNotice
          summary={restoreSummary}
          onDismiss={dismissRestoreNotice}
        />
        <WorkspaceSnapshotDialog
          open={snapshotOpen}
          onClose={() => setSnapshotOpen(false)}
          snapshots={namedSnapshots}
          onSaveCurrent={saveNamedSnapshot}
          onApply={applyNamedSnapshot}
          onDelete={deleteNamedSnapshot}
        />
        <section className={styles.workspaceHeader}>
          <div className={styles.workspaceTitleGroup}>
            <div>
              <div className={styles.workspaceTitleRow}>
                <h1 className={styles.workspaceTitle}>{t('workbench:title')}</h1>
                <span className={styles.sessionBadge}>{t('workbench:sessionBadge')}</span>
              </div>
              <p className={styles.workspacePath}>{workspaceLine}</p>
            </div>
          </div>
          <div className={styles.workspaceHeaderActions}>
            <Button
              variant="ghost"
              size="sm"
              type="button"
              onClick={openSnapshotDialog}
            >
              {t('workbench:workspaceSnapshot.openButton')}
            </Button>
            <div className={styles.projectAutomationMeta}>
              <span>{t('workbench:projectAutomation.scope')}</span>
              <strong>
                {t('workbench:projectAutomation.scopeValue', {
                  project: activeProject?.name ?? t('workbench:projectAutomation.noProject'),
                })}
              </strong>
            </div>
            <Button
              className={styles.projectAutomationButton}
              variant="secondary"
              size="sm"
              icon={<OrchestratorIcon />}
              title={t('workbench:projectAutomation.description')}
              aria-label={t('workbench:projectAutomation.open')}
              aria-pressed={automationConsoleOpen}
              data-active={automationConsoleOpen || undefined}
              disabled={!activeProjectId}
              onClick={handleToggleProjectAutomation}
            >
              {t('workbench:projectAutomation.open')}
            </Button>
          </div>
        </section>

        <section
          className={styles.worktreeBar}
          aria-label={t('workbench:worktrees.label')}
          hidden={automationConsoleOpen}
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
            handleRemoveWorktree={handleRemoveWorktree}
          />
        </section>

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
          {dependencyStatus.status !== 'ready' ? <WorkbenchDependencyCard compact className={styles.dependencyNotice} /> : null}
        </div>

        <div className={styles.mainWorkspace}>
          <div
            className={styles.terminalLayer}
            data-hidden={(!terminalFullscreen && (automationConsoleOpen || workspaceView !== 'terminal')) || undefined}
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
                  resolveAgent={agentRuntime.latestAgentForTerminal}
                />
              }
              actions={
                <>
                  {!terminalFullscreen ? (
                    <Button
                      className={styles.terminalActionButton}
                      variant="secondary"
                      size="sm"
                      icon={<BrowserIcon />}
                      title={t('workbench:browserPreview.openWorkspace')}
                      aria-label={t('workbench:browserPreview.openWorkspace')}
                      data-workbench-responsive-action="true"
                      disabled={!activeProject || !activeWorktree}
                      onClick={() => setWorkspaceView('browser')}
                    >
                      <span data-workbench-responsive-label="true">{t('workbench:browserPreview.openWorkspace')}</span>
                    </Button>
                  ) : null}
                  {!terminalFullscreen ? (
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
                  ) : null}
                  <AgentLedgerWorkbenchChrome showTrigger={!terminalFullscreen} disabled={!activeProjectId}
                    open={projectCtrl.agentLedgerOpen} localOnlyAvailable={projectCtrl.agentLedgerLocalOnly}
                    page={projectCtrl.agentLedgerPage} summary={projectCtrl.agentLedgerSummary}
                    loading={projectCtrl.agentLedgerLoading} loadingMore={projectCtrl.agentLedgerLoadingMore}
                    error={projectCtrl.agentLedgerError} onOpen={projectCtrl.openAgentLedger}
                    onClose={projectCtrl.closeAgentLedger} onLoadMore={() => void projectCtrl.loadMoreAgentLedger()}
                    onRefresh={() => void projectCtrl.refreshAgentLedger()} />
                  {!terminalFullscreen ? (
                    <WorkbenchPromptTools
                      hasActiveSession={!!activeSession}
                      remoteWriteDisabled={remoteWriteDisabled}
                      promptPanelOpen={promptPanelOpen}
                      onTogglePromptOptimizer={togglePromptOptimizerPanel}
                      favoriteOpen={favoriteQuickInput.open}
                      onToggleFavorite={favoriteQuickInput.onToggle}
                    />
                  ) : null}
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
                  <Button
                    className={styles.terminalActionButton}
                    variant="secondary"
                    size="sm"
                    icon={<SplitRightIcon />}
                    title={t('workbench:splitPaneRight')}
                    aria-label={t('workbench:splitPaneRight')}
                    data-workbench-responsive-action="true"
                    disabled={!canUsePanes || remoteWriteDisabled}
                    onClick={() => void handleSplitPane('right')}
                  >
                    <span data-workbench-responsive-label="true">{t('workbench:splitPaneRight')}</span>
                  </Button>
                  <Button
                    className={styles.terminalActionButton}
                    variant="secondary"
                    size="sm"
                    icon={<SplitDownIcon />}
                    title={t('workbench:splitPaneDown')}
                    aria-label={t('workbench:splitPaneDown')}
                    data-workbench-responsive-action="true"
                    disabled={!canUsePanes || remoteWriteDisabled}
                    onClick={() => void handleSplitPane('down')}
                  >
                    <span data-workbench-responsive-label="true">{t('workbench:splitPaneDown')}</span>
                  </Button>
                  <Button
                    className={styles.terminalActionButton}
                    variant="secondary"
                    size="sm"
                    icon={<ArrowRightIcon />}
                    title={t('workbench:switchPane')}
                    aria-label={t('workbench:switchPane')}
                    data-workbench-responsive-action="true"
                    disabled={!canSwitchPane || remoteWriteDisabled}
                    onClick={() => void handleSwitchPane()}
                  >
                    <span data-workbench-responsive-label="true">{t('workbench:switchPane')}</span>
                  </Button>
                  <Button
                    className={styles.terminalActionButton}
                    variant="secondary"
                    size="sm"
                    icon={<XIcon />}
                    title={t('workbench:closePane')}
                    aria-label={t('workbench:closePane')}
                    data-workbench-responsive-action="true"
                    disabled={!canUsePanes || remoteWriteDisabled}
                    onClick={() => void handleClosePane()}
                  >
                    <span data-workbench-responsive-label="true">{t('workbench:closePane')}</span>
                  </Button>
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
                  {!terminalFullscreen ? (
                    <Button
                      className={styles.terminalActionButton}
                      variant="secondary"
                      size="sm"
                      icon={<FileIcon />}
                      title={t('workbench:fileWorkspace.openFiles')}
                      aria-label={t('workbench:fileWorkspace.openFiles')}
                      data-workbench-responsive-action="true"
                      disabled={fileTabs.length === 0}
                      onClick={handleReturnToFiles}
                    >
                      <span data-workbench-responsive-label="true">{t('workbench:fileWorkspace.openFiles')}</span>
                    </Button>
                  ) : null}
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
              automationConsoleOpen={automationConsoleOpen}
              workspaceView={workspaceView}
              activeProject={activeProject}
              visibleSessions={visibleSessions}
              mountedSessions={mountedSessions}
              renderedActiveSessionId={renderedActiveSessionId}
              terminalResizeRequestKey={terminalResizeRequestKey}
              handleInput={handleInput}
              isWriteBlocked={isWriteBlocked}
              handleResize={handleResize}
              handleCursorAnchorChange={handleCursorAnchorChange}
              handleSelectPaneAt={handleSelectPaneAt}
              focusSession={focusSession}
              favoriteQuickInput={favoriteQuickInput}
            />
          </div>

          <div
            className={styles.browserLayer}
            data-hidden={(automationConsoleOpen || workspaceView !== 'browser') || undefined}
          >
            <WorkbenchBrowserWorkspace
              surface="desktop"
              transport={tauriWorkbenchTransport}
              project={activeProject}
              worktree={activeWorktree}
              onReturnToTerminal={handleReturnToTerminal}
              onBrowserTargetUrlChange={setBrowserTargetUrl}
            />
          </div>

          <div
            className={styles.fileLayer}
            data-hidden={(automationConsoleOpen || workspaceView !== 'files') || undefined}
          >
            <WorkbenchFileWorkspace
              tabs={fileTabs}
              activeTabId={activeFileTabId}
              saving={fileSaving}
              writeDisabled={remoteWriteDisabled}
              onActivate={handleActivateFileTab}
              onClose={handleCloseFileTab}
              onReturnToTerminal={handleReturnToTerminal}
              onContentChange={handleFileContentChange}
              onModeChange={handleFileModeChange}
              loadHtmlAsset={handleLoadHtmlAsset}
              onSave={handleSaveFileTab}
              onFormat={handleFormatFileTab}
              onSelectSqliteTable={handleSelectSqliteTable}
            />
          </div>

          {automationConsoleOpen ? (
            <div className={styles.automationLayer}>
              <header className={styles.automationHeader}>
                <div className={styles.automationHeadingGroup}>
                  <span className={styles.automationEyebrow}>{t('workbench:projectAutomation.scope')}</span>
                  <h2 className={styles.automationTitle}>{t('workbench:projectAutomation.title')}</h2>
                  <p className={styles.automationDescription}>{t('workbench:projectAutomation.description')}</p>
                </div>
                <div className={styles.automationContext}>
                  <span>
                    {t('workbench:projectAutomation.scopeValue', {
                      project: activeProject?.name ?? t('workbench:projectAutomation.noProject'),
                    })}
                  </span>
                </div>
              </header>
              <div className={styles.automationBody}>
                <OrchestratorPanel
                  embedded
                  onOpenWorkbench={handleOpenAutomationTaskWorkbench}
                  focusTaskId={automationFocusTaskId}
                  focusOutboxId={automationFocusOutboxId}
                  onFocusTargetNotFound={handleAttentionTargetNotFound}
                />
              </div>
            </div>
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
          activeAgent={
            activeSessionId ? agentRuntime.latestAgentForTerminal(activeSessionId) : null
          }
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
            mergeStages, loadGitHistory,
            handleCommitWorktree, handlePushWorktree, handleMergeWorktree,
          }}
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
