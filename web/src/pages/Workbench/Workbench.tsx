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
import { workbenchApi } from '@/api/workbench';
import { tauriWorkbenchTransport } from '@/api/workbenchTransport';
import { OrchestratorPanel } from '@/pages/Orchestrator';
import {
  WorkbenchBrowserWorkspace,
  WorkbenchDependencyCard,
  WorkbenchFileWorkspace,
} from '@/components/domain';
import type { WorkbenchOpenFileTab } from '@/components/domain';
import { WorkbenchSessionSearch } from '@/components/domain';
import { WorkbenchWorkspaceNav } from '@/components/layout';
import { Button, Card, Input, Pill } from '@/components/primitives';
import { useWorkbenchDependency } from '@/hooks/workbenchDependencyContext';
import { useWorkbenchProjects } from '@/hooks/workbenchProjectsContext';
import { useWorkbenchTerminalBuffers } from '@/hooks/workbenchTerminalBuffersContext';
import {
  ChevronRightIcon,
  BrowserIcon,
  CopyIcon,
  EditIcon,
  FileIcon,
  FolderIcon,
  MaximizeIcon,
  MinimizeIcon,
  OrchestratorIcon,
  PlusIcon,
  RefreshIcon,
  SearchIcon,
  SplitDownIcon,
  SplitRightIcon,
  SyncIcon,
  TrashIcon,
  UploadIcon,
  XIcon,
} from '@/lib/icons';
import type {
  PromptOptimizerFillLanguage,
  WorkbenchFileNode,
  WorkbenchHtmlAsset,
  WorkbenchMergeStage,
  WorkbenchMergeStageId,
  WorkbenchPathInfo,
} from '@/lib/types';
import styles from './Workbench.module.css';
import { parseWorkbenchDeepLink } from './workbenchDeepLink';
import { workbenchTerminalOptions } from './terminalOptions';
import { terminalPanePixelSize } from './terminalSizing';
import type { TerminalLayoutMode } from './terminalSizing';
import { useWorkbenchProjectController } from './controllers/useWorkbenchProjectController';
import {
  useWorkbenchTerminalController,
} from './controllers/useWorkbenchTerminalController';
import type { WorkbenchTerminalErrorKey } from './controllers/useWorkbenchTerminalController';
import {
  useWorkbenchWorktreeGitController,
} from './controllers/useWorkbenchWorktreeGitController';
import type { WorkbenchWorktreeGitErrorKey } from './controllers/useWorkbenchWorktreeGitController';
import { useWorkbenchFileController } from './controllers/useWorkbenchFileController';
import type {
  WorkbenchFileErrorKey,
  WorkbenchFileMessageKey,
} from './controllers/useWorkbenchFileController';
import { useWorkbenchAutomationController } from './controllers/useWorkbenchAutomationController';
import { useWorkbenchPromptOptimizerController } from './controllers/useWorkbenchPromptOptimizerController';
import type { PromptOptimizerConfigLoadResult } from './controllers/useWorkbenchPromptOptimizerController';
import { useWorkbenchSessionSearchController } from './controllers/useWorkbenchSessionSearchController';
import { WorkbenchTerminalPane } from './WorkbenchTerminalPane';
import {
  activeWorktreeRootPath,
  buildGitGraphRows,
  canCommitWorktree,
  canMergeWorktree,
  canPushWorktree,
  canRemoveWorktree,
  composeWorktreeBranchName,
  DEFAULT_WORKTREE_BRANCH_PREFIX,
  formatWorkbenchMergeStages,
  formatCommitRelativeTime,
  hasGitHistory,
  WORKTREE_BRANCH_PREFIXES,
  worktreeChangeCount,
  worktreeStatusTone,
} from './workbenchWorktrees';
import type { WorktreeBranchPrefix } from './workbenchWorktrees';
import {
  collectTabsForPath,
  dirtyTabNames,
  dropExpandedPathTree,
  dropPathTreeEntries,
  isLatestRequest,
  validateJsonText,
  validateTomlText,
  validateYamlText,
  workbenchDirRequestKey,
  workbenchDirRequestKeyMatchesPath,
} from './workbenchFiles';
import type { WorkbenchFileWorkspaceView } from './workbenchFiles';

interface TauriInternalsWindow extends Window {
  __TAURI_INTERNALS__?: {
    transformCallback?: unknown;
  };
}

type WorkbenchInspectorTab = 'files' | 'history';

const GIT_GRAPH_LANE_WIDTH = 14;
const GIT_GRAPH_ROW_HEIGHT = 58;
const GIT_GRAPH_DOT_Y = 22;
const GIT_GRAPH_DOT_RADIUS = 4;

interface FileTreeProps {
  nodes: WorkbenchFileNode[];
  childrenByPath: Record<string, WorkbenchFileNode[]>;
  expandedPaths: Set<string>;
  selectedPath: string | null;
  loadingPath: string | null;
  onToggle: (node: WorkbenchFileNode) => void;
  onSelect: (node: WorkbenchFileNode) => void;
}

