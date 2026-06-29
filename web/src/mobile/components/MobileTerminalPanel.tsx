import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { FitAddon } from '@xterm/addon-fit';
import { Terminal } from '@xterm/xterm';
import '@xterm/xterm/css/xterm.css';
import { httpWorkbenchTransport } from '@/api/workbenchHttp';
import type { WorkbenchPaneSplitDirection } from '@/api/workbench';
import {
  useWorkbenchTerminalBuffer,
  useWorkbenchTerminalBuffers,
} from '@/hooks/workbenchTerminalBuffersContext';
import { PlusIcon, SplitDownIcon, SplitRightIcon, XIcon } from '@/lib/icons';
import type { WorkbenchProject, WorkbenchSession, WorkbenchWorktree } from '@/lib/types';
import {
  planTerminalBufferWrite,
  shouldForwardTerminalInput,
  writeTerminalReplay,
} from '@/pages/Workbench/terminalReplay';
import { workbenchTerminalOptions, workbenchTerminalTheme } from '@/pages/Workbench/terminalOptions';
import { selectPreferredMobileSession } from '../mobileWorkbenchState';
import styles from '../MobileWorkbench.module.css';

const MIN_TERMINAL_COLS = 20;
const MIN_TERMINAL_ROWS = 6;
const DEFAULT_TERMINAL_SIZE = { cols: 80, rows: 24 };

interface InitialReplayBuffer {
  data: string;
  writtenBuffer: string;
}

