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
      replay: vi.fn(() => Promise.resolve(terminalEvents.replayResult)),
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
          onNavigateToPromptOptimizer={() => undefined}
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
});
