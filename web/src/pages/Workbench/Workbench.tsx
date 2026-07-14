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
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { configApi } from '@/api/config';
import { promptOptimizerApi } from '@/api/promptOptimizer';
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
import { useWorkbenchTerminalBuffers } from '@/hooks/workbenchTerminalBuffersContext';
import { useAttention } from '@/hooks/useAttention';
import {
  BrowserIcon, EditIcon, FileIcon, MaximizeIcon, MinimizeIcon, OrchestratorIcon,
  RefreshIcon, SearchIcon, SplitDownIcon, SplitRightIcon, XIcon,
} from '@/lib/icons';
import type { PromptOptimizerFillLanguage } from '@/lib/types';
import styles from './Workbench.module.css';
import { parseWorkbenchDeepLink } from './workbenchDeepLink';
import { workbenchTerminalOptions } from './terminalOptions';
import { terminalPanePixelSize } from './terminalSizing';
import type { TerminalLayoutMode } from './terminalSizing';
import { useWorkbenchProjectController } from './controllers/useWorkbenchProjectController';
import { useWorkbenchTerminalController } from './controllers/useWorkbenchTerminalController';
import type { WorkbenchTerminalErrorKey } from './controllers/useWorkbenchTerminalController';
import { useWorkbenchWorktreeGitController } from './controllers/useWorkbenchWorktreeGitController';
import type { WorkbenchWorktreeGitErrorKey } from './controllers/useWorkbenchWorktreeGitController';
import { useWorkbenchFileController } from './controllers/useWorkbenchFileController';
import type { WorkbenchFileErrorKey, WorkbenchFileMessageKey } from './controllers/useWorkbenchFileController';
import { useWorkbenchAutomationController } from './controllers/useWorkbenchAutomationController';
import { useWorkbenchPromptOptimizerController } from './controllers/useWorkbenchPromptOptimizerController';
import type { PromptOptimizerConfigLoadResult } from './controllers/useWorkbenchPromptOptimizerController';
import { useWorkbenchSessionSearchController } from './controllers/useWorkbenchSessionSearchController';
import { WorkbenchTerminalArea } from './WorkbenchTerminalArea';
import { WorkbenchInspector } from './WorkbenchInspector';
import type { WorkbenchInspectorTab } from './WorkbenchInspector';
import { WorkbenchSessionTabs } from './WorkbenchSessionTabs';
import { WorkbenchStatusCard } from './WorkbenchStatusCard';
import { WorkbenchWorktreeBar } from './WorkbenchWorktreeBar';
import { activeWorktreeRootPath, DEFAULT_WORKTREE_BRANCH_PREFIX } from './workbenchWorktrees';
import type { WorkbenchFileWorkspaceView } from './workbenchFiles';

interface TauriInternalsWindow extends Window {
  __TAURI_INTERNALS__?: {
    transformCallback?: unknown;
  };
}

interface TerminalSize {
  cols: number;
  rows: number;
}

const MIN_TERMINAL_COLS = 20;
const MIN_TERMINAL_ROWS = 6;
const TERMINAL_PANE_HEADER_PX = 36;

/**
 * Business Logic（为什么需要这个函数）:
 *   普通 Vite/Playwright 浏览器环境没有 Tauri event internals，直接 listen 会导致调试白屏。
 *
 * Code Logic（这个函数做什么）:
 *   检测 window.__TAURI_INTERNALS__.transformCallback 是否存在，作为是否注册 Tauri event 的边界。
 */
