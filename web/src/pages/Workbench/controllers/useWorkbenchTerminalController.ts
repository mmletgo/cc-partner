/**
 * Workbench 终端域 controller —— session 生命周期 + focus 同步 + pane 操作 + terminal-status 事件。
 *
 * Business Logic（为什么需要这个 controller）:
 *   Workbench 终端域是最复杂的子域：它持有所有 terminal session 列表、当前 active session、rename draft、
 *   busy/error、全屏态、resize 请求 key；同时驱动多个 effect（active session focus 同步、tmux focus 轮询、
 *   terminal-status 事件订阅、activeSession 同步 defer）。把这些状态和 effect 集中到 controller，让
 *   Workbench.tsx 只负责组合渲染，不再自管 session/effect 时序。
 *
 *   重要边界：controller 只持有 session 元数据和调用外部 buffer 回调（resetBuffer/removeBuffer），
 *   绝不在 React state 中保存终端字节内容——字节内容仍由 WorkbenchTerminalBuffersProvider 持有。
 *
 * Code Logic（这个 controller 做什么）:
 *   - 持有 sessions / activeSessionId / sessionNameDraft / sessionBusy / sessionError / terminalFullscreen /
 *     terminalResizeRequestKey 单一权威状态。
 *   - 维护 activeProjectIdRef / activeWorktreeIdRef / knownSessionIdsRef / lastLocalFocusAtRef，让异步回调读取最新值。
 *   - 暴露 bridge（loadSessions / focusSession / createSessionForWorktree / clearBuffersForWorktree）和渲染数据/动作。
 *   - 注册 focus 同步 effect、tmux focus 轮询 effect、terminal-status listen effect、scopedSessions/activeSession.name defer effect。
 *
 * 不复制邻接 controller 状态：project / worktree / file / application / prompt optimizer 状态仍归 Workbench.tsx 各自所有。
 */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import type { WorkbenchTerminalStatusEvent, WorkbenchSession } from '@/lib/types';
import { workbenchApi } from '@/api/workbench';
import type { WorkbenchPaneSplitDirection } from '@/api/workbench';
import { mountedTerminalSessions, visibleTerminalSessions } from '../terminalSessionOrder';
import { canRefreshTerminalSize } from '../terminalSizing';
import type { TerminalLayoutMode } from '../terminalSizing';
import { sessionsForWorktree } from '../workbenchWorktrees';

/**
 * 终端 resize 命令后端接受 u16，前端需要提前 clamp；与原 Workbench 内部常量保持一致。
 */
const MIN_TERMINAL_COLS = 20;
const MIN_TERMINAL_ROWS = 6;
const TMUX_FOCUS_SYNC_INTERVAL_MS = 700;
const LOCAL_FOCUS_GRACE_MS = 500;

/**
 * Workbench pane split 方向；与后端 workbench API 对齐（从这里再次导出便于页面使用）。
 */
export type { WorkbenchPaneSplitDirection };

/**
 * 终端 cursor 锚点（用于 Prompt 优化浮层定位）；与原 Workbench 内部类型保持一致。
 */
export interface TerminalCursorAnchor {
  left: number;
  top: number;
  bottom: number;
}

/**
 * 终端初始尺寸估算结果；仅在 createSessionForWorktree 内传给后端。
 */
export interface TerminalSize {
  cols: number;
  rows: number;
}

/**
 * controller 输入：窄 API + 回调 + 外部 ref，避免吞并 Projects / Worktrees / Terminal buffer context。
 *
 * 字段说明：
 *   - activeProject / activeProjectId / worktrees：从 WorkbenchProjectsContext 透传，仅用于读取。
 *   - activeWorktreeId：当前激活的 worktree id；scopedSessions 据此过滤。
 *   - remoteWriteDisabled：项目域 controller 决定的只读标记；影响 input/resize/split/close 是否可写。
 *   - terminalPanelRef：渲染层透传的终端面板 ref，用于 createSession 时估算初始 cols/rows。
 *   - resetBuffer / removeBuffer：来自 WorkbenchTerminalBuffersProvider；controller 调用它们清理外部 buffer，
 *     绝不在自身 state 中保存终端字节。
 *   - refreshProjectSessionStats / markRequestFailure / markRequestSuccess / isCurrentProject：项目域 controller 的窄 API。
 */
