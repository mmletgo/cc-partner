// @vitest-environment jsdom
/**
 * MobileTerminalPanel 刷新后终端滚动回归测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   `/mobile` 页面刷新后，终端应仍能向上滚动查看老内容（replay 写入的历史 scrollback）。
 *   早期实现里 terminal effect 的 sessions.replay 写入历史后，外部 buffer store 重建只含
 *   NDJSON live（不含历史），与 writtenBuffer 字符串 diff 失败 → terminal.clear() 清空 scrollback，
 *   且后续每条 live 重复 clear，导致老内容永久丢失、无法上滚。
 *
 * Code Logic（这个测试做什么）:
 *   - mock xterm Terminal 记录 clear/write 调用；
 *   - mock transport.sessions.replay 返回历史 + lastSeq + ownerInstanceId；
 *   - 用真实 terminal buffer store + Provider；
 *   - 模拟刷新序列：replay 写入历史 → store 收到不含历史的 live 增量 → 断言 clear 未被调用、
 *     live 被 append。
 */
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { StrictMode, type ReactElement, type ReactNode } from 'react';

import { OrchestratorRuntimeTransportError } from '@/api/orchestratorRuntimeTransportError';
import { MobileTerminalPanel } from './MobileTerminalPanel';
import * as extraKeys from '../mobileTerminalExtraKeys';
import { WorkbenchTerminalBuffersContext } from '@/hooks/workbenchTerminalBuffersContext';
import type { WorkbenchTerminalBuffersContextValue } from '@/hooks/workbenchTerminalBuffersContext';
import { createWorkbenchTerminalBufferStore } from '@/hooks/workbenchTerminalBuffer';
import type { WorkbenchTerminalBufferStore } from '@/hooks/workbenchTerminalBuffer';
import type {
  WorkbenchProject,
  WorkbenchSession,
  WorkbenchSessionReplay,
  WorkbenchWorktree,
} from '@/lib/types';

interface MockTerminalInstance {
  write: (data: string, cb?: () => void) => void;
  clear: () => void;
  reset: () => void;
  scrollToLine: (line: number) => void;
  scrollLines: (amount: number) => void;
  select: (column: number, row: number, length: number) => void;
  getSelection: () => string;
  clearSelection: () => void;
  buffer: {
    active: {
      type: 'normal' | 'alternate';
      baseY: number;
      viewportY: number;
    };
  };
  modes: { mouseTrackingMode: 'none' | 'vt200' };
  openedElement: HTMLElement | null;
}

const terminalEvents = vi.hoisted(() => ({
  clearCalls: [] as Array<{ instance: number }>,
  writeCalls: [] as Array<{ data: string; instance: number }>,
  replayResult: null as WorkbenchSessionReplay | null,
  replayPromise: null as Promise<WorkbenchSessionReplay | null> | null,
  hydrationResult: null as WorkbenchSessionReplay | null,
  hydrationPromise: null as Promise<WorkbenchSessionReplay | null> | null,
  hydrateCalls: [] as string[],
  resizeCalls: [] as Array<{ sessionId: string; cols: number; rows: number }>,
  resizeError: null as unknown,
  pasteImageCalls: [] as Array<{ sessionId: string; dataUrl: string }>,
  resetCalls: [] as Array<{ instance: number }>,
  scrollToLineCalls: [] as Array<{ instance: number; line: number }>,
  instances: [] as MockTerminalInstance[],
  commitCalls: [] as Array<{ worktreeId: string; message: string | null; clientOperationId: string }>,
  commitResult: null as unknown,
  repairCalls: [] as Array<{ worktreeId: string; hookFailure: unknown }>,
  repairResult: null as unknown,
  clipboardWrites: [] as string[],
}));

vi.mock('../mobileClipboard', () => ({
  writeClipboardText: vi.fn((text: string) => {
    terminalEvents.clipboardWrites.push(text);
    return Promise.resolve({ ok: true, method: 'clipboard' });
  }),
}));

vi.mock('@xterm/xterm', () => {
  let instanceCount = 0;
  return {
    Terminal: class {
      cols = 80;
      rows = 24;
      options: Record<string, unknown> = {};
      buffer = {
        active: {
          type: 'normal' as 'normal' | 'alternate',
          baseY: 0,
          viewportY: 0,
        },
      };
      modes = { mouseTrackingMode: 'none' as 'none' | 'vt200' };
      openedElement: HTMLElement | null = null;
      private selectionText = '';
      private readonly instance: number;
      constructor() {
        this.instance = instanceCount++;
        terminalEvents.instances.push(this);
      }
      loadAddon(): void {}
      open(element: HTMLElement): void {
        this.openedElement = element;
      }
      onData(): { dispose: () => void } {
        return { dispose: () => undefined };
      }
      write(data: string, cb?: () => void): void {
        terminalEvents.writeCalls.push({ data, instance: this.instance });
        const lineCount = data.split('\n').length - 1;
        if (lineCount > this.rows) {
          this.buffer.active.baseY = lineCount - this.rows;
          this.buffer.active.viewportY = this.buffer.active.baseY;
        }
        cb?.();
      }
      clear(): void {
        terminalEvents.clearCalls.push({ instance: this.instance });
        this.buffer.active.baseY = 0;
        this.buffer.active.viewportY = 0;
      }
      scrollLines(): void {}
      select(): void {
        this.selectionText = 'copy-me';
      }
      getSelection(): string {
        return this.selectionText;
      }
      clearSelection(): void {
        this.selectionText = '';
      }
      scrollToLine(line: number): void {
        this.buffer.active.viewportY = line;
        terminalEvents.scrollToLineCalls.push({ instance: this.instance, line });
      }
      reset(): void {
        this.buffer.active.type = 'normal';
        this.buffer.active.baseY = 0;
        this.buffer.active.viewportY = 0;
        this.modes.mouseTrackingMode = 'none';
        terminalEvents.resetCalls.push({ instance: this.instance });
      }
      dispose(): void {}
      blur(): void {}
      focus(): void {}
    },
  };
});

vi.mock('@xterm/addon-fit', () => ({
  FitAddon: class {
    fit(): void {}
  },
}));

vi.mock('@/api/workbenchHttp', () => ({
  createHttpOrchestratorClientRequestId: vi.fn(() => 'op-terminal-1'),
  workbenchHttp: {
    git: {
      commit: vi.fn((request: {
        worktreeId: string;
        message?: string | null;
        clientOperationId: string;
      }) => {
        terminalEvents.commitCalls.push({
          worktreeId: request.worktreeId,
          message: request.message ?? null,
          clientOperationId: request.clientOperationId,
        });
        return Promise.resolve(
          terminalEvents.commitResult ?? {
            kind: 'succeeded',
            value: null,
            clientOperationId: request.clientOperationId,
          },
        );
      }),
      getMutationOperation: vi.fn(() => Promise.resolve(null)),
      repairHookFailure: vi.fn((worktreeId: string, hookFailure: unknown) => {
        terminalEvents.repairCalls.push({ worktreeId, hookFailure });
        return Promise.resolve(
          terminalEvents.repairResult ?? {
            agentSessionId: 'agent-1',
            terminalSessionId: 'term-repair',
            worktreeId,
            projectId: 'p1',
          },
        );
      }),
    },
  },
  httpWorkbenchTransport: {
    sessions: {
      replay: vi.fn(() =>
        terminalEvents.replayPromise ?? Promise.resolve(terminalEvents.replayResult),
      ),
      hydrateScrollback: vi.fn((sessionId: string) => {
        terminalEvents.hydrateCalls.push(sessionId);
        return terminalEvents.hydrationPromise ?? Promise.resolve(terminalEvents.hydrationResult);
      }),
      focus: vi.fn(() => Promise.resolve()),
      zoomPane: vi.fn(() => Promise.resolve()),
      resize: vi.fn((sessionId: string, cols: number, rows: number) => {
        terminalEvents.resizeCalls.push({ sessionId, cols, rows });
        if (terminalEvents.resizeError) {
          return Promise.reject(terminalEvents.resizeError);
        }
        return Promise.resolve();
      }),
      pasteImage: vi.fn((sessionId: string, dataUrl: string) => {
        terminalEvents.pasteImageCalls.push({ sessionId, dataUrl });
        return Promise.resolve({ ok: true, sessionId });
      }),
    },
  },
}));

