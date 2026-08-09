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
import { act, cleanup, render, waitFor } from '@testing-library/react';
import type { ReactElement, ReactNode } from 'react';

import { MobileTerminalPanel } from './MobileTerminalPanel';
import { WorkbenchTerminalBuffersContext } from '@/hooks/workbenchTerminalBuffersContext';
import type { WorkbenchTerminalBuffersContextValue } from '@/hooks/workbenchTerminalBuffersContext';
import { createWorkbenchTerminalBufferStore } from '@/hooks/workbenchTerminalBuffer';
import type { WorkbenchTerminalBufferStore } from '@/hooks/workbenchTerminalBuffer';
import type { WorkbenchProject, WorkbenchSession, WorkbenchSessionReplay } from '@/lib/types';

interface MockTerminalInstance {
  write: (data: string, cb?: () => void) => void;
  clear: () => void;
}

const terminalEvents = vi.hoisted(() => ({
  clearCalls: [] as Array<{ instance: number }>,
  writeCalls: [] as Array<{ data: string; instance: number }>,
  replayResult: null as WorkbenchSessionReplay | null,
  replayPromise: null as Promise<WorkbenchSessionReplay | null> | null,
  instances: [] as MockTerminalInstance[],
}));

vi.mock('@xterm/xterm', () => {
  let instanceCount = 0;
  return {
    Terminal: class {
      cols = 80;
      rows = 24;
      options: Record<string, unknown> = {};
      private readonly instance: number;
      constructor() {
        this.instance = instanceCount++;
      }
      loadAddon(): void {}
      open(): void {}
      onData(): { dispose: () => void } {
        return { dispose: () => undefined };
      }
      write(data: string, cb?: () => void): void {
        terminalEvents.writeCalls.push({ data, instance: this.instance });
        cb?.();
      }
      clear(): void {
        terminalEvents.clearCalls.push({ instance: this.instance });
      }
      scrollLines(): void {}
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
  httpWorkbenchTransport: {
    sessions: {
      replay: vi.fn(() =>
        terminalEvents.replayPromise ?? Promise.resolve(terminalEvents.replayResult),
      ),
      focus: vi.fn(() => Promise.resolve()),
      zoomPane: vi.fn(() => Promise.resolve()),
      resize: vi.fn(() => Promise.resolve()),
    },
  },
}));

vi.mock('react-i18next', () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));

vi.mock('../mobileTerminalInputStream', () => ({
  MobileTerminalInputStream: class {
    enqueue(): void {}
    close(): void {}
  },
}));

vi.mock('@/lib/icons', () => ({
  ArrowRightIcon: (): null => null,
  EditIcon: (): null => null,
  MaximizeIcon: (): null => null,
  MinimizeIcon: (): null => null,
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

function buildSession(): WorkbenchSession {
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

describe('MobileTerminalPanel — refresh scrollback', () => {
  beforeEach(() => {
    terminalEvents.clearCalls.length = 0;
    terminalEvents.writeCalls.length = 0;
    terminalEvents.instances.length = 0;
    terminalEvents.replayResult = null;
    terminalEvents.replayPromise = null;
    // jsdom 没有 ResizeObserver；terminal effect 依赖它做 fit。
    global.ResizeObserver = class {
      observe(): void {}
      unobserve(): void {}
      disconnect(): void {}
    };
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
});