function canListenToTauriEvents(): boolean {
  const internals = (window as TauriInternalsWindow).__TAURI_INTERNALS__;
  return typeof internals?.transformCallback === 'function';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   当前会话状态需要展示运行时长，让用户判断终端会话是否长期运行或已经退出多久。
 *
 * Code Logic（这个函数做什么）:
 *   根据 startedAt 与 exitedAt/当前时间计算秒差，并格式化为 h/m/s 的紧凑文本。
 */
function formatRuntime(
  startedAt: string | null,
  endedAt: string | null,
  nowMs: number,
  emptyValue: string,
): string {
  if (!startedAt) return emptyValue;
  const start = new Date(startedAt).getTime();
  const end = endedAt ? new Date(endedAt).getTime() : nowMs;
  if (Number.isNaN(start) || Number.isNaN(end) || end < start) return emptyValue;
  const totalSeconds = Math.floor((end - start) / 1000);
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  if (hours > 0) return `${hours}h ${minutes}m`;
  if (minutes > 0) return `${minutes}m ${seconds}s`;
  return `${seconds}s`;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   终端 resize 命令后端接受 u16，前端需要提前 clamp，避免极端布局值反序列化失败。
 *
 * Code Logic（这个函数做什么）:
 *   取整数并限制在 1..65535 区间。
 */
function clampU16(value: number, min: number): number {
  const rounded = Math.max(min, Math.round(value));
  return Math.min(65535, rounded);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   交互式终端程序会按 PTY 初始 cols/rows 绘制首屏；如果后端先用默认尺寸启动，前端随后 resize 会导致首屏错位。
 *
 * Code Logic（这个函数做什么）:
 *   按当前终端布局计算单个 pane 的像素尺寸，复用真实 host/viewport 结构创建离屏 xterm；
 *   FitAddon 只读取无 padding 的 viewport 尺寸，测完 cols/rows 后立即销毁。
 */
function measureInitialTerminalSize(
  panel: HTMLElement | null,
  layout: TerminalLayoutMode,
): TerminalSize | undefined {
  if (!panel || panel.clientWidth <= 0 || panel.clientHeight <= 0) return undefined;
  const paneSize = terminalPanePixelSize({
    panelWidth: panel.clientWidth,
    panelHeight: panel.clientHeight,
    layout,
    headerHeight: TERMINAL_PANE_HEADER_PX,
  });
  if (paneSize.width <= 0 || paneSize.height <= 0) return undefined;

  const host = document.createElement('div');
  const viewport = document.createElement('div');
  host.className = styles.terminalHost;
  viewport.className = styles.terminalViewport;
  host.style.position = 'fixed';
  host.style.left = '-10000px';
  host.style.top = '-10000px';
  host.style.width = `${paneSize.width}px`;
  host.style.height = `${paneSize.height}px`;
  host.style.visibility = 'hidden';
  host.style.pointerEvents = 'none';
  host.appendChild(viewport);
  document.body.appendChild(host);

  const terminal = new Terminal(workbenchTerminalOptions());
  const fit = new FitAddon();
  try {
    terminal.loadAddon(fit);
    terminal.open(viewport);
    fit.fit();
    return {
      cols: clampU16(terminal.cols, MIN_TERMINAL_COLS),
      rows: clampU16(terminal.rows, MIN_TERMINAL_ROWS),
    };
  } catch {
    return undefined;
  } finally {
    terminal.dispose();
    host.remove();
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   工作台依赖 Tauri IPC；普通浏览器调试环境会抛底层 invoke 错误，不应把内部异常文本展示给用户。
 *
 * Code Logic（这个函数做什么）:
 *   将已知 Tauri unavailable 错误映射为友好文案；其他 Error 保留 message，未知错误回退默认文案。
 */
function displayErrorMessage(error: unknown, fallback: string, desktopUnavailable: string): string {
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
 *   React lint 要求 effect 主体不要同步触发级联 setState；工作台仍需要在依赖变化后重置或拉取状态。
 *
 * Code Logic（这个函数做什么）:
 *   把 effect 内的状态同步延后到下一个 macrotask，并返回清理函数取消尚未执行的任务。
 */
function deferEffect(work: () => void): () => void {
  const timer = window.setTimeout(work, 0);
  return () => window.clearTimeout(timer);
}

/**
 * Business Logic（为什么需要这个组件）:
 *   工作台是用户进入项目并操作项目终端的主界面。
 *
 * Code Logic（这个组件做什么）:
 *   聚合项目、会话、终端输出 buffer、文件树与文件操作状态，并组合三栏布局。
 */
export function Workbench() {
  const { t } = useTranslation(['workbench', 'common', 'promptOptimizer', 'attention']);
  const location = useLocation();
  const navigate = useNavigate();
  const { status: dependencyStatus } = useWorkbenchDependency();
  const {
    projects,
    activeProjectId,
    activeProject,
    selectProject,
    refreshProjectSessionStats,
  } = useWorkbenchProjects();
  const { resetBuffer: resetTerminalBuffer, removeBuffer: removeTerminalBuffer } =
    useWorkbenchTerminalBuffers();
  const locationSearch = location.search;
  const workbenchDeepLink = useMemo(
    () => parseWorkbenchDeepLink(locationSearch),
    [locationSearch],
  );
  // Business Logic: 项目域（远端离线状态机 + 跨项目请求守卫 + 项目级 deep link）由独立 controller 持有，
  // 避免在 Workbench.tsx 里散落多处 state/effect；controller 接收窄 API/回调，不复制邻接域 state。
  const {
    remoteProjectOffline,
    remoteWriteDisabled,
    isCurrentProject,
    markRequestFailure,
    markRequestSuccess,
    selectProjectFromDeepLink,
  } = useWorkbenchProjectController({
    activeProject,
    activeProjectId,
    projects,
    selectProject,
  });
  const [activeWorktreeId, setActiveWorktreeId] = useState<string | null>(null);
  // Business Logic: workspaceView / automationConsoleOpen 是跨域共享状态（终端全屏、自动化控制台、文件 tab 都会改写），
  // 仍由 Workbench.tsx 持有；文件域 controller 通过 requestWorkspaceView / requestHideAutomationConsole 回调表达意图。
  const [workspaceView, setWorkspaceView] = useState<WorkbenchFileWorkspaceView>('terminal');
  const [automationConsoleOpen, setAutomationConsoleOpen] = useState<boolean>(false);
  const [inspectorTab, setInspectorTab] = useState<WorkbenchInspectorTab>('files');
  const [runtimeNow, setRuntimeNow] = useState<number>(() => Date.now());
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
  // Business Logic: translateError 必须稳定（useCallback），否则 controller 内 loadSessions 等 useCallback 的依赖
  // 每次渲染都变，导致 project-switch effect 反复重跑 loadSessions，把 terminal-status 事件更新覆盖回 running。
  const translateTerminalError = useCallback(
    (key: WorkbenchTerminalErrorKey): string => t(`workbench:errors.${key}`),
    [t],
  );
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
    canRefreshCurrentTerminalSize,
    setSessionNameDraft,
    setSessionError,
    setActiveSessionId,
    setSessions,
    loadSessions,
    focusSession,
    handleCreateSession,
    handleSplitPane,
    handleClosePane,
    handleCloseSession,
    handleRenameSession,
    handleInput,
    handleResize,
    handleRefreshTerminalSize,
  } = terminalController;
  // Business Logic: worktree/Git 域（worktree 生命周期 + 创建表单/busy/error + Git 提交刷新 + merge 阶段）
  // 由独立 controller 持有，避免在 Workbench.tsx 里散落多处 state/effect/handler；controller 接收窄 API/回调 +
  // terminalBridge，不复制邻接域 state。activeWorktreeId 仍由页面持有（终端域 controller / 文件 effect /
  // deep link effect 都读取同一值），controller 通过 setActiveWorktreeId 回写。
  // Code Logic: translateWorktreeError 必须稳定（useCallback），否则 controller 内 useCallback 依赖每次渲染都变。
  const translateWorktreeError = useCallback(
    (key: WorkbenchWorktreeGitErrorKey): string => t(`workbench:errors.${key}`),
    [t],
  );
  const translateWorktreeMessage = useCallback(
    (
      key: 'mergeConfirm' | 'removeConfirm' | 'checkSourceMessage',
      vars?: Record<string, unknown>,
    ): string => {
      if (key === 'mergeConfirm') return t('workbench:worktrees.mergeConfirm', vars);
      if (key === 'removeConfirm') return t('workbench:worktrees.removeConfirm', vars);
      return t('workbench:mergeStages.messages.checkSource');
    },
    [t],
  );
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
    clearMergeStagePanel,
  } = worktreeGitController;
  // Business Logic: 文件工作区域（目录树 + tab + dirty/save/format + create/rename/delete/copy）由独立 controller
  // 持有，避免在 Workbench.tsx 里散落多处 state/handler/effect；controller 接收窄 API/回调，不复制邻接域 state。
  // workspaceView / automationConsoleOpen 仍由页面持有（跨域共享），controller 通过 request* 回调表达切换意图。
  // Code Logic: translateFileError / translateFileMessage 必须稳定（useCallback），否则 controller 内 useCallback
  // 依赖每次渲染都变，导致项目/worktree 切换 effect 反复重跑 loadDir。
  const translateFileError = useCallback(
    (key: WorkbenchFileErrorKey): string => t(`workbench:errors.${key}`),
    [t],
  );
  const translateFileMessage = useCallback(
    (key: WorkbenchFileMessageKey, vars?: Record<string, unknown>): string => {
      if (key === 'saved') return t('workbench:fileWorkspace.saved');
      if (key === 'formatted') return t('workbench:fileWorkspace.formatted');
      if (key === 'pathCopied') return t('workbench:pathCopied');
      if (key === 'confirmCloseDirtyFile') return t('workbench:confirmCloseDirtyFile', vars);
      if (key === 'confirmDeleteDirtyFiles') return t('workbench:confirmDeleteDirtyFiles', vars);
      return t('workbench:confirmDeletePath', vars);
    },
    [t],
  );
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
  const activeSessionRuntime = formatRuntime(
    activeSession?.startedAt ?? null,
    activeSession?.exitedAt ?? null,
    runtimeNow,
    emptyValue,
  );
  const TerminalFullscreenIcon = terminalFullscreen ? MinimizeIcon : MaximizeIcon;
  const terminalFullscreenLabel = terminalFullscreen
    ? t('workbench:terminalExitFullscreen')
    : t('workbench:terminalEnterFullscreen');
  const promptWorkingDirectory = activeRootPath || undefined;
  // Business Logic: Prompt 优化浮层域（配置加载 + 打开/输入/定位 + Control 单键快捷键 + IME 安全 +
  // 流式写入终端 + 焦点回归 + 重新打开清空）由独立 controller 持有，避免在 Workbench.tsx 里散落
  // 多处 state/effect/handler；controller 接收窄 API/回调 + 共享 refs，不复制邻接域 state。
  // Code Logic: translateFillFailed / translateOptimizeFailed / loadConfig / streamToTerminal 必须稳定
  // （useCallback / useCallback 包装），否则 controller 内 runPromptOptimization 的 useCallback 依赖每次渲染都变。
  const translatePromptFillFailed = useCallback(
    (): string => t('workbench:promptOptimizer.fillFailed'),
    [t],
  );
  const translatePromptOptimizeFailed = useCallback(
    (): string => t('workbench:promptOptimizer.optimizeFailed'),
    [t],
  );
  const translatePromptRemoteOffline = useCallback(
    (): string =>
      t('workbench:remoteOfflineNotice', {
        device: activeProject?.deviceName ?? t('workbench:emptyValue'),
      }),
    [activeProject?.deviceName, t],
  );
  const loadPromptOptimizerConfig = useCallback(async (): Promise<PromptOptimizerConfigLoadResult> => {
    const config = await configApi.get();
    return {
      promptOptimizerHotkey: config.promptOptimizerHotkey,
      promptOptimizerFillLanguage: config.promptOptimizerFillLanguage,
    };
  }, []);
  const streamPromptToTerminal = useCallback(
    (
      prompt: string,
      options: {
        workingDirectory?: string | null;
        targetLanguage: PromptOptimizerFillLanguage;
        sessionId: string;
      },
    ) => promptOptimizerApi.streamToTerminal(prompt, options),
    [],
  );
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

  useEffect(() => {
    activeProjectIdRef.current = activeProjectId;
  }, [activeProjectId]);

  useEffect(() => {
    activeWorktreeIdRef.current = activeWorktreeId;
  }, [activeWorktreeId]);

  useEffect(() => {
    const timer = window.setInterval(() => setRuntimeNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, []);

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
      resetFileForContext(activeProjectId, null);
      if (!activeProjectId) {
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
        return;
      }
      setWorktrees([]);
      setActiveWorktreeId(null);
      setCreateWorktreeOpen(false);
      setCreateWorktreeBranchPrefix(DEFAULT_WORKTREE_BRANCH_PREFIX);
      setCreateWorktreeBranchSuffixDraft('');
      setWorkspaceView('terminal');
      setGitCommits([]);
      setGitHistoryError(null);
      void loadWorktrees(activeProjectId);
      void loadSessions(activeProjectId);
    });
  }, [activeProjectId, clearMergeStagePanel, loadSessions, loadWorktrees, resetFileForContext, setActiveSessionId, setSessions, setWorktrees, setCreateWorktreeOpen, setCreateWorktreeBranchPrefix, setCreateWorktreeBranchSuffixDraft, setGitCommits, setGitHistoryError]);

  useEffect(() => {
    return deferEffect(() => {
      // Business Logic: worktree 切换时文件域需要彻底重置（含 stale 守卫），随后按新 worktree 重新加载根目录。
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

  return (
    <div className={styles.page}>
      <main className={styles.centerPane}>
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
          {sessionError ? <StatusMessage tone="danger" className={styles.errorBox}>{sessionError}</StatusMessage> : null}
          {worktreeError ? <StatusMessage tone="danger" className={styles.errorBox}>{worktreeError}</StatusMessage> : null}
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
                      <span data-workbench-responsive-label="true">
                        {t('workbench:browserPreview.openWorkspace')}
                      </span>
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
                      <span data-workbench-responsive-label="true">
                        {t('workbench:sessionSearch.open')}
                      </span>
                    </Button>
                  ) : null}
                  {!terminalFullscreen ? (
                    <Button
                      className={styles.terminalActionButton}
                      variant="secondary"
                      size="sm"
                      icon={<EditIcon />}
                      title={t('workbench:promptOptimizer.open')}
                      aria-label={t('workbench:promptOptimizer.open')}
                      data-workbench-responsive-action="true"
                      data-active={promptPanelOpen || undefined}
                      disabled={!activeSession || (remoteWriteDisabled && !promptPanelOpen)}
                      onClick={togglePromptOptimizerPanel}
                    >
                      <span data-workbench-responsive-label="true">
                        {t('workbench:promptOptimizer.open')}
                      </span>
                    </Button>
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
                    <span data-workbench-responsive-label="true">
                      {t('workbench:fitTerminalSize')}
                    </span>
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
                    <span data-workbench-responsive-label="true">
                      {t('workbench:splitPaneRight')}
                    </span>
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
                    <span data-workbench-responsive-label="true">
                      {t('workbench:splitPaneDown')}
                    </span>
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
                    <span data-workbench-responsive-label="true">
                      {t('workbench:closePane')}
                    </span>
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
                    onClick={
                      terminalFullscreen
                        ? handleExitTerminalFullscreen
                        : handleEnterTerminalFullscreen
                    }
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
                      <span data-workbench-responsive-label="true">
                        {t('workbench:fileWorkspace.openFiles')}
                      </span>
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
              handleResize={handleResize}
              handleCursorAnchorChange={handleCursorAnchorChange}
              focusSession={focusSession}
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

      <aside className={styles.inspectorPane}>
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
          activeSessionRuntime={activeSessionRuntime}
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
            gitHistoryError,
            worktreeBusy,
            unknownMutationLock,
            mergeStages,
            loadGitHistory,
            handleCommitWorktree,
            handlePushWorktree,
            handleMergeWorktree,
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