export interface MobileTerminalPanelProps {
  project: WorkbenchProject | null;
  worktree: WorkbenchWorktree | null;
  sessions: WorkbenchSession[];
  activeSession: WorkbenchSession | null;
  busy: boolean;
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
 *   移动端首次打开终端时会同时拥有 HTTP replay 快照和 NDJSON 增量缓存，二者不能重复写入 xterm。
 *
 * Code Logic（这个函数做什么）:
 *   用桌面端 replay diff helper 判断 live buffer 是否是 replay 的延续；返回要写入 xterm 的初始内容以及后续增量比较基线。
 */
function prepareInitialReplayBuffer(replayBuffer: string, liveBuffer: string): InitialReplayBuffer {
  if (!liveBuffer) return { data: replayBuffer, writtenBuffer: replayBuffer };
  if (!replayBuffer) return { data: liveBuffer, writtenBuffer: liveBuffer };

  const plan = planTerminalBufferWrite(replayBuffer, liveBuffer);
  if (plan.mode === 'append') {
    return { data: `${replayBuffer}${plan.data}`, writtenBuffer: liveBuffer };
  }
  if (plan.mode === 'none') {
    return { data: replayBuffer, writtenBuffer: replayBuffer };
  }
  return { data: `${replayBuffer}${liveBuffer}`, writtenBuffer: liveBuffer };
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
  onSessionsChange,
  onActiveSessionChange,
  onRefreshSessions,
}: MobileTerminalPanelProps): ReactElement {
  const { t } = useTranslation(['workbench']);
  const { resetBuffer, removeBuffer } = useWorkbenchTerminalBuffers();
  const [actionBusy, setActionBusy] = useState<string | null>(null);
  const [panelError, setPanelError] = useState<string | null>(null);
  const surfaceRef = useRef<HTMLDivElement | null>(null);
  const viewportRef = useRef<HTMLDivElement | null>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const bufferRef = useRef<string>('');
  const writtenBufferRef = useRef<string>('');
  const replayGateRef = useRef<boolean>(false);
  const replayReadyRef = useRef<boolean>(false);
  const inputEnabledRef = useRef<boolean>(false);
  const resizeTimerRef = useRef<ReturnType<typeof window.setTimeout> | null>(null);
  const replayRequestIdRef = useRef<number>(0);
  const lastResizeRef = useRef<{ sessionId: string; cols: number; rows: number } | null>(null);
  const lastFocusedSessionIdRef = useRef<string | null>(null);

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
  const canUsePaneActions = Boolean(visibleSession?.supportsPanes);

  useEffect(() => {
    bufferRef.current = buffer;
  }, [buffer]);

  useEffect(() => {
    inputEnabledRef.current = Boolean(sessionId && !busy);
  }, [busy, sessionId]);

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
   *   调用 HTTP focus route；失败时把本地化错误写入面板错误区。
   */
  const focusSessionById = useCallback(
    async (nextSessionId: string): Promise<void> => {
      lastFocusedSessionIdRef.current = nextSessionId;
      try {
        await httpWorkbenchTransport.sessions.focus(nextSessionId);
      } catch (reason) {
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

  useEffect(() => {
    if (!sessionId) {
      lastFocusedSessionIdRef.current = null;
      return;
    }
    if (lastFocusedSessionIdRef.current === sessionId) return;
    void focusSessionById(sessionId);
  }, [focusSessionById, sessionId]);

  useEffect(() => {
    const viewport = viewportRef.current;
    if (!viewport || !sessionId) return undefined;

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
      if (!shouldForwardTerminalInput(replayGateRef, inputEnabledRef.current)) return;
      void httpWorkbenchTransport.sessions.writeInput(sessionId, data).catch((reason) => {
        if (disposed) return;
        setPanelError(
          `${t('workbench:mobile.terminalPanel.errors.write')}: ${getErrorMessage(
            reason,
            t('workbench:errors.sessions'),
          )}`,
        );
      });
    });

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
        const initial = prepareInitialReplayBuffer(replay.buffer, bufferRef.current);
        writeTerminalReplay(terminal, initial.data, replayGateRef);
        writtenBufferRef.current = initial.writtenBuffer;
        replayReadyRef.current = true;
      })
      .catch((reason) => {
        if (disposed || replayRequestIdRef.current !== requestId) return;
        const liveBuffer = bufferRef.current;
        if (liveBuffer) {
          writeTerminalReplay(terminal, liveBuffer, replayGateRef);
          writtenBufferRef.current = liveBuffer;
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
  }, [sessionId, t]);

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
      void focusSessionById(session.id);
    },
    [focusSessionById, onActiveSessionChange],
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
      const initialSize = measureMobileTerminalSize(surfaceRef.current);
      const session = await httpWorkbenchTransport.sessions.create(
        project.id,
        initialSize,
        worktree?.id ?? null,
      );
      const nextSessions = [...sessions.filter((item) => item.id !== session.id), session];
      onSessionsChange(nextSessions);
      resetBuffer(session.id);
      onActiveSessionChange(session);
      await focusSessionById(session.id);
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
    focusSessionById,
    onActiveSessionChange,
    onRefreshSessions,
    onSessionsChange,
    project,
    resetBuffer,
    sessions,
    t,
    worktree?.id,
  ]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   分屏必须由 tmux 创建真实 pane，移动端不能维护自己的 pane 布局模型。
   *
   * Code Logic（这个函数做什么）:
   *   调用 HTTP split-pane route，完成后刷新 session 列表以同步 paneCount。
   */
  const handleSplitPane = useCallback(
    async (direction: WorkbenchPaneSplitDirection): Promise<void> => {
      if (!visibleSession) return;
      setActionBusy(`split-${direction}`);
      setPanelError(null);
      try {
        await httpWorkbenchTransport.sessions.splitPane(visibleSession.id, direction);
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
    [onRefreshSessions, t, visibleSession],
  );

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
    removeBuffer,
    sessions,
    t,
    visibleSession,
    worktree?.id,
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
      activeSession?.id,
      onActiveSessionChange,
      onRefreshSessions,
      onSessionsChange,
      removeBuffer,
      sessions,
      t,
      worktree?.id,
    ],
  );

  const terminalBody = !project ? (
    <div className={styles.mobileTerminalEmpty}>
      {t('workbench:mobile.terminalPanel.noProject')}
    </div>
  ) : !visibleSession ? (
    <div className={styles.mobileTerminalEmpty}>
      <span>{t('workbench:mobile.terminalPanel.noSession')}</span>
      <button
        type="button"
        className={styles.mobileTerminalPrimaryButton}
        disabled={isActionDisabled}
        onClick={() => void handleCreateSession()}
      >
        <PlusIcon size={16} aria-hidden="true" />
        <span>{t('workbench:mobile.terminalPanel.newWindow')}</span>
      </button>
    </div>
  ) : null;

  return (
    <section
      className={`${styles.panel} ${styles.mobileTerminalPanel}`}
      aria-labelledby="mobile-terminal-panel-title"
    >
      <div className={styles.panelHeader}>
        <p className={styles.panelKicker}>{t('workbench:mobile.kicker')}</p>
        <h1 id="mobile-terminal-panel-title">{t('workbench:mobile.terminalPanel.title')}</h1>
      </div>

      <div className={styles.mobileTerminalToolbar}>
        <div
          className={styles.mobileTerminalTabs}
          role="tablist"
          aria-label={t('workbench:mobile.terminalPanel.tabsAriaLabel')}
        >
          {scopedSessions.map((session) => {
            const isActive = session.id === visibleSession?.id;
            return (
              <div
                key={session.id}
                className={styles.mobileSessionTab}
                data-active={isActive || undefined}
              >
                <button
                  type="button"
                  role="tab"
                  aria-selected={isActive}
                  className={styles.mobileSessionSelectButton}
                  onClick={() => handleSelectSession(session)}
                >
                  <span className={styles.mobileSessionDot} data-status={session.status} />
                  <span className={styles.mobileSessionName}>{session.name}</span>
                  <span className={styles.mobileSessionPaneCount}>
                    {t('workbench:mobile.terminalPanel.paneCount', {
                      count: session.paneCount,
                    })}
                  </span>
                </button>
                <button
                  type="button"
                  className={styles.mobileTerminalTabClose}
                  aria-label={t('workbench:mobile.terminalPanel.closeWindow')}
                  disabled={isActionDisabled}
                  onClick={(event) => {
                    event.stopPropagation();
                    void handleCloseSession(session);
                  }}
                >
                  <XIcon size={14} aria-hidden="true" />
                </button>
              </div>
            );
          })}
          <button
            type="button"
            className={styles.mobileTerminalPrimaryButton}
            disabled={!project || isActionDisabled}
            onClick={() => void handleCreateSession()}
          >
            <PlusIcon size={16} aria-hidden="true" />
            <span>{t('workbench:mobile.terminalPanel.newWindow')}</span>
          </button>
        </div>

        <div
          className={styles.mobileTerminalActions}
          aria-label={t('workbench:mobile.terminalPanel.actionsAriaLabel')}
        >
          <button
            type="button"
            className={styles.mobileTerminalActionButton}
            disabled={!canUsePaneActions || isActionDisabled}
            aria-label={t('workbench:mobile.terminalPanel.splitRight')}
            title={t('workbench:mobile.terminalPanel.splitRight')}
            onClick={() => void handleSplitPane('right')}
          >
            <SplitRightIcon size={16} aria-hidden="true" />
            <span>{t('workbench:mobile.terminalPanel.splitRight')}</span>
          </button>
          <button
            type="button"
            className={styles.mobileTerminalActionButton}
            disabled={!canUsePaneActions || isActionDisabled}
            aria-label={t('workbench:mobile.terminalPanel.splitDown')}
            title={t('workbench:mobile.terminalPanel.splitDown')}
            onClick={() => void handleSplitPane('down')}
          >
            <SplitDownIcon size={16} aria-hidden="true" />
            <span>{t('workbench:mobile.terminalPanel.splitDown')}</span>
          </button>
          <button
            type="button"
            className={styles.mobileTerminalActionButton}
            disabled={!canUsePaneActions || isActionDisabled}
            aria-label={t('workbench:mobile.terminalPanel.closePane')}
            title={t('workbench:mobile.terminalPanel.closePane')}
            onClick={() => void handleClosePane()}
          >
            <XIcon size={16} aria-hidden="true" />
            <span>{t('workbench:mobile.terminalPanel.closePane')}</span>
          </button>
        </div>
      </div>

      {busy ? <p className={styles.panelState}>{t('workbench:loading')}</p> : null}
      {panelError ? (
        <p className={styles.panelError}>
          <span>{t('workbench:mobile.projectPanel.error')}</span>
          <span>{panelError}</span>
        </p>
      ) : null}

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
        </div>
      </div>
    </section>
  );
}
