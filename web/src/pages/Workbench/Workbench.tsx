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
 *   - hooks 全部在 early return 之前，避免 React hooks 调用顺序问题
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { CSSProperties } from 'react';
import { useTranslation } from 'react-i18next';
import { listen } from '@tauri-apps/api/event';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { configApi } from '@/api/config';
import { promptOptimizerApi } from '@/api/promptOptimizer';
import { workbenchApi } from '@/api/workbench';
import { WorkbenchDependencyCard } from '@/components/domain';
import { Button, Card, Input, Pill } from '@/components/primitives';
import { useWorkbenchDependency } from '@/hooks/workbenchDependencyContext';
import { useWorkbenchProjects } from '@/hooks/workbenchProjectsContext';
import { useWorkbenchTerminalBuffers } from '@/hooks/workbenchTerminalBuffersContext';
import {
  ChevronRightIcon,
  CopyIcon,
  EditIcon,
  FileIcon,
  FolderIcon,
  PlusIcon,
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
  WorkbenchGitCommit,
  WorkbenchPathInfo,
  WorkbenchSession,
  WorkbenchTerminalStatusEvent,
  WorkbenchWorktree,
} from '@/lib/types';
import styles from './Workbench.module.css';
import {
  canFillPromptIntoTerminal,
  createPromptOptimizerShortcutState,
  promptOptimizerInputKeyAction,
  promptOptimizerShortcutAction,
  reducePromptOptimizerShortcut,
  resetPromptOptimizerTextState,
} from './promptOptimizerWidget';
import { mountedTerminalSessions, visibleTerminalSessions } from './terminalSessionOrder';
import { workbenchTerminalOptions, workbenchTerminalTheme } from './terminalOptions';
import { shouldForwardTerminalInput, writeTerminalReplay } from './terminalReplay';
import { terminalPanePixelSize } from './terminalSizing';
import type { TerminalLayoutMode } from './terminalSizing';
import {
  activeWorktreeRootPath,
  buildGitGraphRows,
  canCommitWorktree,
  canMergeWorktree,
  canPushWorktree,
  canRemoveWorktree,
  composeWorktreeBranchName,
  DEFAULT_WORKTREE_BRANCH_PREFIX,
  formatCommitRelativeTime,
  hasGitHistory,
  sessionsForWorktree,
  WORKTREE_BRANCH_PREFIXES,
  worktreeChangeCount,
  worktreeStatusTone,
} from './workbenchWorktrees';
import type { WorktreeBranchPrefix } from './workbenchWorktrees';

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

interface TerminalPaneProps {
  session: WorkbenchSession | null;
  buffer: string;
  revision: number;
  placeholder: string;
  inputEnabled: boolean;
  onInput: (sessionId: string, data: string) => void;
  onResize: (sessionId: string, cols: number, rows: number) => void;
  onCursorAnchorChange?: (anchor: TerminalCursorAnchor | null) => void;
}

interface TerminalSize {
  cols: number;
  rows: number;
}

interface TerminalCursorAnchor {
  left: number;
  top: number;
  bottom: number;
}

interface PromptOptimizerPanelPosition {
  left: number;
  top: number;
}

const MIN_TERMINAL_COLS = 20;
const MIN_TERMINAL_ROWS = 6;
const TERMINAL_PANE_HEADER_PX = 36;
const TMUX_FOCUS_SYNC_INTERVAL_MS = 700;
const LOCAL_FOCUS_GRACE_MS = 500;

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
 *   Prompt 优化浮层应出现在当前终端输入光标下方，但不能超出终端工作区。
 *
 * Code Logic（这个函数做什么）:
 *   把 viewport 绝对坐标系的光标锚点转换为 terminalArea 内相对坐标，并按面板宽高做 clamp。
 */