vi.mock('react-i18next', () => {
  const translate = (key: string): string => key;
  return {
    useTranslation: () => ({ t: translate }),
    initReactI18next: { type: '3rdParty', init: () => undefined },
  };
});

vi.mock('../mobileTerminalInputStream', () => ({
  MobileTerminalInputStream: class {
    constructor(options?: { onStateChange?: (state: { status: string }) => void }) {
      options?.onStateChange?.({ status: 'ready' });
    }
    enqueue(): void {}
    close(): void {}
  },
}));

vi.mock('@/lib/icons', () => ({
  ArrowRightIcon: (): null => null,
  CommitIcon: (): null => null,
  SyncIcon: (): null => null,
  EditIcon: (): null => null,
  ImageIcon: (): null => null,
  MaximizeIcon: (): null => null,
  MinimizeIcon: (): null => null,
  MoreIcon: (): null => null,
  PlusIcon: (): null => null,
  PromptsIcon: (): null => null,
  RefreshIcon: (): null => null,
  SearchIcon: (): null => null,
  StarIcon: (): null => null,
  XIcon: (): null => null,
}));

function buildProject(): WorkbenchProject {
  return {
    id: 'p1',
    name: 'proj',
    kind: 'local',
    deviceId: 'd1',
    deviceName: 'dev',
    path: '/p',
    lastOpenedAt: '',
    createdAt: '',
    updatedAt: '',
  };
}

function buildSession(overrides: Partial<WorkbenchSession> = {}): WorkbenchSession {
  return {
    id: 's1',
    projectId: 'p1',
    worktreeId: null,
    name: 'term',
    command: 'bash',
    cwd: '/p',
    status: 'running',
    cols: 80,
    rows: 24,
    startedAt: '',
    exitedAt: null,
    exitCode: null,
    supportsPanes: false,
    paneCount: 1,
    ...overrides,
  };
}

function buildWorktree(overrides: Partial<WorkbenchWorktree> = {}): WorkbenchWorktree {
  return {
    id: 'wt-1',
    projectId: 'p1',
    name: 'main',
    branch: 'main',
    baseBranch: null,
    path: '/p',
    isMain: true,
    canCollectMerge: false,
    homeBranch: null,
    collectibleBranches: [],
    status: {
      branch: 'main',
      changed: 2,
      ahead: 0,
      behind: 0,
      conflicts: 0,
      clean: false,
      canPush: true,
    },
    createdAt: '',
    updatedAt: '',
    ...overrides,
  };
}

