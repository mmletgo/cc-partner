/**
 * 工作台页面 - 本机项目、多终端与项目文件夹检查器
 *
 * Business Logic（为什么需要这个页面）:
 *   用户需要指定一个项目文件夹，并在 cc-partner 内为该项目同时管理多个项目终端；
 *   右侧检查器本期展示当前会话状态和可交互项目文件夹，下一期再接入文件预览。
 *
 * Code Logic（这个页面做什么）:
 *   - 拉取/添加/移除工作台项目，并按当前项目加载会话与根目录文件树
 *   - 用 xterm 渲染当前 session，监听后端 terminal output/status 事件同步 UI
 *   - 提供文件夹展开、选中、创建、重命名、删除等基础文件树交互
 *   - hooks 全部在 early return 之前，避免 React hooks 调用顺序问题
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { CSSProperties } from 'react';
import { useTranslation } from 'react-i18next';
import { listen } from '@tauri-apps/api/event';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { workbenchApi } from '@/api/workbench';
import { Button, Card, Input, Pill } from '@/components/primitives';
import { useWorkbenchProjects } from '@/hooks/workbenchProjectsContext';
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
  XIcon,
} from '@/lib/icons';
import type {
  WorkbenchFileNode,
  WorkbenchPathInfo,
  WorkbenchSession,
  WorkbenchTerminalOutputEvent,
  WorkbenchTerminalStatusEvent,
} from '@/lib/types';
import styles from './Workbench.module.css';
import { visibleTerminalSessions } from './terminalSessionOrder';
import { workbenchTerminalOptions, workbenchTerminalTheme } from './terminalOptions';
import { shouldForwardTerminalInput, writeTerminalReplay } from './terminalReplay';
import { terminalPanePixelSize } from './terminalSizing';
import type { TerminalLayoutMode } from './terminalSizing';

interface TauriInternalsWindow extends Window {
  __TAURI_INTERNALS__?: {
    transformCallback?: unknown;
  };
}

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
  onInput: (sessionId: string, data: string) => void;
  onResize: (sessionId: string, cols: number, rows: number) => void;
}

interface TerminalSize {
  cols: number;
  rows: number;
}

const MIN_TERMINAL_COLS = 20;
const MIN_TERMINAL_ROWS = 6;
const MAX_TERMINAL_BUFFER_CHARS = 200_000;
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
 *   ResizeObserver 触发 FitAddon.fit 后把 cols/rows clamp 后回传后端。
 */
