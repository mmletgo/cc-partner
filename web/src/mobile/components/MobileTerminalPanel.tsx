import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { FitAddon } from '@xterm/addon-fit';
import { Terminal } from '@xterm/xterm';
import '@xterm/xterm/css/xterm.css';
import { httpWorkbenchTransport } from '@/api/workbenchHttp';
import {
  useWorkbenchTerminalBuffer,
  useWorkbenchTerminalBufferStore,
  useWorkbenchTerminalBuffers,
} from '@/hooks/workbenchTerminalBuffersContext';
import { ArrowRightIcon, EditIcon, MaximizeIcon, MinimizeIcon, PlusIcon, PromptsIcon, XIcon } from '@/lib/icons';
import type { Prompt, WorkbenchProject, WorkbenchSession, WorkbenchWorktree } from '@/lib/types';
import {
  planTerminalBufferWrite,
  writeTerminalReplay,
} from '@/pages/Workbench/terminalReplay';
import { workbenchTerminalOptions, workbenchTerminalTheme } from '@/pages/Workbench/terminalOptions';
import {
  agentFreshnessI18nKey,
  agentPhaseI18nKey,
  agentProviderShortLabel,
  agentStatusAriaLabel,
} from '@/pages/Workbench/agentPhasePresentation';
import {
  prepareInitialReplayBuffer,
  shouldForwardMobileTerminalInput,
} from '../mobileTerminalReplay';
import {
  beginMobileTerminalTouchScroll,
  mobileTerminalTouchLineHeight,
  updateMobileTerminalTouchScroll,
  type MobileTerminalTouchScrollState,
} from '../mobileTerminalTouchScroll';
import {
  canRunMobilePaneMutation,
  canSwitchMobilePane,
  emptyMobileSessionRuntimeState,
  getMobileCreatePaneDirection,
  getMobileTerminalChromeVisibility,
  mobileAgentForSession,
  selectPreferredMobileSession,
  type MobileSessionRuntimeState,
} from '../mobileWorkbenchState';
import styles from '../MobileWorkbench.module.css';
import {
  MobileTerminalInputStream,
  type MobileTerminalInputStreamState,
} from '../mobileTerminalInputStream';
import {
  applyStickyModifierToInput,
  clearMobileTerminalHelperTextareaAfterCommit,
  enterMobileTerminalTypingMode,
  findMobileTerminalHelperTextarea,
  leaveMobileTerminalTypingMode,
  MOBILE_TERMINAL_STICKY_TIMEOUT_MS,
  resolveMobileTerminalExtraKeyPress,
  toggleStickyModifier,
  type MobileTerminalExtraKeyDef,
  type MobileTerminalExtraKeyPage,
  type MobileTerminalStickyModifier,
} from '../mobileTerminalExtraKeys';
import { MobileTerminalExtraKeys } from './MobileTerminalExtraKeys';
import { MobileFavoriteQuickInput } from './MobileFavoriteQuickInput';
import { MobilePromptOptimizerSheet } from './MobilePromptOptimizerSheet';
import { PointerPrimaryButton } from './PointerPrimaryButton';

const MIN_TERMINAL_COLS = 20;
const MIN_TERMINAL_ROWS = 6;
const DEFAULT_TERMINAL_SIZE = { cols: 80, rows: 24 };

