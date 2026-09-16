import { useEffect, useRef } from 'react';
import type { MutableRefObject, ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { FitAddon } from '@xterm/addon-fit';
import { Terminal } from '@xterm/xterm';
import '@xterm/xterm/css/xterm.css';
import { httpWorkbenchTransport } from '@/api/workbenchHttp';
import {
  MAX_WORKBENCH_TERMINAL_BUFFER_CHARS,
  type TerminalBufferDelta,
} from '@/hooks/workbenchTerminalBuffer';
import type { WorkbenchTerminalBufferStore } from '@/hooks/workbenchTerminalBuffer';
import { classifyTerminalReplayError } from '@/hooks/workbenchTerminalBuffer';
import type { WorkbenchSession } from '@/lib/types';
import {
  createTerminalLiveWriter,
  type TerminalLiveWriter,
} from '@/pages/Workbench/terminalLiveWriter';
import { scrollTerminalBufferLines } from '@/pages/Workbench/terminalWheel';
import { installWorkbenchTerminalSelectionOverrides } from '@/pages/Workbench/terminalSelectionOverrides';
import { workbenchTerminalOptions, workbenchTerminalTheme } from '@/pages/Workbench/terminalOptions';
import {
  clipboardEventImageFile,
  fileToPngDataUrl,
} from '@/pages/Workbench/terminalImagePaste';
import {
  appendHeldLiveAfterReplay,
  shouldForwardMobileTerminalInput,
} from '../mobileTerminalReplay';
import {
  beginMobileTerminalResumePin,
  isMobileTerminalResumePinned,
  isMobileTerminalResumeVisible,
  scrollMobileTerminalToLatest,
  shouldFollowMobileTerminalToLatest,
  type MobileTerminalResumePin,
} from '../mobileTerminalResumeFollow';
import {
  beginMobileTerminalTouchScroll,
  encodeMobileTerminalWheelReports,
  mobileTerminalTouchLineHeight,
  resolveMobileTerminalScrollMode,
  updateMobileTerminalTouchScroll,
  type MobileTerminalTouchScrollState,
} from '../mobileTerminalTouchScroll';
import {
  clearMobileTerminalHelperTextareaAfterCommit,
  enterMobileTerminalTypingMode,
  findMobileTerminalHelperTextarea,
  leaveMobileTerminalTypingMode,
  MOBILE_TERMINAL_EXTRA_KEY_LONG_PRESS_MS,
  applyStickyModifierToInput,
  type MobileTerminalStickyModifier,
} from '../mobileTerminalExtraKeys';
import {
  beginPress,
  cellsToXtermSelect,
  countSelectedLines,
  dragSelecting,
  edgeScrollDelta,
  MOBILE_TERMINAL_SELECT_MOVE_PX,
  noteMove,
  pointerToCell,
  resetGesture,
  shouldBecomeScrolling,
  shouldEnterSelecting,
  startSelecting,
  travelPx,
  type MobileTerminalGestureState,
} from '../mobileTerminalSelection';
import type { MobileTerminalInputStream } from '../mobileTerminalInputStream';
import {
  notifyMobileKeyboardAnchorChanged,
  readDocumentMobileKeyboardShiftPx,
  resolveUnshiftedMobileKeyboardAnchorTop,
  stampMobileKeyboardAnchorTop,
} from '../mobileWorkbenchState';
import styles from '../MobileWorkbench.module.css';

const MIN_TERMINAL_COLS = 20;
const MIN_TERMINAL_ROWS = 6;
const DEFAULT_TERMINAL_SIZE = { cols: 80, rows: 24 };
const SCROLLBACK_HYDRATION_TIMEOUT_MS = 10_000;

function clampU16(value: number, min: number): number {
  if (!Number.isFinite(value)) return min;
  const rounded = Math.round(value);
  return Math.min(65535, Math.max(min, rounded));
}

function isExpectedClosedSessionError(reason: unknown): boolean {
  return classifyTerminalReplayError(reason) === 'not_found';
}

function getErrorMessage(reason: unknown, fallback: string): string {
  if (reason instanceof Error && reason.message.trim()) return reason.message;
  const message = String(reason);
  return message && message !== 'undefined' && message !== 'null' ? message : fallback;
}

export interface MobileTerminalSelectingPatch {
  selecting: boolean;
  selectedLineCount?: number;
  selectionEmpty?: boolean;
  clearCopied?: boolean;
}

export interface MobileTerminalXtermSlotProps {
  session: WorkbenchSession;
  visible: boolean;
  store: WorkbenchTerminalBufferStore;
  inputEnabled: boolean;
  activeAgentIdentity: string | null;
  inputStreamRef: MutableRefObject<MobileTerminalInputStream | null>;
  stickyModifierRef: MutableRefObject<MobileTerminalStickyModifier | null>;
  stickyTimeoutRef: MutableRefObject<number | null>;
  selectingRef: MutableRefObject<boolean>;
  onError: (message: string | null) => void;
  onSelectingChange: (patch: MobileTerminalSelectingPatch) => void;
  onBindSurface: (terminal: Terminal | null, viewport: HTMLDivElement | null) => void;
  onStickyConsumed: () => void;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   切项目/切窗口时要留下已挂载的 xterm，避免每次 HTTP replay。
 *
 * Code Logic（这个组件做什么）:
 *   每个 session 一份 Terminal；visible=false 时隐藏但不断开 replay/live writer。
 */
export function MobileTerminalXtermSlot({
  session,
  visible,
  store,
  inputEnabled,
  activeAgentIdentity,
  inputStreamRef,
  stickyModifierRef,
  stickyTimeoutRef,
  selectingRef,
  onError,
  onSelectingChange,
  onBindSurface,
  onStickyConsumed,
}: MobileTerminalXtermSlotProps): ReactElement {
  const { t } = useTranslation(['workbench']);
  const viewportRef = useRef<HTMLDivElement | null>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const replayGateRef = useRef<boolean>(false);
  const replayReadyRef = useRef<boolean>(false);
  const inputEnabledRef = useRef(inputEnabled);
  const visibleRef = useRef(visible);
  const onErrorRef = useRef(onError);
  const onSelectingChangeRef = useRef(onSelectingChange);
  const onBindSurfaceRef = useRef(onBindSurface);
  const onStickyConsumedRef = useRef(onStickyConsumed);
  const resizeFnRef = useRef<(() => void) | null>(null);
  const lastResizeRef = useRef<{ sessionId: string; cols: number; rows: number } | null>(null);
  const persistedSessionSizeRef = useRef<{
    sessionId: string;
    cols: number;
    rows: number;
  } | null>(null);
  const resizeTimerRef = useRef<number | null>(null);
  const replayRequestIdRef = useRef<number>(0);
  const touchScrollStateRef = useRef<MobileTerminalTouchScrollState | null>(null);
  const gestureRef = useRef<MobileTerminalGestureState>(resetGesture());
  const longPressTimerRef = useRef<number | null>(null);
  const hydratedScrollbackSessionRef = useRef<string | null>(null);
  const activeAgentIdentityRef = useRef<string | null>(null);

  const sessionId = session.id;

  useEffect(() => {
    inputEnabledRef.current = inputEnabled;
    visibleRef.current = visible;
    onErrorRef.current = onError;
    onSelectingChangeRef.current = onSelectingChange;
    onBindSurfaceRef.current = onBindSurface;
    onStickyConsumedRef.current = onStickyConsumed;
    persistedSessionSizeRef.current = {
      sessionId,
      cols: session.cols ?? DEFAULT_TERMINAL_SIZE.cols,
      rows: session.rows ?? DEFAULT_TERMINAL_SIZE.rows,
    };
  }, [
    inputEnabled,
    onBindSurface,
    onError,
    onSelectingChange,
    onStickyConsumed,
    session.cols,
    session.rows,
    sessionId,
    visible,
  ]);

  useEffect(() => {
    if (activeAgentIdentity && activeAgentIdentityRef.current !== activeAgentIdentity) {
      hydratedScrollbackSessionRef.current = null;
    }
    activeAgentIdentityRef.current = activeAgentIdentity;
  }, [activeAgentIdentity]);

  useEffect(() => {
    const viewport = viewportRef.current;
    if (!viewport || !sessionId) return undefined;

    const terminal = new Terminal(workbenchTerminalOptions());
    const fit = new FitAddon();
    const requestId = replayRequestIdRef.current + 1;
    let disposed = false;
    let liveWriter: TerminalLiveWriter | null = null;
    let scrollbackHydrationPending = false;
    let hydrationRequestStarted = false;
    let hydrationSnapshotPendingParse = false;
    let hydrationAutoScrollCancelled = false;
    let pendingHydratedScrollLines = 0;
    let hydrationAgentIdentity: string | null = null;
    let hydrationExpectedGeneration: number | null = null;
    let hydrationRequestToken = 0;
    let hydrationAbortController: AbortController | null = null;
    let hydrationTimeout: number | null = null;
    let resumePin: MobileTerminalResumePin | null = null;
    let resumeRaf: number | null = null;
    replayRequestIdRef.current = requestId;
    replayReadyRef.current = false;
    replayGateRef.current = false;
    // hydration 标记只属于上一份 xterm 实例；即使 sessionId 相同（语言切换、running 状态恢复等）
    // 也必须让新实例在首次回看历史时重新取得 owner 端权威快照，不能信任旧实例的 baseY。
    hydratedScrollbackSessionRef.current = null;
    terminal.loadAddon(fit);
    terminal.open(viewport);
    const restoreSelectionOverrides = installWorkbenchTerminalSelectionOverrides(terminal);
    terminalRef.current = terminal;
    // 后端把同尺寸 resize 也视为“强制重绘”，会通过 rows 抖动让 Claude TUI 的末屏再次
    // 落入 tmux history。以持久化尺寸作为本 xterm 的已上报基线，首次 fit 相同就不回传。
    const persistedSize = persistedSessionSizeRef.current;
    if (lastResizeRef.current?.sessionId !== sessionId) {
      lastResizeRef.current =
        persistedSize?.sessionId === sessionId
          ? {
              sessionId,
              cols: clampU16(persistedSize.cols, MIN_TERMINAL_COLS),
              rows: clampU16(persistedSize.rows, MIN_TERMINAL_ROWS),
            }
          : null;
    }
    // 默认离开打字态：系统键盘只在用户明确点击终端输入区后出现。
    if (visibleRef.current) {
      leaveMobileTerminalTypingMode(findMobileTerminalHelperTextarea(viewport), null);
    }

    /**
     * Business Logic（为什么需要这个函数）:
     *   hydration 失败、超时、换 session 或解析完成后必须释放单飞门闩，否则后续触摸永远不能重试。
     *
     * Code Logic（这个函数做什么）:
     *   可选中止在途 HTTP；清空累计滚动、Agent 身份、AbortController 与 10 秒 timer。
     */
    const clearScrollbackHydration = (abortRequest = false): void => {
      if (abortRequest) {
        hydrationRequestToken += 1;
        hydrationAbortController?.abort();
      }
      hydrationAbortController = null;
      scrollbackHydrationPending = false;
      hydrationRequestStarted = false;
      hydrationSnapshotPendingParse = false;
      hydrationAutoScrollCancelled = false;
      pendingHydratedScrollLines = 0;
      hydrationAgentIdentity = null;
      hydrationExpectedGeneration = null;
      if (hydrationTimeout !== null) {
        window.clearTimeout(hydrationTimeout);
        hydrationTimeout = null;
      }
    };

    /**
     * Business Logic（为什么需要这个函数）:
     *   HTTP 返回不代表长历史已经被 xterm 解析；必须等待 writer 的 write callback 后再执行用户原手势。
     *
     * Code Logic（这个函数做什么）:
     *   复核当前 xterm/session/Agent/mouse mode，标记本实例已 hydration，并用绝对 scrollToLine
     *   应用累计的向历史方向滚动行数。
     */
    const scrollWhenHydrationParsed = (): void => {
      if (
        !scrollbackHydrationPending ||
        !hydrationSnapshotPendingParse ||
        terminalRef.current !== terminal ||
        hydrationAgentIdentity !== activeAgentIdentityRef.current ||
        hydrationExpectedGeneration !== store.getSnapshot(sessionId).cursor.generation
      ) {
        if (scrollbackHydrationPending && hydrationSnapshotPendingParse) {
          clearScrollbackHydration();
        }
        return;
      }
      const active = terminal.buffer.active;
      if (active.type !== 'normal' || terminal.modes.mouseTrackingMode !== 'none') {
        clearScrollbackHydration();
        return;
      }
      const lines = Math.min(-1, pendingHydratedScrollLines);
      if (active.baseY <= 0) {
        clearScrollbackHydration();
        return;
      }
      hydratedScrollbackSessionRef.current = sessionId;
      const shouldAutoScroll = !hydrationAutoScrollCancelled;
      clearScrollbackHydration();
      if (shouldAutoScroll) {
        scrollTerminalBufferLines(terminal, lines);
      }
    };

    /**
     * Business Logic（为什么需要这个函数）:
     *   resume 的旧消息只在 tmux owner 内；移动端首次回看历史必须显式 capture，并在网络往返期间
     *   保住已经推进事件 cursor 的 exact live delta。
     *
     * Code Logic（这个函数做什么）:
     *   initial replay 就绪后单飞调用 refreshHistory replay；listener-first 暂存同 authority 的 live；
     *   成功后以 historyHydration 原因 forceReplace store，由既有 writer 完整重放权威快照。
     */
    const startHydrationRequest = (): void => {
      if (
        disposed ||
        !scrollbackHydrationPending ||
        hydrationRequestStarted ||
        !replayReadyRef.current
      ) {
        return;
      }
      hydrationRequestStarted = true;
      const hydrationToken = hydrationRequestToken + 1;
      hydrationRequestToken = hydrationToken;
      hydrationAbortController = new AbortController();
      onErrorRef.current(null);

      const heldLive: TerminalBufferDelta[] = [];
      let heldLiveChars = 0;
      let heldLiveOverflow = false;
      const unsubscribeHydrationHeldLive = store.subscribeLive(sessionId, (delta) => {
        if (heldLiveOverflow) return;
        heldLiveChars += delta.chunk.length;
        if (heldLiveChars > MAX_WORKBENCH_TERMINAL_BUFFER_CHARS) {
          heldLive.length = 0;
          heldLiveOverflow = true;
          return;
        }
        heldLive.push(delta);
      });
      hydrationTimeout = window.setTimeout(() => {
        hydrationAbortController?.abort();
      }, SCROLLBACK_HYDRATION_TIMEOUT_MS);

      void httpWorkbenchTransport.sessions
        .hydrateScrollback(sessionId, hydrationAbortController.signal)
        .then((replay) => {
          if (
            disposed ||
            replayRequestIdRef.current !== requestId ||
            hydrationRequestToken !== hydrationToken ||
            !scrollbackHydrationPending
          ) {
            return;
          }
          if (heldLiveOverflow) {
            throw new Error(t('workbench:errors.sessions'));
          }
          if (hydrationTimeout !== null) {
            window.clearTimeout(hydrationTimeout);
            hydrationTimeout = null;
          }
          hydrationAbortController = null;
          const hydrated = appendHeldLiveAfterReplay(
            replay.buffer,
            replay.lastSeq,
            replay.ownerInstanceId,
            heldLive,
          );
          hydrationExpectedGeneration =
            store.getSnapshot(sessionId).cursor.generation + 1;
          store.reset(
            sessionId,
            hydrated.buffer,
            hydrated.lastSeq,
            replay.ownerInstanceId,
            { forceReplace: true, reason: 'historyHydration' },
          );
          if (scrollbackHydrationPending) {
            hydrationExpectedGeneration = store.getSnapshot(sessionId).cursor.generation;
          }
        })
        .catch((reason) => {
          if (
            disposed ||
            replayRequestIdRef.current !== requestId ||
            hydrationRequestToken !== hydrationToken
          ) {
            return;
          }
          clearScrollbackHydration();
          if (isExpectedClosedSessionError(reason)) return;
          onErrorRef.current(
            `${t('workbench:mobile.terminalPanel.errors.replay')}: ${getErrorMessage(
              reason,
              t('workbench:errors.sessions'),
            )}`,
          );
        })
        .finally(() => {
          unsubscribeHydrationHeldLive();
        });
    };

    /**
     * Business Logic（为什么需要这个函数）:
     *   initial replay 可能仍在网络中；首次触摸应先累计意图，等 writer ready 后再 capture，禁止两份
     *   baseline 乱序覆盖。后续 touchmove 只能复用同一请求。
     *
     * Code Logic（这个函数做什么）:
     *   首次进入 queued 状态并记录 Agent 身份；随后尝试启动请求，未 ready 时由 snapshot callback 续跑。
     */
    const hydrateScrollback = (): void => {
      if (!scrollbackHydrationPending) {
        scrollbackHydrationPending = true;
        hydrationRequestStarted = false;
        hydrationSnapshotPendingParse = false;
        hydrationAutoScrollCancelled = false;
        hydrationAgentIdentity = activeAgentIdentityRef.current;
        hydrationExpectedGeneration = null;
      }
      startHydrationRequest();
    };

    /**
     * Business Logic（为什么需要这个函数）:
     *   手机旋转、地址栏收缩或分屏后需要同步 xterm 与后端 PTY 尺寸。
     *
     * Code Logic（这个函数做什么）:
     *   执行 FitAddon.fit，clamp cols/rows，并在尺寸变化时调用 HTTP resize route。
     */
    const resizeTerminal = (): void => {
      if (disposed || !visibleRef.current) return;
      try {
        fit.fit();
        const cols = clampU16(terminal.cols, MIN_TERMINAL_COLS);
        const rows = clampU16(terminal.rows, MIN_TERMINAL_ROWS);
        const last = lastResizeRef.current;
        if (last?.sessionId === sessionId && last.cols === cols && last.rows === rows) return;
        lastResizeRef.current = { sessionId, cols, rows };
        void httpWorkbenchTransport.sessions.resize(sessionId, cols, rows).catch((reason) => {
          if (disposed) return;
          if (isExpectedClosedSessionError(reason)) return;
          onErrorRef.current(
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

    /**
     * Business Logic（为什么需要这个监听器）:
     *   手机长按粘贴走浏览器 paste 事件；xterm helper textarea 不会把图片交给 Agent。
     *   必须拦截 image file，经 HTTP paste-image 写 owning device 剪贴板再注入 Ctrl+V。
     *
     * Code Logic（这个监听器做什么）:
     *   capture 阶段取 image file → PNG data URL → sessions.pasteImage；纯文本放行给 xterm。
     */
    const handlePaste = (event: ClipboardEvent): void => {
      if (!visibleRef.current || !inputEnabledRef.current) return;
      const file = clipboardEventImageFile(event);
      if (!file) return;
      event.preventDefault();
      event.stopPropagation();
      void fileToPngDataUrl(file)
        .then((dataUrl) => httpWorkbenchTransport.sessions.pasteImage(sessionId, dataUrl))
        .catch((reason) => {
          if (disposed) return;
          onErrorRef.current(
            `${t('workbench:errors.pasteImage')}: ${getErrorMessage(
              reason,
              t('workbench:errors.pasteImage'),
            )}`,
          );
        });
    };
    viewport.addEventListener('paste', handlePaste, true);

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
        onStickyConsumedRef.current();
      }
      try {
        inputStreamRef.current?.enqueue(sessionId, stickyResult.data);
      } catch (reason) {
        if (disposed) return;
        onErrorRef.current(
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
     *   长按约 400ms 且几乎未移动则进入自管划选，不能弹软键盘或把长按交给 helper textarea。
     *
     * Code Logic（这个函数做什么）:
     *   在 capture 阶段拦截单指手势：pressPending 超 8px 走原 scrollLines/SGR；定时器命中则
     *   startSelecting + terminal.select；划选中只按边缘增量滚历史。轻点才进入打字态。
     */
    let touchMoved = false;
    let suppressClickAfterScroll = false;
    let touchStartY = 0;
    let lastClientX = 0;
    let lastClientY = 0;
    let reanchorPending = false;

    /**
     * Business Logic（为什么需要这个函数）:
     *   长按计时必须能被滚动、抬手和卸终端打断，否则会在滑动后误进划选。
     *
     * Code Logic（这个函数做什么）:
     *   若 longPressTimerRef 有值则 clearTimeout 并置 null。
     */
    const clearLongPressTimer = (): void => {
      if (longPressTimerRef.current !== null) {
        window.clearTimeout(longPressTimerRef.current);
        longPressTimerRef.current = null;
      }
    };

    /**
     * Business Logic（为什么需要这个函数）:
     *   划选高亮必须跟着手指锚点/焦点走 xterm 画布选区，底栏也要同步行数与空选禁用。
     *
     * Code Logic（这个函数做什么）:
     *   仅 selecting 且有 anchor/focus 时 cellsToXtermSelect 后 terminal.select，并写入行数/空选 state。
     */
    const applySelectFromGesture = (): void => {
      const state = gestureRef.current;
      if (state.phase !== 'selecting' || !state.anchor || !state.focus) return;
      const range = cellsToXtermSelect(state.anchor, state.focus, terminal.cols);
      terminal.select(range.column, range.row, range.length);
      onSelectingChangeRef.current({
        selecting: true,
        selectedLineCount: countSelectedLines(state.anchor, state.focus),
        selectionEmpty: terminal.getSelection().length === 0,
      });
    };

    /**
     * Business Logic（为什么需要这个函数）:
     *   长按成立或再按重锚时要立刻进入划选：收起软键盘、钉住格子、抑制随后的 click。
     *
     * Code Logic（这个函数做什么）:
     *   startSelecting(pointerToCell)，leaveMobileTerminalTypingMode，applySelect，set selecting。
     */
    const enterSelectingAtPointer = (clientX: number, clientY: number): void => {
      if (disposed) return;
      const rect = viewport.getBoundingClientRect();
      const cell = pointerToCell(
        clientX,
        clientY,
        rect,
        terminal.cols,
        terminal.rows,
        terminal.buffer.active.viewportY,
      );
      gestureRef.current = startSelecting(gestureRef.current, cell);
      selectingRef.current = true;
      onSelectingChangeRef.current({ selecting: true, clearCopied: true });
      leaveMobileTerminalTypingMode(findMobileTerminalHelperTextarea(viewport), null);
      applySelectFromGesture();
      suppressClickAfterScroll = true;
    };

    /**
     * Business Logic（为什么需要这个函数）:
     *   长按判定与 extra keys `/` 共用 400ms，才能让用户用同一套肌肉记忆进入划选或重锚。
     *
     * Code Logic（这个函数做什么）:
     *   清旧 timer 后 setTimeout；到期且 travel 仍应进选区时 enterSelectingAtPointer。
     */
    const armLongPressTimer = (): void => {
      clearLongPressTimer();
      longPressTimerRef.current = window.setTimeout(() => {
        longPressTimerRef.current = null;
        if (disposed) return;
        const state = gestureRef.current;
        const travel = travelPx(state.originX, state.originY, lastClientX, lastClientY);
        if (!shouldEnterSelecting(MOBILE_TERMINAL_EXTRA_KEY_LONG_PRESS_MS, travel)) {
          return;
        }
        enterSelectingAtPointer(lastClientX, lastClientY);
        reanchorPending = false;
      }, MOBILE_TERMINAL_EXTRA_KEY_LONG_PRESS_MS);
    };

    const handleTouchMove = (event: TouchEvent): void => {
      if (!visibleRef.current) return;
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
      lastClientX = touch.clientX;
      lastClientY = touch.clientY;
      if (selectingRef.current) {
        const travel = travelPx(
          gestureRef.current.originX,
          gestureRef.current.originY,
          touch.clientX,
          touch.clientY,
        );
        if (reanchorPending) {
          if (!shouldBecomeScrolling(travel)) {
            return;
          }
          reanchorPending = false;
          clearLongPressTimer();
        }
        const rect = viewport.getBoundingClientRect();
        const delta = edgeScrollDelta(touch.clientY, rect, terminal.rows);
        if (delta !== 0) {
          scrollTerminalBufferLines(terminal, delta);
        }
        const cell = pointerToCell(
          touch.clientX,
          touch.clientY,
          rect,
          terminal.cols,
          terminal.rows,
          terminal.buffer.active.viewportY,
        );
        gestureRef.current = dragSelecting(gestureRef.current, cell);
        applySelectFromGesture();
        suppressClickAfterScroll = true;
        return;
      }
      if (Math.abs(touch.clientY - touchStartY) > MOBILE_TERMINAL_SELECT_MOVE_PX) {
        touchMoved = true;
      }
      const nextGesture = noteMove(gestureRef.current, touch.clientX, touch.clientY);
      gestureRef.current = nextGesture;
      if (nextGesture.phase !== 'scrolling') {
        return;
      }
      clearLongPressTimer();
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
      if (result.lines === 0) return;
      touchMoved = true;
      resumePin = null;
      // 未权威 hydration 时不能信任 baseY（可能是末屏重绘污染）；mouse mode=none 先抓 tmux history。
      // 已 hydration 的 normal buffer 用绝对 scrollToLine；已协商 mouse 时才发 SGR 64/65。
      const activeBuffer = terminal.buffer.active;
      const mouseTrackingMode = terminal.modes.mouseTrackingMode;
      const scrollMode = resolveMobileTerminalScrollMode(
        activeBuffer.type,
        mouseTrackingMode,
        hydratedScrollbackSessionRef.current === sessionId,
      );
      if (scrollMode === 'scrollback') {
        scrollTerminalBufferLines(terminal, result.lines);
        return;
      }
      if (scrollMode === 'hydrateScrollback') {
        // 位于底部时只有负数代表查看更旧内容；其余方向不触发昂贵 capture。
        if (result.lines >= 0) return;
        pendingHydratedScrollLines = Math.max(
          -Math.max(1, terminal.rows),
          pendingHydratedScrollLines + result.lines,
        );
        hydrateScrollback();
        return;
      }
      if (!inputEnabledRef.current) return;
      // 触点换算为 1-based 字符格，贴近桌面滚轮落点；失败回落 1,1。
      const rect = viewport.getBoundingClientRect();
      const cellW = rect.width / Math.max(terminal.cols, 1);
      const cellH = rect.height / Math.max(terminal.rows, 1);
      const col =
        cellW > 0 ? Math.min(terminal.cols, Math.max(1, Math.floor((touch.clientX - rect.left) / cellW) + 1)) : 1;
      const row =
        cellH > 0 ? Math.min(terminal.rows, Math.max(1, Math.floor((touch.clientY - rect.top) / cellH) + 1)) : 1;
      const wheel = encodeMobileTerminalWheelReports(result.lines, col, row);
      if (!wheel) return;
      try {
        inputStreamRef.current?.enqueue(sessionId, wheel);
      } catch (reason) {
        if (disposed) return;
        onErrorRef.current(
          `${t('workbench:mobile.terminalPanel.errors.write')}: ${getErrorMessage(
            reason,
            t('workbench:errors.sessions'),
          )}`,
        );
      }
    };
    const handleTouchStart = (event: TouchEvent): void => {
      if (!visibleRef.current) return;
      touchMoved = false;
      suppressClickAfterScroll = false;
      const touch = event.touches.length === 1 ? event.touches[0] : null;
      touchStartY = touch?.clientY ?? 0;
      lastClientX = touch?.clientX ?? 0;
      lastClientY = touch?.clientY ?? 0;
      if (!touch) {
        clearLongPressTimer();
        touchScrollStateRef.current = null;
        return;
      }
      if (selectingRef.current) {
        reanchorPending = true;
        gestureRef.current = {
          ...gestureRef.current,
          originX: touch.clientX,
          originY: touch.clientY,
        };
        armLongPressTimer();
        touchScrollStateRef.current = null;
        return;
      }
      reanchorPending = false;
      gestureRef.current = beginPress(touch.clientX, touch.clientY);
      armLongPressTimer();
      touchScrollStateRef.current = beginMobileTerminalTouchScroll(touch.clientY);
    };
    /**
     * Business Logic（为什么需要这个函数）:
     *   系统键盘只应在用户明确点击终端输入区后出现；滑动滚动不得弹出键盘。
     *
     * Code Logic（这个函数做什么）:
     *   去掉 helper readonly/inputmode，写入点击锚点并通知 shell 重算键盘上移，再 terminal.focus()。
     */
    const enterTypingFromUserGesture = (clientY: number): void => {
      if (disposed || selectingRef.current) return;
      const helper = findMobileTerminalHelperTextarea(viewport);
      enterMobileTerminalTypingMode(helper);
      const appliedShift = readDocumentMobileKeyboardShiftPx(
        getComputedStyle(document.documentElement).getPropertyValue('--mobile-keyboard-shift'),
      );
      stampMobileKeyboardAnchorTop(
        helper,
        resolveUnshiftedMobileKeyboardAnchorTop(clientY, appliedShift),
      );
      notifyMobileKeyboardAnchorChanged();
      try {
        terminal.focus();
      } catch {
        // xterm 在 dispose 窗口期可能 focus 失败，忽略。
      }
    };
    const handleTouchEnd = (): void => {
      const wasScroll = touchMoved || gestureRef.current.phase === 'scrolling';
      touchScrollStateRef.current = null;
      touchMoved = false;
      clearLongPressTimer();
      reanchorPending = false;
      // 划选抬手必须留在选区；滑动只滚动；随后合成 click 也必须抑制，避免滚完又弹键盘。
      if (selectingRef.current) {
        suppressClickAfterScroll = true;
        return;
      }
      gestureRef.current = resetGesture();
      if (wasScroll) {
        suppressClickAfterScroll = true;
        return;
      }
      enterTypingFromUserGesture(lastClientY);
    };
    const handleTouchCancel = (): void => {
      clearLongPressTimer();
      reanchorPending = false;
      touchScrollStateRef.current = null;
      touchMoved = false;
      suppressClickAfterScroll = true;
      if (!selectingRef.current) {
        gestureRef.current = resetGesture();
      }
      if (scrollbackHydrationPending) {
        hydrationAutoScrollCancelled = true;
      }
    };
    const handleViewportClick = (event: MouseEvent): void => {
      if (suppressClickAfterScroll || selectingRef.current) {
        suppressClickAfterScroll = false;
        return;
      }
      // 桌面/无 touch 调试；手机轻点与 touchend 幂等。
      enterTypingFromUserGesture(event.clientY);
    };
    /**
     * Business Logic（为什么需要这个监听器）:
     *   系统长按菜单/选区手柄会抢走自管划选，必须在 capture 阶段吞掉 contextmenu。
     *
     * Code Logic（这个监听器做什么）:
     *   preventDefault，不改其它状态。
     */
    const handleContextMenu = (event: Event): void => {
      event.preventDefault();
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
    viewport.addEventListener('contextmenu', handleContextMenu, true);

    const observer = new ResizeObserver(() => {
      if (resizeTimerRef.current !== null) {
        window.clearTimeout(resizeTimerRef.current);
      }
      resizeTimerRef.current = window.setTimeout(resizeTerminal, 80);
    });
    observer.observe(viewport);
    resizeTerminal();

    const applyTheme = (): void => {
      terminal.options.theme = workbenchTerminalTheme();
    };
    window.addEventListener('cp-theme-change', applyTheme);
    window.addEventListener('storage', applyTheme);

    /**
     * Business Logic（为什么需要这个函数）:
     *   回到前台时必须把视口钉在最新输出；划选复制中途不能抢滚动。
     *
     * Code Logic（这个函数做什么）:
     *   可见、未划选且无选区时用绝对 scrollToLine(baseY) 跟到底。
     */
    const followLatestIfAllowed = (): void => {
      if (disposed || !visibleRef.current || terminalRef.current !== terminal) return;
      if (
        !shouldFollowMobileTerminalToLatest({
          visible: isMobileTerminalResumeVisible(document.visibilityState),
          selecting: selectingRef.current,
          hasSelection: terminal.getSelection().length > 0,
        })
      ) {
        return;
      }
      scrollMobileTerminalToLatest(terminal);
      // xterm 6 DOM renderer 在 selection 失效时重建全部行；空选区不改变用户状态。
      terminal.clearSelection();
    };

    /**
     * Business Logic（为什么需要这个函数）:
     *   后台冻结后首帧 buffer 可能仍旧，事件流重连还会再灌一段 catch-up，需要 pin + 双 rAF。
     *
     * Code Logic（这个函数做什么）:
     *   仅在 document 可见时开启 pin 窗口，合并同一帧请求，下一帧后再跟到底。
     */
    const scheduleResumeFollow = (): void => {
      if (disposed || !visibleRef.current) return;
      if (!isMobileTerminalResumeVisible(document.visibilityState)) return;
      if (selectingRef.current) return;
      resumePin = beginMobileTerminalResumePin(Date.now());
      if (resumeRaf !== null) {
        window.cancelAnimationFrame(resumeRaf);
      }
      resumeRaf = window.requestAnimationFrame(() => {
        resumeRaf = window.requestAnimationFrame(() => {
          resumeRaf = null;
          followLatestIfAllowed();
        });
      });
    };

    document.addEventListener('visibilitychange', scheduleResumeFollow);
    window.addEventListener('pageshow', scheduleResumeFollow);
    const unsubscribeResumeLive = store.subscribeLive(sessionId, () => {
      if (!isMobileTerminalResumePinned(resumePin, Date.now())) return;
      followLatestIfAllowed();
    });

    // 必须先于 live writer 注册：historyHydration reset 先把本地 alternate/RIS 恢复成 normal，
    // writer 随后才 clear + replay 完整快照；该 reset 不会写入 tmux/Claude。
    const unsubscribeHydrationReset = store.subscribeReset(sessionId, (event) => {
      if (!scrollbackHydrationPending) return;
      if (event.reason === 'historyHydration') {
        hydrationSnapshotPendingParse = true;
        if (terminal.buffer.active.type === 'alternate') {
          terminal.reset();
        }
        return;
      }
      if (hydrationRequestStarted) {
        clearScrollbackHydration(true);
      }
    });

    // listener-first：HTTP replay 网络往返期间到达的 exact live delta 必须暂存；event stream cursor
    // 已推进后后端不会保证再次发送，cutover 时只能按 owner/seq 去重后接到 replay 尾部。
    const heldLive: TerminalBufferDelta[] = [];
    let heldLiveChars = 0;
    let heldLiveOverflow = false;
    const unsubscribeHeldLive = store.subscribeLive(sessionId, (delta) => {
      if (liveWriter || heldLiveOverflow) return;
      heldLiveChars += delta.chunk.length;
      if (heldLiveChars > MAX_WORKBENCH_TERMINAL_BUFFER_CHARS) {
        heldLive.length = 0;
        heldLiveOverflow = true;
        return;
      }
      heldLive.push(delta);
    });

    void httpWorkbenchTransport.sessions
      .replay(sessionId)
      .then((replay) => {
        if (disposed || replayRequestIdRef.current !== requestId) return;
        const initial = heldLiveOverflow
          ? { buffer: replay.buffer, lastSeq: replay.lastSeq }
          : appendHeldLiveAfterReplay(
              replay.buffer,
              replay.lastSeq,
              replay.ownerInstanceId,
              heldLive,
            );
        // 先把完整 HTTP replay 设为 store baseline，再创建 listener-first live writer：writer 会先订阅
        // exact delta 再读 snapshot，因此不会在 reset 与订阅之间漏掉 NDJSON 增量。
        store.reset(sessionId, initial.buffer, initial.lastSeq, replay.ownerInstanceId);
        liveWriter = createTerminalLiveWriter({
          terminal,
          source: store,
          sessionId,
          gate: replayGateRef,
          resetStrategy: 'preserveScrollback',
          onSnapshotComplete: scrollWhenHydrationParsed,
        });
        unsubscribeHeldLive();
        replayReadyRef.current = true;
        startHydrationRequest();
      })
      .catch((reason) => {
        if (disposed || replayRequestIdRef.current !== requestId) return;
        // replay 失败仍从 listener-first store snapshot 启动，后续 exact live delta 可继续显示；
        // 不把完整 buffer 放进 React diff，也不 clear 已有 scrollback。
        liveWriter = createTerminalLiveWriter({
          terminal,
          source: store,
          sessionId,
          gate: replayGateRef,
          resetStrategy: 'preserveScrollback',
          onSnapshotComplete: scrollWhenHydrationParsed,
        });
        unsubscribeHeldLive();
        replayReadyRef.current = true;
        startHydrationRequest();
        if (isExpectedClosedSessionError(reason)) return;
        onErrorRef.current(
          `${t('workbench:mobile.terminalPanel.errors.replay')}: ${getErrorMessage(
            reason,
            t('workbench:errors.sessions'),
          )}`,
        );
      });

    resizeFnRef.current = resizeTerminal;
    if (visibleRef.current) {
      onBindSurfaceRef.current(terminal, viewport);
    }

    return () => {
      disposed = true;
      clearLongPressTimer();
      restoreSelectionOverrides();
      if (visibleRef.current) {
        selectingRef.current = false;
        gestureRef.current = resetGesture();
        onSelectingChangeRef.current({ selecting: false, selectionEmpty: true });
      }
      clearScrollbackHydration(true);
      observer.disconnect();
      dataDisposable.dispose();
      document.removeEventListener('visibilitychange', scheduleResumeFollow);
      window.removeEventListener('pageshow', scheduleResumeFollow);
      if (resumeRaf !== null) {
        window.cancelAnimationFrame(resumeRaf);
        resumeRaf = null;
      }
      unsubscribeResumeLive();
      viewport.removeEventListener('touchstart', handleTouchStart, touchListenerOptions);
      viewport.removeEventListener('touchmove', handleTouchMove, touchListenerOptions);
      viewport.removeEventListener('touchend', handleTouchEnd, touchListenerOptions);
      viewport.removeEventListener('touchcancel', handleTouchCancel, touchListenerOptions);
      viewport.removeEventListener('click', handleViewportClick);
      viewport.removeEventListener('contextmenu', handleContextMenu, true);
      viewport.removeEventListener('paste', handlePaste, true);
      window.removeEventListener('cp-theme-change', applyTheme);
      window.removeEventListener('storage', applyTheme);
      touchScrollStateRef.current = null;
      if (resizeTimerRef.current !== null) {
        window.clearTimeout(resizeTimerRef.current);
        resizeTimerRef.current = null;
      }
      unsubscribeHeldLive();
      unsubscribeHydrationReset();
      liveWriter?.dispose();
      terminal.dispose();
      if (terminalRef.current === terminal) {
        terminalRef.current = null;
        onBindSurfaceRef.current(null, null);
      }
      replayGateRef.current = false;
      replayReadyRef.current = false;
      resizeFnRef.current = null;
    };
    // xterm 实例只随 session/status/store 重建；输入流与划选 ref 由面板持有且稳定。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId, session.status, store, t]);

  useEffect(() => {
    visibleRef.current = visible;
    if (!visible) return undefined;
    const terminal = terminalRef.current;
    const viewport = viewportRef.current;
    if (terminal && viewport) {
      onBindSurfaceRef.current(terminal, viewport);
    }
    const frame = window.requestAnimationFrame(() => {
      resizeFnRef.current?.();
    });
    return () => window.cancelAnimationFrame(frame);
  }, [visible]);

  return (
    <div
      className={styles.mobileTerminalHost}
      data-hidden={!visible || undefined}
      aria-hidden={!visible}
    >
      <div className={styles.mobileTerminalViewport} ref={viewportRef} />
    </div>
  );
}
