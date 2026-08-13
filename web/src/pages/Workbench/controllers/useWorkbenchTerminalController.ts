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
import type {
  WorkbenchTerminalStatusEvent,
  WorkbenchSession,
  WorkbenchSessionUpdatedEvent,
} from '@/lib/types';
import { workbenchApi } from '@/api/workbench';
import type { WorkbenchPaneSplitDirection } from '@/api/workbench';
import { createTerminalInputPump } from '../terminalInputPump';
import type { TerminalInputPump } from '../terminalInputPump';
import { mountedTerminalSessions, visibleTerminalSessions } from '../terminalSessionOrder';
import { canRefreshTerminalSize } from '../terminalSizing';
import type { TerminalLayoutMode } from '../terminalSizing';
import { sessionsForWorktree } from '../workbenchWorktrees';
import { isLatestRequest } from '../workbenchFiles';
import {
  ackCompletedForTerminal,
  getWorkbenchAgentHintStore,
  type AgentHintSessionIndexEntry,
} from '@/hooks/workbenchAgentHintStore';
function indexSessionsForHints(sessions: WorkbenchSession[]): void {
  const store = getWorkbenchAgentHintStore();
  for (const session of sessions) {
    const entry: AgentHintSessionIndexEntry = {
      sessionId: session.id,
      projectId: session.projectId,
      worktreeId: session.worktreeId,
    };
    store.upsertSessionIndex(entry);
  }
}

interface WorkbenchTerminalInputStateEvent {
  sessionId: string;
  status: 'blocked';
  code: string;
  message: string;
}

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
  | 'renameSession'
  | 'writeSession';

/**
 * controller 暴露给页面 deep link / 项目切换等流程的窄接口。
 *
 * Business Logic: Workbench.tsx 在项目切换 / worktree 创建 / merge / deep link 流程里只需要这四个动作；
 * 把它们封装成 bridge 类型，避免页面散落调用更宽的 controller API。
 */
export interface WorkbenchTerminalBridge {
  loadSessions: (projectId?: string) => Promise<void>;
  focusSession: (sessionId: string) => Promise<boolean>;
  /**
   * 为指定 worktree 创建并注册 session。
   *
   * Business Logic: 仅当目标 worktree 仍是当前 active worktree 时才 focus；这避免「在 A 上发起创建、
   * 中途切到 B」时 A 的 session 抢占 B 的焦点（Codex 二次评审 Finding 4）。返回创建出的 session id
   * （或 null），让编排方（handleCreateWorktree）在显式 setActiveWorktreeId 之后再 focus。
   */
  createSessionForWorktree: (worktreeId: string) => Promise<string | null>;
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
  /** 多 pane 时可循环切换到下一个 tmux pane。 */
  canSwitchPane: boolean;
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
  /**
   * Business Logic（为什么需要这个回调）:
   *   多 pane 终端的字符格点击交由后端按 tmux 真值布局做命中；changed=true 时刷新 sessions。
   */
  handleSelectPaneAt: (sessionId: string, col: number, row: number) => Promise<void>;
  handleClosePane: () => Promise<void>;
  handleCloseSession: (sessionId: string) => Promise<void>;
  renameSessionById: (sessionId: string, name: string) => Promise<boolean>;
  handleRenameSession: () => Promise<void>;
  handleInput: (sessionId: string, data: string) => Promise<void>;
  /**
   * Business Logic（为什么需要这个函数）:
   *   write 失败后 pump lane 永久 blocked，若 xterm 仍启用输入会变成静默键盘黑洞；
   *   视图需按 session 禁用 input，直到 read-only status check 或 loadSessions 成功 recover。
   *
   * Code Logic（这个函数做什么）:
   *   查询 writeBlockedSessionIds 是否包含该 sessionId。
   */
  isWriteBlocked: (sessionId: string) => boolean;
  /**
   * Business Logic（为什么需要这个函数）:
   *   sessionError 的 StatusMessage 仅在仍有 write-blocked session 时展示「重新检查」动作，
   *   避免把 load/create/focus 等非 write 错误也伪装成可自动解锁。
   *
   * Code Logic（这个函数做什么）:
   *   返回 writeBlockedSessionIds 是否非空。
   */
  hasWriteBlockedSessions: boolean;
  /**
   * Business Logic（为什么需要这个函数）:
   *   自动只读 status check 失败时，用户需显式重试解锁，而不必切项目或等下一次 loadSessions。
   *
   * Code Logic（这个函数做什么）:
   *   对当前所有 write-blocked session 再跑一次 read-only list 恢复；不写输入、不重放失败批次。
   */
  retryWriteBlockRecovery: () => Promise<void>;
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
  // Business Logic: write 失败后 lane.blocked 对 React 不可见；用 Set 跟踪 blocked session，
  // 供 UI 禁用 xterm 输入，直到 read-only status check 或 loadSessions 成功 recoverSession。
  const [writeBlockedSessionIds, setWriteBlockedSessionIds] = useState<ReadonlySet<string>>(
    () => new Set(),
  );

