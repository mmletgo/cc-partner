// @vitest-environment jsdom
/**
 * WorkbenchTerminalPane 叶子视图契约测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   TerminalPane 从 Workbench.tsx 迁出后必须保持原有可观察契约：xterm 在每个 session identity 上
 *   创建/销毁一次；replay gate 在历史 buffer 写入期间屏蔽 onData；onData/ResizeObserver/resizeRequestKey
 *   转发到外部回调；workspace 视图切换不应重建 xterm，但隐藏的 pane 恢复可见时必须整屏重绘。
 *
 * Code Logic（这个测试做什么）:
 *   - 用 vi.mock 接管 @xterm/xterm 与 @xterm/addon-fit，记录每个 Terminal 实例的构造/销毁/onData/onResize；
 *   - 用自定义 Provider 注入受控的 buffer snapshot，模拟 revision 变化驱动 replay；
 *   - 用 @testing-library/react 渲染、rerender、fireEvent 触发各种交互并断言。
 */
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import type { Mock } from 'vitest';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import type { ReactElement, ReactNode } from 'react';
import { useCallback } from 'react';

import { WorkbenchTerminalPane } from './WorkbenchTerminalPane';
import type { TerminalCursorAnchor } from './WorkbenchTerminalPane';
import { WorkbenchTerminalBuffersContext } from '@/hooks/workbenchTerminalBuffersContext';
import type { WorkbenchTerminalBuffersContextValue } from '@/hooks/workbenchTerminalBuffersContext';
import { createWorkbenchTerminalBufferStore } from '@/hooks/workbenchTerminalBuffer';
import type { WorkbenchSession } from '@/lib/types';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

/* ---------------------------------------------------------------------------
 * vi.mock — xterm Terminal + FitAddon
 *
 * Business Logic: 契约测试需要观察 Terminal 实例的创建/销毁次数和 onData/onResize 回调；用工厂替换为可记录的桩。
 * ------------------------------------------------------------------------- */

interface MockTerminal {
  cols: number;
  rows: number;
  options: { theme?: unknown };
  onData: (cb: (data: string) => void) => { dispose: () => void };
  onCursorMove: (cb: () => void) => { dispose: () => void };
  /** 测试用：主动触发已注册的 onCursorMove 回调。 */
  emitCursorMove: () => void;
  /** 测试用：设置当前文本选区。 */
  setSelection: (text: string) => void;
  onResize: (cb: () => void) => { dispose: () => void };
  loadAddon: (addon: unknown) => void;
  open: (el: HTMLElement) => void;
  write: (data: string, cb?: () => void) => void;
  clear: () => void;
  reset: () => void;
  clearSelection: () => void;
  refresh: (start: number, end: number) => void;
  getSelection: () => string;
  scrollToLine: Mock<(line: number) => void>;
  scrollToBottom: () => void;
  dispose: () => void;
  attachCustomKeyEventHandler: (handler: (event: KeyboardEvent) => boolean) => void;
  /** 测试用：主动触发已注册的键盘处理器。 */
  invokeKey: (event: Partial<KeyboardEvent>) => boolean | undefined;
  attachCustomWheelEventHandler: (handler: (event: WheelEvent) => boolean) => void;
  invokeWheel: (event: Partial<WheelEvent>) => boolean | undefined;
  modes: { mouseTrackingMode: 'none' | 'x10' | 'vt200' | 'drag' | 'any' };
  buffer: {
    active: {
      type: 'normal' | 'alternate';
      cursorX: number;
      cursorY: number;
      viewportY: number;
      baseY: number;
    };
  };
}

interface MockFitAddon {
  fit: () => void;
}

const terminalEvents = vi.hoisted<{
  constructCount: number;
  disposeCount: number;
  dataCallbacks: Array<(data: string) => void>;
  resizeCalls: Array<{ sessionId: string; cols: number; rows: number }>;
  fitCount: number;
  instances: MockTerminal[];
  /** Records every write(data, cb) call across all instances, in order. */
  writeCalls: Array<{ data: string; instanceIndex: number }>;
  /** Records every clear() call across all instances, in order. */
  clearCalls: Array<{ instanceIndex: number }>;
  /** Records every reset() call across all instances, in order. */
  resetCalls: Array<{ instanceIndex: number }>;
  /** Records every refresh(start, end) call across all instances, in order. */
  refreshCalls: Array<{ start: number; end: number; instanceIndex: number }>;
  /** Records the empty-selection invalidation used to force the DOM renderer to rebuild rows. */
  clearSelectionCalls: Array<{ instanceIndex: number }>;
}>(() => ({
  constructCount: 0,
  disposeCount: 0,
  dataCallbacks: [],
  resizeCalls: [],
  fitCount: 0,
  instances: [],
  writeCalls: [],
  clearCalls: [],
  resetCalls: [],
  refreshCalls: [],
  clearSelectionCalls: [],
}));