function BuffersProvider({
  store,
  children,
}: {
  store: WorkbenchTerminalBufferStore;
  children: ReactNode;
}): ReactElement {
  const value: WorkbenchTerminalBuffersContextValue = {
    store,
    resetBuffer: (sessionId: string) => store.reset(sessionId),
    removeBuffer: (sessionId: string) => store.remove(sessionId),
    getHistorySyncFailure: () => null,
    subscribeHistorySyncFailures: () => () => undefined,
    getHistorySyncFailuresRevision: () => 0,
    retryHistorySync: () => undefined,
    refreshScrollback: () => undefined,
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

/** 返回最近创建的 mock xterm，供触摸 hydration 行为测试驱动公开状态。 */
function latestMockTerminal(): MockTerminalInstance {
  const terminal = terminalEvents.instances.at(-1);
  if (!terminal) throw new Error('expected a mounted mock terminal');
  return terminal;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   jsdom 没有完整 TouchEvent 构造器，移动端回归测试仍需按真实 capture listener 驱动单指手势。
 *
 * Code Logic（这个函数做什么）:
 *   创建可取消冒泡 Event，并注入只读 touches 数组后派发到 xterm viewport。
 */
function dispatchSingleTouch(
  element: HTMLElement,
  type: 'touchstart' | 'touchmove' | 'touchend' | 'touchcancel',
  clientY: number,
  clientX = 12,
): void {
  const event = new Event(type, { bubbles: true, cancelable: true });
  Object.defineProperty(event, 'touches', {
    configurable: true,
    value:
      type === 'touchend' || type === 'touchcancel'
        ? []
        : [{ clientX, clientY }],
  });
  element.dispatchEvent(event);
}

const FAB_MENU_OPEN_LABEL = 'workbench:mobile.terminalPanel.fabMenu.open';
const FAB_MENU_CLOSE_LABEL = 'workbench:mobile.terminalPanel.fabMenu.close';

/**
 * Business Logic（为什么需要这个函数）:
 *   终端右下角动作默认收进一个折叠按钮，测试要点具体 FAB 必须先展开。
 *
 * Code Logic（这个函数做什么）:
 *   若菜单已收起则点击展开按钮；已展开则保持原状。
 */
function openTerminalFabMenu(): void {
  const trigger = screen.queryByRole('button', { name: FAB_MENU_OPEN_LABEL });
  if (!trigger) {
    expect(screen.getByRole('button', { name: FAB_MENU_CLOSE_LABEL })).toBeTruthy();
    return;
  }
  fireEvent.click(trigger);
}

describe('MobileTerminalPanel — refresh scrollback', () => {
  beforeEach(() => {
    terminalEvents.clearCalls.length = 0;
    terminalEvents.writeCalls.length = 0;
    terminalEvents.instances.length = 0;
    terminalEvents.replayResult = null;
    terminalEvents.replayPromise = null;
    terminalEvents.hydrationResult = null;
    terminalEvents.hydrationPromise = null;
    terminalEvents.hydrateCalls.length = 0;
    terminalEvents.resizeCalls.length = 0;
    terminalEvents.resizeError = null;
    terminalEvents.pasteImageCalls.length = 0;
    terminalEvents.resetCalls.length = 0;
    terminalEvents.scrollToLineCalls.length = 0;
    terminalEvents.commitCalls.length = 0;
    terminalEvents.commitResult = null;
    terminalEvents.repairCalls.length = 0;
    terminalEvents.repairResult = null;
    terminalEvents.clipboardWrites.length = 0;
    // jsdom 没有 ResizeObserver；terminal effect 依赖它做 fit。
    global.ResizeObserver = class {
      observe(): void {}
      unobserve(): void {}
      disconnect(): void {}
    };
  });

  test('does not force resize when the fitted viewport matches the persisted session size', async () => {
    // 同尺寸 resize 会让后端抖动 tmux rows 以强制重绘，把 Claude TUI 末屏再次写入 scrollback。
    terminalEvents.replayResult = {
      sessionId: 's1',
      buffer: 'ready\n',
      truncated: false,
      lastSeq: 1,
      ownerInstanceId: 'owner-1',
    };

    const store = createWorkbenchTerminalBufferStore();
    const session = buildSession();

    render(
      <StrictMode>
        <BuffersProvider store={store}>
          <MobileTerminalPanel
            project={buildProject()}
            worktree={null}
            sessions={[session]}
            activeSession={session}
            busy={false}
            onSessionsChange={() => undefined}
            onActiveSessionChange={() => undefined}
          />
        </BuffersProvider>
      </StrictMode>,
    );

    await waitFor(() => {
      expect(terminalEvents.writeCalls.some((call) => call.data.includes('ready'))).toBe(true);
    });
    expect(terminalEvents.resizeCalls).toEqual([]);
  });

  test('resizes when the fitted viewport differs from the persisted session size', async () => {
    terminalEvents.replayResult = {
      sessionId: 's1',
      buffer: 'ready\n',
      truncated: false,
      lastSeq: 1,
      ownerInstanceId: 'owner-1',
    };

    const store = createWorkbenchTerminalBufferStore();
    const session = { ...buildSession(), cols: 79, rows: 23 };

    render(
      <StrictMode>
        <BuffersProvider store={store}>
          <MobileTerminalPanel
            project={buildProject()}
            worktree={null}
            sessions={[session]}
            activeSession={session}
            busy={false}
            onSessionsChange={() => undefined}
            onActiveSessionChange={() => undefined}
          />
        </BuffersProvider>
      </StrictMode>,
    );

    await waitFor(() => {
      expect(terminalEvents.resizeCalls).toEqual([
        { sessionId: 's1', cols: 80, rows: 24 },
      ]);
    });
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   merge 关闭会话后 ResizeObserver 仍可能 resize 旧 sessionId；not_found 是预期拆卸，不得投影加载失败。
   *
   * Code Logic（这个测试做什么）:
   *   持久化尺寸与 fit 不一致以触发 resize；reject `{ code: not_found }`，断言没有 role=alert。
   */
  test('resize not_found after a closed session does not project an alert', async () => {
    terminalEvents.replayResult = {
      sessionId: 's1',
      buffer: 'ready\n',
      truncated: false,
      lastSeq: 1,
      ownerInstanceId: 'owner-1',
    };
    terminalEvents.resizeError = new OrchestratorRuntimeTransportError(
      '工作台会话不存在',
      'protocol',
      'not_found',
    );

    const store = createWorkbenchTerminalBufferStore();
    const session = { ...buildSession(), cols: 79, rows: 23 };

    render(
      <StrictMode>
        <BuffersProvider store={store}>
          <MobileTerminalPanel
            project={buildProject()}
            worktree={null}
            sessions={[session]}
            activeSession={session}
            busy={false}
            onSessionsChange={() => undefined}
            onActiveSessionChange={() => undefined}
          />
        </BuffersProvider>
      </StrictMode>,
    );

    await waitFor(() => {
      expect(terminalEvents.resizeCalls).toEqual([
        { sessionId: 's1', cols: 80, rows: 24 },
      ]);
    });
    await act(async () => {
      await Promise.resolve();
    });
    expect(screen.queryByRole('alert')).toBeNull();
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  test('replay history is not cleared when NDJSON live arrives after refresh', async () => {
    // 历史输出远超一屏（24 行），replay 写入后应进入 xterm scrollback 供向上滚动查看。
    terminalEvents.replayResult = {
      sessionId: 's1',
      buffer: `${'history line\n'.repeat(50)}`,
      truncated: false,
      lastSeq: 100,
      ownerInstanceId: 'owner-1',
    };

    const store = createWorkbenchTerminalBufferStore();
    const session = buildSession();

    render(
      <BuffersProvider store={store}>
        <MobileTerminalPanel
          project={buildProject()}
          worktree={null}
          sessions={[session]}
          activeSession={session}
          busy={false}
          onSessionsChange={() => undefined}
          onActiveSessionChange={() => undefined}
        />
      </BuffersProvider>,
    );

    // 等 replay 把历史写入 xterm。
    await waitFor(() => {
      expect(
        terminalEvents.writeCalls.some((call) => call.data.includes('history line')),
      ).toBe(true);
    });

    // 模拟刷新后 NDJSON live 增量：seq > replay.lastSeq、同 owner、不含历史前缀。
    act(() => {
      store.append('s1', 'fresh-live-chunk\n', 101, 'owner-1');
    });

    // live 增量应被 append 到终端。
    await waitFor(() => {
      expect(
        terminalEvents.writeCalls.some((call) => call.data.includes('fresh-live-chunk')),
      ).toBe(true);
    });

    // 关键断言：scrollback（replay 历史）不得被 terminal.clear() 清空。
    expect(terminalEvents.clearCalls.length).toBe(0);
  });

  test('queues first history swipe until baseline, hydrates once, resets alternate and applies accumulated scroll', async () => {
    let resolveReplay: ((value: WorkbenchSessionReplay) => void) | null = null;
    let resolveHydration: ((value: WorkbenchSessionReplay) => void) | null = null;
    terminalEvents.replayPromise = new Promise<WorkbenchSessionReplay>((resolve) => {
      resolveReplay = resolve;
    });
    terminalEvents.hydrationPromise = new Promise<WorkbenchSessionReplay>((resolve) => {
      resolveHydration = resolve;
    });

    const store = createWorkbenchTerminalBufferStore();
    const session = buildSession();
    render(
      <BuffersProvider store={store}>
        <MobileTerminalPanel
          project={buildProject()}
          worktree={null}
          sessions={[session]}
          activeSession={session}
          busy={false}
          onSessionsChange={() => undefined}
          onActiveSessionChange={() => undefined}
        />
      </BuffersProvider>,
    );

    const terminal = latestMockTerminal();
    const viewport = terminal.openedElement;
    if (!viewport) throw new Error('expected xterm viewport');
    act(() => {
      dispatchSingleTouch(viewport, 'touchstart', 100);
      dispatchSingleTouch(viewport, 'touchmove', 140);
    });
    expect(terminalEvents.hydrateCalls).toHaveLength(0);

    await act(async () => {
      resolveReplay?.({
        sessionId: 's1',
        buffer: 'current screen\n',
        truncated: false,
        lastSeq: 10,
        ownerInstanceId: 'owner-1',
      });
      await Promise.resolve();
    });
    await waitFor(() => expect(terminalEvents.hydrateCalls).toEqual(['s1']));

    act(() => {
      dispatchSingleTouch(viewport, 'touchmove', 180);
      terminal.buffer.active.type = 'alternate';
    });
    expect(terminalEvents.hydrateCalls).toEqual(['s1']);

    await act(async () => {
      resolveHydration?.({
        sessionId: 's1',
        buffer: 'history line\n'.repeat(60),
        truncated: false,
        lastSeq: 20,
        ownerInstanceId: 'owner-1',
      });
      await Promise.resolve();
    });

    await waitFor(() => expect(terminalEvents.scrollToLineCalls).toHaveLength(1));
    expect(terminalEvents.resetCalls).toHaveLength(1);
    expect(terminal.buffer.active.type).toBe('normal');
    expect(terminalEvents.scrollToLineCalls[0]?.line).toBeLessThan(
      terminal.buffer.active.baseY,
    );
  });

  test('unlocks hydration after a request failure so the next history swipe can retry', async () => {
    let rejectHydration: ((reason: Error) => void) | null = null;
    terminalEvents.replayResult = {
      sessionId: 's1',
      buffer: 'current screen\n',
      truncated: false,
      lastSeq: 10,
      ownerInstanceId: 'owner-1',
    };
    terminalEvents.hydrationPromise = new Promise<WorkbenchSessionReplay>((_resolve, reject) => {
      rejectHydration = reject;
    });

    const store = createWorkbenchTerminalBufferStore();
    const session = buildSession();
    render(
      <BuffersProvider store={store}>
        <MobileTerminalPanel
          project={buildProject()}
          worktree={null}
          sessions={[session]}
          activeSession={session}
          busy={false}
          onSessionsChange={() => undefined}
          onActiveSessionChange={() => undefined}
        />
      </BuffersProvider>,
    );

    await waitFor(() => {
      expect(terminalEvents.writeCalls.some((call) => call.data === 'current screen\n')).toBe(true);
    });
    const terminal = latestMockTerminal();
    const viewport = terminal.openedElement;
    if (!viewport) throw new Error('expected xterm viewport');

    act(() => {
      dispatchSingleTouch(viewport, 'touchstart', 100);
      dispatchSingleTouch(viewport, 'touchmove', 140);
    });
    await waitFor(() => expect(terminalEvents.hydrateCalls).toEqual(['s1']));
    await act(async () => {
      rejectHydration?.(new Error('temporary failure'));
      await Promise.resolve();
    });

    terminalEvents.hydrationPromise = null;
    terminalEvents.hydrationResult = {
      sessionId: 's1',
      buffer: 'history line\n'.repeat(60),
      truncated: false,
      lastSeq: 20,
      ownerInstanceId: 'owner-1',
    };
    const retryTerminal = latestMockTerminal();
    const retryViewport = retryTerminal.openedElement;
    if (!retryViewport) throw new Error('expected retry xterm viewport');
    act(() => {
      dispatchSingleTouch(retryViewport, 'touchstart', 100);
      dispatchSingleTouch(retryViewport, 'touchmove', 140);
    });

    await waitFor(() => expect(terminalEvents.hydrateCalls).toEqual(['s1', 's1']));
    await waitFor(() => expect(terminalEvents.scrollToLineCalls).toHaveLength(1));
    expect(terminalEvents.scrollToLineCalls[0]?.line).toBeLessThan(
      retryTerminal.buffer.active.baseY,
    );
  });

  test('continues rendering exact live deltas after the bounded store trims to identical text', async () => {
    // TUI 重绘常会连续产生相同字节；ring buffer 达上限后，append 前后的物化字符串可能完全相同。
    // 若移动端只比较 React buffer 字符串，就会把真实新 delta 当成 no-op，终端从这里开始“卡死”。
    terminalEvents.replayResult = {
      sessionId: 's1',
      buffer: 'AAAA',
      truncated: false,
      lastSeq: 10,
      ownerInstanceId: 'owner-1',
    };

    const store = createWorkbenchTerminalBufferStore({ maxChars: 4 });
    const session = buildSession();

    render(
      <BuffersProvider store={store}>
        <MobileTerminalPanel
          project={buildProject()}
          worktree={null}
          sessions={[session]}
          activeSession={session}
          busy={false}
          onSessionsChange={() => undefined}
          onActiveSessionChange={() => undefined}
        />
      </BuffersProvider>,
    );

    await waitFor(() => {
      expect(terminalEvents.writeCalls.some((call) => call.data === 'AAAA')).toBe(true);
    });

    act(() => {
      store.append('s1', 'A', 11, 'owner-1');
    });

    await waitFor(() => {
      expect(terminalEvents.writeCalls.filter((call) => call.data === 'A')).toHaveLength(1);
    });
    expect(store.getBuffer('s1')).toBe('AAAA');
    expect(terminalEvents.clearCalls).toHaveLength(0);
  });

  test('keeps exact live output that arrives while HTTP replay is in flight', async () => {
    let resolveReplay: ((value: WorkbenchSessionReplay) => void) | null = null;
    terminalEvents.replayPromise = new Promise<WorkbenchSessionReplay>((resolve) => {
      resolveReplay = resolve;
    });

    const store = createWorkbenchTerminalBufferStore();
    const session = buildSession();
    render(
      <BuffersProvider store={store}>
        <MobileTerminalPanel
          project={buildProject()}
          worktree={null}
          sessions={[session]}
          activeSession={session}
          busy={false}
          onSessionsChange={() => undefined}
          onActiveSessionChange={() => undefined}
        />
      </BuffersProvider>,
    );

    // 该 delta 与 replay 没有字符串前后缀关系；只能靠 listener-first owner/seq 握手保住。
    act(() => {
      store.append('s1', '\u001b[2Jfresh-tui-frame', 11, 'owner-1');
    });
    await act(async () => {
      resolveReplay?.({
        sessionId: 's1',
        buffer: 'history',
        truncated: false,
        lastSeq: 10,
        ownerInstanceId: 'owner-1',
      });
      await Promise.resolve();
    });

    await waitFor(() => {
      expect(
        terminalEvents.writeCalls.some(
          (call) => call.data === 'history\u001b[2Jfresh-tui-frame',
        ),
      ).toBe(true);
    });
    expect(store.getBuffer('s1')).toBe('history\u001b[2Jfresh-tui-frame');
    expect(store.getLastSeq('s1')).toBe(11);
  });

  test('pre-existing short live suffix does not become store baseline or clear scrollback', async () => {
    // 刷新后 NDJSON store 只含近期后缀，HTTP replay 才有完整历史。
    // 若 writtenBuffer/store baseline 错误地取 short live，后续 diff 会 clear 掉 scrollback。
    const fullHistory = `${'history line\n'.repeat(40)}recent-tail\n`;
    const shortLive = 'recent-tail\n';
    terminalEvents.replayResult = {
      sessionId: 's1',
      buffer: fullHistory,
      truncated: false,
      lastSeq: 50,
      ownerInstanceId: 'owner-1',
    };

    const store = createWorkbenchTerminalBufferStore();
    // 模拟页面打开前 NDJSON 已写入的短 live 后缀（不含更早历史）。
    act(() => {
      store.append('s1', shortLive, 50, 'owner-1');
    });
    const session = buildSession();

    render(
      <BuffersProvider store={store}>
        <MobileTerminalPanel
          project={buildProject()}
          worktree={null}
          sessions={[session]}
          activeSession={session}
          busy={false}
          onSessionsChange={() => undefined}
          onActiveSessionChange={() => undefined}
        />
      </BuffersProvider>,
    );

    await waitFor(() => {
      expect(
        terminalEvents.writeCalls.some((call) => call.data.includes('history line')),
      ).toBe(true);
    });

    // store baseline 必须是完整历史，不能只剩 short live。
    await waitFor(() => {
      expect(store.getBuffer('s1')).toBe(fullHistory);
    });

    act(() => {
      store.append('s1', 'post-open-chunk\n', 51, 'owner-1');
    });

    await waitFor(() => {
      expect(
        terminalEvents.writeCalls.some((call) => call.data.includes('post-open-chunk')),
      ).toBe(true);
    });

    expect(terminalEvents.clearCalls.length).toBe(0);
    expect(store.getBuffer('s1')).toContain('history line');
    expect(store.getBuffer('s1')).toContain('post-open-chunk');
  });

  test('does not mix another session live buffer into current window replay', async () => {
    // 全局 store 同时缓存两个 window；打开 s1 时只能写入 s1 的 HTTP replay，
    // 不得把 s2 的 live 内容拼进 s1 的 xterm / store baseline。
    terminalEvents.replayResult = {
      sessionId: 's1',
      buffer: 'window-one-history\n$ ls\none.txt\n',
      truncated: false,
      lastSeq: 10,
      ownerInstanceId: 'owner-1',
    };

    const store = createWorkbenchTerminalBufferStore();
    act(() => {
      store.append('s1', 'one.txt\n', 10, 'owner-1');
      store.append('s2', 'window-two-only\n$ pwd\n/tmp/two\n', 20, 'owner-1');
    });
    const session = buildSession();

    render(
      <BuffersProvider store={store}>
        <MobileTerminalPanel
          project={buildProject()}
          worktree={null}
          sessions={[session]}
          activeSession={session}
          busy={false}
          onSessionsChange={() => undefined}
          onActiveSessionChange={() => undefined}
        />
      </BuffersProvider>,
    );

    await waitFor(() => {
      expect(
        terminalEvents.writeCalls.some((call) => call.data.includes('window-one-history')),
      ).toBe(true);
    });

    const mixedIntoXterm = terminalEvents.writeCalls.some((call) =>
      call.data.includes('window-two-only'),
    );
    expect(mixedIntoXterm).toBe(false);
    expect(store.getBuffer('s1')).not.toContain('window-two-only');
    expect(store.getBuffer('s2')).toContain('window-two-only');
  });

  test('misaligned short store snapshot never clears scrollback after replay', async () => {
    // 用户现象：上滚只剩打开时最后一屏，或完全不能上滚。
    // 根因：replay 写入完整历史后，store 又来一段与 written 不对齐/更短的内容，
    // 旧路径 plan.mode=replay → terminal.clear() 清掉 xterm scrollback。
    const fullHistory = `${'history line\n'.repeat(40)}recent-tail\n`;
    terminalEvents.replayResult = {
      sessionId: 's1',
      buffer: fullHistory,
      truncated: false,
      lastSeq: 80,
      ownerInstanceId: 'owner-1',
    };

    const store = createWorkbenchTerminalBufferStore();
    const session = buildSession();

    render(
      <BuffersProvider store={store}>
        <MobileTerminalPanel
          project={buildProject()}
          worktree={null}
          sessions={[session]}
          activeSession={session}
          busy={false}
          onSessionsChange={() => undefined}
          onActiveSessionChange={() => undefined}
        />
      </BuffersProvider>,
    );

    await waitFor(() => {
      expect(
        terminalEvents.writeCalls.some((call) => call.data.includes('history line')),
      ).toBe(true);
    });

    // 模拟 store 被短快照覆盖（与 written 不对齐）：旧逻辑会 clear，新逻辑必须忽略。
    act(() => {
      store.reset('s1', 'recent-tail\n', 80, 'owner-1');
    });

    await act(async () => {
      await Promise.resolve();
    });

    expect(terminalEvents.clearCalls.length).toBe(0);
    // 历史 write 仍应保留在调用记录中（xterm 侧未被 clear 后重写）。
    expect(
      terminalEvents.writeCalls.some((call) => call.data.includes('history line')),
    ).toBe(true);
  });

  test('paste event with an image file posts paste-image instead of typing into xterm', async () => {
    terminalEvents.replayResult = {
      sessionId: 's1',
      buffer: 'ready\n',
      truncated: false,
      lastSeq: 1,
      ownerInstanceId: 'owner-1',
    };

    const store = createWorkbenchTerminalBufferStore();
    const session = buildSession();
    render(
      <BuffersProvider store={store}>
        <MobileTerminalPanel
          project={buildProject()}
          worktree={null}
          sessions={[session]}
          activeSession={session}
          busy={false}
          onSessionsChange={() => undefined}
          onActiveSessionChange={() => undefined}
        />
      </BuffersProvider>,
    );

    const viewport = latestMockTerminal().openedElement;
    if (!viewport) throw new Error('expected xterm viewport');
    await waitFor(() => {
      expect(terminalEvents.writeCalls.some((call) => call.data.includes('ready'))).toBe(true);
    });

    const file = new File([new Uint8Array([137, 80, 78, 71])], 'shot.png', { type: 'image/png' });
    const event = new Event('paste', { bubbles: true, cancelable: true });
    Object.defineProperty(event, 'clipboardData', {
      value: {
        items: [
          {
            kind: 'file',
            type: 'image/png',
            getAsFile: () => file,
          },
        ],
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
      viewport.dispatchEvent(event);
    });
    expect(event.defaultPrevented).toBe(true);
    await waitFor(() => {
      expect(terminalEvents.pasteImageCalls).toEqual([
        {
          sessionId: 's1',
          dataUrl: expect.stringMatching(/^data:image\/png;base64,/),
        },
      ]);
    });
  });
});

describe('MobileTerminalPanel — FAB menu', () => {
  beforeEach(() => {
    terminalEvents.clearCalls.length = 0;
    terminalEvents.writeCalls.length = 0;
    terminalEvents.instances.length = 0;
    terminalEvents.replayResult = {
      sessionId: 's1',
      buffer: 'ready\n',
      truncated: false,
      lastSeq: 1,
      ownerInstanceId: 'owner-1',
    };
    terminalEvents.replayPromise = null;
    global.ResizeObserver = class {
      observe(): void {}
      unobserve(): void {}
      disconnect(): void {}
    };
  });

  afterEach(() => {
    cleanup();
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   四个常驻动作叠在终端右下角会挡输出；默认应收成一个按钮。
   *
   * Code Logic（这个测试做什么）:
   *   有 session 时只暴露展开按钮，贴图/Commit/优化/收藏不在可访问树。
   */
  test('collapses terminal actions behind a single trigger by default', () => {
    const session = buildSession({ worktreeId: 'wt-1' });
    render(
      <BuffersProvider store={createWorkbenchTerminalBufferStore()}>
        <MobileTerminalPanel
          project={buildProject()}
          worktree={buildWorktree()}
          sessions={[session]}
          activeSession={session}
          busy={false}
          onSessionsChange={() => undefined}
          onActiveSessionChange={() => undefined}
        />
      </BuffersProvider>,
    );

    expect(screen.getByRole('button', { name: FAB_MENU_OPEN_LABEL })).toBeTruthy();
    expect(
      screen.queryByRole('button', { name: 'workbench:mobile.terminalPanel.pasteImageButton' }),
    ).toBeNull();
    expect(screen.queryByRole('button', { name: 'workbench:worktrees.commit' })).toBeNull();
    expect(
      screen.queryByRole('button', { name: 'workbench:promptOptimizer.open' }),
    ).toBeNull();
    expect(
      screen.queryByRole('button', {
        name: 'workbench:mobile.favoriteQuickInput.openButton',
      }),
    ).toBeNull();
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   用户点开后仍要一次触达原来的四个动作，顺序与展开前的 FAB 组一致。
   *
   * Code Logic（这个测试做什么）:
   *   点击展开后断言四按钮可访问，且贴图 → Commit → 优化 → 收藏。
   */
  test('expands paste, commit, optimizer and favorite actions in order', () => {
    const session = buildSession({ worktreeId: 'wt-1' });
    render(
      <BuffersProvider store={createWorkbenchTerminalBufferStore()}>
        <MobileTerminalPanel
          project={buildProject()}
          worktree={buildWorktree()}
          sessions={[session]}
          activeSession={session}
          busy={false}
          onSessionsChange={() => undefined}
          onActiveSessionChange={() => undefined}
        />
      </BuffersProvider>,
    );

    openTerminalFabMenu();

    const trigger = screen.getByRole('button', { name: FAB_MENU_CLOSE_LABEL });
    expect(trigger.getAttribute('aria-expanded')).toBe('true');
    const paste = screen.getByRole('button', {
      name: 'workbench:mobile.terminalPanel.pasteImageButton',
    });
    const commit = screen.getByRole('button', { name: 'workbench:worktrees.commit' });
    const optimizer = screen.getByRole('button', { name: 'workbench:promptOptimizer.open' });
    const favorite = screen.getByRole('button', {
      name: 'workbench:mobile.favoriteQuickInput.openButton',
    });
    const group = paste.parentElement;
    expect(group).toBe(commit.parentElement);
    expect(group).toBe(optimizer.parentElement);
    expect(group).toBe(favorite.parentElement);
    const labels = Array.from(group?.querySelectorAll('button') ?? []).map((button) =>
      button.getAttribute('aria-label'),
    );
    expect(labels).toEqual([
      'workbench:mobile.terminalPanel.pasteImageButton',
      'workbench:worktrees.commit',
      'workbench:promptOptimizer.open',
      'workbench:mobile.favoriteQuickInput.openButton',
    ]);
    expect(paste.textContent).toBe('workbench:mobile.terminalPanel.pasteImageButton');
    expect(commit.textContent).toBe('workbench:worktrees.commit');
    expect(optimizer.textContent).toBe('workbench:promptOptimizer.open');
    expect(favorite.textContent).toBe('workbench:mobile.favoriteQuickInput.openButton');
    expect(trigger.textContent).toBe('');
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   再次点触发按钮应收回动作，把终端输出区还回去。
   *
   * Code Logic（这个测试做什么）:
   *   展开后再点关闭，动作按钮离开可访问树。
   */
  test('collapses actions when the trigger is clicked again', () => {
    const session = buildSession({ worktreeId: 'wt-1' });
    render(
      <BuffersProvider store={createWorkbenchTerminalBufferStore()}>
        <MobileTerminalPanel
          project={buildProject()}
          worktree={buildWorktree()}
          sessions={[session]}
          activeSession={session}
          busy={false}
          onSessionsChange={() => undefined}
          onActiveSessionChange={() => undefined}
        />
      </BuffersProvider>,
    );

    openTerminalFabMenu();
    fireEvent.click(screen.getByRole('button', { name: FAB_MENU_CLOSE_LABEL }));

    expect(screen.getByRole('button', { name: FAB_MENU_OPEN_LABEL })).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'workbench:worktrees.commit' })).toBeNull();
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   点空白处应收起，避免挡住继续看终端。
   *
   * Code Logic（这个测试做什么）:
   *   展开后点击 backdrop，动作按钮消失。
   */
  test('collapses actions when the backdrop is pressed', () => {
    const session = buildSession({ worktreeId: 'wt-1' });
    render(
      <BuffersProvider store={createWorkbenchTerminalBufferStore()}>
        <MobileTerminalPanel
          project={buildProject()}
          worktree={buildWorktree()}
          sessions={[session]}
          activeSession={session}
          busy={false}
          onSessionsChange={() => undefined}
          onActiveSessionChange={() => undefined}
        />
      </BuffersProvider>,
    );

    openTerminalFabMenu();
    fireEvent.click(screen.getByTestId('mobile-terminal-fab-backdrop'));

    expect(screen.getByRole('button', { name: FAB_MENU_OPEN_LABEL })).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'workbench:worktrees.commit' })).toBeNull();
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   选完一个动作后菜单应收起，不要继续挡输出。
   *
   * Code Logic（这个测试做什么）:
   *   展开后点贴图，Commit 按钮离开可访问树。
   */
  test('collapses actions after an action is chosen', () => {
    const session = buildSession({ worktreeId: 'wt-1' });
    render(
      <BuffersProvider store={createWorkbenchTerminalBufferStore()}>
        <MobileTerminalPanel
          project={buildProject()}
          worktree={buildWorktree()}
          sessions={[session]}
          activeSession={session}
          busy={false}
          onSessionsChange={() => undefined}
          onActiveSessionChange={() => undefined}
        />
      </BuffersProvider>,
    );

    openTerminalFabMenu();
    fireEvent.click(
      screen.getByRole('button', { name: 'workbench:mobile.terminalPanel.pasteImageButton' }),
    );

    expect(screen.getByRole('button', { name: FAB_MENU_OPEN_LABEL })).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'workbench:worktrees.commit' })).toBeNull();
  });
});

describe('MobileTerminalPanel — commit FAB', () => {
  beforeEach(() => {
    terminalEvents.clearCalls.length = 0;
    terminalEvents.writeCalls.length = 0;
    terminalEvents.instances.length = 0;
    terminalEvents.replayResult = {
      sessionId: 's1',
      buffer: 'ready\n',
      truncated: false,
      lastSeq: 1,
      ownerInstanceId: 'owner-1',
    };
    terminalEvents.replayPromise = null;
    terminalEvents.commitCalls.length = 0;
    terminalEvents.commitResult = null;
    terminalEvents.repairCalls.length = 0;
    terminalEvents.repairResult = null;
    global.ResizeObserver = class {
      observe(): void {}
      unobserve(): void {}
      disconnect(): void {}
    };
  });

  afterEach(() => {
    cleanup();
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   展开后的终端操作组必须把 Commit 放在 Prompt 优化上方，方便在不离开终端时提交。
   *
   * Code Logic（这个测试做什么）:
   *   有 worktree 时断言 Commit 按钮存在，且在 FAB 组内位于优化 Prompt 之前。
   */
  test('renders commit FAB above the prompt optimizer button', () => {
    const session = buildSession({ worktreeId: 'wt-1' });
    render(
      <BuffersProvider store={createWorkbenchTerminalBufferStore()}>
        <MobileTerminalPanel
          project={buildProject()}
          worktree={buildWorktree()}
          sessions={[session]}
          activeSession={session}
          busy={false}
          onSessionsChange={() => undefined}
          onActiveSessionChange={() => undefined}
        />
      </BuffersProvider>,
    );

    openTerminalFabMenu();
    const commit = screen.getByRole('button', { name: 'workbench:worktrees.commit' });
    const optimizer = screen.getByRole('button', { name: 'workbench:promptOptimizer.open' });
    screen.getByRole('button', { name: 'workbench:mobile.favoriteQuickInput.openButton' });
    const group = commit.parentElement;
    expect(group).not.toBeNull();
    expect(group).toBe(optimizer.parentElement);
    const labels = Array.from(group?.querySelectorAll('button') ?? []).map((button) =>
      button.getAttribute('aria-label'),
    );
    expect(labels.indexOf('workbench:worktrees.commit')).toBeLessThan(
      labels.indexOf('workbench:promptOptimizer.open'),
    );
    expect(labels.indexOf('workbench:promptOptimizer.open')).toBeLessThan(
      labels.indexOf('workbench:mobile.favoriteQuickInput.openButton'),
    );
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   无 worktree 时不能发起 AI commit，避免打到空路径。
   *
   * Code Logic（这个测试做什么）:
   *   worktree=null 时 Commit 按钮 disabled。
   */
  test('disables commit FAB when no worktree is selected', () => {
    const session = buildSession();
    render(
      <BuffersProvider store={createWorkbenchTerminalBufferStore()}>
        <MobileTerminalPanel
          project={buildProject()}
          worktree={null}
          sessions={[session]}
          activeSession={session}
          busy={false}
          onSessionsChange={() => undefined}
          onActiveSessionChange={() => undefined}
        />
      </BuffersProvider>,
    );

    openTerminalFabMenu();
    expect(
      (screen.getByRole('button', { name: 'workbench:worktrees.commit' }) as HTMLButtonElement)
        .disabled,
    ).toBe(true);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   点击必须与 PC Git 历史 Commit 同口径：message=null + 稳定 clientOperationId。
   *
   * Code Logic（这个测试做什么）:
   *   点击 Commit FAB，断言 git.commit 参数。
   */
  test('clicking commit FAB posts git.commit with null message', async () => {
    const worktree = buildWorktree();
    terminalEvents.commitResult = {
      kind: 'succeeded',
      value: { ...worktree, status: { ...worktree.status, clean: true, changed: 0 } },
      clientOperationId: 'op-terminal-1',
    };
    const onWorktreeChange = vi.fn();
    const session = buildSession({ worktreeId: 'wt-1' });
    render(
      <BuffersProvider store={createWorkbenchTerminalBufferStore()}>
        <MobileTerminalPanel
          project={buildProject()}
          worktree={worktree}
          sessions={[session]}
          activeSession={session}
          busy={false}
          onSessionsChange={() => undefined}
          onActiveSessionChange={() => undefined}
          onWorktreeChange={onWorktreeChange}
        />
      </BuffersProvider>,
    );

    openTerminalFabMenu();
    fireEvent.click(screen.getByRole('button', { name: 'workbench:worktrees.commit' }));

    await waitFor(() => {
      expect(terminalEvents.commitCalls).toEqual([
        {
          worktreeId: 'wt-1',
          message: null,
          clientOperationId: 'op-terminal-1',
        },
      ]);
    });
    await waitFor(() => {
      expect(onWorktreeChange).toHaveBeenCalledTimes(1);
    });
    expect(screen.getByRole('status').textContent).toContain(
      'workbench:mobile.gitPanel.commitSucceeded',
    );
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   终端 FAB 的 failedHook 必须露出「让 AI 修复」，与桌面 Git 历史一致。
   *
   * Code Logic（这个测试做什么）:
   *   commit 返回 failedHook；点击修复按钮断言 repairHookFailure。
   */
  test('failedHook commit FAB shows repair card and starts AI repair', async () => {
    const hookFailure = {
      stage: 'preCommit' as const,
      stdout: 'lint failed',
      stderr: 'trailing whitespace',
      exitCode: 1,
    };
    terminalEvents.commitResult = {
      kind: 'failedHook',
      clientOperationId: 'op-hook',
      hookFailure,
    };
    const onFocusRepairSession = vi.fn(async () => undefined);
    const session = buildSession({ worktreeId: 'wt-1' });
    render(
      <BuffersProvider store={createWorkbenchTerminalBufferStore()}>
        <MobileTerminalPanel
          project={buildProject()}
          worktree={buildWorktree()}
          sessions={[session]}
          activeSession={session}
          busy={false}
          onSessionsChange={() => undefined}
          onActiveSessionChange={() => undefined}
          onFocusRepairSession={onFocusRepairSession}
        />
      </BuffersProvider>,
    );

    openTerminalFabMenu();
    fireEvent.click(screen.getByRole('button', { name: 'workbench:worktrees.commit' }));
    await screen.findByTestId('mobile-hook-repair-card');
    fireEvent.click(screen.getByRole('button', { name: 'workbench:worktrees.hookRepair.runButton' }));

    await waitFor(() => {
      expect(terminalEvents.repairCalls).toEqual([{ worktreeId: 'wt-1', hookFailure }]);
      expect(onFocusRepairSession).toHaveBeenCalledWith('term-repair');
    });
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   全屏时用户仍要看到 commit 失败，不能把错误藏在已隐藏的外围 chrome 里。
   *
   * Code Logic（这个测试做什么）:
   *   进入全屏后点 Commit，unknown 文案仍在 document 中。
   */
  test('keeps commit errors visible in fullscreen', async () => {
    terminalEvents.commitResult = {
      kind: 'unknown',
      clientOperationId: 'op-terminal-1',
      transportClass: 'timeout',
    };
    const session = buildSession({ worktreeId: 'wt-1' });
    render(
      <BuffersProvider store={createWorkbenchTerminalBufferStore()}>
        <MobileTerminalPanel
          project={buildProject()}
          worktree={buildWorktree()}
          sessions={[session]}
          activeSession={session}
          busy={false}
          onSessionsChange={() => undefined}
          onActiveSessionChange={() => undefined}
        />
      </BuffersProvider>,
    );

    fireEvent.click(
      screen.getByRole('button', { name: 'workbench:mobile.terminalPanel.enterFullscreen' }),
    );
    openTerminalFabMenu();
    fireEvent.click(screen.getByRole('button', { name: 'workbench:worktrees.commit' }));

    await screen.findByText('workbench:errors.mutationUnknown');
    expect(document.querySelector('[data-fullscreen="true"]')).not.toBeNull();
  });
});

describe('MobileTerminalPanel — merge FAB', () => {
  beforeEach(() => {
    terminalEvents.clearCalls.length = 0;
    terminalEvents.writeCalls.length = 0;
    terminalEvents.instances.length = 0;
    terminalEvents.replayResult = {
      sessionId: 's1',
      buffer: 'ready\n',
      truncated: false,
      lastSeq: 1,
      ownerInstanceId: 'owner-1',
    };
    terminalEvents.replayPromise = null;
    terminalEvents.resizeCalls.length = 0;
    terminalEvents.resizeError = null;
    global.ResizeObserver = class {
      observe(): void {}
      unobserve(): void {}
      disconnect(): void {}
    };
  });

  afterEach(() => {
    cleanup();
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   主工作区主分支没有可收集分支时，终端不应露出合并入口，避免误触发 collect-merge。
   *
   * Code Logic（这个测试做什么）:
   *   默认 main worktree 断言没有 merge FAB。
   */
  test('hides merge FAB on main worktree without canCollectMerge', () => {
    const session = buildSession({ worktreeId: 'wt-1' });
    render(
      <BuffersProvider store={createWorkbenchTerminalBufferStore()}>
        <MobileTerminalPanel
          project={buildProject()}
          worktree={buildWorktree()}
          sessions={[session]}
          activeSession={session}
          busy={false}
          onSessionsChange={() => undefined}
          onActiveSessionChange={() => undefined}
          onMergeWorktree={async () => true}
        />
      </BuffersProvider>,
    );

    openTerminalFabMenu();
    expect(screen.queryByRole('button', { name: 'workbench:worktrees.merge' })).toBeNull();
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   功能 worktree 需要在提交按钮正上方提供与桌面 Git 历史相同的合并入口。
   *
   * Code Logic（这个测试做什么）:
   *   非主 worktree 断言 merge FAB 存在，且在 FAB 组内位于 Commit 之前。
   */
  test('renders merge FAB above the commit button on a feature worktree', () => {
    const session = buildSession({ worktreeId: 'wt-feature' });
    const worktree = buildWorktree({
      id: 'wt-feature',
      name: 'feature/mobile',
      branch: 'feature/mobile',
      isMain: false,
    });
    render(
      <BuffersProvider store={createWorkbenchTerminalBufferStore()}>
        <MobileTerminalPanel
          project={buildProject()}
          worktree={worktree}
          sessions={[session]}
          activeSession={session}
          busy={false}
          onSessionsChange={() => undefined}
          onActiveSessionChange={() => undefined}
          onMergeWorktree={async () => true}
        />
      </BuffersProvider>,
    );

    openTerminalFabMenu();
    const merge = screen.getByRole('button', { name: 'workbench:worktrees.merge' });
    const commit = screen.getByRole('button', { name: 'workbench:worktrees.commit' });
    expect(merge.textContent).toBe('workbench:worktrees.merge');
    const group = merge.parentElement;
    expect(group).not.toBeNull();
    expect(group).toBe(commit.parentElement);
    const labels = Array.from(group?.querySelectorAll('button') ?? []).map((button) =>
      button.getAttribute('aria-label'),
    );
    expect(labels.indexOf('workbench:worktrees.merge')).toBeLessThan(
      labels.indexOf('workbench:worktrees.commit'),
    );
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   主工作区切到非主分支时也应露出合并，与「非主分支或非主工作区」的入口条件一致。
   *
   * Code Logic（这个测试做什么）:
   *   isMain 且 branch !== homeBranch 时断言 merge FAB 存在。
   */
  test('renders merge FAB on main worktree when current branch is not home', () => {
    const session = buildSession({ worktreeId: 'wt-1' });
    render(
      <BuffersProvider store={createWorkbenchTerminalBufferStore()}>
        <MobileTerminalPanel
          project={buildProject()}
          worktree={buildWorktree({
            branch: 'feature/local',
            homeBranch: 'main',
            status: {
              branch: 'feature/local',
              changed: 0,
              ahead: 0,
              behind: 0,
              conflicts: 0,
              clean: true,
              canPush: false,
            },
          })}
          sessions={[session]}
          activeSession={session}
          busy={false}
          onSessionsChange={() => undefined}
          onActiveSessionChange={() => undefined}
          onMergeWorktree={async () => true}
        />
      </BuffersProvider>,
    );

    openTerminalFabMenu();
    expect(screen.getByRole('button', { name: 'workbench:worktrees.merge' })).toBeTruthy();
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   终端合并必须复用父级与 Git 面板相同的 dirty guard / envelope merge，而不是另走一套 API。
   *
   * Code Logic（这个测试做什么）:
   *   点击 merge FAB，断言 onMergeWorktree 收到当前 worktree。
   */
  test('clicking merge FAB delegates to onMergeWorktree', async () => {
    const worktree = buildWorktree({
      id: 'wt-feature',
      name: 'feature/mobile',
      branch: 'feature/mobile',
      isMain: false,
    });
    const onMergeWorktree = vi.fn(async () => true);
    const session = buildSession({ worktreeId: 'wt-feature' });
    render(
      <BuffersProvider store={createWorkbenchTerminalBufferStore()}>
        <MobileTerminalPanel
          project={buildProject()}
          worktree={worktree}
          sessions={[session]}
          activeSession={session}
          busy={false}
          onSessionsChange={() => undefined}
          onActiveSessionChange={() => undefined}
          onMergeWorktree={onMergeWorktree}
        />
      </BuffersProvider>,
    );

    openTerminalFabMenu();
    fireEvent.click(screen.getByRole('button', { name: 'workbench:worktrees.merge' }));
    await waitFor(() => {
      expect(onMergeWorktree).toHaveBeenCalledTimes(1);
    });
    expect(onMergeWorktree).toHaveBeenCalledWith(worktree);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   用户取消确认后不得把取消当成失败投影到终端错误区。
   *
   * Code Logic（这个测试做什么）:
   *   onMergeWorktree 返回 false，断言没有 role=alert。
   */
  test('cancelled merge FAB does not project an error', async () => {
    const worktree = buildWorktree({
      id: 'wt-feature',
      name: 'feature/mobile',
      isMain: false,
    });
    const session = buildSession({ worktreeId: 'wt-feature' });
    const onMergeWorktree = vi.fn(async () => false);
    render(
      <BuffersProvider store={createWorkbenchTerminalBufferStore()}>
        <MobileTerminalPanel
          project={buildProject()}
          worktree={worktree}
          sessions={[session]}
          activeSession={session}
          busy={false}
          onSessionsChange={() => undefined}
          onActiveSessionChange={() => undefined}
          onMergeWorktree={onMergeWorktree}
        />
      </BuffersProvider>,
    );

    openTerminalFabMenu();
    fireEvent.click(screen.getByRole('button', { name: 'workbench:worktrees.merge' }));
    await waitFor(() => {
      expect(onMergeWorktree).toHaveBeenCalledTimes(1);
    });
    expect(screen.queryByRole('alert')).toBeNull();
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   merge 成功会关闭源 worktree 会话；即便随后切到 main 使 merge context 过期，终端也必须刷新权威列表。
   *
   * Code Logic（这个测试做什么）:
   *   点击 merge FAB 且 onMergeWorktree 返回 true，断言 onRefreshSessions 被调用，且没有 role=alert。
   */
  test('successful merge FAB refreshes sessions without projecting an error', async () => {
    const worktree = buildWorktree({
      id: 'wt-feature',
      name: 'feature/mobile',
      isMain: false,
    });
    const session = buildSession({ worktreeId: 'wt-feature' });
    const onRefreshSessions = vi.fn(async () => undefined);

    render(
      <BuffersProvider store={createWorkbenchTerminalBufferStore()}>
        <MobileTerminalPanel
          project={buildProject()}
          worktree={worktree}
          sessions={[session]}
          activeSession={session}
          busy={false}
          onSessionsChange={() => undefined}
          onActiveSessionChange={() => undefined}
          onMergeWorktree={async () => true}
          onRefreshSessions={onRefreshSessions}
        />
      </BuffersProvider>,
    );

    openTerminalFabMenu();
    fireEvent.click(screen.getByRole('button', { name: 'workbench:worktrees.merge' }));
    await waitFor(() => {
      expect(onRefreshSessions).toHaveBeenCalledTimes(1);
    });
    expect(screen.queryByRole('alert')).toBeNull();
  });
});

describe('MobileTerminalPanel — copy selection and paste image', () => {
  beforeEach(() => {
    terminalEvents.clearCalls.length = 0;
    terminalEvents.writeCalls.length = 0;
    terminalEvents.instances.length = 0;
    terminalEvents.replayResult = {
      sessionId: 's1',
      buffer: 'ready\n',
      truncated: false,
      lastSeq: 1,
      ownerInstanceId: 'owner-1',
    };
    terminalEvents.replayPromise = null;
    terminalEvents.hydrationResult = null;
    terminalEvents.hydrationPromise = null;
    terminalEvents.hydrateCalls.length = 0;
    terminalEvents.pasteImageCalls.length = 0;
    terminalEvents.clipboardWrites.length = 0;
    global.ResizeObserver = class {
      observe(): void {}
      unobserve(): void {}
      disconnect(): void {}
    };
    vi.useFakeTimers();
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  /**
   * Business Logic（为什么需要这个函数）:
   *   选区与贴图回归共用同一套终端面板挂载，避免每条用例重复 Provider 样板。
   *
   * Code Logic（这个函数做什么）:
   *   用默认 running session（可覆盖）渲染 MobileTerminalPanel 并返回 session。
   */
  function renderPanel(sessionOverrides: Partial<WorkbenchSession> = {}): WorkbenchSession {
    const session = buildSession(sessionOverrides);
    render(
      <BuffersProvider store={createWorkbenchTerminalBufferStore()}>
        <MobileTerminalPanel
          project={buildProject()}
          worktree={null}
          sessions={[session]}
          activeSession={session}
          busy={false}
          onSessionsChange={() => undefined}
          onActiveSessionChange={() => undefined}
        />
      </BuffersProvider>,
    );
    return session;
  }

  /**
   * Business Logic（为什么需要这个测试）:
   *   长按约 400ms 且几乎未移动必须进入自管选区，不能弹软键盘。
   *
   * Code Logic（这个测试做什么）:
   *   fake timer 推进 400ms 后断言选区底栏出现，且 enterMobileTerminalTypingMode 未被调用。
   */
  test('long-press shows selection bar and does not enter typing', () => {
    renderPanel();
    const enterTyping = vi.spyOn(extraKeys, 'enterMobileTerminalTypingMode');
    const viewport = latestMockTerminal().openedElement;
    if (!viewport) throw new Error('expected xterm viewport');

    act(() => {
      dispatchSingleTouch(viewport, 'touchstart', 40);
      vi.advanceTimersByTime(400);
    });

    expect(
      screen.getByRole('button', { name: 'workbench:mobile.terminalPanel.selection.copy' }),
    ).not.toBeNull();
    expect(
      screen.getByLabelText('workbench:mobile.terminalPanel.selection.barAriaLabel'),
    ).not.toBeNull();
    expect(enterTyping).not.toHaveBeenCalled();

    act(() => {
      dispatchSingleTouch(viewport, 'touchend', 40);
    });
    expect(enterTyping).not.toHaveBeenCalled();
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   位移超过 8px 必须走原有滚动而不是选区，避免把回看历史误当成划选。
   *
   * Code Logic（这个测试做什么）:
   *   replay 就绪后 touchmove 40px，断言无选区底栏且触发 hydrateScrollback。
   */
  test('move beyond 8px before 400ms scrolls and does not show selection bar', async () => {
    renderPanel();
    await act(async () => {
      await Promise.resolve();
    });
    const viewport = latestMockTerminal().openedElement;
    if (!viewport) throw new Error('expected xterm viewport');

    act(() => {
      dispatchSingleTouch(viewport, 'touchstart', 100);
      dispatchSingleTouch(viewport, 'touchmove', 140);
      vi.advanceTimersByTime(400);
    });

    expect(
      screen.queryByRole('button', { name: 'workbench:mobile.terminalPanel.selection.copy' }),
    ).toBeNull();
    expect(terminalEvents.hydrateCalls).toEqual(['s1']);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   底栏复制必须把 xterm getSelection 原文写入剪贴板 helper，而不是 PTY。
   *
   * Code Logic（这个测试做什么）:
   *   长按后点复制，断言 writeClipboardText 收到 mock getSelection 的 copy-me。
   */
  test('copy button writes getSelection text to clipboard helper', async () => {
    renderPanel();
    const viewport = latestMockTerminal().openedElement;
    if (!viewport) throw new Error('expected xterm viewport');

    act(() => {
      dispatchSingleTouch(viewport, 'touchstart', 40);
      vi.advanceTimersByTime(400);
    });

    await act(async () => {
      fireEvent.click(
        screen.getByRole('button', { name: 'workbench:mobile.terminalPanel.selection.copy' }),
      );
      await Promise.resolve();
    });

    expect(terminalEvents.clipboardWrites).toEqual(['copy-me']);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   选区复制只读本地 buffer，会话已停仍应能长按复制；贴图会写 PTY，必须禁用。
   *
   * Code Logic（这个测试做什么）:
   *   status=stopped 时长按出现复制按钮且可写入剪贴板，贴图 FAB disabled。
   */
  test('stopped session can long-press copy while paste-image FAB is disabled', async () => {
    renderPanel({ status: 'stopped' });
    openTerminalFabMenu();
    const pasteImage = screen.getByRole('button', {
      name: 'workbench:mobile.terminalPanel.pasteImageButton',
    }) as HTMLButtonElement;
    expect(pasteImage.disabled).toBe(true);

    const viewport = latestMockTerminal().openedElement;
    if (!viewport) throw new Error('expected xterm viewport');

    act(() => {
      dispatchSingleTouch(viewport, 'touchstart', 40);
      vi.advanceTimersByTime(400);
    });

    const copy = screen.getByRole('button', {
      name: 'workbench:mobile.terminalPanel.selection.copy',
    });
    await act(async () => {
      fireEvent.click(copy);
      await Promise.resolve();
    });
    expect(terminalEvents.clipboardWrites).toEqual(['copy-me']);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   贴图主入口是相册 file input，必须 accept=image/* 且不得带 capture，选完立刻 paste-image。
   *
   * Code Logic（这个测试做什么）:
   *   断言隐藏 input 属性，change 一个 PNG File 后 pasteImage 收到 PNG data URL。
   */
  test('paste-image FAB file input has accept=image/* without capture and posts pasteImage', async () => {
    renderPanel();
    vi.useRealTimers();
    const input = document.querySelector(
      'input[type="file"][accept="image/*"]',
    ) as HTMLInputElement | null;
    if (!input) throw new Error('expected paste-image file input');
    expect(input.hasAttribute('capture')).toBe(false);

    const file = new File([new Uint8Array([137, 80, 78, 71])], 'shot.png', { type: 'image/png' });
    Object.defineProperty(input, 'files', {
      configurable: true,
      value: [file],
    });
    await act(async () => {
      fireEvent.change(input);
    });

    await waitFor(() => {
      expect(terminalEvents.pasteImageCalls).toEqual([
        {
          sessionId: 's1',
          dataUrl: expect.stringMatching(/^data:image\/png;base64,/),
        },
      ]);
    });
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   running + 输入流 ready 时贴图 FAB 必须可点，否则相册入口形同虚设。
   *
   * Code Logic（这个测试做什么）:
   *   默认 running session 断言 pasteImageButton 按钮未 disabled。
   */
  test('running ready session keeps paste-image FAB enabled', () => {
    renderPanel();
    openTerminalFabMenu();
    const pasteImage = screen.getByRole('button', {
      name: 'workbench:mobile.terminalPanel.pasteImageButton',
    }) as HTMLButtonElement;
    expect(pasteImage.disabled).toBe(false);
  });
});