function promptOptimizerPanelPosition(
  areaRect: DOMRect,
  anchor: TerminalCursorAnchor,
): PromptOptimizerPanelPosition {
  const panelWidth = Math.min(560, Math.max(280, areaRect.width - 32));
  const estimatedPanelHeight = Math.min(520, Math.max(280, areaRect.height - 32));
  const maxLeft = Math.max(16, areaRect.width - panelWidth - 16);
  const maxTop = Math.max(16, areaRect.height - estimatedPanelHeight - 16);
  const left = Math.min(maxLeft, Math.max(16, anchor.left - areaRect.left));
  const top = Math.min(maxTop, Math.max(16, anchor.bottom - areaRect.top + 8));
  return { left, top };
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
 *   xterm 生命周期较重，应隔离在独立组件内，避免页面其他状态刷新时重复初始化终端实例。
 *
 * Code Logic（这个组件做什么）:
 *   session 变化时创建/销毁 Terminal；buffer revision 变化时只写入新增输出；
 *   仅 inputEnabled=true 的 active 终端转发 onData；ResizeObserver 触发 FitAddon.fit 后把 cols/rows clamp 后回传后端。
 */
function TerminalPane(props: TerminalPaneProps) {
  const {
    session,
    buffer,
    revision,
    placeholder,
    inputEnabled,
    onInput,
    onResize,
    onCursorAnchorChange,
  } = props;
  const viewportRef = useRef<HTMLDivElement | null>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const bufferRef = useRef<string>('');
  const writtenLengthRef = useRef<number>(0);
  const replayGateRef = useRef<boolean>(false);
  const inputEnabledRef = useRef<boolean>(inputEnabled);
  const resizeTimerRef = useRef<number | null>(null);
  const cursorAnchorCallbackRef = useRef<TerminalPaneProps['onCursorAnchorChange']>(
    onCursorAnchorChange,
  );
  const sessionId = session?.id ?? null;

  useEffect(() => {
    bufferRef.current = buffer;
  }, [buffer]);

  useEffect(() => {
    inputEnabledRef.current = inputEnabled;
  }, [inputEnabled]);

  useEffect(() => {
    cursorAnchorCallbackRef.current = onCursorAnchorChange;
  }, [onCursorAnchorChange]);

  useEffect(() => {
    const viewport = viewportRef.current;
    if (!viewport || !sessionId) return undefined;

    const terminal = new Terminal(workbenchTerminalOptions());
    const fit = new FitAddon();
    terminal.loadAddon(fit);
    terminal.open(viewport);
    fit.fit();
    const emitCursorAnchor = () => {
      try {
        const rect = viewport.getBoundingClientRect();
        const cellWidth = rect.width / Math.max(terminal.cols, 1);
        const cellHeight = rect.height / Math.max(terminal.rows, 1);
        const cursorX = terminal.buffer.active.cursorX;
        const cursorY = terminal.buffer.active.cursorY;
        const left = rect.left + cursorX * cellWidth;
        const top = rect.top + cursorY * cellHeight;
        cursorAnchorCallbackRef.current?.({ left, top, bottom: top + cellHeight });
      } catch {
        // 光标定位仅用于浮层摆放，失败不影响终端显示与输入。
      }
    };
    const dataDisposable = terminal.onData((data: string) => {
      if (!shouldForwardTerminalInput(replayGateRef, inputEnabledRef.current)) return;
      onInput(sessionId, data);
    });
    const cursorDisposable = terminal.onCursorMove(emitCursorAnchor);
    writeTerminalReplay(terminal, bufferRef.current, replayGateRef);
    writtenLengthRef.current = bufferRef.current.length;
    emitCursorAnchor();
    const resize = () => {
      try {
        fit.fit();
        onResize(
          sessionId,
          clampU16(terminal.cols, MIN_TERMINAL_COLS),
          clampU16(terminal.rows, MIN_TERMINAL_ROWS),
        );
        emitCursorAnchor();
      } catch {
        // xterm 在容器不可见时 fit 可能失败，下一次 ResizeObserver 会重试。
      }
    };
    const observer = new ResizeObserver(() => {
      if (resizeTimerRef.current !== null) {
        window.clearTimeout(resizeTimerRef.current);
      }
      resizeTimerRef.current = window.setTimeout(resize, 80);
    });
    observer.observe(viewport);
    resize();
    terminalRef.current = terminal;

    return () => {
      observer.disconnect();
      dataDisposable.dispose();
      cursorDisposable.dispose();
      if (resizeTimerRef.current !== null) {
        window.clearTimeout(resizeTimerRef.current);
        resizeTimerRef.current = null;
      }
      cursorAnchorCallbackRef.current?.(null);
      terminal.dispose();
      terminalRef.current = null;
      writtenLengthRef.current = 0;
      replayGateRef.current = false;
    };
  }, [onInput, onResize, sessionId]);

  useEffect(() => {
    const applyTheme = () => {
      const terminal = terminalRef.current;
      if (terminal) {
        terminal.options.theme = workbenchTerminalTheme();
      }
    };
    window.addEventListener('cp-theme-change', applyTheme);
    window.addEventListener('storage', applyTheme);
    return () => {
      window.removeEventListener('cp-theme-change', applyTheme);
      window.removeEventListener('storage', applyTheme);
    };
  }, []);

  useEffect(() => {
    const terminal = terminalRef.current;
    if (!terminal || !sessionId) return;
    const previousLength = writtenLengthRef.current;
    if (buffer.length < previousLength) {
      terminal.clear();
      writeTerminalReplay(terminal, buffer, replayGateRef);
      writtenLengthRef.current = buffer.length;
      return;
    }
    if (buffer.length > previousLength) {
      terminal.write(buffer.slice(previousLength));
      writtenLengthRef.current = buffer.length;
    }
  }, [buffer, revision, sessionId]);

  return (
    <div className={styles.terminalHost}>
      <div className={styles.terminalViewport} ref={viewportRef} />
      {!session ? <div className={styles.terminalPlaceholder}>{placeholder}</div> : null}
    </div>
  );
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
  const { status: dependencyStatus } = useWorkbenchDependency();
  const { activeProjectId, activeProject, refreshProjectSessionStats } = useWorkbenchProjects();
  const {
    buffers: terminalBuffers,
    revision: terminalRevision,
    resetBuffer: resetTerminalBuffer,
    removeBuffer: removeTerminalBuffer,
  } = useWorkbenchTerminalBuffers();
  const [sessions, setSessions] = useState<WorkbenchSession[]>([]);
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);
  const [sessionNameDraft, setSessionNameDraft] = useState<string>('');
  const [sessionBusy, setSessionBusy] = useState<boolean>(false);
  const [sessionError, setSessionError] = useState<string | null>(null);
  const [worktrees, setWorktrees] = useState<WorkbenchWorktree[]>([]);
  const [activeWorktreeId, setActiveWorktreeId] = useState<string | null>(null);
  const [worktreeBusy, setWorktreeBusy] = useState<string | null>(null);
  const [worktreeError, setWorktreeError] = useState<string | null>(null);
  const [createWorktreeOpen, setCreateWorktreeOpen] = useState<boolean>(false);
  const [createWorktreeBranchPrefix, setCreateWorktreeBranchPrefix] =
    useState<WorktreeBranchPrefix>(DEFAULT_WORKTREE_BRANCH_PREFIX);
  const [createWorktreeBranchSuffixDraft, setCreateWorktreeBranchSuffixDraft] =
    useState<string>('');
  const [rootNodes, setRootNodes] = useState<WorkbenchFileNode[]>([]);
  const [childrenByPath, setChildrenByPath] = useState<Record<string, WorkbenchFileNode[]>>({});
  const [expandedPaths, setExpandedPaths] = useState<Set<string>>(new Set());
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  const [selectedInfo, setSelectedInfo] = useState<WorkbenchPathInfo | null>(null);
  const [fileLoadingPath, setFileLoadingPath] = useState<string | null>(null);
  const [fileError, setFileError] = useState<string | null>(null);
  const [fileNotice, setFileNotice] = useState<string | null>(null);
  const [newEntryName, setNewEntryName] = useState<string>('');
  const [renameName, setRenameName] = useState<string>('');
  const [inspectorTab, setInspectorTab] = useState<WorkbenchInspectorTab>('files');
  const [gitCommits, setGitCommits] = useState<WorkbenchGitCommit[]>([]);
  const [gitHistoryLoading, setGitHistoryLoading] = useState<boolean>(false);
  const [gitHistoryError, setGitHistoryError] = useState<string | null>(null);
  const [runtimeNow, setRuntimeNow] = useState<number>(() => Date.now());
  const [promptPanelOpen, setPromptPanelOpen] = useState<boolean>(false);
  const [promptInput, setPromptInput] = useState<string>('');
  const [promptOptimizing, setPromptOptimizing] = useState<boolean>(false);
  const [promptOptimizerHotkey, setPromptOptimizerHotkey] = useState<string>('<ctrl>');
  const [promptOptimizerFillLanguage, setPromptOptimizerFillLanguage] =
    useState<PromptOptimizerFillLanguage>('zh');
  const [promptPanelPosition, setPromptPanelPosition] = useState<PromptOptimizerPanelPosition>({
    left: 24,
    top: 24,
  });
  const activeProjectIdRef = useRef<string | null>(null);
  const activeWorktreeIdRef = useRef<string | null>(null);
  const knownSessionIdsRef = useRef<Set<string>>(new Set());
  const terminalPanelRef = useRef<HTMLElement | null>(null);
  const terminalAreaRef = useRef<HTMLDivElement | null>(null);
  const worktreeBranchInputRef = useRef<HTMLInputElement | null>(null);
  const promptInputRef = useRef<HTMLTextAreaElement | null>(null);
  const promptShortcutStateRef = useRef(createPromptOptimizerShortcutState());
  const lastLocalFocusAtRef = useRef<number>(0);

  const activeWorktree = useMemo(
    () => worktrees.find((worktree) => worktree.id === activeWorktreeId) ?? worktrees[0] ?? null,
    [activeWorktreeId, worktrees],
  );
  const activeWorktreeSessionId = activeWorktree?.id ?? null;
  const scopedSessions = useMemo(
    () => sessionsForWorktree(sessions, activeWorktreeSessionId),
    [activeWorktreeSessionId, sessions],
  );
  const activeSession = useMemo(
    () => scopedSessions.find((session) => session.id === activeSessionId) ?? null,
    [activeSessionId, scopedSessions],
  );
  const visibleSessions = useMemo(
    () => visibleTerminalSessions({ sessions: scopedSessions, activeSessionId }),
    [activeSessionId, scopedSessions],
  );
  const mountedSessions = useMemo(
    () => mountedTerminalSessions({ sessions }),
    [sessions],
  );
  const gitGraphRows = useMemo(() => buildGitGraphRows(gitCommits), [gitCommits]);
  const renderedActiveSessionId = activeSession?.id ?? visibleSessions[0]?.id ?? null;
  const selectedParentPath = selectedInfo
    ? selectedInfo.kind === 'dir'
      ? selectedInfo.path
      : parentPathOf(selectedInfo.path)
    : '';
  const selectedDisplayPath = selectedInfo?.path ?? '';
  const desktopUnavailableMessage = t('workbench:errors.desktopUnavailable');
  const emptyValue = t('workbench:emptyValue');
  const rootPath = t('workbench:rootPath');
  const activeRootPath = activeWorktreeRootPath(activeProject?.path ?? '', activeWorktree);
  const activeSessionRuntime = formatRuntime(
    activeSession?.startedAt ?? null,
    activeSession?.exitedAt ?? null,
    runtimeNow,
    emptyValue,
  );
  const canUsePanes = Boolean(
    activeSession?.supportsPanes && activeSession.status === 'running',
  );
  const promptWorkingDirectory = activeRootPath || undefined;
  const composedWorktreeBranchName = composeWorktreeBranchName(
    createWorktreeBranchPrefix,
    createWorktreeBranchSuffixDraft,
  );

  const updateActiveSession = useCallback((nextSessions: WorkbenchSession[]) => {
    const candidates = sessionsForWorktree(nextSessions, activeWorktreeIdRef.current);
    setActiveSessionId((current) => {
      if (current && candidates.some((session) => session.id === current)) return current;
      return candidates[0]?.id ?? null;
    });
  }, []);

  const focusSession = useCallback((sessionId: string) => {
    lastLocalFocusAtRef.current = Date.now();
    setActiveSessionId(sessionId);
  }, []);

  useEffect(() => {
    if (!activeSessionId) return undefined;
    let cancelled = false;
    void workbenchApi.sessions.focus(activeSessionId).catch((error) => {
      if (cancelled) return;
      setSessionError(
        displayErrorMessage(
          error,
          t('workbench:errors.focusSession'),
          desktopUnavailableMessage,
        ),
      );
    });
    return () => {
      cancelled = true;
    };
  }, [activeSessionId, desktopUnavailableMessage, t]);

  useEffect(() => {
    if (!activeProjectId || !activeWorktreeSessionId || scopedSessions.length === 0) {
      return undefined;
    }
    let cancelled = false;

    const syncFocusedSession = () => {
      if (Date.now() - lastLocalFocusAtRef.current < LOCAL_FOCUS_GRACE_MS) return;
      void workbenchApi.sessions
        .focused(activeProjectId, activeWorktreeSessionId)
        .then(({ sessionId }) => {
          if (cancelled || !sessionId) return;
          if (!scopedSessions.some((session) => session.id === sessionId)) return;
          setActiveSessionId((current) => (current === sessionId ? current : sessionId));
        })
        .catch(() => {
          // tmux focus sync 是辅助状态同步；失败不应打断终端输入和显示。
        });
    };

    syncFocusedSession();
    const timer = window.setInterval(syncFocusedSession, TMUX_FOCUS_SYNC_INTERVAL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [activeProjectId, activeWorktreeSessionId, scopedSessions]);

  const loadSessions = useCallback(
    async (projectId: string) => {
      try {
        setSessionError(null);
        const list = await workbenchApi.sessions.list(projectId);
        if (activeProjectIdRef.current !== projectId) return;
        knownSessionIdsRef.current = new Set(list.map((session) => session.id));
        setSessions(list);
        updateActiveSession(list);
        void refreshProjectSessionStats(projectId);
      } catch (error) {
        if (activeProjectIdRef.current !== projectId) return;
        setSessionError(
          displayErrorMessage(error, t('workbench:errors.sessions'), desktopUnavailableMessage),
        );
      }
    },
    [desktopUnavailableMessage, refreshProjectSessionStats, t, updateActiveSession],
  );

  const loadWorktrees = useCallback(
    async (projectId: string) => {
      try {
        setWorktreeError(null);
        const list = await workbenchApi.worktrees.list(projectId);
        if (activeProjectIdRef.current !== projectId) return;
        setWorktrees(list);
        setActiveWorktreeId((current) => {
          if (current && list.some((worktree) => worktree.id === current)) return current;
          return list[0]?.id ?? null;
        });
      } catch (error) {
        if (activeProjectIdRef.current !== projectId) return;
        setWorktreeError(
          displayErrorMessage(error, t('workbench:errors.worktrees'), desktopUnavailableMessage),
        );
      }
    },
    [desktopUnavailableMessage, t],
  );

  const loadDir = useCallback(
    async (path: string) => {
      const projectId = activeProjectIdRef.current;
      if (!projectId) return;
      const worktreeId = activeWorktreeIdRef.current;
      try {
        setFileError(null);
        setFileLoadingPath(path);
        const nodes = await workbenchApi.files.listDir(projectId, path, worktreeId);
        if (
          activeProjectIdRef.current !== projectId ||
          activeWorktreeIdRef.current !== worktreeId
        ) {
          return;
        }
        if (path === '') {
          setRootNodes(nodes);
        } else {
          setChildrenByPath((current) => ({ ...current, [path]: nodes }));
        }
      } catch (error) {
        if (
          activeProjectIdRef.current !== projectId ||
          activeWorktreeIdRef.current !== worktreeId
        ) {
          return;
        }
        setFileError(
          displayErrorMessage(error, t('workbench:errors.files'), desktopUnavailableMessage),
        );
      } finally {
        if (activeProjectIdRef.current === projectId && activeWorktreeIdRef.current === worktreeId) {
          setFileLoadingPath(null);
        }
      }
    },
    [desktopUnavailableMessage, t],
  );

  const loadGitHistory = useCallback(async () => {
    const projectId = activeProjectIdRef.current;
    if (!projectId) {
      setGitCommits([]);
      setGitHistoryError(null);
      setGitHistoryLoading(false);
      return;
    }
    const worktreeId = activeWorktreeIdRef.current;
    try {
      setGitHistoryLoading(true);
      setGitHistoryError(null);
      const commits = await workbenchApi.git.listCommits(projectId, worktreeId, 30);
      if (
        activeProjectIdRef.current !== projectId ||
        activeWorktreeIdRef.current !== worktreeId
      ) {
        return;
      }
      setGitCommits(commits);
    } catch (error) {
      if (
        activeProjectIdRef.current !== projectId ||
        activeWorktreeIdRef.current !== worktreeId
      ) {
        return;
      }
      setGitCommits([]);
      setGitHistoryError(
        displayErrorMessage(error, t('workbench:errors.gitHistory'), desktopUnavailableMessage),
      );
    } finally {
      if (activeProjectIdRef.current === projectId && activeWorktreeIdRef.current === worktreeId) {
        setGitHistoryLoading(false);
      }
    }
  }, [desktopUnavailableMessage, t]);

  const loadPathInfo = useCallback(
    async (path: string) => {
      const projectId = activeProjectIdRef.current;
      if (!projectId) return;
      const worktreeId = activeWorktreeIdRef.current;
      try {
        const info = await workbenchApi.files.info(projectId, path, worktreeId);
        if (
          activeProjectIdRef.current !== projectId ||
          activeWorktreeIdRef.current !== worktreeId
        ) {
          return;
        }
        setSelectedInfo(info);
        setRenameName(info.name);
      } catch (error) {
        if (
          activeProjectIdRef.current !== projectId ||
          activeWorktreeIdRef.current !== worktreeId
        ) {
          return;
        }
        setFileError(
          displayErrorMessage(error, t('workbench:errors.pathInfo'), desktopUnavailableMessage),
        );
      }
    },
    [desktopUnavailableMessage, t],
  );

  useEffect(() => {
    activeProjectIdRef.current = activeProjectId;
  }, [activeProjectId]);

  useEffect(() => {
    activeWorktreeIdRef.current = activeWorktreeId;
  }, [activeWorktreeId]);

  useEffect(() => {
    knownSessionIdsRef.current = new Set(sessions.map((session) => session.id));
  }, [sessions]);

  useEffect(() => {
    return deferEffect(() => {
      setActiveSessionId((current) => {
        if (current && scopedSessions.some((session) => session.id === current)) return current;
        return scopedSessions[0]?.id ?? null;
      });
    });
  }, [scopedSessions]);

  useEffect(() => {
    return deferEffect(() => {
      setSessionNameDraft(activeSession?.name ?? '');
    });
  }, [activeSession?.name]);

  useEffect(() => {
    const timer = window.setInterval(() => setRuntimeNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, []);

  useEffect(() => {
    let cancelled = false;
    void configApi
      .get()
      .then((config) => {
        if (cancelled) return;
        setPromptOptimizerHotkey(config.promptOptimizerHotkey || '<ctrl>');
        setPromptOptimizerFillLanguage(
          config.promptOptimizerFillLanguage === 'en' ? 'en' : 'zh',
        );
      })
      .catch(() => {
        // 普通浏览器调试环境没有 Tauri invoke；保留默认快捷键与语言即可。
      });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (!promptPanelOpen) return undefined;
    const frame = window.requestAnimationFrame(() => {
      promptInputRef.current?.focus();
    });
    return () => window.cancelAnimationFrame(frame);
  }, [promptPanelOpen]);

  useEffect(() => {
    if (!createWorktreeOpen) return undefined;
    const frame = window.requestAnimationFrame(() => {
      worktreeBranchInputRef.current?.focus();
    });
    return () => window.cancelAnimationFrame(frame);
  }, [createWorktreeOpen]);

  useEffect(() => {
    return deferEffect(() => {
      if (!activeProjectId) {
        knownSessionIdsRef.current = new Set();
        setSessions([]);
        setActiveSessionId(null);
        setWorktrees([]);
        setActiveWorktreeId(null);
        setCreateWorktreeOpen(false);
        setCreateWorktreeBranchPrefix(DEFAULT_WORKTREE_BRANCH_PREFIX);
        setCreateWorktreeBranchSuffixDraft('');
        setRootNodes([]);
        setChildrenByPath({});
        setExpandedPaths(new Set());
        setSelectedPath(null);
        setSelectedInfo(null);
        setGitCommits([]);
        setGitHistoryError(null);
        return;
      }
      setRootNodes([]);
      knownSessionIdsRef.current = new Set();
      setWorktrees([]);
      setActiveWorktreeId(null);
      setCreateWorktreeOpen(false);
      setCreateWorktreeBranchPrefix(DEFAULT_WORKTREE_BRANCH_PREFIX);
      setCreateWorktreeBranchSuffixDraft('');
      setChildrenByPath({});
      setExpandedPaths(new Set());
      setSelectedPath(null);
      setSelectedInfo(null);
      setFileNotice(null);
      setGitCommits([]);
      setGitHistoryError(null);
      void loadWorktrees(activeProjectId);
      void loadSessions(activeProjectId);
    });
  }, [activeProjectId, loadSessions, loadWorktrees]);

  useEffect(() => {
    return deferEffect(() => {
      setRootNodes([]);
      setChildrenByPath({});
      setExpandedPaths(new Set());
      setSelectedPath(null);
      setSelectedInfo(null);
      setFileNotice(null);
      setGitCommits([]);
      setGitHistoryError(null);
      if (activeProjectId && activeWorktreeId) {
        void loadDir('');
      }
    });
  }, [activeProjectId, activeWorktreeId, loadDir]);

  useEffect(() => {
    if (inspectorTab !== 'history') return undefined;
    return deferEffect(() => {
      void loadGitHistory();
    });
  }, [activeProjectId, activeWorktreeId, inspectorTab, loadGitHistory]);

  useEffect(() => {
    if (!canListenToTauriEvents()) return undefined;
    const statusUnlisten = listen<WorkbenchTerminalStatusEvent>(
      'workbench:terminal-status',
      (event) => {
        const payload = event.payload;
        setSessions((current) =>
          current.map((session) =>
            session.id === payload.sessionId
              ? {
                  ...session,
                  status: payload.status,
                  exitCode: payload.exitCode,
                  exitedAt:
                    payload.status === 'exited' || payload.status === 'disconnected'
                      ? new Date(payload.ts).toISOString()
                      : session.exitedAt,
                }
              : session,
          ),
        );
      },
    );
    return () => {
      void statusUnlisten.then((fn) => fn());
    };
  }, []);

  const handleCreateSession = useCallback(async () => {
    const projectId = activeProjectIdRef.current;
    if (!projectId) return;
    const worktreeId = activeWorktreeIdRef.current;
    try {
      setSessionBusy(true);
      setSessionError(null);
      const initialSize = measureInitialTerminalSize(terminalPanelRef.current, 'single');
      const session = await workbenchApi.sessions.create(projectId, initialSize, worktreeId);
      if (
        activeProjectIdRef.current !== projectId ||
        activeWorktreeIdRef.current !== worktreeId
      ) {
        return;
      }
      setSessions((current) => [...current, session]);
      knownSessionIdsRef.current.add(session.id);
      focusSession(session.id);
      resetTerminalBuffer(session.id);
      void refreshProjectSessionStats(projectId);
    } catch (error) {
      if (
        activeProjectIdRef.current !== projectId ||
        activeWorktreeIdRef.current !== worktreeId
      ) {
        return;
      }
      setSessionError(
        displayErrorMessage(
          error,
          t('workbench:errors.createSession'),
          desktopUnavailableMessage,
        ),
      );
    } finally {
      setSessionBusy(false);
    }
  }, [desktopUnavailableMessage, focusSession, refreshProjectSessionStats, resetTerminalBuffer, t]);

  const handleSplitPane = useCallback(
    async (direction: 'right' | 'down') => {
      if (!activeSession) return;
      try {
        setSessionError(null);
        await workbenchApi.sessions.splitPane(activeSession.id, direction);
        await loadSessions(activeSession.projectId);
      } catch (error) {
        setSessionError(
          displayErrorMessage(
            error,
            t('workbench:errors.splitPane'),
            desktopUnavailableMessage,
          ),
        );
      }
    },
    [activeSession, desktopUnavailableMessage, loadSessions, t],
  );

  const handleClosePane = useCallback(async () => {
    if (!activeSession) return;
    try {
      setSessionError(null);
      const result = await workbenchApi.sessions.closePane(activeSession.id);
      const projectId = activeSession.projectId;
      if (result.closedWindow) {
        setSessions((current) => {
          const next = current.filter((session) => session.id !== result.sessionId);
          updateActiveSession(next);
          return next;
        });
        knownSessionIdsRef.current.delete(result.sessionId);
        removeTerminalBuffer(result.sessionId);
      }
      await loadSessions(projectId);
    } catch (error) {
      setSessionError(
        displayErrorMessage(error, t('workbench:errors.closePane'), desktopUnavailableMessage),
      );
    }
  }, [
    activeSession,
    desktopUnavailableMessage,
    loadSessions,
    removeTerminalBuffer,
    t,
    updateActiveSession,
  ]);

  const handleInput = useCallback(async (sessionId: string, data: string) => {
    try {
      await workbenchApi.sessions.writeInput(sessionId, data);
    } catch {
      // 输入写入失败时通常是会话刚退出；状态事件或下一次操作会反映错误。
    }
  }, []);

  const openPromptOptimizerPanel = useCallback(() => {
    const reset = resetPromptOptimizerTextState();
    setPromptInput(reset.input);
    setPromptPanelOpen(true);
  }, []);

  const handleCursorAnchorChange = useCallback((anchor: TerminalCursorAnchor | null) => {
    const area = terminalAreaRef.current;
    if (!area || !anchor) return;
    setPromptPanelPosition(promptOptimizerPanelPosition(area.getBoundingClientRect(), anchor));
  }, []);

  const runPromptOptimization = useCallback(
    async () => {
      if (!promptInput.trim() || promptOptimizing) {
        promptInputRef.current?.focus();
        return;
      }
      if (!activeSession || !canFillPromptIntoTerminal(activeSession)) {
        setSessionError(t('workbench:promptOptimizer.fillFailed'));
        return;
      }
      try {
        setPromptOptimizing(true);
        await promptOptimizerApi.streamToTerminal(promptInput, {
          workingDirectory: promptWorkingDirectory,
          targetLanguage: promptOptimizerFillLanguage,
          sessionId: activeSession.id,
        });
        setPromptPanelOpen(false);
      } catch (error) {
        setSessionError(
          displayErrorMessage(
            error,
            t('workbench:promptOptimizer.optimizeFailed'),
            desktopUnavailableMessage,
          ),
        );
      } finally {
        setPromptOptimizing(false);
      }
    },
    [
      activeSession,
      desktopUnavailableMessage,
      promptInput,
      promptOptimizing,
      promptOptimizerFillLanguage,
      promptWorkingDirectory,
      t,
    ],
  );

  const triggerPromptOptimizerShortcut = useCallback(() => {
    if (!activeProjectIdRef.current) return;
    const action = promptOptimizerShortcutAction(promptPanelOpen, promptInput);
    if (action === 'open') {
      openPromptOptimizerPanel();
      return;
    }
    if (action === 'close') {
      setPromptPanelOpen(false);
      return;
    }
    void runPromptOptimization();
  }, [openPromptOptimizerPanel, promptInput, promptPanelOpen, runPromptOptimization]);

  useEffect(() => {
    const handleShortcutEvent = (event: KeyboardEvent) => {
      const result = reducePromptOptimizerShortcut(
        promptShortcutStateRef.current,
        {
          type: event.type === 'keyup' ? 'keyup' : 'keydown',
          key: event.key,
          ctrlKey: event.ctrlKey,
          metaKey: event.metaKey,
          altKey: event.altKey,
          shiftKey: event.shiftKey,
          repeat: event.repeat,
        },
        promptOptimizerHotkey,
      );
      promptShortcutStateRef.current = result.state;
      if (!result.triggered) return;
      event.preventDefault();
      event.stopPropagation();
      triggerPromptOptimizerShortcut();
    };

    window.addEventListener('keydown', handleShortcutEvent, { capture: true });
    window.addEventListener('keyup', handleShortcutEvent, { capture: true });
    return () => {
      window.removeEventListener('keydown', handleShortcutEvent, { capture: true });
      window.removeEventListener('keyup', handleShortcutEvent, { capture: true });
    };
  }, [promptOptimizerHotkey, triggerPromptOptimizerShortcut]);

  const handleResize = useCallback(async (sessionId: string, cols: number, rows: number) => {
    try {
      await workbenchApi.sessions.resize(
        sessionId,
        clampU16(cols, MIN_TERMINAL_COLS),
        clampU16(rows, MIN_TERMINAL_ROWS),
      );
    } catch {
      // 容器 resize 高频触发，失败不阻断终端显示。
    }
  }, []);

  const handleCloseSession = useCallback(
    async (sessionId: string) => {
      try {
        await workbenchApi.sessions.close(sessionId);
        setSessions((current) => {
          const next = current.filter((session) => session.id !== sessionId);
          updateActiveSession(next);
          return next;
        });
        knownSessionIdsRef.current.delete(sessionId);
        removeTerminalBuffer(sessionId);
        const projectId = activeProjectIdRef.current;
        if (projectId) void refreshProjectSessionStats(projectId);
      } catch (error) {
        setSessionError(
          displayErrorMessage(
            error,
            t('workbench:errors.closeSession'),
            desktopUnavailableMessage,
          ),
        );
      }
    },
    [
      desktopUnavailableMessage,
      refreshProjectSessionStats,
      removeTerminalBuffer,
      t,
      updateActiveSession,
    ],
  );

  const handleRenameSession = useCallback(async () => {
    if (!activeSession || !sessionNameDraft.trim()) return;
    try {
      setSessionError(null);
      const renamed = await workbenchApi.sessions.rename(activeSession.id, sessionNameDraft.trim());
      setSessions((current) =>
        current.map((session) => (session.id === renamed.id ? renamed : session)),
      );
    } catch (error) {
      setSessionError(
        displayErrorMessage(error, t('workbench:errors.renameSession'), desktopUnavailableMessage),
      );
    }
  }, [activeSession, desktopUnavailableMessage, sessionNameDraft, t]);

  const handleOpenCreateWorktree = useCallback(() => {
    if (!activeProjectIdRef.current || worktreeBusy !== null) return;
    setWorktreeError(null);
    setCreateWorktreeBranchPrefix(DEFAULT_WORKTREE_BRANCH_PREFIX);
    setCreateWorktreeBranchSuffixDraft('');
    setCreateWorktreeOpen(true);
  }, [worktreeBusy]);

  const handleCancelCreateWorktree = useCallback(() => {
    if (worktreeBusy === 'create') return;
    setCreateWorktreeOpen(false);
    setCreateWorktreeBranchPrefix(DEFAULT_WORKTREE_BRANCH_PREFIX);
    setCreateWorktreeBranchSuffixDraft('');
  }, [worktreeBusy]);

  const handleCreateWorktree = useCallback(async () => {
    const projectId = activeProjectIdRef.current;
    if (!projectId) return;
    const branchName = composeWorktreeBranchName(
      createWorktreeBranchPrefix,
      createWorktreeBranchSuffixDraft,
    );
    if (!branchName) return;
    try {
      setWorktreeBusy('create');
      setWorktreeError(null);
      const created = await workbenchApi.worktrees.create(projectId, branchName);
      if (activeProjectIdRef.current !== projectId) return;
      await loadWorktrees(projectId);
      setActiveWorktreeId(created.id);
      setCreateWorktreeOpen(false);
      setCreateWorktreeBranchPrefix(DEFAULT_WORKTREE_BRANCH_PREFIX);
      setCreateWorktreeBranchSuffixDraft('');
    } catch (error) {
      if (activeProjectIdRef.current !== projectId) return;
      setWorktreeError(
        displayErrorMessage(
          error,
          t('workbench:errors.createWorktree'),
          desktopUnavailableMessage,
        ),
      );
    } finally {
      setWorktreeBusy(null);
    }
  }, [
    createWorktreeBranchPrefix,
    createWorktreeBranchSuffixDraft,
    desktopUnavailableMessage,
    loadWorktrees,
    t,
  ]);

  const handleCommitWorktree = useCallback(async () => {
    if (!activeWorktree) return;
    try {
      setWorktreeBusy('commit');
      setWorktreeError(null);
      await workbenchApi.worktrees.commit(activeWorktree.id, null);
      await loadWorktrees(activeWorktree.projectId);
      if (inspectorTab === 'history') await loadGitHistory();
    } catch (error) {
      await loadWorktrees(activeWorktree.projectId);
      if (inspectorTab === 'history') await loadGitHistory();
      setWorktreeError(
        displayErrorMessage(error, t('workbench:errors.commitWorktree'), desktopUnavailableMessage),
      );
    } finally {
      setWorktreeBusy(null);
    }
  }, [activeWorktree, desktopUnavailableMessage, inspectorTab, loadGitHistory, loadWorktrees, t]);

  const handlePushWorktree = useCallback(async () => {
    if (!activeWorktree) return;
    try {
      setWorktreeBusy('push');
      setWorktreeError(null);
      await workbenchApi.worktrees.push(activeWorktree.id);
      await loadWorktrees(activeWorktree.projectId);
      if (inspectorTab === 'history') await loadGitHistory();
    } catch (error) {
      setWorktreeError(
        displayErrorMessage(error, t('workbench:errors.pushWorktree'), desktopUnavailableMessage),
      );
    } finally {
      setWorktreeBusy(null);
    }
  }, [activeWorktree, desktopUnavailableMessage, inspectorTab, loadGitHistory, loadWorktrees, t]);

  const handleMergeWorktree = useCallback(async () => {
    if (!activeWorktree || activeWorktree.isMain) return;
    if (!window.confirm(t('workbench:worktrees.mergeConfirm', { name: activeWorktree.name }))) {
      return;
    }
    try {
      setWorktreeBusy('merge');
      setWorktreeError(null);
      await workbenchApi.worktrees.merge(activeWorktree.id);
      await loadWorktrees(activeWorktree.projectId);
      if (inspectorTab === 'history') await loadGitHistory();
    } catch (error) {
      setWorktreeError(
        displayErrorMessage(error, t('workbench:errors.mergeWorktree'), desktopUnavailableMessage),
      );
    } finally {
      setWorktreeBusy(null);
    }
  }, [activeWorktree, desktopUnavailableMessage, inspectorTab, loadGitHistory, loadWorktrees, t]);

  const handleRemoveWorktree = useCallback(async () => {
    if (!activeWorktree || activeWorktree.isMain) return;
    if (!window.confirm(t('workbench:worktrees.removeConfirm', { name: activeWorktree.name }))) {
      return;
    }
    try {
      setWorktreeBusy('remove');
      setWorktreeError(null);
      await workbenchApi.worktrees.remove(activeWorktree.id);
      if (activeWorktreeIdRef.current === activeWorktree.id) {
        const next = worktrees.find((worktree) => worktree.id !== activeWorktree.id);
        setActiveWorktreeId(next?.id ?? null);
      }
      await loadWorktrees(activeWorktree.projectId);
    } catch (error) {
      setWorktreeError(
        displayErrorMessage(error, t('workbench:errors.removeWorktree'), desktopUnavailableMessage),
      );
    } finally {
      setWorktreeBusy(null);
    }
  }, [activeWorktree, desktopUnavailableMessage, loadWorktrees, t, worktrees]);

  const handleToggleNode = useCallback(
    (node: WorkbenchFileNode) => {
      if (node.kind !== 'dir') return;
      setExpandedPaths((current) => {
        const next = new Set(current);
        if (next.has(node.path)) {
          next.delete(node.path);
        } else {
          next.add(node.path);
          if (!childrenByPath[node.path]) {
            void loadDir(node.path);
          }
        }
        return next;
      });
    },
    [childrenByPath, loadDir],
  );

  const handleSelectNode = useCallback(
    (node: WorkbenchFileNode) => {
      setSelectedPath(node.path);
      setSelectedInfo({
        name: node.name,
        path: node.path,
        kind: node.kind,
        size: node.size,
        modifiedAt: node.modifiedAt,
      });
      setRenameName(node.name);
      void loadPathInfo(node.path);
    },
    [loadPathInfo],
  );

  const refreshParentDir = useCallback(
    async (path: string) => {
      const parent = parentPathOf(path);
      await loadDir(parent);
      if (parent === '') await loadDir('');
    },
    [loadDir],
  );

  const handleCreateEntry = useCallback(
    async (kind: 'file' | 'dir') => {
      const projectId = activeProjectIdRef.current;
      if (!projectId || !newEntryName.trim()) return;
      const worktreeId = activeWorktreeIdRef.current;
      try {
        setFileError(null);
        setFileNotice(null);
        const parentPath = selectedParentPath;
        const created =
          kind === 'file'
            ? await workbenchApi.files.createFile(
                projectId,
                parentPath,
                newEntryName.trim(),
                worktreeId,
              )
            : await workbenchApi.files.createDir(
                projectId,
                parentPath,
                newEntryName.trim(),
                worktreeId,
              );
        if (
          activeProjectIdRef.current !== projectId ||
          activeWorktreeIdRef.current !== worktreeId
        ) {
          return;
        }
        setNewEntryName('');
        setSelectedPath(created.path);
        setSelectedInfo(created);
        setRenameName(created.name);
        if (parentPath) {
          setExpandedPaths((current) => new Set(current).add(parentPath));
        }
        await loadDir(parentPath);
      } catch (error) {
        if (
          activeProjectIdRef.current !== projectId ||
          activeWorktreeIdRef.current !== worktreeId
        ) {
          return;
        }
        setFileError(
          displayErrorMessage(error, t('workbench:errors.createPath'), desktopUnavailableMessage),
        );
      }
    },
    [desktopUnavailableMessage, loadDir, newEntryName, selectedParentPath, t],
  );

  const handleRenamePath = useCallback(async () => {
    const projectId = activeProjectIdRef.current;
    if (!projectId || !selectedInfo || !renameName.trim()) return;
    const worktreeId = activeWorktreeIdRef.current;
    try {
      setFileError(null);
      setFileNotice(null);
      const originalPath = selectedInfo.path;
      const renamed = await workbenchApi.files.renamePath(
        projectId,
        originalPath,
        renameName.trim(),
        worktreeId,
      );
      if (
        activeProjectIdRef.current !== projectId ||
        activeWorktreeIdRef.current !== worktreeId
      ) {
        return;
      }
      setSelectedPath(renamed.path);
      setSelectedInfo(renamed);
      setRenameName(renamed.name);
      await refreshParentDir(originalPath);
    } catch (error) {
      if (
        activeProjectIdRef.current !== projectId ||
        activeWorktreeIdRef.current !== worktreeId
      ) {
        return;
      }
      setFileError(
        displayErrorMessage(error, t('workbench:errors.renamePath'), desktopUnavailableMessage),
      );
    }
  }, [desktopUnavailableMessage, refreshParentDir, renameName, selectedInfo, t]);

  const handleDeletePath = useCallback(async () => {
    const projectId = activeProjectIdRef.current;
    if (!projectId || !selectedInfo) return;
    if (!window.confirm(t('workbench:confirmDeletePath', { name: selectedInfo.name }))) return;
    const worktreeId = activeWorktreeIdRef.current;
    const path = selectedInfo.path;
    try {
      setFileError(null);
      setFileNotice(null);
      await workbenchApi.files.deletePath(projectId, path, worktreeId);
      if (
        activeProjectIdRef.current !== projectId ||
        activeWorktreeIdRef.current !== worktreeId
      ) {
        return;
      }
      setSelectedPath(null);
      setSelectedInfo(null);
      setRenameName('');
      await refreshParentDir(path);
    } catch (error) {
      if (
        activeProjectIdRef.current !== projectId ||
        activeWorktreeIdRef.current !== worktreeId
      ) {
        return;
      }
      setFileError(
        displayErrorMessage(error, t('workbench:errors.deletePath'), desktopUnavailableMessage),
      );
    }
  }, [desktopUnavailableMessage, refreshParentDir, selectedInfo, t]);

  const handleCopySelectedPath = useCallback(async () => {
    if (!selectedInfo) return;
    try {
      const value = selectedInfo.path || '.';
      await navigator.clipboard.writeText(value);
      setFileError(null);
      setFileNotice(t('workbench:pathCopied'));
    } catch (error) {
      setFileError(
        displayErrorMessage(error, t('workbench:errors.copyPath'), desktopUnavailableMessage),
      );
    }
  }, [desktopUnavailableMessage, selectedInfo, t]);

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
        </section>

        <section className={styles.worktreeBar} aria-label={t('workbench:worktrees.label')}>
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
                    disabled={worktreeBusy === 'create'}
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
                  disabled={worktreeBusy === 'create'}
                  onChange={(event) => setCreateWorktreeBranchSuffixDraft(event.target.value)}
                />
                <Button
                  type="submit"
                  size="sm"
                  variant="primary"
                  loading={worktreeBusy === 'create'}
                  disabled={!composedWorktreeBranchName || worktreeBusy !== null}
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
                disabled={!activeProjectId || worktreeBusy !== null}
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
              disabled={!canRemoveWorktree(activeWorktree, worktreeBusy)}
              onClick={() => void handleRemoveWorktree()}
            />
          </div>
        </section>

        <div className={styles.noticeStack}>
          {sessionError ? <div className={styles.errorBox}>{sessionError}</div> : null}
          {worktreeError ? <div className={styles.errorBox}>{worktreeError}</div> : null}
          {dependencyStatus.status !== 'ready' ? (
            <WorkbenchDependencyCard compact className={styles.dependencyNotice} />
          ) : null}
        </div>

        <section className={styles.sessionTabs} aria-label={t('workbench:terminalTabs')}>
          {scopedSessions.map((session) => (
            <button
              key={session.id}
              type="button"
              className={styles.sessionTab}
              data-active={session.id === activeSessionId || undefined}
              onClick={() => focusSession(session.id)}
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
            </button>
          ))}
          <Button
            className={styles.newSessionButton}
            variant="secondary"
            size="sm"
            icon={<PlusIcon />}
            loading={sessionBusy}
            disabled={!activeProjectId || !activeWorktree}
            onClick={() => void handleCreateSession()}
          >
            {t('workbench:newSession')}
          </Button>
          <div className={styles.paneActions} aria-label={t('workbench:paneActions')}>
            <Button
              variant="icon"
              icon={<EditIcon />}
              title={t('workbench:promptOptimizer.open')}
              aria-label={t('workbench:promptOptimizer.open')}
              data-active={promptPanelOpen || undefined}
              onClick={() => {
                if (promptPanelOpen) {
                  setPromptPanelOpen(false);
                } else {
                  openPromptOptimizerPanel();
                }
              }}
            />
            <Button
              variant="icon"
              icon={<SplitRightIcon />}
              title={t('workbench:splitPaneRight')}
              aria-label={t('workbench:splitPaneRight')}
              disabled={!canUsePanes}
              onClick={() => void handleSplitPane('right')}
            />
            <Button
              variant="icon"
              icon={<SplitDownIcon />}
              title={t('workbench:splitPaneDown')}
              aria-label={t('workbench:splitPaneDown')}
              disabled={!canUsePanes}
              onClick={() => void handleSplitPane('down')}
            />
            <Button
              variant="icon"
              icon={<XIcon />}
              title={t('workbench:closePane')}
              aria-label={t('workbench:closePane')}
              disabled={!canUsePanes}
              onClick={() => void handleClosePane()}
            />
          </div>
        </section>

        <div className={styles.terminalArea} ref={terminalAreaRef}>
          {promptPanelOpen ? (
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
                onKeyDown={(event) => {
                  const action = promptOptimizerInputKeyAction(
                    {
                      key: event.key,
                      shiftKey: event.shiftKey,
                      isComposing: event.nativeEvent.isComposing,
                    },
                    promptInput,
                  );
                  if (action !== 'optimize') return;
                  event.preventDefault();
                  event.stopPropagation();
                  void runPromptOptimization();
                }}
                placeholder={t('promptOptimizer:inputPlaceholder')}
                aria-label={t('promptOptimizer:inputAriaLabel')}
                disabled={promptOptimizing}
              />
            </aside>
          ) : null}

          <section
            className={styles.terminalPanel}
            data-layout="single"
            ref={terminalPanelRef}
          >
            {visibleSessions.length === 0 ? (
              <TerminalPane
                session={null}
                buffer=""
                revision={terminalRevision}
                placeholder={
                  activeProject
                    ? t('workbench:terminalPlaceholder')
                    : t('workbench:terminalNoProject')
                }
                onInput={handleInput}
                onResize={handleResize}
                inputEnabled={false}
                onCursorAnchorChange={handleCursorAnchorChange}
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
                <TerminalPane
                  session={session}
                  buffer={terminalBuffers[session.id] ?? ''}
                  revision={terminalRevision}
                  placeholder={t('workbench:terminalPlaceholder')}
                  onInput={handleInput}
                  onResize={handleResize}
                  inputEnabled={session.id === renderedActiveSessionId}
                  onCursorAnchorChange={
                    session.id === renderedActiveSessionId ? handleCursorAnchorChange : undefined
                  }
                />
              </div>
            ))}
          </section>
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
              disabled={!activeSession}
            />
            <div className={styles.statusButtonRow}>
              <Button
                size="sm"
                variant="secondary"
                icon={<EditIcon />}
                disabled={!activeSession || !sessionNameDraft.trim()}
                onClick={() => void handleRenameSession()}
              >
                {t('workbench:renameSession')}
              </Button>
              <Button
                size="sm"
                variant="danger"
                icon={<XIcon />}
                disabled={!activeSession}
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
                  disabled={!activeProjectId || !newEntryName.trim()}
                  onClick={() => void handleCreateEntry('file')}
                >
                  {t('workbench:createFile')}
                </Button>
                <Button
                  size="sm"
                  variant="secondary"
                  icon={<FolderIcon />}
                  disabled={!activeProjectId || !newEntryName.trim()}
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
                  disabled={!selectedInfo}
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
                  disabled={!selectedInfo || !renameName.trim()}
                  onClick={() => void handleRenamePath()}
                >
                  {t('workbench:rename')}
                </Button>
                <Button
                  size="sm"
                  variant="danger"
                  icon={<TrashIcon />}
                  disabled={!selectedInfo}
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
                  disabled={!canCommitWorktree(activeWorktree, worktreeBusy)}
                  onClick={() => void handleCommitWorktree()}
                >
                  {t('workbench:worktrees.commit')}
                </Button>
                <Button
                  size="sm"
                  variant="secondary"
                  icon={<UploadIcon />}
                  loading={worktreeBusy === 'push'}
                  disabled={!canPushWorktree(activeWorktree, worktreeBusy)}
                  onClick={() => void handlePushWorktree()}
                >
                  {t('workbench:worktrees.push')}
                </Button>
                <Button
                  size="sm"
                  variant="secondary"
                  icon={<SyncIcon />}
                  loading={worktreeBusy === 'merge'}
                  disabled={!canMergeWorktree(activeWorktree, worktreeBusy)}
                  onClick={() => void handleMergeWorktree()}
                >
                  {t('workbench:worktrees.merge')}
                </Button>
              </div>
            </div>

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
    </div>
  );
}