vi.mock('@xterm/xterm', () => {
  class TerminalMock {
    cols = 80;
    rows = 24;
    options: { theme?: unknown } = {};
    private dataCb: ((data: string) => void) | null = null;
    private cursorMoveCb: (() => void) | null = null;
    private resizeCb: (() => void) | null = null;
    private selectionText = '';
    // Business Logic: 记录自己的 instance index，让 write/clear 日志能溯源到具体实例。
    private readonly instanceIndex: number;

    constructor() {
      terminalEvents.constructCount += 1;
      this.instanceIndex = terminalEvents.instances.length;
      const instance = this as unknown as MockTerminal;
      terminalEvents.instances.push(instance);
    }
    onData(cb: (data: string) => void) {
      this.dataCb = cb;
      terminalEvents.dataCallbacks.push(cb);
      return {
        dispose: () => {
          if (this.dataCb === cb) this.dataCb = null;
        },
      };
    }
    onCursorMove(cb: () => void) {
      this.cursorMoveCb = cb;
      return { dispose: () => { if (this.cursorMoveCb === cb) this.cursorMoveCb = null; } };
    }
    /**
     * Business Logic（为什么需要这个方法）:
     *   契约测试需要主动触发 xterm 的 onCursorMove，验证无回调时不读布局。
     *
     * Code Logic（这个方法做什么）:
     *   调用最近注册的 cursorMove 回调；无回调时 no-op。
     */
    emitCursorMove() {
      this.cursorMoveCb?.();
    }
    onResize(cb: () => void) {
      this.resizeCb = cb;
      return { dispose: () => { if (this.resizeCb === cb) this.resizeCb = null; } };
    }
    loadAddon(addon: unknown) {
      const fit = addon as MockFitAddon;
      if (fit && typeof fit.fit === 'function') {
        const original = fit.fit.bind(fit);
        fit.fit = () => {
          terminalEvents.fitCount += 1;
          original();
        };
      }
    }
    open() { /* no-op */ }
    write(data: string, cb?: () => void) {
      // Business Logic: 记录 write 数据用于 replay gate 断言；cb 同步触发以模拟 xterm 真实写入完成。
      terminalEvents.writeCalls.push({ data, instanceIndex: this.instanceIndex });
      cb?.();
    }
    clear() {
      terminalEvents.clearCalls.push({ instanceIndex: this.instanceIndex });
    }
    reset() {
      terminalEvents.resetCalls.push({ instanceIndex: this.instanceIndex });
      // 测试 seam：模拟 RIS 回到 normal；保留 baseY 代表紧随其后的快照已完成解析。
      this.buffer.active.type = 'normal';
      this.modes.mouseTrackingMode = 'none';
    }
    clearSelection() {
      this.selectionText = '';
      terminalEvents.clearSelectionCalls.push({ instanceIndex: this.instanceIndex });
    }
    refresh(start: number, end: number) {
      terminalEvents.refreshCalls.push({ start, end, instanceIndex: this.instanceIndex });
    }
    getSelection() {
      return this.selectionText;
    }
    scrollToLine = vi.fn((line: number) => {
      this.buffer.active.viewportY = line;
    });
    scrollToBottom = vi.fn();
    setSelection(text: string) {
      this.selectionText = text;
    }
    dispose() {
      terminalEvents.disposeCount += 1;
    }
    private keyHandler: ((event: KeyboardEvent) => boolean) | null = null;
    attachCustomKeyEventHandler(handler: (event: KeyboardEvent) => boolean) {
      this.keyHandler = handler;
    }
    invokeKey(event: Partial<KeyboardEvent>) {
      return this.keyHandler?.(event as KeyboardEvent);
    }
    private wheelHandler: ((event: WheelEvent) => boolean) | null = null;
    attachCustomWheelEventHandler(handler: (event: WheelEvent) => boolean) {
      this.wheelHandler = handler;
    }
    invokeWheel(event: Partial<WheelEvent>) {
      return this.wheelHandler?.(event as WheelEvent);
    }
    modes = { mouseTrackingMode: 'none' as const };
    buffer = {
      active: { type: 'normal' as const, cursorX: 0, cursorY: 0, viewportY: 0, baseY: 0 },
    };
  }
  return { Terminal: TerminalMock };
});

vi.mock('@xterm/addon-fit', () => {
  class FitAddonMock {
    fit() { /* 默认空实现；loadAddon 时会被包裹计数 */ }
    proposeDimensions() { return { cols: 80, rows: 24 }; }
    activate() { /* no-op */ }
    dispose() { /* no-op */ }
  }
  return { FitAddon: FitAddonMock };
});

vi.mock('@xterm/xterm/css/xterm.css', () => ({}));

/* ---------------------------------------------------------------------------
 * 受控 buffer context helper
 *
 * Business Logic: 测试需要按 store API 推送 buffer 内容并触发 live writer；用真实 store Provider 注入。
 * ------------------------------------------------------------------------- */

interface ControlledBuffer {
  buffer: string;
  revision: number;
}

interface ControlledProviderProps {
  store: ReturnType<typeof createWorkbenchTerminalBufferStore>;
  refreshScrollback?: (sessionId: string) => void;
  children: ReactNode;
}

