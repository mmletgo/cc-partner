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
 *   - 仅 inputEnabled=true 的 active 终端转发 onData；ResizeObserver 触发 FitAddon.fit 后把 cols/rows clamp 后回传后端。
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
  shouldForwardTerminalInput,
  type TerminalReplayGate,
} from './terminalReplay';

export interface WorkbenchTerminalPaneProps {
  session: WorkbenchSession | null;
  placeholder: string;
  inputEnabled: boolean;
  onInput: (sessionId: string, data: string) => void;
  onResize: (sessionId: string, cols: number, rows: number) => void;
  resizeRequestKey?: number;
  onCursorAnchorChange?: (anchor: TerminalCursorAnchor | null) => void;
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
 *   仅 inputEnabled=true 的 active 终端转发 onData；ResizeObserver 触发 FitAddon.fit 后把 cols/rows clamp 后回传后端。
 */
export const WorkbenchTerminalPane = memo(function WorkbenchTerminalPane(props: WorkbenchTerminalPaneProps) {
  const {
    session,
    placeholder,
    inputEnabled,
    onInput,
    onResize,
    resizeRequestKey = 0,
    onCursorAnchorChange,
  } = props;
  const viewportRef = useRef<HTMLDivElement | null>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const liveWriterRef = useRef<TerminalLiveWriter | null>(null);
  const replayGateRef = useRef<TerminalReplayGate>({ current: false });
  const inputEnabledRef = useRef<boolean>(inputEnabled);
  const resizeTimerRef = useRef<number | null>(null);
  const forceResizeRef = useRef<(() => void) | null>(null);
  const cursorAnchorCallbackRef = useRef<WorkbenchTerminalPaneProps['onCursorAnchorChange']>(
    onCursorAnchorChange,
  );
  // Business Logic: resize/theme 后 cell 尺寸会变；缓存 metrics，避免每个 onCursorMove 强制布局。
  const cursorMetricsRef = useRef<TerminalCursorMetrics | null>(null);
  const sessionId = session?.id ?? null;
  const store = useWorkbenchTerminalBufferStore();

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
    cursorMetricsRef.current = null;
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
    const resize = () => {
      try {
        fit.fit();
        // fit 后 cell 尺寸变化，失效缓存；仅 callback 存在时由 emitCursorAnchor 重算。
        cursorMetricsRef.current = null;
        // 始终把当前 cols/rows 回传：即使与上次相同，后端也会 bump 尺寸强制
        // tmux/PTY 重绘，避免冷启动 replay 后 status bar 停在历史帧中间。
        onResize(
          sessionId,
          clampU16(terminal.cols, MIN_TERMINAL_COLS),
          clampU16(terminal.rows, MIN_TERMINAL_ROWS),
        );
        // 同步刷新 xterm 视口度量，避免 canvas 与容器错位。
        terminal.refresh(0, Math.max(0, terminal.rows - 1));
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
    // 同时观察 host 与 viewport：冷启动后 host 高度从 min-height 撑满时，
    // 仅 observe viewport 偶发不触发，导致长期停在 ~90x24、tmux status 悬空。
    const host = viewport.parentElement;
    observer.observe(viewport);
    if (host) observer.observe(host);
    forceResizeRef.current = resize;
    resize();
    // 布局可能在首帧后才完成（absolute 层 + grid 1fr）；补两次延迟 fit。
    const layoutRaf = window.requestAnimationFrame(() => {
      resize();
      resizeTimerRef.current = window.setTimeout(resize, 120);
    });
    terminalRef.current = terminal;

    return () => {
      window.cancelAnimationFrame(layoutRaf);
      liveWriterRef.current?.dispose();
      liveWriterRef.current = null;
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
      forceResizeRef.current = null;
      replayGateRef.current = { current: false };
    };
  }, [onInput, onResize, sessionId, store]);

  useEffect(() => {
    if (resizeRequestKey <= 0) return;
    forceResizeRef.current?.();
  }, [resizeRequestKey]);

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
