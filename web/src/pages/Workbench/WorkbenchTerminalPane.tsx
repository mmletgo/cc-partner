/**
 * Workbench 终端叶子视图（TerminalPane）—— xterm 生命周期与 PTY 输出写入的隔离组件。
 *
 * Business Logic（为什么需要这个文件）:
 *   xterm 生命周期较重，必须隔离在独立组件内，避免 Workbench 页面其他状态刷新时重复初始化终端实例。
 *   live 输出经 terminalLiveWriter 直接写 xterm，不再经 React buffer effect / full-buffer KMP。
 *
 * Code Logic（这个文件做什么）:
 *   - 暴露 WorkbenchTerminalPane（memo 组件）和 TerminalCursorAnchor / WorkbenchTerminalPaneProps 类型；
 *   - session id 变化时创建/销毁 Terminal；open 后创建 live writer 订阅 store delta；
 *   - 仅 inputEnabled=true 的 active 终端转发 onData；ResizeObserver 触发 FitAddon.fit 后把 cols/rows clamp 后回传后端；
 *   - 终端从隐藏切回可见或应用窗口恢复焦点时，下一帧强制 PTY 与 DOM 渲染层重绘。
 */
import { memo, useEffect, useRef } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { useWorkbenchTerminalBufferStore } from '@/hooks/workbenchTerminalBuffersContext';
import type { WorkbenchSession } from '@/lib/types';
import styles from './Workbench.module.css';
import { createTerminalLiveWriter, type TerminalLiveWriter } from './terminalLiveWriter';
import { workbenchTerminalOptions, workbenchTerminalTheme } from './terminalOptions';
import {
  shouldSelectPaneOnClick,
  terminalCellFromPointer,
  type TerminalCell,
} from './terminalPaneClick';
import {
  shouldForwardTerminalInput,
  type TerminalReplayGate,
} from './terminalReplay';
import { installWorkbenchTerminalSelectionOverrides } from './terminalSelectionOverrides';
import {
  consumeWorkbenchTerminalWheelLines,
  encodeTerminalSgrWheelReports,
  resolveWorkbenchTerminalWheelAction,
  type WorkbenchTerminalBufferType,
  type WorkbenchTerminalMouseTrackingMode,
} from './terminalWheel';

export interface WorkbenchTerminalPaneProps {
  session: WorkbenchSession | null;
  placeholder: string;
  /** 当前 pane 是否真正处于用户可见的终端表面，独立于是否允许键盘输入。 */
  renderVisible: boolean;
  inputEnabled: boolean;
  /** 权威 Agent Runtime 是否确认该 terminal 仍由活跃 Agent 持有虚拟 transcript。 */
  agentTranscriptActive: boolean;
  onInput: (sessionId: string, data: string) => void;
  onResize: (sessionId: string, cols: number, rows: number) => void;
  resizeRequestKey?: number;
  onCursorAnchorChange?: (anchor: TerminalCursorAnchor | null) => void;
  /**
   * Business Logic（为什么需要这个回调）:
   *   tmux 分栏是同一 xterm 网格上的分区，用户点击某个分栏时前端只能提供字符格坐标，
   *   由后端读取 tmux 真实布局完成命中并 select-pane。
   *
   * Code Logic（这个回调做什么）:
   *   仅在“未拖拽、无选区、视口在底部、允许写”的左键点击时触发，参数为 0 基 col/row。
   */
  onSelectPaneAt?: (sessionId: string, col: number, row: number) => void;
}


export interface TerminalCursorAnchor {
  left: number;
  top: number;
  bottom: number;
}

/**
 * Business Logic（为什么需要这个类型）:
 *   光标锚点换算依赖 viewport 原点与 cell 尺寸；缓存后可在高频 onCursorMove 上避免重复强制布局读取。
 *
 * Code Logic（这个类型做什么）:
 *   保存 viewport 左上角与单个 cell 的宽高（像素）。
 */