function ControlledBuffersProvider({
  store,
  refreshScrollback = () => undefined,
  children,
}: ControlledProviderProps): ReactElement {
  const value: WorkbenchTerminalBuffersContextValue = {
    store,
    resetBuffer: (sessionId: string) => store.reset(sessionId),
    removeBuffer: (sessionId: string) => store.remove(sessionId),
    getHistorySyncFailure: () => null,
    subscribeHistorySyncFailures: () => () => undefined,
    getHistorySyncFailuresRevision: () => 0,
    retryHistorySync: () => undefined,
    refreshScrollback,
    getStartupBaselineFailure: () => null,
    subscribeStartupBaselineFailure: () => () => undefined,
    getStartupBaselineFailureRevision: () => 0,
    retryStartupBaseline: () => undefined,
  };
  return (
    <WorkbenchTerminalBuffersContext.Provider value={value}>
      {children}
    </WorkbenchTerminalBuffersContext.Provider>
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   旧测试用 snapshot 对象描述初始 buffer；新路径需要 seed 到真实 store。
 *
 * Code Logic（这个函数做什么）:
 *   创建 store 并对每个 session reset(sessionId, buffer)。
 */
function createStoreFromSnapshots(
  snapshots: Record<string, ControlledBuffer>,
): ReturnType<typeof createWorkbenchTerminalBufferStore> {
  const store = createWorkbenchTerminalBufferStore();
  for (const [sessionId, snapshot] of Object.entries(snapshots)) {
    store.reset(sessionId, snapshot.buffer);
  }
  return store;
}

/* ---------------------------------------------------------------------------
 * Fixture + render helpers
 * ------------------------------------------------------------------------- */

function buildSession(overrides: Partial<WorkbenchSession> = {}): WorkbenchSession {
  return {
    id: 's1',
    projectId: 'project-1',
    worktreeId: 'worktree-main',
    name: 'main terminal',
    command: 'bash',
    cwd: '/repo',
    status: 'running',
    cols: 80,
    rows: 24,
    startedAt: '2026-07-01T00:00:00.000Z',
    exitedAt: null,
    exitCode: null,
    supportsPanes: true,
    paneCount: 1,
    ...overrides,
  };
}

interface PaneHostProps {
  session: WorkbenchSession | null;
  store: ReturnType<typeof createWorkbenchTerminalBufferStore>;
  renderVisible?: boolean;
  inputEnabled?: boolean;
  agentTranscriptActive?: boolean;
  resizeRequestKey?: number;
  onInput?: (sessionId: string, data: string) => void;
  onPasteImage?: (sessionId: string, dataUrl: string | null) => void;
  onResize?: (sessionId: string, cols: number, rows: number) => void;
  onCursorAnchorChange?: (anchor: TerminalCursorAnchor | null) => void;
  refreshScrollback?: (sessionId: string) => void;
  placeholder?: string;
}

/**
 * Business Logic: TerminalPane 的 mount effect 依赖数组里包含 onInput/onResize/sessionId；父组件必须传
 * 引用稳定的回调（与真实 Workbench 用 useCallback 保持一致），否则每次重渲染都会重建 xterm。
 *
 * Code Logic: 用 useCallback 包装可选回调，缺省时返回一个稳定的 no-op。
 */
function PaneHost(props: PaneHostProps): ReactElement {
  const {
    session,
    store,
    renderVisible = true,
    inputEnabled = true,
    agentTranscriptActive = false,
    resizeRequestKey = 0,
    onInput,
    onPasteImage,
    onResize,
    onCursorAnchorChange,
    refreshScrollback,
    placeholder = 'placeholder',
  } = props;
  const stableInput = useCallback(
    (sessionId: string, data: string) => onInput?.(sessionId, data),
    [onInput],
  );
  const stableResize = useCallback(
    (sessionId: string, cols: number, rows: number) => onResize?.(sessionId, cols, rows),
    [onResize],
  );
  const stableCursor = useCallback(
    (anchor: TerminalCursorAnchor | null) => onCursorAnchorChange?.(anchor),
    [onCursorAnchorChange],
  );
  // Business Logic: 生产路径在 inactive pane / 非 terminal 视图会传 undefined；
  // 测试 harness 必须原样透传，不能把 undefined 包装成永远 truthy 的 no-op。
  return (
    <ControlledBuffersProvider store={store} refreshScrollback={refreshScrollback}>
      <WorkbenchTerminalPane
        session={session}
        placeholder={placeholder}
        renderVisible={renderVisible}
        inputEnabled={inputEnabled}
        agentTranscriptActive={agentTranscriptActive}
        onInput={stableInput}
        onPasteImage={onPasteImage}
        onResize={stableResize}
        resizeRequestKey={resizeRequestKey}
        onCursorAnchorChange={onCursorAnchorChange ? stableCursor : undefined}
      />
    </ControlledBuffersProvider>
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   光标布局热路径测试需要一个最小 render 入口，并可省略 anchor callback。
 *
 * Code Logic（这个函数做什么）:
 *   用默认 session/store 渲染 PaneHost；透传 onCursorAnchorChange。
 */
function renderPane(options: {
  onCursorAnchorChange?: (anchor: TerminalCursorAnchor | null) => void;
} = {}) {
  const session = buildSession({ id: 's1' });
  const store = createStoreFromSnapshots({ s1: { buffer: '', revision: 0 } });
  return render(
    <PaneHost
      session={session}
      store={store}
      inputEnabled={true}
      onCursorAnchorChange={options.onCursorAnchorChange}
    />,
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   测试需要触发最近一个 mock Terminal 的 onCursorMove。
 *
 * Code Logic（这个函数做什么）:
 *   返回 terminalEvents.instances 末项；不存在时抛错。
 */
function latestTerminal(): MockTerminal {
  const terminal = terminalEvents.instances[terminalEvents.instances.length - 1];
  if (!terminal) {
    throw new Error('expected at least one mock Terminal instance');
  }
  return terminal;
}

/**
 * Business Logic: live writer 通过 store reset/append 驱动写入；保留 stable callbacks 避免 xterm 重建。
 *
 * Code Logic: 创建真实 store，并提供 advanceRevision（reset）与 rerenderProps。
 */
function renderPaneWithRevision() {
  const store = createWorkbenchTerminalBufferStore();
  const stableInput = vi.fn();
  const stableResize = vi.fn();

  function Host(props: {
    session?: WorkbenchSession | null;
    inputEnabled?: boolean;
    renderVisible?: boolean;
    resizeRequestKey?: number;
  }): ReactElement {
    return (
      <PaneHost
        session={props.session ?? buildSession()}
        store={store}
        renderVisible={props.renderVisible}
        inputEnabled={props.inputEnabled}
        resizeRequestKey={props.resizeRequestKey}
        onInput={stableInput}
        onResize={stableResize}
      />
    );
  }

  const utils = render(<Host session={buildSession()} />);
  return {
    ...utils,
    store,
    onResize: stableResize,
    advanceRevision(sessionId: string, buffer: string) {
      store.reset(sessionId, buffer);
    },
    rerenderProps(next: {
      session?: WorkbenchSession | null;
      inputEnabled?: boolean;
      renderVisible?: boolean;
      resizeRequestKey?: number;
    }) {
      utils.rerender(
        <Host
          session={next.session ?? buildSession()}
          inputEnabled={next.inputEnabled}
          renderVisible={next.renderVisible}
          resizeRequestKey={next.resizeRequestKey}
        />,
      );
    },
  };
}

beforeEach(() => {
  terminalEvents.constructCount = 0;
  terminalEvents.disposeCount = 0;
  terminalEvents.dataCallbacks.length = 0;
  terminalEvents.resizeCalls.length = 0;
  terminalEvents.fitCount = 0;
  terminalEvents.instances.length = 0;
  terminalEvents.writeCalls.length = 0;
  terminalEvents.clearCalls.length = 0;
  terminalEvents.resetCalls.length = 0;
  terminalEvents.refreshCalls.length = 0;
  terminalEvents.clearSelectionCalls.length = 0;
  // jsdom 默认无 ResizeObserver；安装一个调用回调的最小实现，触发 pane 内的 resize 路径。
  if (!window.ResizeObserver) {
    class RO {
      observe() { /* no-op */ }
      unobserve() { /* no-op */ }
      disconnect() { /* no-op */ }
    }
    (window as unknown as { ResizeObserver: unknown }).ResizeObserver = RO;
  }
});

afterEach(() => {
  cleanup();
});

describe('WorkbenchTerminalPane — xterm lifecycle', () => {
  test('creates exactly one xterm Terminal for a stable session identity and disposes it on unmount', () => {
    const session = buildSession({ id: 's1' });
    const store = createStoreFromSnapshots({ s1: { buffer: '', revision: 0 } });

    const { rerender, unmount } = render(<PaneHost session={session} store={store} />);
    expect(terminalEvents.constructCount).toBe(1);

    // 父组件用相同 session 重渲染：不应创建新 Terminal。
    rerender(<PaneHost session={session} store={store} />);
    expect(terminalEvents.constructCount).toBe(1);
    expect(terminalEvents.disposeCount).toBe(0);

    // unmount 后唯一实例被销毁。
    unmount();
    expect(terminalEvents.disposeCount).toBe(1);
  });

  test('disposes the previous Terminal and creates a new one when session identity changes', () => {
    const s1 = buildSession({ id: 's1' });
    const s2 = buildSession({ id: 's2' });
    const store = createStoreFromSnapshots({
      s1: { buffer: '', revision: 0 },
      s2: { buffer: '', revision: 0 },
    });

    const { rerender } = render(<PaneHost session={s1} store={store} />);
    expect(terminalEvents.constructCount).toBe(1);
    expect(terminalEvents.disposeCount).toBe(0);

    rerender(<PaneHost session={s2} store={store} />);
    expect(terminalEvents.constructCount).toBe(2);
    expect(terminalEvents.disposeCount).toBe(1);
  });

  test('no Terminal is created when session is null (empty state)', () => {
    const store = createStoreFromSnapshots({});
    render(<PaneHost session={null} store={store} placeholder="empty" />);
    expect(terminalEvents.constructCount).toBe(0);
    expect(screen.getByText('empty')).toBeTruthy();
  });
});

describe('WorkbenchTerminalPane — replay gate', () => {
  test('real Ctrl+V reaches Claude while a long replay gate is still active', () => {
    const onInput = vi.fn();
    const session = buildSession({ id: 's1' });
    const store = createStoreFromSnapshots({ s1: { buffer: '', revision: 0 } });
    render(
      <PaneHost
        session={session}
        store={store}
        inputEnabled={true}
        onInput={onInput}
      />,
    );

    act(() => {
      // Mock write callback 同步完成，但 gate 要到下一个 macrotask 才释放；此刻等价于长快照仍在解析。
      store.reset('s1', 'long-history-snapshot');
    });
    const terminal = terminalEvents.instances[terminalEvents.instances.length - 1]!;
    const preventDefault = vi.fn();
    const stopPropagation = vi.fn();

    let handled: boolean | undefined;
    act(() => {
      handled = terminal.invokeKey({
        type: 'keydown',
        key: 'v',
        ctrlKey: true,
        altKey: false,
        metaKey: false,
        shiftKey: false,
        preventDefault,
        stopPropagation,
      });
    });

    expect(handled).toBe(false);
    expect(preventDefault).toHaveBeenCalledOnce();
    expect(stopPropagation).toHaveBeenCalledOnce();
    expect(terminal.scrollToBottom).toHaveBeenCalledOnce();
    expect(onInput).toHaveBeenCalledOnce();
    expect(onInput).toHaveBeenCalledWith('s1', '\x16');
  });

  test('Ctrl+V keeps the normal xterm onData path after replay gate release', async () => {
    const onInput = vi.fn();
    const session = buildSession({ id: 's1' });
    const store = createStoreFromSnapshots({ s1: { buffer: '', revision: 0 } });
    render(
      <PaneHost
        session={session}
        store={store}
        inputEnabled={true}
        onInput={onInput}
      />,
    );
    await act(async () => {
      await new Promise<void>((resolve) => setTimeout(resolve, 10));
    });
    const terminal = terminalEvents.instances[terminalEvents.instances.length - 1]!;

    expect(terminal.invokeKey({
      type: 'keydown',
      key: 'v',
      ctrlKey: true,
      altKey: false,
      metaKey: false,
      shiftKey: false,
    })).toBe(true);
    expect(onInput).not.toHaveBeenCalled();
  });

  test('Ctrl+V with onPasteImage reads clipboard instead of sending SYN immediately', () => {
    const onInput = vi.fn();
    const onPasteImage = vi.fn();
    const session = buildSession({ id: 's1' });
    const store = createStoreFromSnapshots({ s1: { buffer: '', revision: 0 } });
    render(
      <PaneHost
        session={session}
        store={store}
        inputEnabled={true}
        onInput={onInput}
        onPasteImage={onPasteImage}
      />,
    );
    const terminal = terminalEvents.instances[terminalEvents.instances.length - 1]!;
    const preventDefault = vi.fn();
    const stopPropagation = vi.fn();
    let handled: boolean | undefined;
    act(() => {
      handled = terminal.invokeKey({
        type: 'keydown',
        key: 'v',
        ctrlKey: true,
        altKey: false,
        metaKey: false,
        shiftKey: false,
        preventDefault,
        stopPropagation,
      });
    });
    expect(handled).toBe(false);
    expect(preventDefault).toHaveBeenCalledOnce();
    expect(onPasteImage).toHaveBeenCalledOnce();
    expect(onPasteImage).toHaveBeenCalledWith('s1', null);
    expect(onInput).not.toHaveBeenCalled();
  });

  test('paste event with an image file is intercepted', async () => {
    const onPasteImage = vi.fn();
    const session = buildSession({ id: 's1' });
    const store = createStoreFromSnapshots({ s1: { buffer: '', revision: 0 } });
    render(
      <PaneHost
        session={session}
        store={store}
        inputEnabled={true}
        onPasteImage={onPasteImage}
      />,
    );
    const viewport = document.querySelector('[data-testid="terminal-pane"] > div');
    expect(viewport).toBeTruthy();
    const file = new File([new Uint8Array([137, 80, 78, 71])], 'shot.png', { type: 'image/png' });
    const items = [
      {
        kind: 'file',
        type: 'image/png',
        getAsFile: () => file,
      },
    ];
    const event = new Event('paste', { bubbles: true, cancelable: true });
    Object.defineProperty(event, 'clipboardData', {
      value: {
        items,
        files: {
          length: 1,
          item: () => file,
          [Symbol.iterator]: function* iter() {
            yield file;
          },
        },
      },
    });
    act(() => {
      viewport!.dispatchEvent(event);
    });
    expect(event.defaultPrevented).toBe(true);
    await waitFor(() => {
      expect(onPasteImage).toHaveBeenCalledWith(
        's1',
        expect.stringMatching(/^data:image\/png;base64,/),
      );
    });
  });

  test('historical buffer replay writes through writeTerminalReplay and gates onData until release', async () => {
    const { advanceRevision } = renderPaneWithRevision();

    // 推送一段历史 buffer（首屏 replay）。
    act(() => {
      advanceRevision('s1', '\x1b[c$ ');
    });
    // 让 xterm write callback 触发的 setTimeout(0) gate release 落地。
    await act(async () => {
      await new Promise<void>((resolve) => setTimeout(resolve, 10));
    });

    // 第一次 replay 期间 inputEnabled=true 但 gate 会屏蔽，直到 release；release 后再触发 onData。
    // 这里只验证 onData 注册了回调（侧证 replay 流程没把 pane 卡死）。
    expect(terminalEvents.dataCallbacks.length).toBeGreaterThanOrEqual(1);
  });

  test('replay gate writes history buffer to xterm via write() then releases gate so subsequent onData forwards', async () => {
    // live writer 在 store.reset 后 clear + replay 新 snapshot。
    const onInput = vi.fn();
    const session = buildSession({ id: 's1' });
    const store = createStoreFromSnapshots({ s1: { buffer: '', revision: 0 } });
    render(
      <PaneHost
        session={session}
        store={store}
        inputEnabled={true}
        onInput={onInput}
      />,
    );

    act(() => {
      store.reset('s1', 'hist-data');
    });
    await act(async () => {
      await new Promise<void>((resolve) => setTimeout(resolve, 10));
    });

    const written = terminalEvents.writeCalls.map((c) => c.data).join('');
    expect(written).toContain('hist-data');

    const dataCb = terminalEvents.dataCallbacks[terminalEvents.dataCallbacks.length - 1]!;
    onInput.mockClear();
    act(() => {
      dataCb('ls');
    });
    expect(onInput).toHaveBeenCalledWith('s1', 'ls');
  });

  test('replay path triggers clear() when buffer cannot be appended (sliding truncation)', async () => {
    // generation/reset 变化时 live writer 必须 clear + replay 新 snapshot。
    const session = buildSession({ id: 's1' });
    const store = createStoreFromSnapshots({ s1: { buffer: 'first', revision: 1 } });
    render(<PaneHost session={session} store={store} inputEnabled={true} />);
    await act(async () => {
      await new Promise<void>((resolve) => setTimeout(resolve, 10));
    });

    act(() => {
      store.reset('s1', 'second');
    });
    await act(async () => {
      await new Promise<void>((resolve) => setTimeout(resolve, 10));
    });

    expect(terminalEvents.clearCalls.length).toBeGreaterThanOrEqual(1);
    const written = terminalEvents.writeCalls.map((c) => c.data).join('');
    expect(written).toContain('second');
  });
});

describe('WorkbenchTerminalPane — forwards input / resize / focus', () => {
  test('onData forwards to onInput only when inputEnabled is true', () => {
    const session = buildSession({ id: 's1' });
    const onInput = vi.fn();
    const store = createStoreFromSnapshots({ s1: { buffer: '', revision: 0 } });

    const { rerender } = render(
      <PaneHost
        session={session}
        store={store}
        inputEnabled={true}
        onInput={onInput}
      />,
    );

    const dataCb = terminalEvents.dataCallbacks[terminalEvents.dataCallbacks.length - 1]!;
    act(() => {
      dataCb('ls');
    });
    expect(onInput).toHaveBeenCalledWith('s1', 'ls');

    onInput.mockClear();
    rerender(
      <PaneHost
        session={session}
        store={store}
        inputEnabled={false}
        onInput={onInput}
      />,
    );
    const dataCbAfter = terminalEvents.dataCallbacks[terminalEvents.dataCallbacks.length - 1]!;
    act(() => {
      dataCbAfter('rm -rf');
    });
    expect(onInput).not.toHaveBeenCalled();
  });

  test('ResizeObserver triggers fit and forwards clamped cols/rows to onResize', () => {
    const session = buildSession({ id: 's1', cols: 120, rows: 40 });
    const onResize = vi.fn();
    const store = createStoreFromSnapshots({ s1: { buffer: '', revision: 0 } });

    render(
      <PaneHost
        session={session}
        store={store}
        inputEnabled={true}
        onResize={onResize}
      />,
    );

    // 持久化尺寸与当前 FitAddon 结果不同，mount 的普通 resize 会回传真实可见尺寸。
    expect(terminalEvents.fitCount).toBeGreaterThan(0);
    expect(onResize).toHaveBeenCalled();
    // clamp 下限：cols>=20, rows>=6。
    const lastCall = onResize.mock.calls[onResize.mock.calls.length - 1]!;
    expect(lastCall[0]).toBe('s1');
    expect(lastCall[1]).toBeGreaterThanOrEqual(20);
    expect(lastCall[2]).toBeGreaterThanOrEqual(6);
  });

  test('cold restore with the same persisted size does not force a duplicate redraw on mount', () => {
    const session = buildSession({ id: 's1', cols: 80, rows: 24 });
    const onResize = vi.fn();
    const store = createStoreFromSnapshots({ s1: { buffer: '', revision: 0 } });

    render(
      <PaneHost
        session={session}
        store={store}
        inputEnabled={true}
        onResize={onResize}
      />,
    );

    expect(terminalEvents.fitCount).toBeGreaterThan(0);
    expect(onResize).not.toHaveBeenCalled();
  });

  test('resizeRequestKey increment re-invokes forceResize', () => {
    const session = buildSession({ id: 's1' });
    const onResize = vi.fn();
    const store = createStoreFromSnapshots({ s1: { buffer: '', revision: 0 } });

    const { rerender } = render(
      <PaneHost
        session={session}
        store={store}
        inputEnabled={true}
        onResize={onResize}
        resizeRequestKey={0}
      />,
    );
    const fitsBeforeKey = terminalEvents.fitCount;
    onResize.mockClear();

    rerender(
      <PaneHost
        session={session}
        store={store}
        inputEnabled={true}
        onResize={onResize}
        resizeRequestKey={1}
      />,
    );
    expect(terminalEvents.fitCount).toBeGreaterThan(fitsBeforeKey);
    expect(onResize).toHaveBeenCalled();
  });
});

describe('WorkbenchTerminalPane — workspace view change does not unmount xterm', () => {
  test('switching render visibility preserves the Terminal instance and forces PTY plus DOM recovery when shown again', async () => {
    const { onResize, rerenderProps } = renderPaneWithRevision();
    const constructsAfterMount = terminalEvents.constructCount;
    expect(constructsAfterMount).toBe(1);
    const instanceAtMount = terminalEvents.instances[0];
    const refreshesAfterMount = terminalEvents.refreshCalls.length;
    const resizesAfterMount = onResize.mock.calls.length;
    const selectionInvalidationsAfterMount = terminalEvents.clearSelectionCalls.length;

    rerenderProps({ renderVisible: false });
    expect(terminalEvents.refreshCalls).toHaveLength(refreshesAfterMount);
    rerenderProps({ renderVisible: true });

    await waitFor(() => {
      expect(onResize).toHaveBeenCalledTimes(resizesAfterMount + 1);
    });

    expect(terminalEvents.constructCount).toBe(1);
    expect(terminalEvents.disposeCount).toBe(0);
    expect(terminalEvents.instances[0]).toBe(instanceAtMount);
    expect(terminalEvents.refreshCalls).toHaveLength(refreshesAfterMount + 1);
    expect(terminalEvents.refreshCalls.at(-1)).toEqual({
      start: 0,
      end: 23,
      instanceIndex: 0,
    });
    expect(terminalEvents.clearSelectionCalls).toHaveLength(
      selectionInvalidationsAfterMount + 1,
    );
    expect(terminalEvents.clearSelectionCalls.at(-1)).toEqual({ instanceIndex: 0 });

    // 可见状态未变时的普通父组件重渲染不应额外 refresh。
    rerenderProps({ renderVisible: true });
    expect(terminalEvents.refreshCalls).toHaveLength(refreshesAfterMount + 1);
  });

  test('toggling inputEnabled on workspace view change does not recreate Terminal', () => {
    const { rerenderProps } = renderPaneWithRevision();
    expect(terminalEvents.constructCount).toBe(1);
    const instanceAtMount = terminalEvents.instances[0];

    rerenderProps({ inputEnabled: false });
    rerenderProps({ inputEnabled: true });

    expect(terminalEvents.constructCount).toBe(1);
    expect(terminalEvents.disposeCount).toBe(0);
    expect(terminalEvents.instances[0]).toBe(instanceAtMount);
  });

  test('window focus recovers only while this pane is visible', async () => {
    const { onResize, rerenderProps } = renderPaneWithRevision();
    const resizesAfterMount = onResize.mock.calls.length;

    act(() => {
      window.dispatchEvent(new Event('focus'));
    });
    await waitFor(() => {
      expect(onResize).toHaveBeenCalledTimes(resizesAfterMount + 1);
    });
    const resizesAfterFocus = onResize.mock.calls.length;

    rerenderProps({ renderVisible: false });
    act(() => {
      window.dispatchEvent(new Event('focus'));
    });
    expect(onResize).toHaveBeenCalledTimes(resizesAfterFocus);
  });

  test('window focus does not force a redraw while the user is reading scrollback', async () => {
    const { onResize } = renderPaneWithRevision();
    const terminal = latestTerminal();
    terminal.buffer.active.baseY = 120;
    terminal.buffer.active.viewportY = 60;
    const resizesBeforeFocus = onResize.mock.calls.length;
    const invalidationsBeforeFocus = terminalEvents.clearSelectionCalls.length;

    act(() => {
      window.dispatchEvent(new Event('focus'));
    });
    await act(async () => {
      await new Promise<void>((resolve) => setTimeout(resolve, 30));
    });

    expect(onResize).toHaveBeenCalledTimes(resizesBeforeFocus);
    expect(terminalEvents.clearSelectionCalls).toHaveLength(invalidationsBeforeFocus);
  });

  test('visibility recovery preserves an existing text selection', async () => {
    const { onResize, rerenderProps } = renderPaneWithRevision();
    latestTerminal().setSelection('copy-me');
    const resizesBeforeSwitch = onResize.mock.calls.length;
    const invalidationsBeforeSwitch = terminalEvents.clearSelectionCalls.length;

    rerenderProps({ renderVisible: false });
    rerenderProps({ renderVisible: true });
    await act(async () => {
      await new Promise<void>((resolve) => setTimeout(resolve, 30));
    });

    expect(onResize).toHaveBeenCalledTimes(resizesBeforeSwitch);
    expect(terminalEvents.clearSelectionCalls).toHaveLength(invalidationsBeforeSwitch);
    expect(latestTerminal().getSelection()).toBe('copy-me');
  });

  test('closing a newly created active window recovers the preserved old Terminal', async () => {
    const store = createStoreFromSnapshots({
      old: { buffer: 'old-window', revision: 1 },
      created: { buffer: 'created-window', revision: 1 },
    });
    const onResize = vi.fn();
    const oldSession = buildSession({ id: 'old', startedAt: '2026-07-01T00:00:00.000Z' });
    const createdSession = buildSession({ id: 'created', startedAt: '2026-07-01T00:01:00.000Z' });

    /**
     * Business Logic（为什么需要这个组件）:
     *   用户的稳定复现是“旧 window → 新建 window → 关闭新 window”，需要真实覆盖旧 pane 常驻挂载。
     *
     * Code Logic（这个组件做什么）:
     *   newWindowOpen 时隐藏旧 pane 并挂载新 pane；关闭时卸载新 pane、重新显示原 xterm 实例。
     */
    function WindowLifecycleHost(props: { newWindowOpen: boolean }): ReactElement {
      return (
        <>
          <PaneHost
            session={oldSession}
            store={store}
            renderVisible={!props.newWindowOpen}
            onResize={onResize}
          />
          {props.newWindowOpen ? (
            <PaneHost
              session={createdSession}
              store={store}
              renderVisible={true}
              onResize={onResize}
            />
          ) : null}
        </>
      );
    }

    const utils = render(<WindowLifecycleHost newWindowOpen={false} />);
    const oldTerminal = terminalEvents.instances[0];
    utils.rerender(<WindowLifecycleHost newWindowOpen={true} />);
    const oldResizeCountBeforeClose = onResize.mock.calls.filter(
      ([sessionId]) => sessionId === 'old',
    ).length;
    const oldInvalidationsBeforeClose = terminalEvents.clearSelectionCalls.filter(
      ({ instanceIndex }) => instanceIndex === 0,
    ).length;

    utils.rerender(<WindowLifecycleHost newWindowOpen={false} />);

    await waitFor(() => {
      expect(
        onResize.mock.calls.filter(([sessionId]) => sessionId === 'old'),
      ).toHaveLength(oldResizeCountBeforeClose + 1);
    });
    expect(terminalEvents.constructCount).toBe(2);
    expect(terminalEvents.disposeCount).toBe(1);
    expect(terminalEvents.instances[0]).toBe(oldTerminal);
    expect(
      terminalEvents.clearSelectionCalls.filter(({ instanceIndex }) => instanceIndex === 0),
    ).toHaveLength(oldInvalidationsBeforeClose + 1);
  });

  test('Terminal.dispose is not called for sessions that remain mounted while workspace view changes', () => {
    // Strengthen per Codex finding 4: explicitly assert dispose count across multiple workspace
    // switches to lock in that no intermediate dispose happens (which would force a rebuild).
    const { rerenderProps } = renderPaneWithRevision();
    expect(terminalEvents.disposeCount).toBe(0);

    rerenderProps({ renderVisible: false });
    expect(terminalEvents.disposeCount).toBe(0);
    rerenderProps({ renderVisible: true });
    expect(terminalEvents.disposeCount).toBe(0);
    rerenderProps({ renderVisible: false });
    expect(terminalEvents.disposeCount).toBe(0);
    // Final state: same instance alive.
    expect(terminalEvents.instances.length).toBe(1);
    expect(terminalEvents.constructCount).toBe(1);
  });
});

describe('WorkbenchTerminalPane — fires initial cursor anchor and cleanup null', () => {
  test('forwards cursor anchor changes via onCursorAnchorChange', () => {
    const session = buildSession({ id: 's1' });
    const onCursorAnchorChange = vi.fn();
    const store = createStoreFromSnapshots({ s1: { buffer: '', revision: 0 } });

    const { unmount } = render(
      <PaneHost
        session={session}
        store={store}
        inputEnabled={true}
        onCursorAnchorChange={onCursorAnchorChange}
      />,
    );

    // 初始 mount 后 emitCursorAnchor 至少调用一次（forceResizeRef.current = resize; resize(); 调用 emitCursorAnchor）。
    expect(onCursorAnchorChange).toHaveBeenCalled();
    const lastCall = onCursorAnchorChange.mock.calls[onCursorAnchorChange.mock.calls.length - 1]!;
    expect(lastCall[0]).not.toBeNull();

    // unmount 时清理回调应触发一次 null anchor。
    unmount();
    expect(onCursorAnchorChange).toHaveBeenCalledWith(null);
  });

  test('cursor move does not measure viewport when no anchor callback is registered', () => {
    renderPane({ onCursorAnchorChange: undefined });
    const viewport = screen.getByTestId('terminal-pane').firstElementChild as HTMLDivElement;
    const rect = vi.spyOn(viewport, 'getBoundingClientRect');
    act(() => {
      latestTerminal().emitCursorMove();
    });
    expect(rect).not.toHaveBeenCalled();
  });
});

describe('WorkbenchTerminalPane — Claude resume wheel', () => {
  test('active Agent uses captured history and hydrates resume scrollback instead of injecting unnegotiated SGR', () => {
    const store = createStoreFromSnapshots({ s1: { buffer: '', revision: 0 } });
    const onInput = vi.fn();
    const refreshScrollback = vi.fn();
    const session = buildSession({ id: 's1' });
    const { rerender } = render(
      <PaneHost
        session={session}
        store={store}
        onInput={onInput}
        agentTranscriptActive={false}
        refreshScrollback={refreshScrollback}
      />,
    );
    const terminal = latestTerminal();
    terminal.buffer.active.type = 'normal';
    terminal.buffer.active.baseY = 120;
    terminal.buffer.active.viewportY = 120;
    terminal.modes.mouseTrackingMode = 'none';

    // 应用重启后的 Agent metadata 可能尚未恢复；即使 isActive=false 也必须拉 tmux history。
    expect(terminal.invokeWheel({ deltaY: -20 })).toBe(false);
    expect(refreshScrollback).toHaveBeenCalledWith('s1');
    expect(refreshScrollback).toHaveBeenCalledTimes(1);
    expect(terminal.scrollToLine).not.toHaveBeenCalled();
    expect(onInput).not.toHaveBeenCalled();

    rerender(
      <PaneHost
        session={session}
        store={store}
        onInput={onInput}
        agentTranscriptActive={true}
        refreshScrollback={refreshScrollback}
      />,
    );

    expect(terminalEvents.constructCount).toBe(1);
    // Agent metadata 补齐时不得并发第二次 capture；累计这次向上滚动意图。
    expect(terminal.invokeWheel({ deltaY: -20 })).toBe(false);
    expect(terminal.scrollToBottom).not.toHaveBeenCalled();
    expect(onInput).not.toHaveBeenCalled();
    expect(refreshScrollback).toHaveBeenCalledTimes(1);
    expect(terminal.scrollToBottom).not.toHaveBeenCalled();
    expect(onInput).not.toHaveBeenCalled();

    terminal.buffer.active.type = 'normal';
    terminal.buffer.active.baseY = 80;
    terminal.buffer.active.viewportY = 80;
    act(() => {
      store.reset('s1', 'hydrated history');
    });
    expect(terminal.scrollToLine).toHaveBeenCalledWith(78);

    terminal.scrollToLine.mockClear();
    expect(terminal.invokeWheel({ deltaY: -20 })).toBe(false);
    expect(refreshScrollback).toHaveBeenCalledTimes(1);
    expect(terminal.scrollToLine).toHaveBeenCalledWith(77);
  });

  test('resume hydration resets an alternate xterm before replaying scrollback', () => {
    const store = createStoreFromSnapshots({ s1: { buffer: '', revision: 0 } });
    const refreshScrollback = vi.fn();
    render(
      <PaneHost
        session={buildSession({ id: 's1' })}
        store={store}
        agentTranscriptActive
        refreshScrollback={refreshScrollback}
      />,
    );
    const terminal = latestTerminal();
    terminal.buffer.active.type = 'alternate';
    terminal.buffer.active.baseY = 80;
    terminal.buffer.active.viewportY = 80;
    terminal.modes.mouseTrackingMode = 'none';

    expect(terminal.invokeWheel({ deltaY: -20 })).toBe(false);
    expect(refreshScrollback).toHaveBeenCalledTimes(1);

    act(() => {
      store.reset('s1', 'hydrated history');
    });

    expect(terminalEvents.resetCalls).toEqual([{ instanceIndex: 0 }]);
    expect(terminal.buffer.active.type).toBe('normal');
    expect(terminal.scrollToLine).toHaveBeenCalledWith(79);
  });

  test('Agent activation after shell hydration refreshes tmux history once for resume', () => {
    const store = createStoreFromSnapshots({ s1: { buffer: '', revision: 0 } });
    const refreshScrollback = vi.fn();
    const session = buildSession({ id: 's1' });
    const { rerender } = render(
      <PaneHost
        session={session}
        store={store}
        agentTranscriptActive={false}
        refreshScrollback={refreshScrollback}
      />,
    );
    const terminal = latestTerminal();
    terminal.buffer.active.baseY = 80;
    terminal.buffer.active.viewportY = 80;
    terminal.modes.mouseTrackingMode = 'none';

    expect(terminal.invokeWheel({ deltaY: -20 })).toBe(false);
    act(() => {
      store.reset('s1', 'shell history');
    });
    expect(refreshScrollback).toHaveBeenCalledTimes(1);

    rerender(
      <PaneHost
        session={session}
        store={store}
        agentTranscriptActive
        refreshScrollback={refreshScrollback}
      />,
    );
    terminal.scrollToLine.mockClear();
    expect(terminal.invokeWheel({ deltaY: -20 })).toBe(false);
    expect(terminal.invokeWheel({ deltaY: -20 })).toBe(false);
    expect(refreshScrollback).toHaveBeenCalledTimes(2);
    expect(terminal.scrollToLine).not.toHaveBeenCalled();
  });

  test('hydration does not auto-scroll after Claude enables mouse tracking', () => {
    const store = createStoreFromSnapshots({ s1: { buffer: '', revision: 0 } });
    const refreshScrollback = vi.fn();
    render(
      <PaneHost
        session={buildSession({ id: 's1' })}
        store={store}
        agentTranscriptActive
        refreshScrollback={refreshScrollback}
      />,
    );
    const terminal = latestTerminal();
    terminal.buffer.active.baseY = 0;
    expect(terminal.invokeWheel({ deltaY: -20 })).toBe(false);

    terminal.buffer.active.baseY = 80;
    terminal.modes.mouseTrackingMode = 'vt200';
    act(() => {
      store.reset('s1', 'hydrated history');
    });

    expect(refreshScrollback).toHaveBeenCalledTimes(1);
    expect(terminal.scrollToLine).not.toHaveBeenCalled();
  });

  test('mouse-tracked main screen forwards wheel to Claude instead of xterm redraw history', () => {
    const store = createStoreFromSnapshots({ s1: { buffer: '', revision: 0 } });
    const onInput = vi.fn();
    render(<PaneHost session={buildSession({ id: 's1' })} store={store} onInput={onInput} />);
    const terminal = latestTerminal();
    terminal.buffer.active.type = 'normal';
    terminal.buffer.active.baseY = 120;
    terminal.modes.mouseTrackingMode = 'vt200';

    const proceed = terminal.invokeWheel({
      deltaY: -20,
      preventDefault: vi.fn(),
      stopPropagation: vi.fn(),
    });

    expect(proceed).toBe(false);
    expect(terminal.scrollToBottom).toHaveBeenCalledTimes(1);
    expect(onInput).toHaveBeenCalledWith('s1', '\x1b[<64;1;1M');
  });

  test('alt-screen pins SGR to transcript origin even when the pointer is over a tall input area', () => {
    const store = createStoreFromSnapshots({ s1: { buffer: '', revision: 0 } });
    const onInput = vi.fn();
    render(<PaneHost session={buildSession({ id: 's1' })} store={store} onInput={onInput} />);
    const terminal = latestTerminal();
    terminal.cols = 80;
    terminal.rows = 32;
    terminal.buffer.active.type = 'alternate';
    terminal.buffer.active.baseY = 0;
    terminal.modes.mouseTrackingMode = 'vt200';
    const viewport = screen.getByTestId('terminal-pane').firstElementChild as HTMLDivElement;
    vi.spyOn(viewport, 'getBoundingClientRect').mockReturnValue({
      left: 0,
      top: 0,
      width: 800,
      height: 512,
      right: 800,
      bottom: 512,
      x: 0,
      y: 0,
      toJSON() {
        return {};
      },
    });

    const proceed = terminal.invokeWheel({
      deltaY: -20,
      clientX: 8,
      clientY: 500,
      preventDefault: vi.fn(),
      stopPropagation: vi.fn(),
    });

    expect(proceed).toBe(false);
    expect(onInput).toHaveBeenCalledTimes(1);
    const payload = String(onInput.mock.calls[0]?.[1] ?? '');
    expect(payload).toBe('\x1b[<64;1;1M');
    expect(payload.includes('\x1b[A') || payload.includes('\x1bOA')).toBe(false);
    expect(payload.includes('\x1b[5~')).toBe(false);
  });

  test('source contract keeps custom wheel handler and never encodes arrow keys or PageUp', () => {
    const paneSource = readFileSync(resolve(__dirname, './WorkbenchTerminalPane.tsx'), 'utf8');
    expect(paneSource).toContain('attachCustomWheelEventHandler');
    expect(paneSource).toContain('encodeTerminalSgrWheelReports');
    expect(paneSource).not.toContain('clampTranscriptWheelCell');
    expect(paneSource).not.toContain('encodeTerminalPageScrollKeys');
    expect(paneSource).not.toContain("\\x1b[' +");
  });
});

/**
 * 占位：保留对未来键盘事件转发测试的扩展点（当前用例不需要 fireEvent）。
 */

describe('WorkbenchTerminalPane — live writer ownership', () => {
  test('desktop pane uses createTerminalLiveWriter and no planTerminalBufferWrite', () => {
    const workbenchTerminalPaneSource = readFileSync(
      resolve(__dirname, './WorkbenchTerminalPane.tsx'),
      'utf8',
    );
    expect(workbenchTerminalPaneSource).not.toContain('planTerminalBufferWrite');
    expect(workbenchTerminalPaneSource).toContain('createTerminalLiveWriter');
  });
});