  // Business Logic: 异步加载回调返回时，active project / worktree 可能已经切换；用 ref 读取最新 id 做 stale guard。
  const activeProjectIdRef = useRef<string | null>(activeProjectId);
  const activeWorktreeIdRef = useRef<string | null>(activeWorktreeId);
  // Business Logic: 用 knownSessionIdsRef 维护当前已知 session id 集合，与原 Workbench.tsx 行为保持一致。
  const knownSessionIdsRef = useRef<Set<string>>(new Set());
  // Business Logic: 用 lastLocalFocusAtRef 抑制本地 focus 操作后 500ms 内的 tmux focus 轮询，避免把焦点抢回。
  const lastLocalFocusAtRef = useRef<number>(0);
  // Business Logic: 用 localFocusPendingRef 标记本地 focusSession 已发出但后端 focus IPC 尚未确认成功；
  // 在此期间轮询不得用后端 tmux current 覆盖本地选择（IPC 飞行/失败时后端 tmux 可能仍是旧 window）。
  const localFocusPendingRef = useRef<string | null>(null);
  // Business Logic: 同一 project 的 session list 可能并发；用单调 request seq 丢弃过期 list，避免 create/close/split 后被慢响应回写旧列表。
  const sessionListRequestSeqRef = useRef<Record<string, number>>({});
  // Business Logic: terminal-status listen 只想在 mount 时注册一次；用 ref 读取最新的 canListenToTauriEvents，
  // 避免把它放进 effect 依赖导致 listener 反复重注册（与原 Workbench.tsx 的 [] 依赖行为一致）。
  const canListenToTauriEventsRef = useRef(canListenToTauriEventsParam);
  // Business Logic: 每 session 有序输入泵挂在 controller 生命周期上，避免每键并发 writeInput。
  // StrictMode 会 setup→cleanup→setup；cleanup dispose 后必须重建，否则 ref 仍指向 disposed 泵。
  const terminalInputPumpRef = useRef<TerminalInputPump | null>(null);
  // Business Logic: write 失败上报回调经 ref 注入，泵 effect 保持 [] 依赖，避免 StrictMode/依赖抖动。
  const reportWriteErrorRef = useRef<(sessionId: string, error: unknown) => void>(() => {});
  // Business Logic: onWriteError / loadSessions / dispose 路径都要读写 blocked 集合；ref 保证异步回调读到最新 Set。
  const writeBlockedSessionIdsRef = useRef<ReadonlySet<string>>(writeBlockedSessionIds);
  // Business Logic: 同一 session 的 write-block status check 合并为单次 in-flight，避免快速失败刷爆 list。
  const writeBlockRecoveryInflightRef = useRef<Map<string, Promise<void>>>(new Map());
  // Business Logic: isCurrentProject 经 ref 读取，避免 status-check 回调闭包绑定过期函数。
  const isCurrentProjectRef = useRef(isCurrentProject);
  useEffect(() => {
    canListenToTauriEventsRef.current = canListenToTauriEventsParam;
  });

  useEffect(() => {
    activeProjectIdRef.current = activeProjectId;
  }, [activeProjectId]);

  useEffect(() => {
    activeWorktreeIdRef.current = activeWorktreeId;
  }, [activeWorktreeId]);

  useEffect(() => {
    writeBlockedSessionIdsRef.current = writeBlockedSessionIds;
  }, [writeBlockedSessionIds]);