export interface UseWorkbenchTerminalControllerParams {
  activeProjectId: string | null;
  activeWorktreeId: string | null;
  remoteWriteDisabled: boolean;
  terminalPanelRef: React.RefObject<HTMLElement | null>;
  resetBuffer: (sessionId: string) => void;
  removeBuffer: (sessionId: string) => void;
  refreshProjectSessionStats: (projectId: string) => void;
  markRequestFailure: (projectId: string, error: unknown) => void;
  markRequestSuccess: (projectId: string) => void;
  isCurrentProject: (projectId: string) => boolean;
  /**
   * 可选注入：测量 createSession 时的初始终端尺寸。
   *
   * Business Logic: 默认实现复用 Workbench 页面里的离屏 xterm 测量逻辑；测试或叶子视图可注入更轻量的实现。
   */
  measureInitialTerminalSize?: (
    panel: HTMLElement | null,
    layout: TerminalLayoutMode,
  ) => TerminalSize | undefined;
  /**
   * 可选注入：终端域错误文案构造；默认实现复用 Workbench 页面里的 displayErrorMessage。
   *
   * Business Logic: Tauri 不可用时把底层 invoke 错误映射为友好文案；controller 单元测试可不注入。
   */
  displayErrorMessage?: (error: unknown, fallback: string, desktopUnavailable: string) => string;
  /** i18n fallback 文案（来自 Workbench t()）。 */
  translateError?: (key: WorkbenchTerminalErrorKey) => string;
  /** 桌面不可用提示文案。 */
  desktopUnavailableMessage: string;
  /**
   * 可选注入：判断当前运行时是否可注册 Tauri event listener；默认实现复用 transformCallback 检测。
   *
   * Business Logic: 普通 Vite/Playwright 浏览器环境没有 Tauri event internals，直接 listen 会抛底层错误；
   * 与原 Workbench.tsx 行为一致——非 Tauri 环境跳过 terminal-status listen 注册。
   */
  canListenToTauriEvents?: () => boolean;
}

/** controller 用到的 i18n 错误文案 key；调用方注入对应 t('workbench:errors.X')。 */
export type WorkbenchTerminalErrorKey =
  | 'sessions'
  | 'focusSession'
  | 'createSession'
  | 'splitPane'
  | 'switchPane'
  | 'zoomPane'
  | 'closePane'
  | 'closeSession'
  | 'renameSession';

/**
 * controller 暴露给页面 deep link / 项目切换等流程的窄接口。
 *
 * Business Logic: Workbench.tsx 在项目切换 / worktree 创建 / merge / deep link 流程里只需要这四个动作；
 * 把它们封装成 bridge 类型，避免页面散落调用更宽的 controller API。
 */
export interface WorkbenchTerminalBridge {
  loadSessions: (projectId?: string) => Promise<void>;
  focusSession: (sessionId: string) => Promise<boolean>;
  createSessionForWorktree: (worktreeId: string) => Promise<void>;
  clearBuffersForWorktree: (worktreeId: string) => void;
}

/**
 * controller 返回值：终端域权威状态 + 操作函数 + bridge 视图。
 */
export interface WorkbenchTerminalControllerResult extends WorkbenchTerminalBridge {
  // ---- 渲染数据 ----
  sessions: WorkbenchSession[];
  scopedSessions: WorkbenchSession[];
  activeSession: WorkbenchSession | null;
  activeSessionId: string | null;
  visibleSessions: WorkbenchSession[];
  mountedSessions: WorkbenchSession[];
  renderedActiveSessionId: string | null;
  sessionNameDraft: string;
  sessionBusy: boolean;
  sessionError: string | null;
  terminalFullscreen: boolean;
  terminalResizeRequestKey: number;
  canUsePanes: boolean;
  canRefreshCurrentTerminalSize: boolean;
  // ---- 派生 actions ----
  setSessionNameDraft: (next: string | ((prev: string) => string)) => void;
  setSessionError: (next: string | null) => void;
  setActiveSessionId: (next: string | null) => void;
  setSessions: (next: WorkbenchSession[] | ((prev: WorkbenchSession[]) => WorkbenchSession[])) => void;
  /** 在 close/merge 流程后按最新 sessions 重算 active session；与原 Workbench 行为一致。 */
  updateActiveSession: (nextSessions: WorkbenchSession[]) => void;
  handleCreateSession: () => Promise<void>;
  handleSplitPane: (direction: WorkbenchPaneSplitDirection) => Promise<void>;
  handleSwitchPane: () => Promise<void>;
  handleZoomPane: () => Promise<void>;
  handleClosePane: () => Promise<void>;
  handleCloseSession: (sessionId: string) => Promise<void>;
  handleRenameSession: () => Promise<void>;
  handleInput: (sessionId: string, data: string) => Promise<void>;
  handleResize: (sessionId: string, cols: number, rows: number) => Promise<void>;
  handleRefreshTerminalSize: () => void;
  handleEnterTerminalFullscreen: () => void;
  handleExitTerminalFullscreen: () => void;
}