interface FileTreeNodeProps extends FileTreeProps {
  node: WorkbenchFileNode;
  depth: number;
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
 *   文件操作默认作用在当前选中文件夹；若选中的是文件，则作用在它的父目录。
 *
 * Code Logic（这个函数做什么）:
 *   从相对路径中取最后一个 `/` 之前的部分；根级文件返回空字符串。
 */
function parentPathOf(path: string): string {
  const index = path.lastIndexOf('/');
  return index >= 0 ? path.slice(0, index) : '';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   文件树和状态栏需要展示简短路径名；根目录没有 basename 时显示根符号。
 *
 * Code Logic（这个函数做什么）:
 *   取相对路径最后一段；空路径返回 `/`。
 */
function basename(path: string, rootLabel: string): string {
  if (!path) return rootLabel;
  const parts = path.split('/').filter(Boolean);
  return parts[parts.length - 1] ?? rootLabel;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   检查器要展示文件大小，直接展示字节数不利于扫描。
 *
 * Code Logic（这个函数做什么）:
 *   把字节数格式化为 B/KB/MB/GB；目录或未知大小返回占位符。
 */
function formatSize(size: number | null, emptyValue: string): string {
  if (size === null) return emptyValue;
  if (size < 1024) return `${size} B`;
  const kb = size / 1024;
  if (kb < 1024) return `${kb.toFixed(1)} KB`;
  const mb = kb / 1024;
  if (mb < 1024) return `${mb.toFixed(1)} MB`;
  return `${(mb / 1024).toFixed(1)} GB`;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   最近打开时间、文件修改时间需要展示成用户本地可读格式。
 *
 * Code Logic（这个函数做什么）:
 *   使用浏览器本地化短日期时间；解析失败时回退原始字符串。
 */
function formatDateTime(value: string | null, emptyValue: string): string {
  if (!value) return emptyValue;
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString(undefined, {
    month: 'short',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  });
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
 *   Git graph 需要多条稳定颜色 lane，但具体颜色由 design token 控制。
 *
 * Code Logic（这个函数做什么）:
 *   将 graph helper 的 colorIndex 映射到 CSS custom property。
 */
function gitGraphColorStyle(colorIndex: number): CSSProperties {
  return {
    '--git-graph-color': `var(--git-graph-${colorIndex % 6})`,
  } as CSSProperties;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Git graph SVG 需要按 lane 数动态扩展宽度，避免 merge 线被裁切。
 *
 * Code Logic（这个函数做什么）:
 *   根据 laneCount 计算紧凑 graph 宽度。
 */
function gitGraphWidth(laneCount: number): number {
  return Math.max(24, laneCount * GIT_GRAPH_LANE_WIDTH + 10);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Git graph 每个 lane 需要稳定 x 坐标，供点、竖线和 merge 曲线复用。
 *
 * Code Logic（这个函数做什么）:
 *   将 lane index 映射到 SVG 内部横坐标。
 */
function gitGraphX(lane: number): number {
  return 5 + lane * GIT_GRAPH_LANE_WIDTH;
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
 * Business Logic（为什么需要这个函数）:
 *   状态 Pill 需要把 session status 映射为稳定 tone，便于用户快速判断运行/退出/断开。
 *
 * Code Logic（这个函数做什么）:
 *   running→success，exited→neutral，disconnected→danger，其余状态使用 warn。
 */
function statusTone(status: string): 'neutral' | 'success' | 'warn' | 'danger' {
  if (status === 'running') return 'success';
  if (status === 'exited') return 'neutral';
  if (status === 'disconnected') return 'danger';
  return 'warn';
}

/**
 * Business Logic（为什么需要这个组件）:
 *   文件树需要懒加载多级目录，同时保持目录展开、选中态和 loading 态一致。
 *
 * Code Logic（这个组件做什么）:
 *   递归渲染 WorkbenchFileNode；目录按钮负责展开/收起，文件点击只更新选中路径。
 */
function FileTreeNode(props: FileTreeNodeProps) {
  const { node, depth, childrenByPath, expandedPaths, selectedPath, loadingPath, onToggle, onSelect } =
    props;
  const isDir = node.kind === 'dir';
  const expanded = expandedPaths.has(node.path);
  const selected = selectedPath === node.path;
  const children = childrenByPath[node.path] ?? [];
  const paddingStyle = { paddingLeft: 8 + depth * 14 } as CSSProperties;

  return (
    <div className={styles.treeBranch}>
      <button
        type="button"
        className={styles.treeRow}
        data-selected={selected || undefined}
        style={paddingStyle}
        onClick={() => {
          onSelect(node);
          if (isDir) onToggle(node);
        }}
      >
        <span className={styles.treeChevron} data-expanded={expanded || undefined}>
          {isDir ? <ChevronRightIcon size={14} /> : null}
        </span>
        <span className={styles.treeIcon}>
          {isDir ? <FolderIcon size={14} /> : <FileIcon size={14} />}
        </span>
        <span className={styles.treeName}>{node.name}</span>
        {loadingPath === node.path ? <span className={styles.treeLoading}>…</span> : null}
      </button>
      {isDir && expanded ? (
        <FileTree
          nodes={children}
          childrenByPath={childrenByPath}
          expandedPaths={expandedPaths}
          selectedPath={selectedPath}
          loadingPath={loadingPath}
          onToggle={onToggle}
          onSelect={onSelect}
          depth={depth + 1}
        />
      ) : null}
    </div>
  );
}

interface NestedFileTreeProps extends FileTreeProps {
  depth?: number;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   右侧检查器需要展示可交互项目文件夹，支持目录展开与文件选中。
 *
 * Code Logic（这个组件做什么）:
 *   渲染同层节点列表，并把当前递归深度传给 FileTreeNode 控制缩进。
 */
function FileTree(props: NestedFileTreeProps) {
  const { nodes, depth = 0 } = props;
  return (
    <div className={styles.treeList}>
      {nodes.map((node) => (
        <FileTreeNode key={node.path || node.name} {...props} node={node} depth={depth} />
      ))}
    </div>
  );
}

/**
 * Business Logic（为什么需要这个组件）:
 *   工作台是用户进入项目并操作项目终端的主界面。
 *
 * Code Logic（这个组件做什么）:
 *   聚合项目、会话、终端输出 buffer、文件树与文件操作状态，并组合三栏布局。
 */
export function Workbench() {
  const { t } = useTranslation(['workbench', 'common', 'promptOptimizer']);
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
    loadPathInfo,
    handleToggleNode,
    handleSelectNode,
    handleOpenFile,
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
    openAutomation: openAutomationConsole,
    closeAutomation: closeAutomationConsole,
    openTaskWorkbench,
  } = automationController;
  const activeWorktree = useMemo(
    () => worktrees.find((worktree) => worktree.id === activeWorktreeId) ?? worktrees[0] ?? null,
    [activeWorktreeId, worktrees],
  );
  const gitGraphRows = useMemo(() => buildGitGraphRows(gitCommits), [gitCommits]);
  const renderedMergeStages = useMemo(
    () => (mergeStages.length > 0 ? formatWorkbenchMergeStages(mergeStages) : []),
    [mergeStages],
  );
  const selectedParentPath = selectedInfo
    ? selectedInfo.kind === 'dir'
      ? selectedInfo.path
      : parentPathOf(selectedInfo.path)
    : '';
  const selectedDisplayPath = selectedInfo?.path ?? '';
  const emptyValue = t('workbench:emptyValue');
  const rootPath = t('workbench:rootPath');
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
  const composedWorktreeBranchName = composeWorktreeBranchName(
    createWorktreeBranchPrefix,
    createWorktreeBranchSuffixDraft,
  );
  const mergeStageLabel = useCallback(
    (stageId: WorkbenchMergeStageId): string => {
      switch (stageId) {
        case 'checkSource':
          return t('workbench:mergeStages.labels.checkSource');
        case 'closeSessions':
          return t('workbench:mergeStages.labels.closeSessions');
        case 'mergeMain':
          return t('workbench:mergeStages.labels.mergeMain');
        case 'resolveConflicts':
          return t('workbench:mergeStages.labels.resolveConflicts');
        case 'cleanup':
          return t('workbench:mergeStages.labels.cleanup');
      }
    },
    [t],
  );
  const mergeStageFallbackMessage = useCallback(
    (stage: WorkbenchMergeStage): string => {
      switch (stage.status) {
        case 'pending':
          return t('workbench:mergeStages.status.pending');
        case 'running':
          return t('workbench:mergeStages.status.running');
        case 'completed':
          return t('workbench:mergeStages.status.completed');
        case 'failed':
          return t('workbench:mergeStages.status.failed');
        case 'skipped':
          return t('workbench:mergeStages.status.skipped');
      }
    },
    [t],
  );

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

  /**
   * Business Logic（为什么需要这个函数）:
   *   桌面端用户需要把当前终端临时铺满屏幕，隐藏项目标题、worktree 管理层、文件层和右侧检查器以专注操作。
   *
   * Code Logic（这个函数做什么）:
   *   关闭可能遮挡终端的 Prompt 优化浮层，切回 terminal 工作区，并通过 controller 打开 terminalLayer 的 fixed overlay 状态。
   */
  const handleEnterTerminalFullscreen = useCallback((): void => {
    closePromptPanel();
    setWorkspaceView('terminal');
    terminalController.handleEnterTerminalFullscreen();
  }, [closePromptPanel, terminalController]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   进入终端全屏后必须有明确出口，恢复完整 Workbench 布局和其他面板内容。
   *
   * Code Logic（这个函数做什么）:
   *   通过 controller 关闭 terminalLayer 的 fixed overlay 状态；不改变当前 session/worktree 或文件 tab 状态。
   */
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

  const sessionStatusLabel = activeSession
    ? activeSession.status === 'running'
      ? t('workbench:sessionStatus.running')
      : activeSession.status === 'exited'
        ? t('workbench:sessionStatus.exited')
        : activeSession.status === 'disconnected'
          ? t('workbench:sessionStatus.disconnected')
          : activeSession.status
    : t('workbench:sessionStatus.none');
  const selectedKindLabel = selectedInfo
    ? selectedInfo.kind === 'dir'
      ? t('workbench:pathKinds.dir')
      : selectedInfo.kind === 'file'
        ? t('workbench:pathKinds.file')
        : selectedInfo.kind
    : emptyValue;
  const workspaceLine = activeProject
    ? `${activeProject.deviceName} · ${activeProject.path}`
    : t('workbench:noProjectHint');
  const activeWorktreeTone = activeWorktree ? worktreeStatusTone(activeWorktree) : 'neutral';
  const activeWorktreePillTone = activeWorktreeTone === 'warning' ? 'warn' : activeWorktreeTone;
  const activeWorktreeChangedCount = worktreeChangeCount(activeWorktree);
  const activeWorktreeStatusLabel = activeWorktree
    ? activeWorktree.status.conflicts > 0
      ? t('workbench:worktrees.status.conflict', { count: activeWorktree.status.conflicts })
      : activeWorktree.status.clean
        ? t('workbench:worktrees.status.clean')
        : t('workbench:worktrees.status.dirty', { count: activeWorktree.status.changed })
    : emptyValue;
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
          <div className={styles.worktreeStrip}>
            {worktrees.length === 0 ? (
              <span className={styles.worktreeEmpty}>{t('workbench:worktrees.empty')}</span>
            ) : (
              worktrees.map((worktree) => {
                const tone = worktreeStatusTone(worktree);
                const label = worktree.branch ?? worktree.name;
                return (
                  <button
                    key={worktree.id}
                    type="button"
                    className={styles.worktreeChip}
                    data-active={worktree.id === activeWorktree?.id || undefined}
                    data-tone={tone}
                    onClick={() => setActiveWorktreeId(worktree.id)}
                  >
                    <span className={styles.worktreeDot} data-tone={tone} />
                    <span className={styles.worktreeName}>{label}</span>
                    <span className={styles.worktreeMeta}>
                      {worktree.isMain
                        ? t('workbench:worktrees.main')
                        : t('workbench:worktrees.linked')}
                    </span>
                  </button>
                );
              })
            )}
          </div>
          <div className={styles.worktreeActions}>
            {createWorktreeOpen ? (
              <form
                className={styles.worktreeCreateForm}
                onSubmit={(event) => {
                  event.preventDefault();
                  void handleCreateWorktree();
                }}
              >
                <label className={styles.worktreePrefixField}>
                  <span className={styles.srOnly}>{t('workbench:worktrees.prefixLabel')}</span>
                  <select
                    className={styles.worktreePrefixSelect}
                    value={createWorktreeBranchPrefix}
                    disabled={worktreeBusy === 'create' || remoteWriteDisabled}
                    aria-label={t('workbench:worktrees.prefixLabel')}
                    onChange={(event) =>
                      setCreateWorktreeBranchPrefix(event.target.value as WorktreeBranchPrefix)
                    }
                  >
                    {WORKTREE_BRANCH_PREFIXES.map((prefix) => (
                      <option key={prefix} value={prefix}>
                        {prefix}
                      </option>
                    ))}
                  </select>
                </label>
                <span className={styles.worktreeBranchSlash}>/</span>
                <Input
                  ref={worktreeBranchInputRef}
                  size="sm"
                  mono
                  className={styles.worktreeBranchInput}
                  value={createWorktreeBranchSuffixDraft}
                  placeholder={t('workbench:worktrees.suffixPlaceholder')}
                  aria-label={t('workbench:worktrees.suffixLabel')}
                  disabled={worktreeBusy === 'create' || remoteWriteDisabled}
                  onChange={(event) => setCreateWorktreeBranchSuffixDraft(event.target.value)}
                />
                <Button
                  type="submit"
                  size="sm"
                  variant="primary"
                  loading={worktreeBusy === 'create'}
                  disabled={!composedWorktreeBranchName || worktreeBusy !== null || remoteWriteDisabled}
                >
                  {t('common:action.confirm')}
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={worktreeBusy === 'create'}
                  onClick={handleCancelCreateWorktree}
                >
                  {t('common:action.cancel')}
                </Button>
              </form>
            ) : (
              <Button
                size="sm"
                variant="secondary"
                icon={<PlusIcon />}
                loading={worktreeBusy === 'create'}
                disabled={!activeProjectId || worktreeBusy !== null || remoteWriteDisabled}
                onClick={handleOpenCreateWorktree}
              >
                {t('workbench:worktrees.create')}
              </Button>
            )}
            <Button
              variant="icon"
              icon={<TrashIcon />}
              title={t('workbench:worktrees.remove')}
              aria-label={t('workbench:worktrees.remove')}
              loading={worktreeBusy === 'remove'}
              disabled={!canRemoveWorktree(activeWorktree, worktreeBusy) || remoteWriteDisabled}
              onClick={() => void handleRemoveWorktree()}
            />
          </div>
        </section>

        <div className={styles.noticeStack}>
          {remoteProjectOffline ? (
            <div className={styles.noticeBox}>
              {t('workbench:remoteOfflineNotice', {
                device: activeProject?.deviceName ?? emptyValue,
              })}
            </div>
          ) : null}
          {sessionError ? <div className={styles.errorBox}>{sessionError}</div> : null}
          {worktreeError ? <div className={styles.errorBox}>{worktreeError}</div> : null}
          {dependencyStatus.status !== 'ready' ? (
            <WorkbenchDependencyCard compact className={styles.dependencyNotice} />
          ) : null}
        </div>

        <div className={styles.mainWorkspace}>
          <div
            className={styles.terminalLayer}
            data-hidden={
              (!terminalFullscreen && (automationConsoleOpen || workspaceView !== 'terminal')) ||
              undefined
            }
            data-fullscreen={terminalFullscreen || undefined}
          >
            <WorkbenchWorkspaceNav
              ariaLabel={t('workbench:terminalTabs')}
              actionsAriaLabel={t('workbench:paneActions')}
              tabs={
                <div className={styles.sessionTabs} role="tablist">
                  {scopedSessions.map((session) => (
                    <div
                      key={session.id}
                      className={styles.sessionTab}
                      role="tab"
                      tabIndex={0}
                      aria-selected={session.id === activeSessionId}
                      data-active={session.id === activeSessionId || undefined}
                      onClick={() => focusSession(session.id)}
                      onKeyDown={(event) => {
                        if (event.key !== 'Enter' && event.key !== ' ') return;
                        event.preventDefault();
                        focusSession(session.id);
                      }}
                    >
                      <span className={styles.sessionDot} data-status={session.status} />
                      <span className={styles.sessionName}>{session.name}</span>
                      <Button
                        variant="icon"
                        icon={<XIcon />}
                        title={t('workbench:closeTerminal')}
                        aria-label={t('workbench:closeTerminal')}
                        onClick={(event) => {
                          event.stopPropagation();
                          void handleCloseSession(session.id);
                        }}
                      />
                    </div>
                  ))}
                  <Button
                    className={styles.newSessionButton}
                    variant="secondary"
                    size="sm"
                    icon={<PlusIcon />}
                    title={t('workbench:newSession')}
                    aria-label={t('workbench:newSession')}
                    data-workbench-responsive-action="true"
                    loading={sessionBusy}
                    disabled={!activeProjectId || !activeWorktree || remoteWriteDisabled}
                    onClick={() => void handleCreateSession()}
                  >
                    <span data-workbench-responsive-label="true">{t('workbench:newSession')}</span>
                  </Button>
                </div>
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

            <div className={styles.terminalArea} ref={terminalAreaRef}>
              {promptPanelOpen && !terminalFullscreen ? (
                <aside
                  className={styles.promptOptimizerPanel}
                  style={promptPanelStyle}
                  aria-label={t('workbench:promptOptimizer.panelAriaLabel')}
                >
                  <textarea
                    ref={promptInputRef}
                    className={styles.promptOptimizerInput}
                    value={promptInput}
                    onChange={(event) => setPromptInput(event.target.value)}
                    onKeyDown={handlePromptInputKeyDown}
                    placeholder={t('promptOptimizer:inputPlaceholder')}
                    aria-label={t('promptOptimizer:inputAriaLabel')}
                    disabled={promptOptimizing || remoteWriteDisabled}
                  />
                </aside>
              ) : null}

              <section
                className={styles.terminalPanel}
                data-layout="single"
                ref={terminalPanelRef}
              >
                {visibleSessions.length === 0 ? (
                  <WorkbenchTerminalPane
                    session={null}
                    placeholder={
                      activeProject
                        ? t('workbench:terminalPlaceholder')
                        : t('workbench:terminalNoProject')
                    }
                    onInput={handleInput}
                    onResize={handleResize}
                    resizeRequestKey={0}
                    inputEnabled={false}
                    onCursorAnchorChange={
                      !automationConsoleOpen && workspaceView === 'terminal'
                        ? handleCursorAnchorChange
                        : undefined
                    }
                  />
                ) : null}
                {mountedSessions.map((session) => (
                  <div
                    key={session.id}
                    className={styles.terminalPaneFrame}
                    data-active={session.id === renderedActiveSessionId || undefined}
                    onClick={() => focusSession(session.id)}
                  >
                    <div className={styles.terminalPaneHeader}>
                      <span className={styles.sessionDot} data-status={session.status} />
                      <span className={styles.sessionName}>{session.name}</span>
                      <span className={styles.terminalPaneStatus}>
                        {session.status === 'running'
                          ? t('workbench:sessionStatus.running')
                          : session.status === 'exited'
                            ? t('workbench:sessionStatus.exited')
                            : session.status === 'disconnected'
                              ? t('workbench:sessionStatus.disconnected')
                              : session.status}
                      </span>
                    </div>
                    <WorkbenchTerminalPane
                      session={session}
                      placeholder={t('workbench:terminalPlaceholder')}
                      onInput={handleInput}
                      onResize={handleResize}
                      resizeRequestKey={
                        session.id === renderedActiveSessionId ? terminalResizeRequestKey : 0
                      }
                      inputEnabled={
                        !automationConsoleOpen &&
                        workspaceView === 'terminal' &&
                        session.id === renderedActiveSessionId &&
                        !remoteWriteDisabled
                      }
                      onCursorAnchorChange={
                        !automationConsoleOpen &&
                        workspaceView === 'terminal' &&
                        session.id === renderedActiveSessionId
                          ? handleCursorAnchorChange
                          : undefined
                      }
                    />
                  </div>
                ))}
              </section>
            </div>
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
                  <span className={styles.automationEyebrow}>
                    {t('workbench:projectAutomation.scope')}
                  </span>
                  <h2 className={styles.automationTitle}>
                    {t('workbench:projectAutomation.title')}
                  </h2>
                  <p className={styles.automationDescription}>
                    {t('workbench:projectAutomation.description')}
                  </p>
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
                <OrchestratorPanel embedded onOpenWorkbench={handleOpenAutomationTaskWorkbench} />
              </div>
            </div>
          ) : null}
        </div>
      </main>

      <aside className={styles.inspectorPane}>
        <Card className={styles.statusCard} padding="sm">
          <div className={styles.cardTitleRow}>
            <h3 className={styles.cardTitle}>{t('workbench:sessionStatusTitle')}</h3>
            <Pill tone={activeSession ? statusTone(activeSession.status) : 'neutral'} dot>
              {sessionStatusLabel}
            </Pill>
          </div>
          <dl className={styles.statusGrid}>
            <div>
              <dt>{t('workbench:statusDevice')}</dt>
              <dd>{activeProject?.deviceName ?? emptyValue}</dd>
            </div>
            <div>
              <dt>{t('workbench:statusProject')}</dt>
              <dd>{activeProject?.name ?? emptyValue}</dd>
            </div>
            <div>
              <dt>{t('workbench:statusWorktree')}</dt>
              <dd>{activeWorktree?.name ?? emptyValue}</dd>
            </div>
            <div>
              <dt>{t('workbench:statusProjectPath')}</dt>
              <dd>{activeRootPath || emptyValue}</dd>
            </div>
            <div>
              <dt>{t('workbench:statusSession')}</dt>
              <dd>{activeSession?.name ?? emptyValue}</dd>
            </div>
            <div>
              <dt>{t('workbench:statusCommand')}</dt>
              <dd>{activeSession?.command ?? emptyValue}</dd>
            </div>
            <div>
              <dt>{t('workbench:statusState')}</dt>
              <dd>{sessionStatusLabel}</dd>
            </div>
            <div>
              <dt>{t('workbench:statusRuntime')}</dt>
              <dd>{activeSessionRuntime}</dd>
            </div>
            <div>
              <dt>{t('workbench:statusSize')}</dt>
              <dd>{activeSession ? `${activeSession.cols} × ${activeSession.rows}` : emptyValue}</dd>
            </div>
            <div>
              <dt>{t('workbench:statusStarted')}</dt>
              <dd>{formatDateTime(activeSession?.startedAt ?? null, emptyValue)}</dd>
            </div>
            <div>
              <dt>{t('workbench:statusExit')}</dt>
              <dd>{activeSession?.exitCode ?? emptyValue}</dd>
            </div>
          </dl>
          <div className={styles.statusActions}>
            <Input
              value={sessionNameDraft}
              onChange={(event) => setSessionNameDraft(event.target.value)}
              placeholder={t('workbench:sessionNamePlaceholder')}
              size="sm"
              disabled={!activeSession || remoteWriteDisabled}
            />
            <div className={styles.statusButtonRow}>
              <Button
                size="sm"
                variant="secondary"
                icon={<EditIcon />}
                disabled={!activeSession || !sessionNameDraft.trim() || remoteWriteDisabled}
                onClick={() => void handleRenameSession()}
              >
                {t('workbench:renameSession')}
              </Button>
              <Button
                size="sm"
                variant="danger"
                icon={<XIcon />}
                disabled={!activeSession || remoteWriteDisabled}
                onClick={() => activeSession && void handleCloseSession(activeSession.id)}
              >
                {t('workbench:closeTerminal')}
              </Button>
            </div>
          </div>
        </Card>

        <div className={styles.inspectorTabs} role="tablist" aria-label={t('workbench:inspectorTabs')}>
          <button
            type="button"
            className={styles.inspectorTab}
            data-active={inspectorTab === 'files' || undefined}
            role="tab"
            aria-selected={inspectorTab === 'files'}
            onClick={() => setInspectorTab('files')}
          >
            {t('workbench:filesTitle')}
          </button>
          <button
            type="button"
            className={styles.inspectorTab}
            data-active={inspectorTab === 'history' || undefined}
            role="tab"
            aria-selected={inspectorTab === 'history'}
            onClick={() => setInspectorTab('history')}
          >
            {t('workbench:gitHistoryTitle')}
          </button>
        </div>

        {inspectorTab === 'files' ? (
          <Card className={styles.filesCard} padding="sm">
            <div className={styles.cardTitleRow}>
              <h3 className={styles.cardTitle}>{t('workbench:filesTitle')}</h3>
              <Button
                variant="icon"
                icon={<SyncIcon />}
                title={t('workbench:refreshFiles')}
                aria-label={t('workbench:refreshFiles')}
                disabled={!activeProjectId}
                onClick={() => void loadDir('')}
              />
            </div>

            {fileError ? <div className={styles.errorBox}>{fileError}</div> : null}
            {fileNotice ? <div className={styles.noticeBox}>{fileNotice}</div> : null}

            <div className={styles.fileActions}>
              <Input
                value={newEntryName}
                onChange={(event) => setNewEntryName(event.target.value)}
                placeholder={t('workbench:newEntryPlaceholder')}
                size="sm"
              />
              <div className={styles.fileActionButtons}>
                <Button
                  size="sm"
                  variant="secondary"
                  icon={<FileIcon />}
                  disabled={!activeProjectId || !newEntryName.trim() || remoteWriteDisabled}
                  onClick={() => void handleCreateEntry('file')}
                >
                  {t('workbench:createFile')}
                </Button>
                <Button
                  size="sm"
                  variant="secondary"
                  icon={<FolderIcon />}
                  disabled={!activeProjectId || !newEntryName.trim() || remoteWriteDisabled}
                  onClick={() => void handleCreateEntry('dir')}
                >
                  {t('workbench:createFolder')}
                </Button>
              </div>
            </div>

            <div className={styles.treePanel}>
              {!activeProjectId ? (
                <div className={styles.treeEmpty}>{t('workbench:filesNoProject')}</div>
              ) : rootNodes.length === 0 && fileLoadingPath === '' ? (
                <div className={styles.treeEmpty}>{t('workbench:loading')}</div>
              ) : rootNodes.length === 0 ? (
                <div className={styles.treeEmpty}>{t('workbench:filesEmpty')}</div>
              ) : (
                <FileTree
                  nodes={rootNodes}
                  childrenByPath={childrenByPath}
                  expandedPaths={expandedPaths}
                  selectedPath={selectedPath}
                  loadingPath={fileLoadingPath}
                  onToggle={handleToggleNode}
                  onSelect={handleSelectNode}
                />
              )}
            </div>

            <div className={styles.pathInfo}>
              <div className={styles.pathInfoHeader}>
                <span className={styles.pathInfoName}>{basename(selectedDisplayPath, rootPath)}</span>
                <span className={styles.pathInfoPath}>{selectedDisplayPath || emptyValue}</span>
              </div>
              <dl className={styles.pathInfoGrid}>
                <div>
                  <dt>{t('workbench:pathKind')}</dt>
                  <dd>{selectedKindLabel}</dd>
                </div>
                <div>
                  <dt>{t('workbench:pathSize')}</dt>
                  <dd>{formatSize(selectedInfo?.size ?? null, emptyValue)}</dd>
                </div>
                <div>
                  <dt>{t('workbench:pathModified')}</dt>
                  <dd>{formatDateTime(selectedInfo?.modifiedAt ?? null, emptyValue)}</dd>
                </div>
                <div>
                  <dt>{t('workbench:pathParent')}</dt>
                  <dd>{selectedParentPath || rootPath}</dd>
                </div>
              </dl>
              <div className={styles.renameRow}>
                <Input
                  value={renameName}
                  onChange={(event) => setRenameName(event.target.value)}
                  placeholder={t('workbench:renamePlaceholder')}
                  size="sm"
                  disabled={!selectedInfo || remoteWriteDisabled}
                />
                <Button
                  size="sm"
                  variant="secondary"
                  icon={<CopyIcon />}
                  disabled={!selectedInfo}
                  onClick={() => void handleCopySelectedPath()}
                >
                  {t('workbench:copyRelativePath')}
                </Button>
                <Button
                  size="sm"
                  variant="secondary"
                  icon={<EditIcon />}
                  disabled={!selectedInfo || !renameName.trim() || remoteWriteDisabled}
                  onClick={() => void handleRenamePath()}
                >
                  {t('workbench:rename')}
                </Button>
                <Button
                  size="sm"
                  variant="danger"
                  icon={<TrashIcon />}
                  disabled={!selectedInfo || remoteWriteDisabled}
                  onClick={() => void handleDeletePath()}
                >
                  {t('workbench:delete')}
                </Button>
              </div>
            </div>
          </Card>
        ) : (
          <Card className={styles.historyCard} padding="sm">
            <div className={styles.cardTitleRow}>
              <h3 className={styles.cardTitle}>{t('workbench:gitHistoryTitle')}</h3>
              <Button
                variant="icon"
                icon={<SyncIcon />}
                title={t('workbench:refreshGitHistory')}
                aria-label={t('workbench:refreshGitHistory')}
                disabled={!activeProjectId || gitHistoryLoading}
                onClick={() => void loadGitHistory()}
              />
            </div>

            <div className={styles.gitActionBar}>
              <div className={styles.gitActionStatus}>
                <Pill tone={activeWorktreePillTone} dot>
                  {activeWorktreeStatusLabel}
                </Pill>
                <span className={styles.gitActionBranch}>
                  {activeWorktree?.branch ?? activeWorktree?.name ?? emptyValue}
                </span>
              </div>
              <div className={styles.gitActionButtons}>
                <Button
                  size="sm"
                  variant={activeWorktreeChangedCount > 0 ? 'primary' : 'secondary'}
                  icon={<EditIcon />}
                  loading={worktreeBusy === 'commit'}
                  disabled={!canCommitWorktree(activeWorktree, worktreeBusy) || remoteWriteDisabled}
                  onClick={() => void handleCommitWorktree()}
                >
                  {t('workbench:worktrees.commit')}
                </Button>
                <Button
                  size="sm"
                  variant="secondary"
                  icon={<UploadIcon />}
                  loading={worktreeBusy === 'push'}
                  disabled={!canPushWorktree(activeWorktree, worktreeBusy) || remoteWriteDisabled}
                  onClick={() => void handlePushWorktree()}
                >
                  {t('workbench:worktrees.push')}
                </Button>
                <Button
                  size="sm"
                  variant="secondary"
                  icon={<SyncIcon />}
                  loading={worktreeBusy === 'merge'}
                  disabled={!canMergeWorktree(activeWorktree, worktreeBusy) || remoteWriteDisabled}
                  onClick={() => void handleMergeWorktree()}
                >
                  {t('workbench:worktrees.merge')}
                </Button>
              </div>
            </div>

            {renderedMergeStages.length > 0 ? (
              <div className={styles.mergeStagePanel} role="status" aria-live="polite">
                {renderedMergeStages.map((stage) => (
                  <div
                    key={stage.id}
                    className={styles.mergeStageItem}
                    data-status={stage.status}
                  >
                    <span className={styles.mergeStageDot} aria-hidden="true" />
                    <div className={styles.mergeStageCopy}>
                      <span className={styles.mergeStageLabel}>
                        {mergeStageLabel(stage.id)}
                      </span>
                      <span className={styles.mergeStageMessage}>
                        {stage.message || mergeStageFallbackMessage(stage)}
                      </span>
                    </div>
                  </div>
                ))}
              </div>
            ) : null}

            {gitHistoryError ? <div className={styles.errorBox}>{gitHistoryError}</div> : null}

            <div className={styles.historyPanel}>
              {!activeProjectId ? (
                <div className={styles.treeEmpty}>{t('workbench:gitHistoryNoProject')}</div>
              ) : gitHistoryLoading ? (
                <div className={styles.treeEmpty}>{t('workbench:gitHistoryLoading')}</div>
              ) : !hasGitHistory(gitCommits) ? (
                <div className={styles.treeEmpty}>{t('workbench:gitHistoryEmpty')}</div>
              ) : (
                <div className={styles.commitList}>
                  {gitGraphRows.map((row) => {
                    const graphWidth = gitGraphWidth(row.laneCount);
                    return (
                      <article key={row.commit.hash} className={styles.commitItem}>
                        <div className={styles.commitGraph} style={{ width: graphWidth }}>
                          <svg
                            className={styles.commitGraphSvg}
                            viewBox={`0 0 ${graphWidth} ${GIT_GRAPH_ROW_HEIGHT}`}
                            aria-hidden="true"
                          >
                            {row.activeLanes.map((lane, laneIndex) => {
                              const x = gitGraphX(laneIndex);
                              const isCommitLane = laneIndex === row.lane;
                              const continues = row.parentLanes.includes(laneIndex);
                              const y2 = isCommitLane && !continues ? GIT_GRAPH_DOT_Y : GIT_GRAPH_ROW_HEIGHT;
                              return (
                                <line
                                  key={`${row.commit.hash}-${lane.hash}-${laneIndex}`}
                                  className={styles.graphLine}
                                  style={gitGraphColorStyle(lane.colorIndex)}
                                  x1={x}
                                  y1={0}
                                  x2={x}
                                  y2={y2}
                                />
                              );
                            })}
                            {row.parentLanes
                              .filter((parentLane) => parentLane !== row.lane)
                              .map((parentLane) => {
                                const fromX = gitGraphX(row.lane);
                                const toX = gitGraphX(parentLane);
                                return (
                                  <path
                                    key={`${row.commit.hash}-${parentLane}`}
                                    className={styles.graphLine}
                                    style={gitGraphColorStyle(row.colorIndex)}
                                    d={`M ${fromX} ${GIT_GRAPH_DOT_Y} C ${fromX} 32 ${toX} 32 ${toX} ${GIT_GRAPH_ROW_HEIGHT}`}
                                  />
                                );
                              })}
                            <circle
                              className={styles.graphDot}
                              style={gitGraphColorStyle(row.colorIndex)}
                              cx={gitGraphX(row.lane)}
                              cy={GIT_GRAPH_DOT_Y}
                              r={GIT_GRAPH_DOT_RADIUS}
                            />
                          </svg>
                        </div>
                        <div className={styles.commitContent}>
                          <div className={styles.commitHeader}>
                            <span className={styles.commitSummary}>
                              {row.commit.summary || emptyValue}
                            </span>
                            <span className={styles.commitTime}>
                              {formatCommitRelativeTime(row.commit.authoredAt, emptyValue)}
                            </span>
                          </div>
                          {row.commit.refs.length > 0 ? (
                            <div className={styles.refList}>
                              {row.commit.refs.map((ref) => (
                                <span
                                  key={`${row.commit.hash}-${ref.fullName}`}
                                  className={styles.refBadge}
                                  data-kind={ref.kind}
                                  title={ref.fullName}
                                >
                                  {ref.kind === 'remote' ? <UploadIcon size={12} /> : null}
                                  {ref.name}
                                </span>
                              ))}
                            </div>
                          ) : null}
                          <div className={styles.commitMeta}>
                            <span className={styles.commitHash}>{row.commit.shortHash}</span>
                            <span>{row.commit.authorName || row.commit.authorEmail || emptyValue}</span>
                          </div>
                        </div>
                      </article>
                    );
                  })}
                </div>
              )}
            </div>
          </Card>
        )}
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
