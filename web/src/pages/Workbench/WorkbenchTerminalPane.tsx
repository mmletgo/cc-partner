/**
 * Workbench 终端叶子视图（TerminalPane）—— xterm 生命周期与 PTY 输出写入的隔离组件。
 *
 * Business Logic（为什么需要这个文件）:
 *   xterm 生命周期较重，必须隔离在独立组件内，避免 Workbench 页面其他状态刷新时重复初始化终端实例。
 *   本文件由 Workbench.tsx 原 TerminalPane 组件逐字（VERBATIM）迁移而来，保留原有 effect 依赖数组、
 *   xterm options、replay sequencing、DOM refs 与 cleanup；本任务不修改终端行为或 CSS。
 *
 * Code Logic（这个文件做什么）:
 *   - 暴露 WorkbenchTerminalPane（memo 组件）和 TerminalCursorAnchor / WorkbenchTerminalPaneProps 类型；
 *   - session id 变化时创建/销毁 Terminal；buffer revision 变化时只写入新增输出；
 *   - 仅 inputEnabled=true 的 active 终端转发 onData；ResizeObserver 触发 FitAddon.fit 后把 cols/rows clamp 后回传后端。
 */
import { memo, useEffect, useRef } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { useWorkbenchTerminalBuffer } from '@/hooks/workbenchTerminalBuffersContext';
import type { WorkbenchSession } from '@/lib/types';
import styles from './Workbench.module.css';
import { workbenchTerminalOptions, workbenchTerminalTheme } from './terminalOptions';
import {
  planTerminalBufferWrite,
  shouldForwardTerminalInput,
  writeTerminalReplay,
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
 * Business Logic（为什么需要这个组件）:
 *   xterm 生命周期较重，应隔离在独立组件内，避免页面其他状态刷新时重复初始化终端实例。
 *
 * Code Logic（这个组件做什么）:
 *   session 变化时创建/销毁 Terminal；buffer revision 变化时只写入新增输出；
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
  const bufferRef = useRef<string>('');
  const writtenBufferRef = useRef<string>('');
  const replayGateRef = useRef<boolean>(false);
  const inputEnabledRef = useRef<boolean>(inputEnabled);
  const resizeTimerRef = useRef<number | null>(null);
  const forceResizeRef = useRef<(() => void) | null>(null);
  const cursorAnchorCallbackRef = useRef<WorkbenchTerminalPaneProps['onCursorAnchorChange']>(
    onCursorAnchorChange,
  );
  const sessionId = session?.id ?? null;
  const { buffer, revision } = useWorkbenchTerminalBuffer(sessionId);

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
    writtenBufferRef.current = bufferRef.current;
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
    forceResizeRef.current = resize;
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
      forceResizeRef.current = null;
      writtenBufferRef.current = '';
      replayGateRef.current = false;
    };
  }, [onInput, onResize, sessionId]);

  useEffect(() => {
    if (resizeRequestKey <= 0) return;
    forceResizeRef.current?.();
  }, [resizeRequestKey]);

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

  return (
    <div className={styles.terminalHost}>
      <div className={styles.terminalViewport} ref={viewportRef} />
      {!session ? <div className={styles.terminalPlaceholder}>{placeholder}</div> : null}
    </div>
  );
});