/**
 * Business Logic（为什么是默认导出 hook）:
 *   Workbench.tsx 在 early return 之前调用本 hook，与其它 controller 并列组合；保持 React hooks 顺序稳定。
 *
 * Code Logic（这个 hook 做什么）:
 *   1. 持有 sessions / activeSessionId / sessionNameDraft / sessionBusy / sessionError / terminalFullscreen /
 *      terminalResizeRequestKey state；
 *   2. 用 ref 跟踪 activeProjectId / activeWorktreeId / knownSessionIds / lastLocalFocusAt，让异步回调读到最新值；
 *   3. 注册 focus 同步 effect、tmux focus 轮询 effect、terminal-status listen effect、scopedSessions/activeSession.name defer effect；
 *   4. 暴露稳定的操作函数（useCallback + ref 输入）和 bridge 视图，便于 Workbench 在多处复用。
 */
export function useWorkbenchTerminalController(
  params: UseWorkbenchTerminalControllerParams,
): WorkbenchTerminalControllerResult {
  const {
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
    displayErrorMessage: displayErrorMessageParam,
    translateError,
    desktopUnavailableMessage,
    canListenToTauriEvents: canListenToTauriEventsParam,
  } = params;

  const [sessions, setSessions] = useState<WorkbenchSession[]>([]);
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);
  const [sessionNameDraft, setSessionNameDraftState] = useState<string>('');
  const [sessionBusy, setSessionBusy] = useState<boolean>(false);
  const [sessionError, setSessionError] = useState<string | null>(null);
  const [terminalFullscreen, setTerminalFullscreen] = useState<boolean>(false);
  const [terminalResizeRequestKey, setTerminalResizeRequestKey] = useState<number>(0);

  // Business Logic: 异步加载回调返回时，active project / worktree 可能已经切换；用 ref 读取最新 id 做 stale guard。
  const activeProjectIdRef = useRef<string | null>(activeProjectId);
  const activeWorktreeIdRef = useRef<string | null>(activeWorktreeId);
  // Business Logic: 用 knownSessionIdsRef 维护当前已知 session id 集合，与原 Workbench.tsx 行为保持一致。
  const knownSessionIdsRef = useRef<Set<string>>(new Set());
  // Business Logic: 用 lastLocalFocusAtRef 抑制本地 focus 操作后 500ms 内的 tmux focus 轮询，避免把焦点抢回。
  const lastLocalFocusAtRef = useRef<number>(0);
  // Business Logic: terminal-status listen 只想在 mount 时注册一次；用 ref 读取最新的 canListenToTauriEvents，
  // 避免把它放进 effect 依赖导致 listener 反复重注册（与原 Workbench.tsx 的 [] 依赖行为一致）。
  const canListenToTauriEventsRef = useRef(canListenToTauriEventsParam);
  useEffect(() => {
    canListenToTauriEventsRef.current = canListenToTauriEventsParam;
  });

  useEffect(() => {
    activeProjectIdRef.current = activeProjectId;
  }, [activeProjectId]);

  useEffect(() => {
    activeWorktreeIdRef.current = activeWorktreeId;
  }, [activeWorktreeId]);

  // Business Logic: 与原 Workbench.tsx 行为一致——sessions 变化时同步 knownSessionIdsRef。
  useEffect(() => {
    knownSessionIdsRef.current = new Set(sessions.map((session) => session.id));
  }, [sessions]);

  const scopedSessions = useMemo(
    () => sessionsForWorktree(sessions, activeWorktreeId),
    [activeWorktreeId, sessions],
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
  const renderedActiveSessionId = activeSession?.id ?? visibleSessions[0]?.id ?? null;
  const canUsePanes = Boolean(
    activeSession?.supportsPanes && activeSession.status === 'running',
  );
  const canRefreshCurrentTerminalSize = canRefreshTerminalSize(activeSession, remoteWriteDisabled);

  /**
   * Business Logic（为什么需要这个函数）:
   *   Workbench 页面内多处文案构造都依赖 displayErrorMessage；controller 注入版让测试可替换，且和原实现保持一致。
   */
  const displayErrorMessage = useCallback(
    (error: unknown, fallback: string): string => {
      if (displayErrorMessageParam) {
        return displayErrorMessageParam(error, fallback, desktopUnavailableMessage);
      }
      // 兜底实现：与原 Workbench.tsx 内联的 displayErrorMessage 行为一致。
      const message =
        error instanceof Error
          ? error.message
          : typeof error === 'string'
            ? error
            : String(error);
      const normalized = message.toLowerCase();
      if (
        normalized.includes('invoke') ||
        normalized.includes('__tauri') ||
        normalized.includes("reading 'invoke'") ||
        normalized.includes('reading "invoke"')
      ) {
        return desktopUnavailableMessage;
      }
      return message && message !== 'undefined' && message !== 'null' ? message : fallback;
    },
    [displayErrorMessageParam, desktopUnavailableMessage],
  );

  const t = useCallback(
    (key: WorkbenchTerminalErrorKey): string => {
      if (translateError) return translateError(key);
      // 兜底：测试或无 i18n 环境下返回 key 本身。
      return `workbench:errors.${key}`;
    },
    [translateError],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户关闭/合并 pane 后 sessions 变化，需要按最新 active worktree 重算 active session，
   *   保持当前 active 不变，仅在它已不存在时回退到第一个候选。
   */
  const updateActiveSession = useCallback((nextSessions: WorkbenchSession[]) => {
    const candidates = sessionsForWorktree(nextSessions, activeWorktreeIdRef.current);
    setActiveSessionId((current) => {
      if (current && candidates.some((session) => session.id === current)) return current;
      return candidates[0]?.id ?? null;
    });
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户点击 terminal tab 或 deep link 到某 session 时，需要立即把焦点切到该 session 并通知后端。
   *   返回 Promise<boolean> 是 bridge 契约要求；当前实现同步设置 activeSessionId，effect 链路异步完成 focus API。
   *
   * Code Logic（这个函数做什么）:
   *   1. 记录本地 focus 时间戳，抑制随后的 tmux focus 轮询；
   *   2. 设置 activeSessionId，触发 focus 同步 effect 调用 workbenchApi.sessions.focus。
   */
  const focusSession = useCallback(async (sessionId: string): Promise<boolean> => {
    lastLocalFocusAtRef.current = Date.now();
    setActiveSessionId(sessionId);
    return true;
  }, []);

  // Business Logic: 与原 Workbench.tsx 行为一致——activeSessionId 变化时通过 focus API 同步后端 tmux current
  // window；失败时通过项目域 controller 标记离线，并展示 sessionError。
  useEffect(() => {
    if (!activeSessionId) return undefined;
    let cancelled = false;
    void workbenchApi.sessions.focus(activeSessionId).catch((error) => {
      if (cancelled) return;
      const projectId = activeProjectIdRef.current;
      if (projectId) markRequestFailure(projectId, error);
      setSessionError(displayErrorMessage(error, t('focusSession')));
    });
    return () => {
      cancelled = true;
    };
  }, [activeSessionId, displayErrorMessage, markRequestFailure, t]);

  // Business Logic: 与原 Workbench.tsx 行为一致——每 TMUX_FOCUS_SYNC_INTERVAL_MS 轮询后端 get_focused_workbench_session，
  // 把外部（如另一台设备/移动端）的焦点变化同步到当前 worktree 的 active session。
  // 最近的本地 focus 操作在 LOCAL_FOCUS_GRACE_MS 内抑制轮询，避免与用户刚点击的 tab 冲突。
  useEffect(() => {
    if (!activeProjectId || !activeWorktreeId || scopedSessions.length === 0) {
      return undefined;
    }
    let cancelled = false;

    const syncFocusedSession = () => {
      if (Date.now() - lastLocalFocusAtRef.current < LOCAL_FOCUS_GRACE_MS) return;
      void workbenchApi.sessions
        .focused(activeProjectId, activeWorktreeId)
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
  }, [activeProjectId, activeWorktreeId, scopedSessions]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   切换项目或刷新时需要重新拉取当前项目的所有 terminal window。
   *
   * Code Logic（这个函数做什么）:
   *   1. 调用 sessions.list(projectId)，并通过 isCurrentProject 做 stale guard；
   *   2. 成功时更新 sessions / knownSessionIds，并 refreshProjectSessionStats / markRequestSuccess；
   *   3. 失败时 markRequestFailure + 展示 sessionError。
   *
   *   projectId 缺省时使用 activeProjectIdRef.current，便于 bridge.loadSessions() 无参调用。
   */
  const loadSessions = useCallback(
    async (projectId?: string): Promise<void> => {
      const resolvedProjectId = projectId ?? activeProjectIdRef.current;
      if (!resolvedProjectId) return;
      try {
        setSessionError(null);
        const list = await workbenchApi.sessions.list(resolvedProjectId);
        if (!isCurrentProject(resolvedProjectId)) return;
        markRequestSuccess(resolvedProjectId);
        knownSessionIdsRef.current = new Set(list.map((session) => session.id));
        setSessions(list);
        updateActiveSession(list);
        void refreshProjectSessionStats(resolvedProjectId);
      } catch (error) {
        if (!isCurrentProject(resolvedProjectId)) return;
        markRequestFailure(resolvedProjectId, error);
        setSessionError(displayErrorMessage(error, t('sessions')));
      }
    },
    [
      displayErrorMessage,
      isCurrentProject,
      markRequestFailure,
      markRequestSuccess,
      refreshProjectSessionStats,
      t,
      updateActiveSession,
    ],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   新建 worktree / 用户主动 new session 时，需要按当前布局估算初始 cols/rows 并创建 session。
   *
   * Code Logic（这个函数做什么）:
   *   1. 读取当前 project/worktree ref，估算初始尺寸；
   *   2. 调用 sessions.create；如果 create 后 project 已切换，则丢弃响应；
   *   3. 成功时 append session、记录 known id、focus、reset buffer、refresh stats；
   *   4. 失败时 markRequestFailure + 展示 sessionError。
   *
   *   重要边界：worktreeId 是显式入参；不再用 activeWorktreeIdRef 做 stale guard。
   *   原因：useWorkbenchWorktreeGitController.handleCreateWorktree 的编排顺序是先 create worktree、
   *   再 createSessionForWorktree、最后 setActiveWorktreeId。在 bridge 被调用的时刻，新 worktree 还没
   *   成为 activeWorktreeId；若用 activeWorktreeIdRef !== worktreeId 守卫，会把 session 从 UI 静默丢弃
   *   （后端已创建）。这里只用 projectId 守卫跨项目 stale；同一项目内的 worktreeId 是显式 target，
   *   无论当前 active 是哪个都应接受 session append。loadWorktrees / setActiveWorktreeId 随后会把
   *   新 worktree 设为 active，scopedSessions 计算会自然包含新 session。
   */
  const createSessionForWorktree = useCallback(
    async (worktreeId: string): Promise<void> => {
      const projectId = activeProjectIdRef.current;
      if (!projectId) return;
      try {
        setSessionBusy(true);
        setSessionError(null);
        const initialSize = measureInitialTerminalSize?.(terminalPanelRef.current, 'single');
        const session = await workbenchApi.sessions.create(projectId, initialSize, worktreeId);
        // Business Logic: 仅跨项目切换时丢弃；worktreeId 是显式 target，不与 activeWorktreeIdRef 比较。
        if (activeProjectIdRef.current !== projectId) {
          return;
        }
        setSessions((current) => [...current, session]);
        knownSessionIdsRef.current.add(session.id);
        await focusSession(session.id);
        resetTerminalBuffer(session.id);
        void refreshProjectSessionStats(projectId);
      } catch (error) {
        if (activeProjectIdRef.current !== projectId) {
          return;
        }
        markRequestFailure(projectId, error);
        setSessionError(displayErrorMessage(error, t('createSession')));
      } finally {
        setSessionBusy(false);
      }
    },
    [
      displayErrorMessage,
      focusSession,
      markRequestFailure,
      measureInitialTerminalSize,
      refreshProjectSessionStats,
      resetTerminalBuffer,
      t,
      terminalPanelRef,
    ],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户主动点击 "新建终端" 按钮；与 createSessionForWorktree 区别是它使用当前 active worktree id。
   */
  const handleCreateSession = useCallback(async (): Promise<void> => {
    const worktreeId = activeWorktreeIdRef.current;
    if (!worktreeId) return;
    await createSessionForWorktree(worktreeId);
  }, [createSessionForWorktree]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户在当前 terminal window 内 split 出新 pane（左右/上下）。
   *
   * Code Logic（这个函数做什么）:
   *   1. 仅当存在 active session 且 remoteWriteDisabled=false 时执行；
   *   2. 调用 sessions.splitPane，再 loadSessions 刷新 paneCount；
   *   3. 失败时 markRequestFailure + 展示 sessionError。
   */
  const handleSplitPane = useCallback(
    async (direction: WorkbenchPaneSplitDirection): Promise<void> => {
      if (!activeSession) return;
      if (remoteWriteDisabled) return;
      try {
        setSessionError(null);
        await workbenchApi.sessions.splitPane(activeSession.id, direction);
        await loadSessions(activeSession.projectId);
      } catch (error) {
        markRequestFailure(activeSession.projectId, error);
        setSessionError(displayErrorMessage(error, t('splitPane')));
      }
    },
    [activeSession, displayErrorMessage, loadSessions, markRequestFailure, remoteWriteDisabled, t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   桌面端用户切换到当前 tmux window 的下一个 pane（与移动端一致的入口）。
   */
  const handleSwitchPane = useCallback(async (): Promise<void> => {
    if (!activeSession) return;
    if (remoteWriteDisabled) return;
    try {
      await workbenchApi.sessions.switchPane(activeSession.id);
    } catch (error) {
      markRequestFailure(activeSession.projectId, error);
      setSessionError(displayErrorMessage(error, t('switchPane')));
    }
  }, [activeSession, displayErrorMessage, markRequestFailure, remoteWriteDisabled, t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   桌面端用户希望以单 pane 视图查看当前 tmux active pane（与移动端一致的入口）。
   */
  const handleZoomPane = useCallback(async (): Promise<void> => {
    if (!activeSession) return;
    if (remoteWriteDisabled) return;
    try {
      await workbenchApi.sessions.zoomPane(activeSession.id);
    } catch (error) {
      markRequestFailure(activeSession.projectId, error);
      setSessionError(displayErrorMessage(error, t('zoomPane')));
    }
  }, [activeSession, displayErrorMessage, markRequestFailure, remoteWriteDisabled, t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户关闭当前 tmux pane；最后一个 pane 关闭时后端返回 closedWindow=true，此时整窗 session 也要清理。
   *
   * Code Logic（这个函数做什么）:
   *   1. 调用 sessions.closePane；返回 closedWindow=true 时从 state 移除 session 并 removeBuffer；
   *   2. 随后 loadSessions 刷新 pane 元数据；失败时 markRequestFailure + 展示 sessionError。
   */
  const handleClosePane = useCallback(async (): Promise<void> => {
    if (!activeSession) return;
    if (remoteWriteDisabled) return;
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
      markRequestFailure(activeSession.projectId, error);
      setSessionError(displayErrorMessage(error, t('closePane')));
    }
  }, [
    activeSession,
    displayErrorMessage,
    loadSessions,
    markRequestFailure,
    removeTerminalBuffer,
    remoteWriteDisabled,
    t,
    updateActiveSession,
  ]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户在 terminal tab 上点击关闭按钮；后端关闭整窗 PTY 资源。
   *
   * Code Logic（这个函数做什么）:
   *   1. remoteWriteDisabled 时静默拒绝；
   *   2. 调用 sessions.close，从 state 移除 session，updateActiveSession 重算焦点；
   *   3. 同步 knownSessionIdsRef，removeTerminalBuffer 清理 buffer，refreshProjectSessionStats。
   */
  const handleCloseSession = useCallback(
    async (sessionId: string): Promise<void> => {
      if (remoteWriteDisabled) return;
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
        const projectId = activeProjectIdRef.current;
        if (projectId) markRequestFailure(projectId, error);
        setSessionError(displayErrorMessage(error, t('closeSession')));
      }
    },
    [
      displayErrorMessage,
      markRequestFailure,
      refreshProjectSessionStats,
      removeTerminalBuffer,
      remoteWriteDisabled,
      t,
      updateActiveSession,
    ],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户在 inspector rename 输入框里改名后提交。
   *
   * Code Logic（这个函数做什么）:
   *   1. 仅当 active session 存在、draft 非空、remoteWriteDisabled=false 时执行；
   *   2. 调用 sessions.rename，把返回的 session 替换到 state。
   */
  const handleRenameSession = useCallback(async (): Promise<void> => {
    if (!activeSession || !sessionNameDraft.trim()) return;
    if (remoteWriteDisabled) return;
    try {
      setSessionError(null);
      const renamed = await workbenchApi.sessions.rename(
        activeSession.id,
        sessionNameDraft.trim(),
      );
      setSessions((current) =>
        current.map((session) => (session.id === renamed.id ? renamed : session)),
      );
    } catch (error) {
      markRequestFailure(activeSession.projectId, error);
      setSessionError(displayErrorMessage(error, t('renameSession')));
    }
  }, [
    activeSession,
    displayErrorMessage,
    markRequestFailure,
    remoteWriteDisabled,
    sessionNameDraft,
    t,
  ]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   xterm onData 把用户输入转发到后端 PTY；remoteWriteDisabled 时静默拒绝，避免必然失败的远端写。
   */
  const handleInput = useCallback(
    async (sessionId: string, data: string): Promise<void> => {
      if (remoteWriteDisabled) return;
      try {
        await workbenchApi.sessions.writeInput(sessionId, data);
      } catch {
        // 输入写入失败时通常是会话刚退出；状态事件或下一次操作会反映错误。
      }
    },
    [remoteWriteDisabled],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   xterm ResizeObserver 把 cols/rows 同步到后端 PTY；高频触发，失败不阻断终端显示。
   *
   * Code Logic（这个函数做什么）:
   *   按 MIN_TERMINAL_COLS / MIN_TERMINAL_ROWS clamp 后调用 sessions.resize。
   */
  const handleResize = useCallback(
    async (sessionId: string, cols: number, rows: number): Promise<void> => {
      try {
        await workbenchApi.sessions.resize(
          sessionId,
          clampU16(cols, MIN_TERMINAL_COLS),
          clampU16(rows, MIN_TERMINAL_ROWS),
        );
      } catch {
        // 容器 resize 高频触发，失败不阻断终端显示。
      }
    },
    [],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   移动端使用同一终端后可能把共享 tmux/PTY 尺寸改成手机视口，桌面用户需要一键恢复为当前 PC 终端尺寸。
   *
   * Code Logic（这个函数做什么）:
   *   仅当 canRefreshCurrentTerminalSize=true 时递增 resize request key；由当前可见 TerminalPane 使用自身
   *   xterm 实例重新 fit 并复用现有后端 resize。
   */
  const handleRefreshTerminalSize = useCallback((): void => {
    if (!canRefreshCurrentTerminalSize) return;
    setTerminalResizeRequestKey((current) => current + 1);
  }, [canRefreshCurrentTerminalSize]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   桌面端用户需要把当前终端临时铺满屏幕，隐藏项目标题、worktree 管理层、文件层和右侧检查器以专注操作。
   *
   * Code Logic（这个函数做什么）:
   *   打开 terminalLayer 的 fixed overlay 状态；不重置当前 session/worktree。
   *
   *   注意：setWorkspaceView / setPromptPanelOpen 由页面层负责（不属于终端域）；controller 只切换 fullscreen 标记。
   */
  const handleEnterTerminalFullscreen = useCallback((): void => {
    setTerminalFullscreen(true);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   进入终端全屏后必须有明确出口，恢复完整 Workbench 布局和其他面板内容。
   */
  const handleExitTerminalFullscreen = useCallback((): void => {
    setTerminalFullscreen(false);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   merge / remove worktree 等流程完成后需要清理对应 worktree 下所有 session 的终端 buffer；
   *   controller 通过外部 removeBuffer 回调完成清理，自己不持有字节内容。
   *
   * Code Logic（这个函数做什么）:
   *   按 sessionsForWorktree(sessions, worktreeId) 找到该 worktree 下的全部 session id，逐个 removeTerminalBuffer。
   */
  const clearBuffersForWorktree = useCallback(
    (worktreeId: string): void => {
      sessionsForWorktree(sessions, worktreeId).forEach((session) => {
        removeTerminalBuffer(session.id);
      });
    },
    [removeTerminalBuffer, sessions],
  );

  // Business Logic: 与原 Workbench.tsx 行为一致——scopedSessions 变化时（如 worktree 切换、close session）
  // 通过 deferEffect 重算 active session id，避免在 effect 主体同步 setState 触发级联。
  useEffect(() => {
    const timer = window.setTimeout(() => {
      setActiveSessionId((current) => {
        if (current && scopedSessions.some((session) => session.id === current)) return current;
        return scopedSessions[0]?.id ?? null;
      });
    }, 0);
    return () => window.clearTimeout(timer);
  }, [scopedSessions]);

  // Business Logic: 与原 Workbench.tsx 行为一致——active session 名字变化时把 draft 同步成最新名字，
  // 用 deferEffect 与原 effect 顺序保持一致。
  useEffect(() => {
    const timer = window.setTimeout(() => {
      setSessionNameDraftState(activeSession?.name ?? '');
    }, 0);
    return () => window.clearTimeout(timer);
  }, [activeSession?.name]);

  // Business Logic: 与原 Workbench.tsx 行为一致——监听后端 terminal-status 事件，按 sessionId 过滤并更新
  // 对应 session 的 status / exitCode / exitedAt；未知 sessionId 静默忽略。
  // 非 Tauri 环境（普通浏览器调试）跳过 listen 注册，避免底层 invoke 报错。
  useEffect(() => {
    const canListen = canListenToTauriEventsRef.current ?? canListenToTauriEventsDefault;
    if (!canListen()) return undefined;
    let registered = true;
    let unlistenFn: (() => void) | null = null;
    void listen<WorkbenchTerminalStatusEvent>(
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
    ).then((fn) => {
      if (!registered) {
        // 注册期间组件已卸载；立即释放。
        fn();
        return;
      }
      unlistenFn = fn;
    });
    return () => {
      registered = false;
      unlistenFn?.();
    };
  }, []);

  /**
   * Business Logic（为什么需要这个 setter）:
   *   页面在用户编辑 rename 输入框时同步 draft；controller 暴露 functional setter 以兼容现有 onChange 写法。
   */
  const setSessionNameDraft = useCallback(
    (next: string | ((prev: string) => string)): void => {
      setSessionNameDraftState((prev) =>
        typeof next === 'function' ? (next as (prev: string) => string)(prev) : next,
      );
    },
    [],
  );

  return {
    // 渲染数据
    sessions,
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
    // 派生 actions
    setSessionNameDraft,
    setSessionError,
    setActiveSessionId,
    setSessions,
    updateActiveSession,
    handleCreateSession,
    handleSplitPane,
    handleSwitchPane,
    handleZoomPane,
    handleClosePane,
    handleCloseSession,
    handleRenameSession,
    handleInput,
    handleResize,
    handleRefreshTerminalSize,
    handleEnterTerminalFullscreen,
    handleExitTerminalFullscreen,
    // bridge 视图
    loadSessions,
    focusSession,
    createSessionForWorktree,
    clearBuffersForWorktree,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   终端 resize 命令后端接受 u16，前端需要提前 clamp，避免极端布局值反序列化失败。
 *
 * Code Logic（这个函数做什么）:
 *   取整数并限制在 min..65535 区间。
 */
function clampU16(value: number, min: number): number {
  const rounded = Math.max(min, Math.round(value));
  return Math.min(65535, rounded);
}

interface TauriInternalsWindow extends Window {
  __TAURI_INTERNALS__?: {
    transformCallback?: unknown;
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   普通 Vite/Playwright 浏览器环境没有 Tauri event internals，直接 listen 会导致调试白屏或底层 invoke 报错。
 *   与原 Workbench.tsx 内联实现保持一致，作为 controller 的默认实现。
 *
 * Code Logic（这个函数做什么）:
 *   检测 window.__TAURI_INTERNALS__.transformCallback 是否存在且为函数。
 */
function canListenToTauriEventsDefault(): boolean {
  if (typeof window === 'undefined') return false;
  const internals = (window as TauriInternalsWindow).__TAURI_INTERNALS__;
  return typeof internals?.transformCallback === 'function';
}