  useEffect(() => {
    isCurrentProjectRef.current = isCurrentProject;
  }, [isCurrentProject]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   session 消失（close / list 替换 / worktree clear）时，对应 write-blocked 跟踪必须同步移除，
   *   否则 UI 可能对已不存在的 id 保留陈旧封锁标记。
   *
   * Code Logic（这个函数做什么）:
   *   若 Set 含 sessionId 则删掉并 setState；无变化时保持同一 Set 引用。
   */
  const untrackWriteBlocked = useCallback((sessionId: string): void => {
    if (!writeBlockedSessionIdsRef.current.has(sessionId)) return;
    const next = new Set(writeBlockedSessionIdsRef.current);
    next.delete(sessionId);
    writeBlockedSessionIdsRef.current = next;
    setWriteBlockedSessionIds(next);
  }, []);

  // Business Logic: GUI Rust 的输入 WS 在 invoke admission 后异步失败时，立即封锁对应前端 lane；
  // 不等下一次按键才发现断线，也绝不重放 sent-unacked 输入。
  useEffect(() => {
    const canListen = canListenToTauriEventsRef.current ?? canListenToTauriEventsDefault;
    if (!canListen()) return undefined;
    let registered = true;
    let unlistenFn: (() => void) | null = null;
    void listen<WorkbenchTerminalInputStateEvent>(
      'workbench:terminal-input-state',
      (event) => {
        const payload = event.payload;
        if (!knownSessionIdsRef.current.has(payload.sessionId)) return;
        terminalInputPumpRef.current?.blockSession(payload.sessionId);
        reportWriteErrorRef.current(payload.sessionId, new Error(payload.message));
      },
    ).then((fn) => {
      if (!registered) {
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
   * Business Logic（为什么需要这个函数）:
   *   sessions.list 权威刷新成功后，只应对仍真正可写的 session 解除写失败封锁，让用户可再次输入；
   *   exited/disconnected 仍在列表中的 session 不得解锁；绝不能自动重放失败批次。
   *
   * Code Logic（这个函数做什么）:
   *   对当前 blocked ids：仅当权威 list 中对应 entry 的 status 严格为 `'running'` 时
   *   调用 pump.recoverSession 并从 React blocked Set 移除；其余保持 blocked。
   *   列表中已消失的 id 由 loadSessions 既有 dispose+untrack 处理，本函数不重复 untrack。
   */
  const recoverAllWriteBlockedSessions = useCallback((list: WorkbenchSession[]): void => {
    const blocked = writeBlockedSessionIdsRef.current;
    if (blocked.size === 0) return;
    const runningIds = new Set(
      list.filter((session) => session.status === 'running').map((session) => session.id),
    );
    const next = new Set(blocked);
    let changed = false;
    for (const sessionId of blocked) {
      if (!runningIds.has(sessionId)) continue;
      terminalInputPumpRef.current?.recoverSession(sessionId);
      next.delete(sessionId);
      changed = true;
    }
    if (!changed) return;
    writeBlockedSessionIdsRef.current = next;
    setWriteBlockedSessionIds(next);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   瞬时 control/write 故障会把 lane 永久 blocked 且 `inputEnabled=false`；生产路径不会主动
   *   loadSessions，用户又不会为此切项目，因此 write fail 后必须 kick 一次只读 status check。
   *   仅当权威 list 显示 session 仍严格 `running` 时才能解锁；exited/disconnected 仍在列表中
   *   不得解锁，否则用户会对已退出终端误以为可输入。绝不能自动重放失败输入批次。
   *
   * Code Logic（这个函数做什么）:
   *   1. 同一 session 并发 check 合并为 in-flight Promise；
   *   2. 读取 activeProjectId，调用只读 sessions.list（不得 writeInput / 重放）；
   *   3. stale guard：project 仍 active、session 仍 blocked；
   *   4. 在 list 中按 id 查找 entry；若找到则把 status/exitCode/exitedAt 同步进 React sessions；
   *   5. 仅当 found.status === 'running'：recoverSession + untrackWriteBlocked；
   *      blocked 集合空时清 sessionError；
   *   6. 未找到或 status !== 'running'：保持 blocked + sessionError，不 recover。
   */
  const checkAndRecoverWriteBlock = useCallback(async (sessionId: string): Promise<void> => {
    const inflight = writeBlockRecoveryInflightRef.current.get(sessionId);
    if (inflight) {
      await inflight;
      return;
    }

    const run = (async (): Promise<void> => {
      if (!writeBlockedSessionIdsRef.current.has(sessionId)) return;
      const projectId = activeProjectIdRef.current;
      if (!projectId) return;
      try {
        const list = await workbenchApi.sessions.list(projectId);
        if (!isCurrentProjectRef.current(projectId)) return;
        if (!writeBlockedSessionIdsRef.current.has(sessionId)) return;
        const found = list.find((session) => session.id === sessionId);
        if (!found) return;
        // Business Logic: status check 拿到权威元数据后先同步到 UI，让 exited 等状态立即可见，
        // 再决定是否 unlock；避免 React sessions 仍显示 running 而输入已错误解锁。
        setSessions((prev) => {
          const index = prev.findIndex((session) => session.id === sessionId);
          if (index < 0) return prev;
          const current = prev[index];
          if (
            current.status === found.status &&
            current.exitCode === found.exitCode &&
            current.exitedAt === found.exitedAt
          ) {
            return prev;
          }
          const next = prev.slice();
          next[index] = {
            ...current,
            status: found.status,
            exitCode: found.exitCode,
            exitedAt: found.exitedAt,
          };
          return next;
        });
        if (found.status !== 'running') return;
        terminalInputPumpRef.current?.recoverSession(sessionId);
        untrackWriteBlocked(sessionId);
        if (writeBlockedSessionIdsRef.current.size === 0) {
          setSessionError(null);
        }
      } catch {
        // 只读 status check 失败时保持 blocked + sessionError；用户可手动 retry 或后续 loadSessions。
      }
    })();

    writeBlockRecoveryInflightRef.current.set(sessionId, run);
    try {
      await run;
    } finally {
      if (writeBlockRecoveryInflightRef.current.get(sessionId) === run) {
        writeBlockRecoveryInflightRef.current.delete(sessionId);
      }
    }
  }, [untrackWriteBlocked]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   StatusMessage 需要显式「重新检查」动作；自动 status check 失败后用户可重试，
   *   而不必切换项目或依赖其它路径的 loadSessions。
   *
   * Code Logic（这个函数做什么）:
   *   对当前 writeBlockedSessionIds 中的每个 session 再跑 checkAndRecoverWriteBlock；
   *   空集合时 no-op；不写输入、不重放失败批次。
   */
  const retryWriteBlockRecovery = useCallback(async (): Promise<void> => {
    const blocked = [...writeBlockedSessionIdsRef.current];
    if (blocked.length === 0) return;
    await Promise.all(blocked.map((sessionId) => checkAndRecoverWriteBlock(sessionId)));
  }, [checkAndRecoverWriteBlock]);

  // Business Logic: sessions 变化时同步 knownSessionIdsRef；对已消失的 session 丢弃输入 pending，
  // 覆盖 close / loadSessions 项目切换 / 列表替换，但不触碰仍存活的 session。
  useEffect(() => {
    const nextIds = new Set(sessions.map((session) => session.id));
    for (const previousId of knownSessionIdsRef.current) {
      if (!nextIds.has(previousId)) {
        terminalInputPumpRef.current?.disposeSession(previousId);
        untrackWriteBlocked(previousId);
      }
    }
    knownSessionIdsRef.current = nextIds;
  }, [sessions, untrackWriteBlocked]);

  // Business Logic: mount 时创建输入泵；unmount/StrictMode cleanup 时 dispose 并清空 ref，
  // 下次 setup 重建，避免 disposed 泵永久吞掉全部 enqueue。
  useEffect(() => {
    const pump = createTerminalInputPump({
      write: (sessionId, data) => workbenchApi.sessions.enqueueInput(sessionId, data),
      onWriteError: (sessionId, error) => reportWriteErrorRef.current(sessionId, error),
    });
    terminalInputPumpRef.current = pump;
    return () => {
      pump.dispose();
      if (terminalInputPumpRef.current === pump) {
        terminalInputPumpRef.current = null;
      }
    };
  }, []);

  // Business Logic: 远端进入 offline 后写路径已禁用；丢弃当前 session 的 pending 输入，
  // 避免恢复在线后把离线期间积压字节误送；in-flight 请求仍由后端 settle，不重放。
  const prevRemoteWriteDisabledRef = useRef(remoteWriteDisabled);
  useEffect(() => {
    if (!prevRemoteWriteDisabledRef.current && remoteWriteDisabled) {
      for (const sessionId of knownSessionIdsRef.current) {
        terminalInputPumpRef.current?.disposeSession(sessionId);
      }
      if (writeBlockedSessionIdsRef.current.size > 0) {
        const empty = new Set<string>();
        writeBlockedSessionIdsRef.current = empty;
        setWriteBlockedSessionIds(empty);
      }
    }
    prevRemoteWriteDisabledRef.current = remoteWriteDisabled;
  }, [remoteWriteDisabled]);

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
  // Business Logic: tmux focus 轮询 effect 不再把 scopedSessions 作为依赖（避免 sessions 引用变化
  // 触发立即查后端、与用户刚点击的本地选择 race）；改为通过 ref 读取最新列表。
  const scopedSessionsRef = useRef(scopedSessions);
  useEffect(() => {
    scopedSessionsRef.current = scopedSessions;
  }, [scopedSessions]);
  const renderedActiveSessionId = activeSession?.id ?? visibleSessions[0]?.id ?? null;
  const canUsePanes = Boolean(
    activeSession?.supportsPanes && activeSession.status === 'running',
  );
  // Business Logic: 单 pane window 内循环切换无可见效果；与 mobile canSwitchMobilePane 一致。
  const canSwitchPane = canUsePanes && (activeSession?.paneCount ?? 0) > 1;
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

  // Business Logic: 输入泵 write 失败后封锁 lane，投影 sessionError，并记录 blocked session
  // 供 UI 禁用 xterm 输入（避免 enqueue 静默 no-op 变成键盘黑洞）；随后 kick 只读 status check，
  // 不重放失败批次，也不要求用户切换项目。
  useEffect(() => {
    reportWriteErrorRef.current = (sessionId, error) => {
      setSessionError(displayErrorMessage(error, t('writeSession')));
      // Business Logic: ref 必须在 kick status check 前同步标记 blocked；
      // setState updater 可能延后执行，异步 list 不能依赖尚未提交的 React state。
      if (!writeBlockedSessionIdsRef.current.has(sessionId)) {
        const next = new Set(writeBlockedSessionIdsRef.current);
        next.add(sessionId);
        writeBlockedSessionIdsRef.current = next;
        setWriteBlockedSessionIds(next);
      }
      void checkAndRecoverWriteBlock(sessionId);
    };
  }, [checkAndRecoverWriteBlock, displayErrorMessage, t]);

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
    // 标记本地 focus pending：在后端 focus IPC 确认成功前，禁止轮询用后端 tmux current 覆盖本地选择。
    localFocusPendingRef.current = sessionId;
    setActiveSessionId(sessionId);
    ackCompletedForTerminal(sessionId);
    return true;
  }, []);

  // Business Logic: activeSessionId 变化时通过 focus API 同步后端 tmux current window；
  // 成功确认后清本地 focus pending，允许轮询恢复外部焦点同步；失败时标记离线并展示 sessionError，
  // pending 保留以持续保护本地用户选择（后端 tmux 仍可能是旧 window）。
  useEffect(() => {
    // disconnected/exited 只是持久化历史 tab，不存在可 focus/replay 的运行时。
    if (!activeSessionId || activeSession?.status !== 'running') return undefined;
    const focusedSessionId = activeSessionId;
    let cancelled = false;
    void workbenchApi.sessions
      .focus(focusedSessionId)
      .then(() => {
        if (cancelled) return;
        // focus 成功确认：清 pending，后续轮询可正常同步外部焦点变化（移动端/tmux status bar 切换）。
        if (localFocusPendingRef.current === activeSessionId) {
          localFocusPendingRef.current = null;
        }
      })
      .catch((error) => {
        if (cancelled) return;
        const projectId = activeProjectIdRef.current;
        if (projectId) markRequestFailure(projectId, error);
        setSessionError(displayErrorMessage(error, t('focusSession')));
      });
    return () => {
      cancelled = true;
      // compare-and-clear：迟到 cleanup 不会清掉随后已聚焦的新窗口。
      void workbenchApi.sessions.focus(focusedSessionId, false).catch(() => {
        // cleanup 是带宽收敛的 best-effort；后端 idle/下一次 focus 仍会纠正目标。
      });
    };
  }, [activeSession?.status, activeSessionId, displayErrorMessage, markRequestFailure, t]);

  // Business Logic: 每 TMUX_FOCUS_SYNC_INTERVAL_MS 轮询后端 get_focused_workbench_session，
  // 把外部（如另一台设备/移动端）的焦点变化同步到当前 worktree 的 active session。
  // 最近的本地 focus 操作在 LOCAL_FOCUS_GRACE_MS 内抑制轮询，避免与用户刚点击的 tab 冲突。
  //
  // 关键：effect 依赖只含 [activeProjectId, activeWorktreeId]，**不**含 scopedSessions。
  // 否则 terminal-status 事件（setSessions(.map) 产生新数组引用）会让 effect 反复 cleanup+setup，
  // 每次都立即查 focused()；若此时本地刚 focusSession 而后端 tmux select-window 尚未生效，
  // 旧的后端 current window 会把用户刚选中的 window 覆盖回去（"切回第一个"race）。
  // scopedSessions 改为通过 ref 在 syncFocusedSession 内读取最新值。
  useEffect(() => {
    if (!activeProjectId || !activeWorktreeId) {
      return undefined;
    }
    let cancelled = false;

    const syncFocusedSession = () => {
      if (Date.now() - lastLocalFocusAtRef.current < LOCAL_FOCUS_GRACE_MS) return;
      // focus 飞行保护：本地刚 focusSession 但后端 focus IPC 尚未确认成功时，
      // 禁止轮询用后端 tmux current 覆盖本地选择（IPC 飞行/失败时后端可能仍是旧 window）。
      if (localFocusPendingRef.current !== null) return;
      const currentScoped = scopedSessionsRef.current;
      if (currentScoped.length === 0) return;
      void workbenchApi.sessions
        .focused(activeProjectId, activeWorktreeId)
        .then(({ sessionId }) => {
          if (cancelled || !sessionId) return;
          if (!currentScoped.some((session) => session.id === sessionId)) return;
          setActiveSessionId((current) => {
            if (current === sessionId) return current;
            ackCompletedForTerminal(sessionId);
            return sessionId;
          });
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
  }, [activeProjectId, activeWorktreeId]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   create/close/split/rename 等 mutation 成功后，任何仍在飞行中的旧 list 响应都不能再写回 UI。
   *
   * Code Logic（这个函数做什么）:
   *   递增指定 project 的 session list request seq，使先前 loadSessions 的 isLatest 判断失败。
   */
  const invalidateSessionListRequests = useCallback((projectId: string): void => {
    const current = sessionListRequestSeqRef.current[projectId] ?? 0;
    sessionListRequestSeqRef.current[projectId] = current + 1;
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   切换项目或刷新时需要重新拉取当前项目的所有 terminal window。
   *
   * Code Logic（这个函数做什么）:
   *   1. 为 project 递增单调 request seq 后调用 sessions.list；
   *   2. 仅当 project 仍 active 且 request 仍是最新时写入 sessions / knownSessionIds / success；
   *   3. 失败时同样校验 seq，再 markRequestFailure + 展示 sessionError。
   *   projectId 缺省时使用 activeProjectIdRef.current，便于 bridge.loadSessions() 无参调用。
   */
  const loadSessions = useCallback(
    async (projectId?: string): Promise<void> => {
      const resolvedProjectId = projectId ?? activeProjectIdRef.current;
      if (!resolvedProjectId) return;
      const requestSeq = (sessionListRequestSeqRef.current[resolvedProjectId] ?? 0) + 1;
      sessionListRequestSeqRef.current[resolvedProjectId] = requestSeq;
      try {
        setSessionError(null);
        const list = await workbenchApi.sessions.list(resolvedProjectId);
        if (
          !isCurrentProject(resolvedProjectId) ||
          !isLatestRequest(sessionListRequestSeqRef.current[resolvedProjectId], requestSeq)
        ) {
          return;
        }
        markRequestSuccess(resolvedProjectId);
        // Business Logic: 项目切换 / 列表替换时，先对消失 session 丢弃 pending 输入，
        // 再更新 known ids；不能只写 Set，否则 sessions effect 读不到旧 id。
        const nextIds = new Set(list.map((session) => session.id));
        for (const previousId of knownSessionIdsRef.current) {
          if (!nextIds.has(previousId)) {
            terminalInputPumpRef.current?.disposeSession(previousId);
            untrackWriteBlocked(previousId);
          }
        }
        knownSessionIdsRef.current = nextIds;
        // Business Logic: 权威 list 成功后仅对 status==='running' 的 blocked session 解锁；
        // exited/disconnected 仍在列表中保持 blocked；recoverSession 只开新 generation，
        // 绝不自动重放失败批次。sessionError 已在 try 开头清空。
        recoverAllWriteBlockedSessions(list);
        indexSessionsForHints(list);
        setSessions(list);
        updateActiveSession(list);
        void refreshProjectSessionStats(resolvedProjectId);
      } catch (error) {
        if (
          !isCurrentProject(resolvedProjectId) ||
          !isLatestRequest(sessionListRequestSeqRef.current[resolvedProjectId], requestSeq)
        ) {
          return;
        }
        markRequestFailure(resolvedProjectId, error);
        setSessionError(displayErrorMessage(error, t('sessions')));
      }
    },
    [
      displayErrorMessage,
      isCurrentProject,
      markRequestFailure,
      markRequestSuccess,
      recoverAllWriteBlockedSessions,
      refreshProjectSessionStats,
      t,
      untrackWriteBlocked,
      updateActiveSession,
    ],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   新建 worktree / 用户主动 new session 时，需要按当前布局估算初始 cols/rows 并创建 session。
   *
   * Code Logic（这个函数做什么）:
   *   1. 读取当前 project ref，估算初始尺寸；
   *   2. 调用 sessions.create；如果 create 后 project 已切换，则丢弃响应；
   *   3. 成功时 append session、记录 known id、reset buffer、refresh stats；
   *   4. 仅当目标 worktreeId 仍是当前 active worktree 时才 focus（防止 session 竞态抢占焦点）；
   *   5. 失败时 markRequestFailure + 展示 sessionError；
   *   6. 返回新创建的 session id（或 null），让编排方在显式 setActiveWorktreeId 后再 focus。
   *
   * 重要边界（Codex 二次评审 Finding 4 修复）：
   *   worktreeId 是显式入参，session 创建/注册不能因 activeWorktreeIdRef 不等于 worktreeId 而丢弃
   *   （后端已创建，丢弃会让 session 永久消失）。但 focus 必须区分两种调用场景：
   *     - 普通「新建终端」(handleCreateSession)：目标就是当前 active worktree，创建期间用户若切到别的
   *       worktree，则不应把刚创建的 session 抢成焦点。故用 activeWorktreeIdRef.current === worktreeId
   *       守卫 focus。
   *     - 新建 worktree 流程 (handleCreateWorktree)：调用时新 worktree 还不是 active，此守卫会跳过 focus；
   *       编排方在 setActiveWorktreeId(created.id) 之后再显式 focusSession(sessionId)，此时 active 已正确。
   */
  const createSessionForWorktree = useCallback(
    async (worktreeId: string): Promise<string | null> => {
      const projectId = activeProjectIdRef.current;
      if (!projectId) return null;
      try {
        setSessionBusy(true);
        setSessionError(null);
        const initialSize = measureInitialTerminalSize?.(terminalPanelRef.current, 'single');
        const session = await workbenchApi.sessions.create(projectId, initialSize, worktreeId);
        // Business Logic: 跨项目切换则丢弃响应（后端已创建，但 UI 不再属于当前 project）。
        if (activeProjectIdRef.current !== projectId) {
          return null;
        }
        // Business Logic: mutation 成功后立刻作废旧 list，防止慢速 list 覆盖刚创建的 session。
        invalidateSessionListRequests(projectId);
        setSessions((current) => [...current, session]);
        knownSessionIdsRef.current.add(session.id);
        resetTerminalBuffer(session.id);
        void refreshProjectSessionStats(projectId);
        // Business Logic: 仅当目标 worktree 仍是当前 active worktree 时才 focus。新建 worktree 流程下，
        // 此时 active 仍是旧 worktree（=== worktreeId 为 false），focus 交由编排方在激活后显式调用。
        if (activeWorktreeIdRef.current === worktreeId) {
          await focusSession(session.id);
        }
        return session.id;
      } catch (error) {
        if (activeProjectIdRef.current !== projectId) {
          return null;
        }
        markRequestFailure(projectId, error);
        setSessionError(displayErrorMessage(error, t('createSession')));
        return null;
      } finally {
        setSessionBusy(false);
      }
    },
    [
      displayErrorMessage,
      focusSession,
      invalidateSessionListRequests,
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
        invalidateSessionListRequests(activeSession.projectId);
        await loadSessions(activeSession.projectId);
      } catch (error) {
        markRequestFailure(activeSession.projectId, error);
        setSessionError(displayErrorMessage(error, t('splitPane')));
      }
    },
    [
      activeSession,
      displayErrorMessage,
      invalidateSessionListRequests,
      loadSessions,
      markRequestFailure,
      remoteWriteDisabled,
      t,
    ],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   桌面端用户切换到当前 tmux window 的下一个 pane（与移动端一致的入口与视觉:
   *   切换后立即 zoom,只显示当前 active pane,避免未 zoom 的 window 把所有 pane 并排渲染、
   *   active 边框逐个循环移动）。
   */
  const handleSwitchPane = useCallback(async (): Promise<void> => {
    if (!activeSession) return;
    if (remoteWriteDisabled) return;
    try {
      await workbenchApi.sessions.switchPane(activeSession.id);
    } catch (error) {
      markRequestFailure(activeSession.projectId, error);
      setSessionError(displayErrorMessage(error, t('switchPane')));
      return;
    }
    // 与移动端 ensurePaneZoomedById 一致:切换成功后立即把当前 active pane zoom 成单 pane 视图。
    // zoomPane 后端幂等（pane 数 <=1 或已 zoom 时 no-op）；失败单独报错，不影响已完成的切换。
    try {
      await workbenchApi.sessions.zoomPane(activeSession.id);
    } catch (error) {
      markRequestFailure(activeSession.projectId, error);
      setSessionError(displayErrorMessage(error, t('zoomPane')));
    }
  }, [activeSession, displayErrorMessage, markRequestFailure, remoteWriteDisabled, t]);

  /**
   * Business Logic: 多 pane 终端内点击目标 pane 时，前端只能提供字符格坐标，由后端按
   *   tmux 真实布局命中并 select-pane；绝对坐标可重放，与 `.+` 循环不同。
   *
   * Code Logic: 守卫 activeSession / 远端离线，写入 sessions.selectPaneAt；
   *   仅在 changed=true 时刷新 sessions，使 paneCount / 焦点反映新 active pane。
   */
  const handleSelectPaneAt = useCallback(
    async (sessionId: string, col: number, row: number): Promise<void> => {
      if (remoteWriteDisabled) return;
      const target = sessions.find((s) => s.id === sessionId) ?? activeSession;
      if (!target) return;
      try {
        const result = await workbenchApi.sessions.selectPaneAt(target.id, col, row);
        if (result.changed) {
          await loadSessions();
        }
      } catch (error) {
        markRequestFailure(target.projectId, error);
        setSessionError(displayErrorMessage(error, t('switchPane')));
      }
    },
    [activeSession, sessions, displayErrorMessage, loadSessions, markRequestFailure, remoteWriteDisabled, t],
  );

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
        terminalInputPumpRef.current?.disposeSession(result.sessionId);
        untrackWriteBlocked(result.sessionId);
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
    untrackWriteBlocked,
    updateActiveSession,
  ]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户在 terminal tab 上点击关闭按钮；后端关闭整窗 PTY 资源。
   *
   * Code Logic（这个函数做什么）:
   *   1. remoteWriteDisabled 时静默拒绝；
   *   2. 在 await 前捕获 sourceProjectId（发起关闭时的 active 项目）；
   *   3. 调用 sessions.close；成功后只作废 source 项目的 list seq；
   *   4. 若用户已切到其它项目，不改当前 sessions/buffer/stats/error（避免 A 关闭污染 B）；
   *   5. 仍在 source 项目时：从 state 移除 session、updateActiveSession、清 buffer、refresh stats。
   */
  const handleCloseSession = useCallback(
    async (sessionId: string): Promise<void> => {
      if (remoteWriteDisabled) return;
      // Business Logic: 关闭请求可能慢于用户切项目；必须绑定发起时的 project，不能读返回后的 active。
      const sourceProjectId = activeProjectIdRef.current;
      try {
        await workbenchApi.sessions.close(sessionId);
        if (sourceProjectId) {
          // Business Logic: 只作废源项目 list，禁止递增新项目 seq 导致 B 的有效 list 被当 stale 丢弃。
          invalidateSessionListRequests(sourceProjectId);
        }
        if (activeProjectIdRef.current !== sourceProjectId) {
          return;
        }
        setSessions((current) => {
          const next = current.filter((session) => session.id !== sessionId);
          updateActiveSession(next);
          return next;
        });
        knownSessionIdsRef.current.delete(sessionId);
        removeTerminalBuffer(sessionId);
        terminalInputPumpRef.current?.disposeSession(sessionId);
        untrackWriteBlocked(sessionId);
        if (sourceProjectId) void refreshProjectSessionStats(sourceProjectId);
      } catch (error) {
        if (activeProjectIdRef.current !== sourceProjectId) {
          return;
        }
        if (sourceProjectId) markRequestFailure(sourceProjectId, error);
        setSessionError(displayErrorMessage(error, t('closeSession')));
      }
    },
    [
      displayErrorMessage,
      invalidateSessionListRequests,
      markRequestFailure,
      refreshProjectSessionStats,
      removeTerminalBuffer,
      remoteWriteDisabled,
      t,
      untrackWriteBlocked,
      updateActiveSession,
    ],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   inspector rename 输入框与终端标签「双击行内编辑」共用同一条改名链路；按 id 重命名任意 session，
   *   复用既有 stale-guard / 错误处理，避免在两处复制 IPC 与错误文案逻辑。
   *
   * Code Logic（这个函数做什么）:
   *   1. 守卫：name 非空、remoteWriteDisabled=false、目标 session 属于当前 active project（跨项目 stale 时 no-op）；
   *   2. 调用 sessions.rename，把返回的 session 替换到 state，返回是否成功。
   */
  const renameSessionById = useCallback(
    async (sessionId: string, name: string): Promise<boolean> => {
      const trimmed = name.trim();
      if (!trimmed || remoteWriteDisabled) return false;
      const target = sessions.find((session) => session.id === sessionId);
      if (!target || target.projectId !== activeProjectIdRef.current) return false;
      try {
        setSessionError(null);
        const renamed = await workbenchApi.sessions.rename(sessionId, trimmed);
        invalidateSessionListRequests(target.projectId);
        setSessions((current) =>
          current.map((session) => (session.id === renamed.id ? renamed : session)),
        );
        return true;
      } catch (error) {
        markRequestFailure(target.projectId, error);
        setSessionError(displayErrorMessage(error, t('renameSession')));
        return false;
      }
    },
    [
      displayErrorMessage,
      invalidateSessionListRequests,
      markRequestFailure,
      remoteWriteDisabled,
      sessions,
      t,
    ],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户在 inspector rename 输入框里改名后提交（键盘 / 读屏路径）。
   *
   * Code Logic（这个函数做什么）:
   *   委派给 renameSessionById；空 draft / 远端离线由后者短路返回。
   */
  const handleRenameSession = useCallback(async (): Promise<void> => {
    if (!activeSession) return;
    await renameSessionById(activeSession.id, sessionNameDraft);
  }, [activeSession, renameSessionById, sessionNameDraft]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   xterm onData 把用户输入转发到后端 PTY；remoteWriteDisabled 时静默拒绝，避免必然失败的远端写。
   *   快速输入必须经 per-session 有序泵：leading-edge 立即发送、in-flight 期间 coalescing，
   *   失败批次永不自动重放，避免重复写入 PTY。
   *
   * Code Logic（这个函数做什么）:
   *   remoteWriteDisabled 时直接返回；否则 enqueue 到 terminalInputPump（不 await write settle）。
   */
  const handleInput = useCallback(
    async (sessionId: string, data: string): Promise<void> => {
      if (remoteWriteDisabled) return;
      terminalInputPumpRef.current?.enqueue(sessionId, data);
    },
    [remoteWriteDisabled],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   write 失败后 pump 会 silent-block enqueue；视图需用本查询禁用 xterm 输入，
   *   并配合 sessionError 让用户看到错误而非键盘黑洞。
   *
   * Code Logic（这个函数做什么）:
   *   查询 writeBlockedSessionIds 是否包含 sessionId。
   */
  const isWriteBlocked = useCallback(
    (sessionId: string): boolean => writeBlockedSessionIds.has(sessionId),
    [writeBlockedSessionIds],
  );

  // Business Logic: StatusMessage 只在仍有 write-blocked session 时挂「重新检查」；
  // load/create/focus 等其它 sessionError 不伪装成 write-block 恢复入口。
  const hasWriteBlockedSessions = writeBlockedSessionIds.size > 0;

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
        terminalInputPumpRef.current?.disposeSession(session.id);
        untrackWriteBlocked(session.id);
      });
    },
    [removeTerminalBuffer, sessions, untrackWriteBlocked],
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

  // Business Logic: agent 自动标题 / 用户 rename 后后端 emit 完整 session DTO；
  // 立即合并到 sessions，避免 tab 名等到下一次 list 才刷新。
  // activeSession?.name 的 draft 同步由上方 defer effect 负责；未知 id 静默忽略。
  useEffect(() => {
    const canListen = canListenToTauriEventsRef.current ?? canListenToTauriEventsDefault;
    if (!canListen()) return undefined;
    let registered = true;
    let unlistenFn: (() => void) | null = null;
    void listen<WorkbenchSessionUpdatedEvent>(
      'workbench:session-updated',
      (event) => {
        const payload = event.payload;
        if (!payload?.id) return;
        setSessions((current) => {
          const exists = current.some((session) => session.id === payload.id);
          if (!exists) return current;
          return current.map((session) =>
            session.id === payload.id ? { ...session, ...payload } : session,
          );
        });
      },
    ).then((fn) => {
      if (!registered) {
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
    canSwitchPane,
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
    handleSelectPaneAt,
    handleClosePane,
    handleCloseSession,
    renameSessionById,
    handleRenameSession,
    handleInput,
    isWriteBlocked,
    hasWriteBlockedSessions,
    retryWriteBlockRecovery,
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