interface TerminalCursorMetrics {
  left: number;
  top: number;
  cellWidth: number;
  cellHeight: number;
}

const MIN_TERMINAL_COLS = 20;
const MIN_TERMINAL_ROWS = 6;

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
 *   Prompt 浮层需要把 xterm 光标坐标换算为 viewport 像素锚点，首次或失效后需要一次同步布局读取。
 *
 * Code Logic（这个函数做什么）:
 *   读取 viewport.getBoundingClientRect，按 terminal.cols/rows 估算 cell 尺寸并返回 metrics。
 */
function measureTerminalCursorMetrics(
  viewport: HTMLDivElement,
  terminal: Terminal,
): TerminalCursorMetrics {
  const rect = viewport.getBoundingClientRect();
  return {
    left: rect.left,
    top: rect.top,
    cellWidth: rect.width / Math.max(terminal.cols, 1),
    cellHeight: rect.height / Math.max(terminal.rows, 1),
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   浮层定位消费的是 viewport 像素坐标，而 xterm 只暴露 cell 行列。
 *
 * Code Logic（这个函数做什么）:
 *   用缓存 metrics 与 cursorX/cursorY 计算 left/top/bottom 锚点。
 */
function cursorAnchorFromMetrics(
  metrics: TerminalCursorMetrics,
  cursorX: number,
  cursorY: number,
): TerminalCursorAnchor {
  const left = metrics.left + cursorX * metrics.cellWidth;
  const top = metrics.top + cursorY * metrics.cellHeight;
  return { left, top, bottom: top + metrics.cellHeight };
}

/**
 * Business Logic（为什么需要这个组件）:
 *   xterm 生命周期较重，应隔离在独立组件内，避免页面其他状态刷新时重复初始化终端实例。
 *
 * Code Logic（这个组件做什么）:
 *   session 变化时创建/销毁 Terminal；open 后挂 live writer 直接写增量；
 *   仅 inputEnabled=true 的 active 终端转发 onData；ResizeObserver 触发 FitAddon.fit 后把 cols/rows clamp 后回传后端；
 *   renderVisible 从 false 变为 true 或应用恢复焦点时，下一帧强制 fit/PTY 与 DOM 全行重绘，不销毁 xterm 实例。
 */
export const WorkbenchTerminalPane = memo(function WorkbenchTerminalPane(props: WorkbenchTerminalPaneProps) {
  const {
    session,
    placeholder,
    renderVisible,
    inputEnabled,
    agentTranscriptActive,
    onInput,
    onResize,
    resizeRequestKey = 0,
    onCursorAnchorChange,
    onSelectPaneAt,
  } = props;
  const viewportRef = useRef<HTMLDivElement | null>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const liveWriterRef = useRef<TerminalLiveWriter | null>(null);
  const replayGateRef = useRef<TerminalReplayGate>({ current: false });
  const inputEnabledRef = useRef<boolean>(inputEnabled);
  const agentTranscriptActiveRef = useRef<boolean>(agentTranscriptActive);
  const renderVisibleRef = useRef<boolean>(renderVisible);
  const previousRenderVisibleRef = useRef<boolean>(renderVisible);
  const resizeTimerRef = useRef<number | null>(null);
  const forceResizeRef = useRef<(() => void) | null>(null);
  const cursorAnchorCallbackRef = useRef<WorkbenchTerminalPaneProps['onCursorAnchorChange']>(
    onCursorAnchorChange,
  );
  const selectPaneCallbackRef = useRef<WorkbenchTerminalPaneProps['onSelectPaneAt']>(onSelectPaneAt);
  // Business Logic: 区分“点击切换分栏”与“拖拽选中文字”，需要记住 mousedown 落在哪个字符格。
  const pointerDownCellRef = useRef<TerminalCell | null>(null);
  // Business Logic: 同格内仍可能拖出几个像素形成选区；用按下坐标算位移，避免只靠字符格误判。
  const pointerDownPointRef = useRef<{ x: number; y: number } | null>(null);
  // Business Logic: pointerup 可能早于 xterm 的 mouseup 提交选区；延迟判定用的 timer 需在 dispose 时清理。
  const selectPaneDecideTimerRef = useRef<number | null>(null);
  // Business Logic: resize/theme 后 cell 尺寸会变；缓存 metrics，避免每个 onCursorMove 强制布局。
  const cursorMetricsRef = useRef<TerminalCursorMetrics | null>(null);
  // Business Logic: 触控板小 delta 必须跨事件累计成整行，再发打在 transcript 的 SGR。
  const wheelRemainderRef = useRef(0);
  const sessionId = session?.id ?? null;
  // Business Logic: 冷恢复的后端已经按持久化尺寸 attach 并完成一次必要重绘；
  // 首次挂载只有 FitAddon 算出的可见尺寸不同才需要再次通知后端，避免同尺寸强制重绘末屏。
  const persistedSizeRef = useRef<{
    sessionId: string;
    cols: number;
    rows: number;
  } | null>(null);
  persistedSizeRef.current = session
    ? { sessionId: session.id, cols: session.cols, rows: session.rows }
    : null;
  const store = useWorkbenchTerminalBufferStore();

  useEffect(() => {
    inputEnabledRef.current = inputEnabled;
  }, [inputEnabled]);

  useEffect(() => {
    agentTranscriptActiveRef.current = agentTranscriptActive;
  }, [agentTranscriptActive]);

  useEffect(() => {
    renderVisibleRef.current = renderVisible;
  }, [renderVisible]);

  useEffect(() => {
    cursorAnchorCallbackRef.current = onCursorAnchorChange;
  }, [onCursorAnchorChange]);

  useEffect(() => {
    selectPaneCallbackRef.current = onSelectPaneAt;
  }, [onSelectPaneAt]);

  useEffect(() => {
    const viewport = viewportRef.current;
    if (!viewport || !sessionId) return undefined;

    const terminal = new Terminal(workbenchTerminalOptions());
    const fit = new FitAddon();
    terminal.loadAddon(fit);
    terminal.open(viewport);
    // open 之后 selectionService 才存在；必须优先于 mouse-mode CSI 落地前装上选区保护。
    const restoreSelectionOverrides = installWorkbenchTerminalSelectionOverrides(terminal);
    wheelRemainderRef.current = 0;
    /**
     * Business Logic（为什么需要这个函数）:
     *   Claude 新版会在 main/alternate screen 上维护自己的虚拟 transcript。只按 buffer 类型判断时，
     *   main screen 的全屏重绘会落进 xterm scrollback，向上滚动便会重复最后一屏。
     *   冷启动 attach 不会重放 Agent 先前发出的 DECSET mouse mode；此时新 xterm 虽报告 none，
     *   tmux 内仍运行的 Agent 仍期待 SGR。Agent Runtime 的活跃投影用于补回这段丢失状态。
     *   已协商 mouse 时 xterm 会按指针坐标发 SGR；resume 后输入区高度不固定，
     *   即使把坐标向上估算若干行也可能仍命中输入区，导致事件送达但 transcript 不滚。
     *   PageUp 只在 Scroll 上下文生效，Chat 输入聚焦时整页不动。
     *
     * Code Logic（这个函数做什么）:
     *   mouse tracking / 活跃 Agent 优先于 buffer 类型：命中时拦截并固定向 transcript 左上角
     *   发 SGR 64/65；仅未启用 tracking 且没有活跃 Agent 的普通 buffer 交给 xterm。
     *   转发前回到底部，退出误入的本地重绘历史。
     */
    terminal.attachCustomWheelEventHandler((event: WheelEvent) => {
      const active = terminal.buffer.active;
      const bufferType: WorkbenchTerminalBufferType =
        active.type === 'alternate' ? 'alternate' : 'normal';
      const mouseTrackingMode: WorkbenchTerminalMouseTrackingMode =
        terminal.modes.mouseTrackingMode;
      const action = resolveWorkbenchTerminalWheelAction({
        bufferType,
        baseY: active.baseY,
        mouseTrackingMode,
        agentTranscriptActive: agentTranscriptActiveRef.current,
      });
      if (action !== 'sgrFallback') return true;
      if (!shouldForwardTerminalInput(replayGateRef.current, inputEnabledRef.current)) {
        return false;
      }
      if (typeof event.preventDefault === 'function') event.preventDefault();
      if (typeof event.stopPropagation === 'function') event.stopPropagation();
      const metrics = cursorMetricsRef.current ?? measureTerminalCursorMetrics(viewport, terminal);
      cursorMetricsRef.current = metrics;
      const consumed = consumeWorkbenchTerminalWheelLines(
        event.deltaY,
        wheelRemainderRef.current,
        metrics.cellHeight,
      );
      wheelRemainderRef.current = consumed.remainder;
      if (consumed.lines === 0) return false;
      const payload = encodeTerminalSgrWheelReports(consumed.lines, 1, 1);
      if (payload) {
        terminal.scrollToBottom();
        onInput(sessionId, payload);
      }
      return false;
    });
    // inactive pane 用 display:none 丢弃 WebView 合成层；此时不得按零尺寸 fit。
    if (renderVisibleRef.current) {
      fit.fit();
    }
    cursorMetricsRef.current = null;
    pointerDownCellRef.current = null;
    pointerDownPointRef.current = null;
    if (selectPaneDecideTimerRef.current !== null) {
      window.clearTimeout(selectPaneDecideTimerRef.current);
      selectPaneDecideTimerRef.current = null;
    }

    /**
     * Business Logic（为什么需要这个函数）:
     *   xterm 字符网格作为分栏切换的唯一坐标系；必须先消费 mousedown 才允许 mouseup 判定。
     *
     * Code Logic（这个函数做什么）:
     *   读取缓存 metrics，把 clientX/Y 换算为字符格并 clamp；inputEnabled=false 时清缓存返回。
     */
    const cellForPointer = (clientX: number, clientY: number): TerminalCell | null => {
      if (!inputEnabledRef.current) return null;
      const metrics =
        cursorMetricsRef.current ?? measureTerminalCursorMetrics(viewport, terminal);
      cursorMetricsRef.current = metrics;
      return terminalCellFromPointer(metrics, clientX, clientY, terminal.cols, terminal.rows);
    };

    /**
     * Business Logic（为什么需要这个函数）:
     *   xterm 不提供原生的“点击 vs 拖拽”手势判定，需要把 mousedown 落在哪个字符格记下来，
     *   后续 mouseup 才能区分“纯点击”与“拖拽选中文字”。
     *
     * Code Logic（这个函数做什么）:
     *   读 onSelectPaneAt 与当前 cell，落点为 null 时清空 ref；否则写入 pointerDownCellRef / 像素点。
     */
    const handlePointerDown = (event: PointerEvent): void => {
      if (selectPaneDecideTimerRef.current !== null) {
        window.clearTimeout(selectPaneDecideTimerRef.current);
        selectPaneDecideTimerRef.current = null;
      }
      if (!selectPaneCallbackRef.current) {
        pointerDownCellRef.current = null;
        pointerDownPointRef.current = null;
        return;
      }
      if (event.button !== 0) {
        pointerDownCellRef.current = null;
        pointerDownPointRef.current = null;
        return;
      }
      pointerDownCellRef.current = cellForPointer(event.clientX, event.clientY);
      pointerDownPointRef.current = { x: event.clientX, y: event.clientY };
    };

    /**
     * Business Logic（为什么需要这个函数）:
     *   只有“未拖拽、无选区、视口在底部、允许写”的左键点击才代表用户想切换分栏。
     *   其余情况一律让原生 xterm / 浏览器文本选中行为接管。
     *   误触发 select-pane 会让后端/tmux 重绘并清掉 xterm 选区，导致“选中立刻消失、无法复制”。
     *
     * Code Logic（这个函数做什么）:
     *   记录 down/up 与像素位移后，延后到下一 macrotask 再读 hasSelection（等 xterm mouseup 提交选区），
     *   统一交给纯函数判定；命中时用 ref 调 onSelectPaneAt(sessionId, col, row)。
     */
    const handlePointerUp = (event: PointerEvent): void => {
      const callback = selectPaneCallbackRef.current;
      const downCell = pointerDownCellRef.current;
      const downPoint = pointerDownPointRef.current;
      pointerDownCellRef.current = null;
      pointerDownPointRef.current = null;
      if (selectPaneDecideTimerRef.current !== null) {
        window.clearTimeout(selectPaneDecideTimerRef.current);
        selectPaneDecideTimerRef.current = null;
      }
      if (!callback) return;
      if (event.button !== 0) return;
      const upCell = cellForPointer(event.clientX, event.clientY);
      const pointerTravelPx = downPoint
        ? Math.hypot(event.clientX - downPoint.x, event.clientY - downPoint.y)
        : 0;
      // pointerup 往往早于 xterm document mouseup；同步读 getSelection 会漏选区并把拖选当成 click。
      selectPaneDecideTimerRef.current = window.setTimeout(() => {
        selectPaneDecideTimerRef.current = null;
        if (terminalRef.current !== terminal) return;
        // 视口在底部：xterm buffer.viewportY === baseY；其余行不再对应 tmux 屏幕。
        const buffer = terminal.buffer.active;
        const atBottom = buffer.viewportY === buffer.baseY;
        const hasSelection = terminal.getSelection().length > 0;
        const cell = shouldSelectPaneOnClick({
          down: downCell,
          up: upCell,
          hasSelection,
          atBottom,
          writeEnabled: inputEnabledRef.current,
          pointerTravelPx,
        });
        if (cell) callback(sessionId, cell.col, cell.row);
      }, 0);
    };

    viewport.addEventListener('pointerdown', handlePointerDown);
    viewport.addEventListener('pointerup', handlePointerUp);
    /**
     * Business Logic（为什么需要这个函数）:
     *   Prompt 浮层定位依赖光标锚点，但 inactive pane / 非 terminal 视图不会注册回调；
     *   高频 onCursorMove 不得在无消费者时强制 getBoundingClientRect。
     *
     * Code Logic（这个函数做什么）:
     *   无 callback 时立即返回；有 callback 时用缓存 metrics（缺失则测量一次）换算并回调。
     */
    const emitCursorAnchor = (): void => {
      const callback = cursorAnchorCallbackRef.current;
      if (!callback) return;
      try {
        const metrics = cursorMetricsRef.current ?? measureTerminalCursorMetrics(viewport, terminal);
        cursorMetricsRef.current = metrics;
        callback(
          cursorAnchorFromMetrics(
            metrics,
            terminal.buffer.active.cursorX,
            terminal.buffer.active.cursorY,
          ),
        );
      } catch {
        // 光标定位仅用于浮层摆放，失败不影响终端显示与输入。
      }
    };
    const dataDisposable = terminal.onData((data: string) => {
      if (!shouldForwardTerminalInput(replayGateRef.current, inputEnabledRef.current)) return;
      onInput(sessionId, data);
    });
    const cursorDisposable = terminal.onCursorMove(emitCursorAnchor);
    const writer = createTerminalLiveWriter({
      terminal,
      source: store,
      sessionId,
      gate: replayGateRef.current,
    });
    liveWriterRef.current = writer;
    emitCursorAnchor();
    // Business Logic: 仅在真实 cols/rows 变化时回传后端；同尺寸仍 bump 会强制 tmux 整屏重绘，
    // 并可能重新打开 mouse tracking，从而 clearSelection → “选中立刻消失、无法复制”。
    // 用户点「适应尺寸」走 force=true，保留原 bump 语义。
    const persistedSize = persistedSizeRef.current;
    let lastReportedSize: { cols: number; rows: number } | null =
      persistedSize?.sessionId === sessionId
        ? {
            cols: clampU16(persistedSize.cols, MIN_TERMINAL_COLS),
            rows: clampU16(persistedSize.rows, MIN_TERMINAL_ROWS),
          }
        : null;
    const resize = (options?: { force?: boolean }): void => {
      try {
        // 隐藏 window 不参与布局；等重新可见后由 recovery 路径强制 fit/resize。
        if (!renderVisibleRef.current) return;
        const force = options?.force === true;
        // 有文本选区时 ResizeObserver 的 fit 可能改 rows 并清掉选区；仅 force 路径允许打断。
        if (!force && terminal.getSelection().length > 0) {
          return;
        }
        fit.fit();
        // fit 后 cell 尺寸变化，失效缓存；仅 callback 存在时由 emitCursorAnchor 重算。
        cursorMetricsRef.current = null;
        const cols = clampU16(terminal.cols, MIN_TERMINAL_COLS);
        const rows = clampU16(terminal.rows, MIN_TERMINAL_ROWS);
        const sizeChanged =
          lastReportedSize == null ||
          lastReportedSize.cols !== cols ||
          lastReportedSize.rows !== rows;
        if (force || sizeChanged) {
          lastReportedSize = { cols, rows };
          // force 时即使尺寸相同也回传，后端会 bump 一行强制 SIGWINCH/redraw。
          onResize(sessionId, cols, rows);
          // 同步刷新 xterm 视口度量，避免 canvas 与容器错位。
          terminal.refresh(0, Math.max(0, terminal.rows - 1));
        }
        emitCursorAnchor();
      } catch {
        // xterm 在容器不可见时 fit 可能失败，下一次 ResizeObserver 会重试。
      }
    };
    const observer = new ResizeObserver(() => {
      if (resizeTimerRef.current !== null) {
        window.clearTimeout(resizeTimerRef.current);
      }
      resizeTimerRef.current = window.setTimeout(() => resize(), 80);
    });
    // 同时观察 host 与 viewport：冷启动后 host 高度从 min-height 撑满时，
    // 仅 observe viewport 偶发不触发，导致长期停在 ~90x24、tmux status 悬空。
    const host = viewport.parentElement;
    observer.observe(viewport);
    if (host) observer.observe(host);
    forceResizeRef.current = () => resize({ force: true });
    // 后端 restore 已按持久化尺寸完成一次必要 redraw；同尺寸 mount 不再 force bump。
    resize();
    // 布局可能在首帧后才完成（absolute 层 + grid 1fr）；补两次延迟 fit。
    const layoutRaf = window.requestAnimationFrame(() => {
      resize();
      resizeTimerRef.current = window.setTimeout(() => resize(), 120);
    });
    terminalRef.current = terminal;

    return () => {
      window.cancelAnimationFrame(layoutRaf);
      liveWriterRef.current?.dispose();
      liveWriterRef.current = null;
      observer.disconnect();
      viewport.removeEventListener('pointerdown', handlePointerDown);
      viewport.removeEventListener('pointerup', handlePointerUp);
      dataDisposable.dispose();
      cursorDisposable.dispose();
      pointerDownCellRef.current = null;
      pointerDownPointRef.current = null;
      if (selectPaneDecideTimerRef.current !== null) {
        window.clearTimeout(selectPaneDecideTimerRef.current);
        selectPaneDecideTimerRef.current = null;
      }
      if (resizeTimerRef.current !== null) {
        window.clearTimeout(resizeTimerRef.current);
        resizeTimerRef.current = null;
      }
      cursorAnchorCallbackRef.current?.(null);
      restoreSelectionOverrides();
      terminal.dispose();
      terminalRef.current = null;
      forceResizeRef.current = null;
      replayGateRef.current = { current: false };
    };
  }, [onInput, onResize, sessionId, store]);

  useEffect(() => {
    if (resizeRequestKey <= 0) return;
    forceResizeRef.current?.();
  }, [resizeRequestKey]);

  useEffect(() => {
    const becameVisible = renderVisible && !previousRenderVisibleRef.current;
    previousRenderVisibleRef.current = renderVisible;
    if (!renderVisible) return undefined;
    let recoveryRaf: number | null = null;

    /**
     * Business Logic（为什么需要这个函数）:
     *   PC WebView 长时复用隐藏的 xterm DOM 层时，普通 refresh 可能仍合成旧行；
     *   关闭新 window 后旧 window 恢复可见，必须等 CSS 可见状态提交后再强制 PTY 和 DOM 重绘。
     *
     * Code Logic（这个函数做什么）:
     *   合并同一帧的多次请求；下一帧复查可见性与选区，调用 forceResize 触发
     *   fit + 后端 SIGWINCH 重绘，再用空 clearSelection 走 xterm DOM renderer 的独立全行失效路径。
     */
    const scheduleVisibleTerminalRecovery = (): void => {
      if (document.visibilityState === 'hidden') return;
      const terminal = terminalRef.current;
      if (!terminal) return;
      if (terminal.getSelection().length > 0) return;
      if (recoveryRaf !== null) {
        window.cancelAnimationFrame(recoveryRaf);
      }
      recoveryRaf = window.requestAnimationFrame(() => {
        // display:none 恢复后再多等一帧，让 xterm IntersectionObserver 先恢复字符尺寸。
        recoveryRaf = window.requestAnimationFrame(() => {
          recoveryRaf = null;
          const currentTerminal = terminalRef.current;
          if (!currentTerminal || currentTerminal !== terminal) return;
          if (document.visibilityState === 'hidden') return;
          if (currentTerminal.getSelection().length > 0) return;
          cursorMetricsRef.current = null;
          forceResizeRef.current?.();
          // xterm 6 DOM renderer 会在 selection 失效时重建全部行；空选区不会改变用户状态。
          currentTerminal.clearSelection();
        });
      });
    };

    if (becameVisible) {
      scheduleVisibleTerminalRecovery();
    }
    window.addEventListener('focus', scheduleVisibleTerminalRecovery);
    document.addEventListener('visibilitychange', scheduleVisibleTerminalRecovery);
    return () => {
      if (recoveryRaf !== null) {
        window.cancelAnimationFrame(recoveryRaf);
      }
      window.removeEventListener('focus', scheduleVisibleTerminalRecovery);
      document.removeEventListener('visibilitychange', scheduleVisibleTerminalRecovery);
    };
  }, [renderVisible, sessionId]);

  useEffect(() => {
    const applyTheme = () => {
      const terminal = terminalRef.current;
      if (terminal) {
        terminal.options.theme = workbenchTerminalTheme();
        // 主题切换可能改变 cell 度量相关样式，失效缓存。
        cursorMetricsRef.current = null;
      }
    };
    window.addEventListener('cp-theme-change', applyTheme);
    window.addEventListener('storage', applyTheme);
    return () => {
      window.removeEventListener('cp-theme-change', applyTheme);
      window.removeEventListener('storage', applyTheme);
    };
  }, []);

  return (
    <div className={styles.terminalHost} data-testid="terminal-pane">
      <div className={styles.terminalViewport} ref={viewportRef} />
      {!session ? <div className={styles.terminalPlaceholder}>{placeholder}</div> : null}
    </div>
  );
});