function TerminalPane(props: TerminalPaneProps) {
  const { session, buffer, revision, placeholder, onInput, onResize } = props;
  const viewportRef = useRef<HTMLDivElement | null>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const bufferRef = useRef<string>('');
  const writtenLengthRef = useRef<number>(0);
  const replayGateRef = useRef<boolean>(false);
  const resizeTimerRef = useRef<number | null>(null);
  const sessionId = session?.id ?? null;

  useEffect(() => {
    bufferRef.current = buffer;
  }, [buffer]);

  useEffect(() => {
    const viewport = viewportRef.current;
    if (!viewport || !sessionId) return undefined;

    const terminal = new Terminal(workbenchTerminalOptions());
    const fit = new FitAddon();
    terminal.loadAddon(fit);
    terminal.open(viewport);
    fit.fit();
    const dataDisposable = terminal.onData((data: string) => {
      if (!shouldForwardTerminalInput(replayGateRef)) return;
      onInput(sessionId, data);
    });
    writeTerminalReplay(terminal, bufferRef.current, replayGateRef);
    writtenLengthRef.current = bufferRef.current.length;
    const resize = () => {
      try {
        fit.fit();
        onResize(
          sessionId,
          clampU16(terminal.cols, MIN_TERMINAL_COLS),
          clampU16(terminal.rows, MIN_TERMINAL_ROWS),
        );
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
      if (resizeTimerRef.current !== null) {
        window.clearTimeout(resizeTimerRef.current);
        resizeTimerRef.current = null;
      }
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
  const { t } = useTranslation(['workbench', 'common']);
  const { activeProjectId, activeProject } = useWorkbenchProjects();
  const [sessions, setSessions] = useState<WorkbenchSession[]>([]);
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);
  const [sessionNameDraft, setSessionNameDraft] = useState<string>('');
  const [sessionBusy, setSessionBusy] = useState<boolean>(false);
  const [sessionError, setSessionError] = useState<string | null>(null);
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
  const [terminalBuffers, setTerminalBuffers] = useState<Record<string, string>>({});
  const [terminalRevision, setTerminalRevision] = useState<number>(0);
  const [runtimeNow, setRuntimeNow] = useState<number>(() => Date.now());
  const activeProjectIdRef = useRef<string | null>(null);
  const knownSessionIdsRef = useRef<Set<string>>(new Set());
  const terminalPanelRef = useRef<HTMLElement | null>(null);
  const lastLocalFocusAtRef = useRef<number>(0);

  const activeSession = useMemo(
    () => sessions.find((session) => session.id === activeSessionId) ?? null,
    [activeSessionId, sessions],
  );
  const visibleSessions = useMemo(
    () => visibleTerminalSessions({ sessions, activeSessionId }),
    [activeSessionId, sessions],
  );
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
  const activeSessionRuntime = formatRuntime(
    activeSession?.startedAt ?? null,
    activeSession?.exitedAt ?? null,
    runtimeNow,
    emptyValue,
  );
  const canUsePanes = Boolean(
    activeSession?.supportsPanes && activeSession.status === 'running',
  );

  const updateActiveSession = useCallback((nextSessions: WorkbenchSession[]) => {
    setActiveSessionId((current) => {
      if (current && nextSessions.some((session) => session.id === current)) return current;
      return nextSessions[0]?.id ?? null;
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
    if (!activeProjectId || sessions.length === 0) return undefined;
    let cancelled = false;

    const syncFocusedSession = () => {
      if (Date.now() - lastLocalFocusAtRef.current < LOCAL_FOCUS_GRACE_MS) return;
      void workbenchApi.sessions
        .focused(activeProjectId)
        .then(({ sessionId }) => {
          if (cancelled || !sessionId) return;
          if (!sessions.some((session) => session.id === sessionId)) return;
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
  }, [activeProjectId, sessions]);

  const loadSessions = useCallback(
    async (projectId: string) => {
      try {
        setSessionError(null);
        const list = await workbenchApi.sessions.list(projectId);
        if (activeProjectIdRef.current !== projectId) return;
        knownSessionIdsRef.current = new Set(list.map((session) => session.id));
        setSessions(list);
        updateActiveSession(list);
      } catch (error) {
        if (activeProjectIdRef.current !== projectId) return;
        setSessionError(
          displayErrorMessage(error, t('workbench:errors.sessions'), desktopUnavailableMessage),
        );
      }
    },
    [desktopUnavailableMessage, t, updateActiveSession],
  );

  const loadDir = useCallback(
    async (path: string) => {
      const projectId = activeProjectIdRef.current;
      if (!projectId) return;
      try {
        setFileError(null);
        setFileLoadingPath(path);
        const nodes = await workbenchApi.files.listDir(projectId, path);
        if (activeProjectIdRef.current !== projectId) return;
        if (path === '') {
          setRootNodes(nodes);
        } else {
          setChildrenByPath((current) => ({ ...current, [path]: nodes }));
        }
      } catch (error) {
        if (activeProjectIdRef.current !== projectId) return;
        setFileError(
          displayErrorMessage(error, t('workbench:errors.files'), desktopUnavailableMessage),
        );
      } finally {
        if (activeProjectIdRef.current === projectId) {
          setFileLoadingPath(null);
        }
      }
    },
    [desktopUnavailableMessage, t],
  );

  const loadPathInfo = useCallback(
    async (path: string) => {
      const projectId = activeProjectIdRef.current;
      if (!projectId) return;
      try {
        const info = await workbenchApi.files.info(projectId, path);
        if (activeProjectIdRef.current !== projectId) return;
        setSelectedInfo(info);
        setRenameName(info.name);
      } catch (error) {
        if (activeProjectIdRef.current !== projectId) return;
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
    knownSessionIdsRef.current = new Set(sessions.map((session) => session.id));
  }, [sessions]);

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
    return deferEffect(() => {
      if (!activeProjectId) {
        knownSessionIdsRef.current = new Set();
        setSessions([]);
        setActiveSessionId(null);
        setRootNodes([]);
        setChildrenByPath({});
        setExpandedPaths(new Set());
        setSelectedPath(null);
        setSelectedInfo(null);
        return;
      }
      setRootNodes([]);
      knownSessionIdsRef.current = new Set();
      setChildrenByPath({});
      setExpandedPaths(new Set());
      setSelectedPath(null);
      setSelectedInfo(null);
      setFileNotice(null);
      void loadSessions(activeProjectId);
      void loadDir('');
    });
  }, [activeProjectId, loadDir, loadSessions]);

  useEffect(() => {
    if (!canListenToTauriEvents()) return undefined;
    const outputUnlisten = listen<WorkbenchTerminalOutputEvent>(
      'workbench:terminal-output',
      (event) => {
        const payload = event.payload;
        setTerminalBuffers((current) => {
          const nextBuffer = `${current[payload.sessionId] ?? ''}${payload.chunk}`;
          return {
            ...current,
            [payload.sessionId]:
              nextBuffer.length > MAX_TERMINAL_BUFFER_CHARS
                ? nextBuffer.slice(-MAX_TERMINAL_BUFFER_CHARS)
                : nextBuffer,
          };
        });
        setTerminalRevision((current) => current + 1);
      },
    );
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
      void outputUnlisten.then((fn) => fn());
      void statusUnlisten.then((fn) => fn());
    };
  }, []);

  const handleCreateSession = useCallback(async () => {
    const projectId = activeProjectIdRef.current;
    if (!projectId) return;
    try {
      setSessionBusy(true);
      setSessionError(null);
      const initialSize = measureInitialTerminalSize(terminalPanelRef.current, 'single');
      const session = await workbenchApi.sessions.create(projectId, initialSize);
      if (activeProjectIdRef.current !== projectId) return;
      setSessions((current) => [...current, session]);
      knownSessionIdsRef.current.add(session.id);
      focusSession(session.id);
      setTerminalBuffers((current) => ({ ...current, [session.id]: '' }));
      setTerminalRevision((current) => current + 1);
    } catch (error) {
      if (activeProjectIdRef.current !== projectId) return;
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
  }, [desktopUnavailableMessage, focusSession, t]);

  const handleSplitPane = useCallback(
    async (direction: 'right' | 'down') => {
      if (!activeSession) return;
      try {
        setSessionError(null);
        await workbenchApi.sessions.splitPane(activeSession.id, direction);
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
    [activeSession, desktopUnavailableMessage, t],
  );

  const handleClosePane = useCallback(async () => {
    if (!activeSession) return;
    try {
      setSessionError(null);
      const result = await workbenchApi.sessions.closePane(activeSession.id);
      if (result.closedWindow) {
        setSessions((current) => {
          const next = current.filter((session) => session.id !== result.sessionId);
          updateActiveSession(next);
          return next;
        });
        knownSessionIdsRef.current.delete(result.sessionId);
        setTerminalBuffers((current) => {
          const next = { ...current };
          delete next[result.sessionId];
          return next;
        });
        setTerminalRevision((current) => current + 1);
      }
    } catch (error) {
      setSessionError(
        displayErrorMessage(error, t('workbench:errors.closePane'), desktopUnavailableMessage),
      );
    }
  }, [activeSession, desktopUnavailableMessage, t, updateActiveSession]);

  const handleInput = useCallback(async (sessionId: string, data: string) => {
    try {
      await workbenchApi.sessions.writeInput(sessionId, data);
    } catch {
      // 输入写入失败时通常是会话刚退出；状态事件或下一次操作会反映错误。
    }
  }, []);

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
        setTerminalBuffers((current) => {
          const next = { ...current };
          delete next[sessionId];
          return next;
        });
        setTerminalRevision((current) => current + 1);
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
    [desktopUnavailableMessage, t, updateActiveSession],
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
      try {
        setFileError(null);
        setFileNotice(null);
        const parentPath = selectedParentPath;
        const created =
          kind === 'file'
            ? await workbenchApi.files.createFile(projectId, parentPath, newEntryName.trim())
            : await workbenchApi.files.createDir(projectId, parentPath, newEntryName.trim());
        if (activeProjectIdRef.current !== projectId) return;
        setNewEntryName('');
        setSelectedPath(created.path);
        setSelectedInfo(created);
        setRenameName(created.name);
        if (parentPath) {
          setExpandedPaths((current) => new Set(current).add(parentPath));
        }
        await loadDir(parentPath);
      } catch (error) {
        if (activeProjectIdRef.current !== projectId) return;
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
    try {
      setFileError(null);
      setFileNotice(null);
      const originalPath = selectedInfo.path;
      const renamed = await workbenchApi.files.renamePath(
        projectId,
        originalPath,
        renameName.trim(),
      );
      if (activeProjectIdRef.current !== projectId) return;
      setSelectedPath(renamed.path);
      setSelectedInfo(renamed);
      setRenameName(renamed.name);
      await refreshParentDir(originalPath);
    } catch (error) {
      if (activeProjectIdRef.current !== projectId) return;
      setFileError(
        displayErrorMessage(error, t('workbench:errors.renamePath'), desktopUnavailableMessage),
      );
    }
  }, [desktopUnavailableMessage, refreshParentDir, renameName, selectedInfo, t]);

  const handleDeletePath = useCallback(async () => {
    const projectId = activeProjectIdRef.current;
    if (!projectId || !selectedInfo) return;
    if (!window.confirm(t('workbench:confirmDeletePath', { name: selectedInfo.name }))) return;
    const path = selectedInfo.path;
    try {
      setFileError(null);
      setFileNotice(null);
      await workbenchApi.files.deletePath(projectId, path);
      if (activeProjectIdRef.current !== projectId) return;
      setSelectedPath(null);
      setSelectedInfo(null);
      setRenameName('');
      await refreshParentDir(path);
    } catch (error) {
      if (activeProjectIdRef.current !== projectId) return;
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
  const contextPath = activeProject ? selectedDisplayPath || rootPath : emptyValue;

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

        <section className={styles.contextBar}>
          <span className={styles.contextLabel}>{t('workbench:projectContextLabel')}</span>
          <strong>{activeProject?.name ?? emptyValue}</strong>
          <span className={styles.contextSlash}>/</span>
          <span className={styles.contextPath}>{contextPath}</span>
        </section>

        {sessionError ? <div className={styles.errorBox}>{sessionError}</div> : null}

        <section className={styles.sessionTabs} aria-label={t('workbench:terminalTabs')}>
          {sessions.map((session) => (
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
            disabled={!activeProjectId}
            onClick={() => void handleCreateSession()}
          >
            {t('workbench:newSession')}
          </Button>
          <div className={styles.paneActions} aria-label={t('workbench:paneActions')}>
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
            />
          ) : (
            visibleSessions.map((session) => (
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
                />
              </div>
            ))
          )}
        </section>
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
              <dt>{t('workbench:statusProjectPath')}</dt>
              <dd>{activeProject?.path ?? emptyValue}</dd>
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
      </aside>
    </div>
  );
}