export interface MobileTerminalPanelProps {
  project: WorkbenchProject | null;
  worktree: WorkbenchWorktree | null;
  sessions: WorkbenchSession[];
  activeSession: WorkbenchSession | null;
  busy: boolean;
  /** session 运行时投影（terminal status + Agent）；点击 Agent 只选中 terminal。 */
  sessionRuntime?: MobileSessionRuntimeState;
  onSessionsChange: (next: WorkbenchSession[]) => void;
  onActiveSessionChange: (session: WorkbenchSession | null) => void;
  onRefreshSessions?: () => Promise<void> | void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   后端 PTY resize 接受 u16 尺寸，移动端 xterm 在极小视口下也不能把 0 或异常值传给后端。
 *
 * Code Logic（这个函数做什么）:
 *   将输入数字四舍五入后限制在 min..65535；非法数字返回 min。
 */
function clampU16(value: number, min: number): number {
  if (!Number.isFinite(value)) return min;
  const rounded = Math.round(value);
  return Math.min(65535, Math.max(min, rounded));
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端新建 terminal window 时也应尽量使用当前可见终端区域尺寸，避免 TUI 首屏按默认列宽绘制后错位。
 *
 * Code Logic（这个函数做什么）:
 *   在屏幕外创建临时 xterm + FitAddon，按容器尺寸测出 cols/rows 后立即销毁。
 */
function measureMobileTerminalSize(container: HTMLElement | null): typeof DEFAULT_TERMINAL_SIZE {
  if (!container || container.clientWidth <= 0 || container.clientHeight <= 0) {
    return DEFAULT_TERMINAL_SIZE;
  }

  const host = document.createElement('div');
  const viewport = document.createElement('div');
  host.className = styles.mobileTerminalHost;
  viewport.className = styles.mobileTerminalViewport;
  host.style.position = 'fixed';
  host.style.left = '-10000px';
  host.style.top = '-10000px';
  host.style.width = `${container.clientWidth}px`;
  host.style.height = `${container.clientHeight}px`;
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
    return DEFAULT_TERMINAL_SIZE;
  } finally {
    terminal.dispose();
    host.remove();
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端终端操作失败时需要展示可读错误，并兼容非 Error 抛出值。
 *
 * Code Logic（这个函数做什么）:
 *   优先返回 Error.message；未知值转成字符串，空值回退到 fallback。
 */
function getErrorMessage(reason: unknown, fallback: string): string {
  if (reason instanceof Error && reason.message.trim()) return reason.message;
  const message = String(reason);
  return message && message !== 'undefined' && message !== 'null' ? message : fallback;
}

/**
 * MobileTerminalPanel（移动端真实终端面板）
 *
 * Business Logic（为什么需要这个组件）:
 *   `/mobile` 需要在手机浏览器中展示真实 Workbench tmux-backed terminal window/pane，并复用桌面端 session/window/pane 模型。
 *
 * Code Logic（这个组件做什么）:
 *   按 active project/worktree/session 渲染 session tabs、xterm viewport 和 window/pane 控制按钮；
 *   首屏通过 HTTP replay 写入历史 buffer，后续只消费外部 terminal buffer store 增量，输入/resize/focus/split/close 全部调用 HTTP transport。
 */
export function MobileTerminalPanel({
  project,
  worktree,
  sessions,
  activeSession,
  busy,
  sessionRuntime = emptyMobileSessionRuntimeState(),
  onSessionsChange,
  onActiveSessionChange,
  onRefreshSessions,
}: MobileTerminalPanelProps): ReactElement {
  const { t } = useTranslation(['workbench']);
  const { resetBuffer, removeBuffer } = useWorkbenchTerminalBuffers();
  const store = useWorkbenchTerminalBufferStore();
  const [actionBusy, setActionBusy] = useState<string | null>(null);
  const [panelError, setPanelError] = useState<string | null>(null);
  const [terminalFullscreen, setTerminalFullscreen] = useState<boolean>(false);
  const [inputStreamState, setInputStreamState] = useState<MobileTerminalInputStreamState>({
    status: 'connecting',
  });
  const [extraKeysPage, setExtraKeysPage] = useState<MobileTerminalExtraKeyPage>(1);
  const [stickyModifier, setStickyModifier] = useState<MobileTerminalStickyModifier | null>(null);
  const [favoriteSheetOpen, setFavoriteSheetOpen] = useState<boolean>(false);
  const [promptOptimizerSheetOpen, setPromptOptimizerSheetOpen] = useState<boolean>(false);
  const surfaceRef = useRef<HTMLDivElement | null>(null);
  const viewportRef = useRef<HTMLDivElement | null>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const bufferRef = useRef<string>('');
  const writtenBufferRef = useRef<string>('');
  const replayGateRef = useRef<boolean>(false);
  const replayReadyRef = useRef<boolean>(false);
  const inputEnabledRef = useRef<boolean>(false);
  const stickyModifierRef = useRef<MobileTerminalStickyModifier | null>(null);
  const stickyTimeoutRef = useRef<number | null>(null);
  const resizeTimerRef = useRef<number | null>(null);
  const replayRequestIdRef = useRef<number>(0);
  const lastResizeRef = useRef<{ sessionId: string; cols: number; rows: number } | null>(null);
  const lastFocusedSessionIdRef = useRef<string | null>(null);
  const touchScrollStateRef = useRef<MobileTerminalTouchScrollState | null>(null);
  const inputStreamRef = useRef<MobileTerminalInputStream | null>(null);

  const scopedSessions = useMemo(
    () =>
      project
        ? sessions.filter(
            (session) =>
              session.projectId === project.id &&
              (!worktree || session.worktreeId === worktree.id),
          )
        : [],
    [project, sessions, worktree],
  );
  const visibleSession = useMemo(
    () =>
      activeSession && scopedSessions.some((session) => session.id === activeSession.id)
        ? activeSession
        : null,
    [activeSession, scopedSessions],
  );
  const sessionId = visibleSession?.id ?? null;
  const { buffer, revision } = useWorkbenchTerminalBuffer(sessionId);
  const isActionDisabled = busy || actionBusy !== null;
  const canUsePaneActions = canRunMobilePaneMutation(visibleSession, isActionDisabled);
  const canSwitchPane = canSwitchMobilePane(visibleSession, isActionDisabled);
  // 收藏 Prompt 快捷输入按钮需要写终端输入流：session running 且 input stream ready 才启用。
  const canOpenFavoriteQuickInput =
    canUsePaneActions && inputStreamState.status === 'ready';
  const isTerminalFullscreen = terminalFullscreen && visibleSession !== null;
  const terminalChrome = getMobileTerminalChromeVisibility(isTerminalFullscreen);
  const TerminalFullscreenIcon = terminalChrome.exitFullscreen ? MinimizeIcon : MaximizeIcon;
  const terminalFullscreenLabel = terminalChrome.exitFullscreen
    ? t('workbench:mobile.terminalPanel.exitFullscreen')
    : t('workbench:mobile.terminalPanel.enterFullscreen');

  useEffect(() => {
    bufferRef.current = buffer;
  }, [buffer]);

  useEffect(() => {
    inputEnabledRef.current = Boolean(
      sessionId &&
        visibleSession?.status === 'running' &&
        !busy &&
        inputStreamState.status === 'ready',
    );
  }, [busy, inputStreamState.status, sessionId, visibleSession?.status]);

  useEffect(() => {
    stickyModifierRef.current = stickyModifier;
  }, [stickyModifier]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   sticky Ctrl/Alt 武装后若用户忘记再按键，应自动解除，避免后续普通输入被意外改写。
   *
   * Code Logic（这个函数做什么）:
   *   清掉旧 timer；武装时启动 3s timeout 将 sticky 置 null；disarm 只清 timer。
   */
  const armStickyModifier = useCallback((modifier: MobileTerminalStickyModifier | null): void => {
    if (stickyTimeoutRef.current !== null) {
      window.clearTimeout(stickyTimeoutRef.current);
      stickyTimeoutRef.current = null;
    }
    stickyModifierRef.current = modifier;
    setStickyModifier(modifier);
    if (!modifier) return;
    stickyTimeoutRef.current = window.setTimeout(() => {
      stickyTimeoutRef.current = null;
      stickyModifierRef.current = null;
      setStickyModifier(null);
    }, MOBILE_TERMINAL_STICKY_TIMEOUT_MS);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   Extra keys 与 xterm onData 共用同一输入流；失败时需要同一错误面板文案。
   *
   * Code Logic（这个函数做什么）:
   *   在 input 可用时 enqueue；捕获异常写入 panelError。
   */
  const sendTerminalInput = useCallback(
    (data: string): void => {
      if (!sessionId || !inputEnabledRef.current) return;
      try {
        inputStreamRef.current?.enqueue(sessionId, data);
      } catch (reason) {
        setPanelError(
          `${t('workbench:mobile.terminalPanel.errors.write')}: ${getErrorMessage(
            reason,
            t('workbench:errors.sessions'),
          )}`,
        );
      }
    },
    [sessionId, t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   收藏 Prompt 面板选中条目后，需要把 prompt 内容写入当前终端输入行（不回车），
   *   与桌面端快捷键浮层语义一致，让用户接着编辑或自行 Enter。
   *
   * Code Logic（这个函数做什么）:
   *   有 active sessionId 时复用 sendTerminalInput（自带 inputEnabled 门闩），
   *   不拼 \r；写入失败由 sendTerminalInput 统一投影 panelError。
   */
  const handleSelectFavoritePrompt = useCallback(
    (prompt: Prompt): void => {
      if (!sessionId) return;
      sendTerminalInput(prompt.content);
    },
    [sendTerminalInput, sessionId],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   额外键条的 payload/modifier/page 动作需要在面板层统一消化。
   *
   * Code Logic（这个函数做什么）:
   *   resolve 键定义 → send / toggle sticky / 翻页；payload 发送不消耗 sticky（与 Termux 独立宏键一致）。
   */
  const handleExtraKeyPress = useCallback(
    (key: MobileTerminalExtraKeyDef): void => {
      // 按 extra key 只发送按键，不 blur 终端 helper textarea：避免输入态下打乱 xterm 输入追踪
      // （已输入内容被重复发送）。焦点保持在终端，软键盘由用户点击终端外区域收起。
      const action = resolveMobileTerminalExtraKeyPress(key);
      if (action.type === 'send') {
        sendTerminalInput(action.data);
        return;
      }
      if (action.type === 'toggleModifier') {
        const next = toggleStickyModifier(stickyModifierRef.current, action.modifier);
        armStickyModifier(next.type === 'arm' ? next.modifier : null);
        return;
      }
      if (action.type === 'setPage') {
        setExtraKeysPage(action.page);
      }
    },
    [armStickyModifier, sendTerminalInput],
  );

  // 终端输入常驻 WebSocket 在面板挂载时建立一次(依赖 [])。StrictMode(dev)会双调用本 effect:
  // 首个 stream 在 CONNECTING 阶段就被 cleanup 的 stream.close() 中止,浏览器对中止未完成的
  // upgrade 会触发 error 事件;若放任其 onStateChange 继续 setState,会污染随后 ready stream
  // 的展示(残留"终端输入连接失败")。用 active 标志守卫:cleanup 后被废弃 stream 的事件不再写 state。
  useEffect(() => {
    let active = true;
    const stream = new MobileTerminalInputStream({
      onStateChange: (state) => {
        if (!active) return;
        setInputStreamState(state);
        if (state.status === 'blocked') {
          setPanelError(state.message);
        } else if (state.status === 'ready') {
          // 连接(重)建立后清掉历史 blocked 错误,避免 ready 后仍显示"终端输入连接失败"。
          setPanelError(null);
        }
      },
    });
    inputStreamRef.current = stream;
    return () => {
      active = false;
      inputStreamRef.current = null;
      stream.close();
      if (stickyTimeoutRef.current !== null) {
        window.clearTimeout(stickyTimeoutRef.current);
        stickyTimeoutRef.current = null;
      }
    };
  }, []);

  useEffect(() => {
    if (!project) {
      if (activeSession) onActiveSessionChange(null);
      return;
    }
    if (visibleSession) return;
    const nextSession = selectPreferredMobileSession(scopedSessions, worktree?.id ?? null);
    if ((nextSession?.id ?? null) !== (activeSession?.id ?? null)) {
      onActiveSessionChange(nextSession);
    }
  }, [
    activeSession,
    onActiveSessionChange,
    project,
    scopedSessions,
    visibleSession,
    worktree?.id,
  ]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   app tab 选中态必须同步到底层 tmux current window，避免手机 UI 与真实 tmux window 分裂。
   *
   * Code Logic（这个函数做什么）:
   *   调用 HTTP focus route；成功后记录已聚焦 session，失败时清空去重基线并把本地化错误写入面板错误区。
   */
  const focusSessionById = useCallback(
    async (nextSessionId: string): Promise<void> => {
      try {
        await httpWorkbenchTransport.sessions.focus(nextSessionId);
        lastFocusedSessionIdRef.current = nextSessionId;
      } catch (reason) {
        lastFocusedSessionIdRef.current = null;
        setPanelError(
          `${t('workbench:mobile.terminalPanel.errors.focus')}: ${getErrorMessage(
            reason,
            t('workbench:errors.focusSession'),
          )}`,
        );
      }
    },
    [t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   移动端屏幕只能高效操作单个 pane，进入多 pane window 后必须隐藏 tmux 分屏布局。
   *
   * Code Logic（这个函数做什么）:
   *   仅对 running tmux-backed session 调用 HTTP zoom-pane route；后端会幂等判断 paneCount/zoom 状态。
   */
  const ensurePaneZoomedById = useCallback(
    async (session: WorkbenchSession): Promise<void> => {
      if (session.status !== 'running' || !session.supportsPanes) return;
      try {
        await httpWorkbenchTransport.sessions.zoomPane(session.id);
      } catch (reason) {
        setPanelError(
          `${t('workbench:mobile.terminalPanel.errors.zoomPane')}: ${getErrorMessage(
            reason,
            t('workbench:errors.sessions'),
          )}`,
        );
      }
    },
    [t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   移动端切换 terminal window 时，前端 active session、tmux current window 和单 pane 展示状态必须一起同步。
   *
   * Code Logic（这个函数做什么）:
   *   先调用 focus 同步 tmux window，再按 session 能力调用 zoom-pane 确保当前 active pane 全屏显示。
   */
  const focusSessionAndZoomById = useCallback(
    async (session: WorkbenchSession): Promise<void> => {
      if (session.status !== 'running') return;
      await focusSessionById(session.id);
      await ensurePaneZoomedById(session);
    },
    [ensurePaneZoomedById, focusSessionById],
  );

  useEffect(() => {
    if (!sessionId) {
      lastFocusedSessionIdRef.current = null;
      return;
    }
    if (lastFocusedSessionIdRef.current === sessionId) {
      if (visibleSession) {
        queueMicrotask(() => {
          void ensurePaneZoomedById(visibleSession);
        });
      }
      return;
    }
    if (!visibleSession || visibleSession.status !== 'running') return;
    queueMicrotask(() => {
      void focusSessionAndZoomById(visibleSession);
    });
  }, [ensurePaneZoomedById, focusSessionAndZoomById, sessionId, visibleSession]);

  useEffect(() => {
    const viewport = viewportRef.current;
    if (!viewport || !sessionId || visibleSession?.status !== 'running') return undefined;

    const terminal = new Terminal(workbenchTerminalOptions());
    const fit = new FitAddon();
    const requestId = replayRequestIdRef.current + 1;
    let disposed = false;
    replayRequestIdRef.current = requestId;
    replayReadyRef.current = false;
    writtenBufferRef.current = '';
    replayGateRef.current = false;
    terminal.loadAddon(fit);
    terminal.open(viewport);
    terminalRef.current = terminal;
    // 默认离开打字态：系统键盘只在用户明确点击终端输入区后出现。
    leaveMobileTerminalTypingMode(findMobileTerminalHelperTextarea(viewport), null);

    /**
     * Business Logic（为什么需要这个函数）:
     *   手机旋转、地址栏收缩或分屏后需要同步 xterm 与后端 PTY 尺寸。
     *
     * Code Logic（这个函数做什么）:
     *   执行 FitAddon.fit，clamp cols/rows，并在尺寸变化时调用 HTTP resize route。
     */
    const resizeTerminal = (): void => {
      if (disposed) return;
      try {
        fit.fit();
        const cols = clampU16(terminal.cols, MIN_TERMINAL_COLS);
        const rows = clampU16(terminal.rows, MIN_TERMINAL_ROWS);
        const last = lastResizeRef.current;
        if (last?.sessionId === sessionId && last.cols === cols && last.rows === rows) return;
        lastResizeRef.current = { sessionId, cols, rows };
        void httpWorkbenchTransport.sessions.resize(sessionId, cols, rows).catch((reason) => {
          if (disposed) return;
          setPanelError(
            `${t('workbench:mobile.terminalPanel.errors.resize')}: ${getErrorMessage(
              reason,
              t('workbench:errors.sessions'),
            )}`,
          );
        });
      } catch {
        // xterm 在不可见或尺寸尚未稳定时可能 fit 失败，下一次 ResizeObserver 会重试。
      }
    };

    const dataDisposable = terminal.onData((data: string) => {
      if (
        !shouldForwardMobileTerminalInput(
          replayGateRef,
          replayReadyRef.current,
          inputEnabledRef.current,
        )
      ) {
        return;
      }
      const stickyResult = applyStickyModifierToInput(stickyModifierRef.current, data);
      if (stickyResult.consume) {
        if (stickyTimeoutRef.current !== null) {
          window.clearTimeout(stickyTimeoutRef.current);
          stickyTimeoutRef.current = null;
        }
        stickyModifierRef.current = null;
        setStickyModifier(null);
      }
      try {
        inputStreamRef.current?.enqueue(sessionId, stickyResult.data);
      } catch (reason) {
        if (disposed) return;
        setPanelError(
          `${t('workbench:mobile.terminalPanel.errors.write')}: ${getErrorMessage(
            reason,
            t('workbench:errors.sessions'),
          )}`,
        );
      } finally {
        // xterm 6 在移动端中文 IME（尤其全角括号）提交后不会清空 helper textarea；
        // 残留 value 会在下次 composition/input 时被再次 substring 发出，造成「已输入内容重复」。
        // onData 触发时 xterm 已读完本次提交，此时清空安全。
        clearMobileTerminalHelperTextareaAfterCommit(
          findMobileTerminalHelperTextarea(viewport),
        );
      }
    });

    /**
     * Business Logic（为什么需要这个函数）:
     *   终端区域的移动端滑动必须留在 xterm 内部，否则浏览器会把它当页面滚动并显示/隐藏地址栏。
     *   xterm 原生 .xterm-viewport 仍是 overflow scroll，canvas 上的 touch 默认会先驱动它。
     *
     * Code Logic（这个函数做什么）:
     *   在 capture 阶段拦截单指 touchmove，读取 viewport 与 xterm rows 得到行高，
     *   交给 touch helper 累计像素并调用 terminal.scrollLines；滑动过程中不进入打字态。
     *   轻点（未滚动）在 touchend 进入打字态并 focus，系统键盘才出现；shell 会随 visualViewport 上移。
     */
    let touchMoved = false;
    let suppressClickAfterScroll = false;
    let touchStartY = 0;
    const handleTouchMove = (event: TouchEvent): void => {
      if (event.touches.length !== 1) {
        touchScrollStateRef.current = null;
        return;
      }
      // 单指滑动一旦开始就必须取消浏览器默认滚动（页面 / xterm-viewport），否则 scrollLines 无效。
      if (event.cancelable) {
        event.preventDefault();
      }
      event.stopPropagation();
      const touch = event.touches[0];
      if (Math.abs(touch.clientY - touchStartY) > 8) {
        touchMoved = true;
      }
      const baseState =
        touchScrollStateRef.current ?? beginMobileTerminalTouchScroll(touch.clientY);
      const fallbackLineHeight =
        Number(terminal.options.fontSize ?? 13) * Number(terminal.options.lineHeight ?? 1);
      const result = updateMobileTerminalTouchScroll(
        baseState,
        touch.clientY,
        mobileTerminalTouchLineHeight(viewport.clientHeight, terminal.rows, fallbackLineHeight),
      );
      touchScrollStateRef.current = result.state;
      if (result.lines !== 0) {
        touchMoved = true;
        terminal.scrollLines(result.lines);
      }
    };
    const handleTouchStart = (event: TouchEvent): void => {
      touchMoved = false;
      suppressClickAfterScroll = false;
      touchStartY = event.touches.length === 1 ? event.touches[0].clientY : 0;
      touchScrollStateRef.current =
        event.touches.length === 1
          ? beginMobileTerminalTouchScroll(event.touches[0].clientY)
          : null;
    };
    /**
     * Business Logic（为什么需要这个函数）:
     *   系统键盘只应在用户明确点击终端输入区后出现；滑动滚动不得弹出键盘。
     *
     * Code Logic（这个函数做什么）:
     *   去掉 helper readonly/inputmode 并 terminal.focus()。
     */
    const enterTypingFromUserGesture = (): void => {
      if (disposed) return;
      enterMobileTerminalTypingMode(findMobileTerminalHelperTextarea(viewport));
      try {
        terminal.focus();
      } catch {
        // xterm 在 dispose 窗口期可能 focus 失败，忽略。
      }
    };
    const handleTouchEnd = (): void => {
      const wasScroll = touchMoved;
      touchScrollStateRef.current = null;
      touchMoved = false;
      // 滑动只滚动；随后合成 click 也必须抑制，避免滚完又弹键盘。
      if (wasScroll) {
        suppressClickAfterScroll = true;
        return;
      }
      enterTypingFromUserGesture();
    };
    const handleTouchCancel = (): void => {
      touchScrollStateRef.current = null;
      touchMoved = false;
      suppressClickAfterScroll = true;
    };
    const handleViewportClick = (): void => {
      if (suppressClickAfterScroll) {
        suppressClickAfterScroll = false;
        return;
      }
      // 桌面/无 touch 调试；手机轻点与 touchend 幂等。
      enterTypingFromUserGesture();
    };
    // capture=true：在 canvas / .xterm-viewport 自己消费 touch 前先接管手势。
    const touchListenerOptions: AddEventListenerOptions = { capture: true };
    viewport.addEventListener('touchstart', handleTouchStart, {
      ...touchListenerOptions,
      passive: true,
    });
    viewport.addEventListener('touchmove', handleTouchMove, {
      ...touchListenerOptions,
      passive: false,
    });
    viewport.addEventListener('touchend', handleTouchEnd, {
      ...touchListenerOptions,
      passive: true,
    });
    viewport.addEventListener('touchcancel', handleTouchCancel, {
      ...touchListenerOptions,
      passive: true,
    });
    viewport.addEventListener('click', handleViewportClick);

    const observer = new ResizeObserver(() => {
      if (resizeTimerRef.current !== null) {
        window.clearTimeout(resizeTimerRef.current);
      }
      resizeTimerRef.current = window.setTimeout(resizeTerminal, 80);
    });
    observer.observe(viewport);
    resizeTerminal();

    void httpWorkbenchTransport.sessions
      .replay(sessionId)
      .then((replay) => {
        if (disposed || replayRequestIdRef.current !== requestId) return;
        // 只读当前 session 的 store 快照，禁止用 bufferRef（可能短暂滞后或跨 tab 切换串味）。
        const liveForSession = store.getBuffer(sessionId);
        const initial = prepareInitialReplayBuffer(replay.buffer, liveForSession);
        writeTerminalReplay(terminal, initial.data, replayGateRef);
        // writtenBuffer 必须等于实际写入 xterm 的完整历史（见 prepareInitialReplayBuffer）。
        writtenBufferRef.current = initial.writtenBuffer;
        replayReadyRef.current = true;
        // 以完整 written 内容作本 session baseline；后续 live（seq > lastSeq、同 owner）走 append。
        store.reset(sessionId, initial.writtenBuffer, replay.lastSeq, replay.ownerInstanceId);
      })
      .catch((reason) => {
        if (disposed || replayRequestIdRef.current !== requestId) return;
        const liveForSession = store.getBuffer(sessionId);
        if (liveForSession) {
          writeTerminalReplay(terminal, liveForSession, replayGateRef);
          writtenBufferRef.current = liveForSession;
        }
        replayReadyRef.current = true;
        setPanelError(
          `${t('workbench:mobile.terminalPanel.errors.replay')}: ${getErrorMessage(
            reason,
            t('workbench:errors.sessions'),
          )}`,
        );
      });

    return () => {
      disposed = true;
      observer.disconnect();
      dataDisposable.dispose();
      viewport.removeEventListener('touchstart', handleTouchStart, touchListenerOptions);
      viewport.removeEventListener('touchmove', handleTouchMove, touchListenerOptions);
      viewport.removeEventListener('touchend', handleTouchEnd, touchListenerOptions);
      viewport.removeEventListener('touchcancel', handleTouchCancel, touchListenerOptions);
      viewport.removeEventListener('click', handleViewportClick);
      touchScrollStateRef.current = null;
      if (resizeTimerRef.current !== null) {
        window.clearTimeout(resizeTimerRef.current);
        resizeTimerRef.current = null;
      }
      terminal.dispose();
      terminalRef.current = null;
      writtenBufferRef.current = '';
      replayGateRef.current = false;
      replayReadyRef.current = false;
    };
  }, [sessionId, store, t, visibleSession?.status]);

  useEffect(() => {
    const applyTheme = (): void => {
      const terminal = terminalRef.current;
      if (!terminal) return;
      terminal.options.theme = workbenchTerminalTheme();
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
    if (!terminal || !sessionId || !replayReadyRef.current) return;

    const plan = planTerminalBufferWrite(writtenBufferRef.current, buffer);
    if (plan.mode === 'replay') {
      // 空 next buffer 的 replay 只会 clear 掉已有 scrollback（打开页后短暂空 store 竞态），
      // 没有可写内容时不得清屏。
      if (plan.data.length === 0) return;
      terminal.clear();
      writeTerminalReplay(terminal, plan.data, replayGateRef);
      writtenBufferRef.current = buffer;
      return;
    }
    if (plan.mode === 'append') {
      terminal.write(plan.data);
      writtenBufferRef.current = buffer;
    }
  }, [buffer, revision, sessionId]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户点击 terminal window tab 时，移动端状态栏、xterm 和底层 tmux current window 都要切换到同一个 session。
   *
   * Code Logic（这个函数做什么）:
   *   写入父组件 active session，并调用 focus HTTP route；错误由 focusSessionById 展示。
   */
  const handleSelectSession = useCallback(
    (session: WorkbenchSession): void => {
      onActiveSessionChange(session);
      void focusSessionAndZoomById(session);
    },
    [focusSessionAndZoomById, onActiveSessionChange],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   手机端需要能在当前项目/worktree 中创建真实 terminal window，而不是渲染假的前端 tab。
   *
   * Code Logic（这个函数做什么）:
   *   测量当前终端区域尺寸，调用 HTTP create route，追加 session、重置 buffer、设为 active，并刷新权威 session 列表。
   */
  const handleCreateSession = useCallback(async (): Promise<void> => {
    if (!project) return;
    setActionBusy('create');
    setPanelError(null);
    try {
      // Prefer the terminal viewport (excludes extra-keys bar) so initial PTY size matches fit area.
      const initialSize = measureMobileTerminalSize(
        viewportRef.current ?? surfaceRef.current,
      );
      const session = await httpWorkbenchTransport.sessions.create(
        project.id,
        initialSize,
        worktree?.id ?? null,
      );
      const nextSessions = [...sessions.filter((item) => item.id !== session.id), session];
      onSessionsChange(nextSessions);
      resetBuffer(session.id);
      onActiveSessionChange(session);
      await focusSessionAndZoomById(session);
      await onRefreshSessions?.();
    } catch (reason) {
      setPanelError(
        `${t('workbench:mobile.terminalPanel.errors.create')}: ${getErrorMessage(
          reason,
          t('workbench:errors.createSession'),
        )}`,
      );
    } finally {
      setActionBusy(null);
    }
  }, [
    focusSessionAndZoomById,
    onActiveSessionChange,
    onRefreshSessions,
    onSessionsChange,
    project,
    resetBuffer,
    sessions,
    t,
    worktree,
  ]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   手机端新增 pane 必须由 tmux 创建真实 pane，但不需要让用户选择左右/上下分屏方向。
   *
   * Code Logic（这个函数做什么）:
   *   使用移动端固定 split 方向调用 HTTP split-pane route，完成后刷新 session 列表以同步 paneCount。
   */
  const handleCreatePane = useCallback(
    async (): Promise<void> => {
      if (!visibleSession) return;
      setActionBusy('create-pane');
      setPanelError(null);
      try {
        const direction = getMobileCreatePaneDirection();
        await httpWorkbenchTransport.sessions.splitPane(visibleSession.id, direction);
        await ensurePaneZoomedById(visibleSession);
        await onRefreshSessions?.();
      } catch (reason) {
        setPanelError(
          `${t('workbench:mobile.terminalPanel.errors.split')}: ${getErrorMessage(
            reason,
            t('workbench:errors.splitPane'),
          )}`,
        );
      } finally {
        setActionBusy(null);
      }
    },
    [ensurePaneZoomedById, onRefreshSessions, t, visibleSession],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   手机端用户无法方便地按 tmux 快捷键，工具栏需要一键切到当前 window 的下一个 pane。
   *
   * Code Logic（这个函数做什么）:
   *   调用 HTTP switch-pane route；该操作不改变 session 列表，只让后端 tmux 选择下一个 active pane。
   */
  const handleSwitchPane = useCallback(async (): Promise<void> => {
    if (!visibleSession) return;
    setActionBusy('switch-pane');
    setPanelError(null);
    try {
      await httpWorkbenchTransport.sessions.switchPane(visibleSession.id);
      await ensurePaneZoomedById(visibleSession);
    } catch (reason) {
      setPanelError(
        `${t('workbench:mobile.terminalPanel.errors.switchPane')}: ${getErrorMessage(
          reason,
          t('workbench:errors.sessions'),
        )}`,
      );
    } finally {
      setActionBusy(null);
    }
  }, [ensurePaneZoomedById, t, visibleSession]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   关闭 pane 应映射到真实 tmux pane；关闭最后一个 pane 时要移除对应 terminal window tab 和 buffer。
   *
   * Code Logic（这个函数做什么）:
   *   调用 HTTP close-pane route；closedWindow=true 时本地先移除 session 并选择同 worktree 的下一个优先 session，再刷新权威列表。
   */
  const handleClosePane = useCallback(async (): Promise<void> => {
    if (!visibleSession) return;
    setActionBusy('close-pane');
    setPanelError(null);
    try {
      const result = await httpWorkbenchTransport.sessions.closePane(visibleSession.id);
      if (result.closedWindow) {
        const nextSessions = sessions.filter((session) => session.id !== result.sessionId);
        onSessionsChange(nextSessions);
        removeBuffer(result.sessionId);
        onActiveSessionChange(selectPreferredMobileSession(nextSessions, worktree?.id ?? null));
      } else {
        await ensurePaneZoomedById(visibleSession);
      }
      await onRefreshSessions?.();
    } catch (reason) {
      setPanelError(
        `${t('workbench:mobile.terminalPanel.errors.closePane')}: ${getErrorMessage(
          reason,
          t('workbench:errors.closePane'),
        )}`,
      );
    } finally {
      setActionBusy(null);
    }
  }, [
    onActiveSessionChange,
    onRefreshSessions,
    onSessionsChange,
    ensurePaneZoomedById,
    removeBuffer,
    sessions,
    t,
    visibleSession,
    worktree,
  ]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   手机端也需要能关闭当前 terminal window，释放后端 PTY/tmux attach 与前端 buffer。
   *
   * Code Logic（这个函数做什么）:
   *   调用 HTTP close route，移除本地 session 与缓存，并选择当前 worktree 下的下一个优先 session。
   */
  const handleCloseSession = useCallback(
    async (session: WorkbenchSession): Promise<void> => {
      setActionBusy(`close-${session.id}`);
      setPanelError(null);
      try {
        await httpWorkbenchTransport.sessions.close(session.id);
        const nextSessions = sessions.filter((item) => item.id !== session.id);
        onSessionsChange(nextSessions);
        removeBuffer(session.id);
        if (activeSession?.id === session.id) {
          onActiveSessionChange(selectPreferredMobileSession(nextSessions, worktree?.id ?? null));
        }
        await onRefreshSessions?.();
      } catch (reason) {
        setPanelError(
          `${t('workbench:mobile.terminalPanel.errors.closeWindow')}: ${getErrorMessage(
            reason,
            t('workbench:errors.closeSession'),
          )}`,
        );
      } finally {
        setActionBusy(null);
      }
    },
    [
      activeSession,
      onActiveSessionChange,
      onRefreshSessions,
      onSessionsChange,
      removeBuffer,
      sessions,
      t,
      worktree,
    ],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   手机屏幕空间有限，用户需要把当前终端临时铺满屏幕，并隐藏移动端 shell 与 window tabs。
   *
   * Code Logic（这个函数做什么）:
   *   有可见 session 时打开全屏状态；没有终端 window 时忽略，避免空态占满屏幕。
   */
  const handleEnterTerminalFullscreen = useCallback((): void => {
    if (!visibleSession) return;
    setTerminalFullscreen(true);
  }, [visibleSession]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   进入全屏后必须保留明确退出入口，让用户回到完整移动端 Workbench shell。
   *
   * Code Logic（这个函数做什么）:
   *   关闭本地全屏状态，触发 CSS 从 fixed overlay 回到普通面板布局。
   */
  const handleExitTerminalFullscreen = useCallback((): void => {
    setTerminalFullscreen(false);
  }, []);

  const terminalBody = !project ? (
    <div className={styles.mobileTerminalEmpty}>
      {t('workbench:mobile.terminalPanel.noProject')}
    </div>
  ) : !visibleSession ? (
    <div className={styles.mobileTerminalEmpty}>
      <span>{t('workbench:mobile.terminalPanel.noSession')}</span>
      <PointerPrimaryButton
        type="button"
        className={styles.mobileTerminalPrimaryButton}
        disabled={isActionDisabled}
        onPrimary={() => void handleCreateSession()}
      >
        <PlusIcon size={16} aria-hidden="true" />
        <span>{t('workbench:mobile.terminalPanel.newWindow')}</span>
      </PointerPrimaryButton>
    </div>
  ) : null;

  return (
    <section
      className={`${styles.panel} ${styles.mobileTerminalPanel}`}
      data-fullscreen={isTerminalFullscreen || undefined}
      aria-labelledby="mobile-terminal-panel-title"
    >
      {terminalChrome.panelHeader ? (
        <div className={styles.panelHeader}>
          <p className={styles.panelKicker}>{t('workbench:mobile.kicker')}</p>
          <h1 id="mobile-terminal-panel-title">{t('workbench:mobile.terminalPanel.title')}</h1>
        </div>
      ) : null}

      <div
        className={styles.mobileTerminalToolbar}
        data-fullscreen={isTerminalFullscreen || undefined}
      >
        {terminalChrome.windowTabs ? (
          <div
            className={styles.mobileTerminalTabs}
            role="tablist"
            aria-label={t('workbench:mobile.terminalPanel.tabsAriaLabel')}
          >
            {scopedSessions.map((session) => {
              const isActive = session.id === visibleSession?.id;
              const agent = mobileAgentForSession(sessionRuntime, session.id);
              const phaseLabel = agent
                ? t(`workbench:${agentPhaseI18nKey(agent.phase)}`)
                : null;
              const freshnessKey = agent ? agentFreshnessI18nKey(agent.freshness) : null;
              const freshnessLabel = freshnessKey
                ? t(`workbench:${freshnessKey}`)
                : null;
              const agentAria =
                agent && phaseLabel ? agentStatusAriaLabel(agent, phaseLabel) : null;
              return (
                <div
                  key={session.id}
                  className={styles.mobileSessionTab}
                  data-active={isActive || undefined}
                >
                  <PointerPrimaryButton
                    type="button"
                    role="tab"
                    aria-selected={isActive}
                    className={styles.mobileSessionSelectButton}
                    onPrimary={() => handleSelectSession(session)}
                  >
                    <span className={styles.mobileSessionDot} data-status={session.status} />
                    <span className={styles.mobileSessionName}>{session.name}</span>
                    {agent && phaseLabel ? (
                      <span
                        className={styles.mobileSessionAgent}
                        role="status"
                        aria-label={agentAria ?? phaseLabel}
                        title={agentAria ?? phaseLabel}
                        // 点击 Agent 状态只导航到该 terminal，永不发送输入；pointerDown 即触发，避免 IME 吞 click。
                        onPointerDown={(event) => {
                          event.preventDefault();
                          event.stopPropagation();
                          handleSelectSession(session);
                        }}
                      >
                        {agentProviderShortLabel(agent.providerId)} · {phaseLabel}
                        {freshnessLabel ? ` · ${freshnessLabel}` : null}
                      </span>
                    ) : null}
                    <span className={styles.mobileSessionPaneCount}>
                      {t('workbench:mobile.terminalPanel.paneCount', {
                        count: session.paneCount,
                      })}
                    </span>
                  </PointerPrimaryButton>
                  <PointerPrimaryButton
                    type="button"
                    className={styles.mobileTerminalTabClose}
                    aria-label={t('workbench:mobile.terminalPanel.closeWindow')}
                    disabled={isActionDisabled}
                    onPrimary={(event) => {
                      event.stopPropagation();
                      void handleCloseSession(session);
                    }}
                  >
                    <XIcon size={14} aria-hidden="true" />
                  </PointerPrimaryButton>
                </div>
              );
            })}
            <PointerPrimaryButton
              type="button"
              className={styles.mobileTerminalPrimaryButton}
              disabled={!project || isActionDisabled}
              onPrimary={() => void handleCreateSession()}
            >
              <PlusIcon size={16} aria-hidden="true" />
              <span>{t('workbench:mobile.terminalPanel.newWindow')}</span>
            </PointerPrimaryButton>
          </div>
        ) : null}

        {terminalChrome.paneActions ? (
          <div
            className={styles.mobileTerminalActions}
            aria-label={t('workbench:mobile.terminalPanel.actionsAriaLabel')}
          >
            <PointerPrimaryButton
              type="button"
              className={styles.mobileTerminalActionButton}
              disabled={!canUsePaneActions}
              aria-label={t('workbench:mobile.terminalPanel.addPane')}
              title={t('workbench:mobile.terminalPanel.addPane')}
              onPrimary={() => void handleCreatePane()}
            >
              <PlusIcon size={16} aria-hidden="true" />
              <span>{t('workbench:mobile.terminalPanel.addPane')}</span>
            </PointerPrimaryButton>
            <PointerPrimaryButton
              type="button"
              className={styles.mobileTerminalActionButton}
              disabled={!canSwitchPane}
              aria-label={t('workbench:mobile.terminalPanel.switchPane')}
              title={t('workbench:mobile.terminalPanel.switchPane')}
              onPrimary={() => void handleSwitchPane()}
            >
              <ArrowRightIcon size={16} aria-hidden="true" />
              <span>{t('workbench:mobile.terminalPanel.switchPane')}</span>
            </PointerPrimaryButton>
            <PointerPrimaryButton
              type="button"
              className={styles.mobileTerminalActionButton}
              disabled={!canUsePaneActions}
              aria-label={t('workbench:mobile.terminalPanel.closePane')}
              title={t('workbench:mobile.terminalPanel.closePane')}
              onPrimary={() => void handleClosePane()}
            >
              <XIcon size={16} aria-hidden="true" />
              <span>{t('workbench:mobile.terminalPanel.closePane')}</span>
            </PointerPrimaryButton>
            <PointerPrimaryButton
              type="button"
              className={styles.mobileTerminalActionButton}
              disabled={!terminalChrome.exitFullscreen && !visibleSession}
              aria-label={terminalFullscreenLabel}
              title={terminalFullscreenLabel}
              onPrimary={() =>
                terminalChrome.exitFullscreen
                  ? handleExitTerminalFullscreen()
                  : handleEnterTerminalFullscreen()
              }
            >
              <TerminalFullscreenIcon size={16} aria-hidden="true" />
              <span>{terminalFullscreenLabel}</span>
            </PointerPrimaryButton>
          </div>
        ) : null}
      </div>

      {!isTerminalFullscreen && busy ? (
        <p className={styles.panelState}>{t('workbench:loading')}</p>
      ) : null}
      {!isTerminalFullscreen && panelError ? (
        <p className={styles.panelError}>
          <span>{t('workbench:mobile.projectPanel.error')}</span>
          <span>{panelError}</span>
        </p>
      ) : null}

      {terminalChrome.terminalSurface ? (
        <div
          className={styles.mobileTerminalSurface}
          ref={surfaceRef}
          aria-label={t('workbench:mobile.terminalPanel.terminalAriaLabel')}
        >
          {terminalBody}
          <div
            className={styles.mobileTerminalHost}
            data-hidden={!visibleSession || undefined}
            aria-hidden={!visibleSession}
          >
            <div className={styles.mobileTerminalViewport} ref={viewportRef} />
            {visibleSession ? (
              <div className={styles.mobileTerminalFabGroup}>
                <PointerPrimaryButton
                  type="button"
                  className={styles.mobileTerminalFab}
                  disabled={!canOpenFavoriteQuickInput}
                  aria-label={t('workbench:promptOptimizer.open')}
                  title={t('workbench:promptOptimizer.open')}
                  onPrimary={() => setPromptOptimizerSheetOpen(true)}
                >
                  <EditIcon size={18} aria-hidden="true" />
                </PointerPrimaryButton>
                <PointerPrimaryButton
                  type="button"
                  className={styles.mobileTerminalFab}
                  disabled={!canOpenFavoriteQuickInput}
                  aria-label={t('workbench:mobile.favoriteQuickInput.openButton')}
                  title={t('workbench:mobile.favoriteQuickInput.openButton')}
                  onPrimary={() => setFavoriteSheetOpen(true)}
                >
                  <PromptsIcon size={18} aria-hidden="true" />
                </PointerPrimaryButton>
              </div>
            ) : null}
          </div>
          {visibleSession ? (
            <MobileTerminalExtraKeys
              disabled={
                !sessionId ||
                visibleSession.status !== 'running' ||
                busy ||
                inputStreamState.status !== 'ready'
              }
              page={extraKeysPage}
              stickyModifier={stickyModifier}
              onKeyPress={handleExtraKeyPress}
            />
          ) : null}
        </div>
      ) : null}
      <MobileFavoriteQuickInput
        open={favoriteSheetOpen}
        onClose={() => setFavoriteSheetOpen(false)}
        onSelectPrompt={handleSelectFavoritePrompt}
      />
      <MobilePromptOptimizerSheet
        open={promptOptimizerSheetOpen}
        onClose={() => setPromptOptimizerSheetOpen(false)}
        worktree={worktree}
        session={activeSession}
      />
    </section>
  );
}
