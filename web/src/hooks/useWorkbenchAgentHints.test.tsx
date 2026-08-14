// @vitest-environment jsdom
/**
 * useWorkbenchAgentHints handshake 测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   侧栏数字必须全项目 listener-first；snapshot 只建 waiting；completed 靠 live/persist。
 *
 * Code Logic（这个测试做什么）:
 *   mock listen/getSnapshot/localStorage，覆盖 handshake、跨项目、ack persist。
 */

import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, renderHook, waitFor } from '@testing-library/react';

import type { AgentRuntimeSnapshot, AgentSessionRuntimeDto } from '@/lib/types/agentRuntime';
import {
  ACKED_COMPLETED_STORAGE_KEY,
  SEEN_COMPLETED_STORAGE_KEY,
} from '@/lib/workbenchAgentHints';

const getSnapshotMock = vi.fn();
const listSessionsMock = vi.fn();
const listenMock = vi.fn();

type EventHandler = (event: { payload: unknown }) => void;
const listenerHandlers = new Map<string, EventHandler>();

vi.mock('@/api/workbench', async () => {
  const actual = await vi.importActual<typeof import('@/api/workbench')>('@/api/workbench');
  return {
    ...actual,
    workbenchApi: {
      ...actual.workbenchApi,
      agentRuntime: {
        getSnapshot: (...args: unknown[]) => getSnapshotMock(...args),
      },
      sessions: {
        ...actual.workbenchApi.sessions,
        list: (...args: unknown[]) => listSessionsMock(...args),
      },
    },
  };
});

vi.mock('@tauri-apps/api/event', () => ({
  listen: (...args: unknown[]) => listenMock(...args),
}));

import { useWorkbenchAgentHints } from './useWorkbenchAgentHints';
import { createWorkbenchAgentHintStore } from './workbenchAgentHintStore';
import { WORKBENCH_AGENT_RUNTIME_EVENT } from '@/api/workbench';
import type { WorkbenchAgentHintStore } from './workbenchAgentHintStore';
function persistFromLocalStorage() {
  return {
    getItem: (key: string) => window.localStorage.getItem(key),
    setItem: (key: string, value: string) => {
      window.localStorage.setItem(key, value);
    },
  };
}

function session(partial: Partial<AgentSessionRuntimeDto> = {}): AgentSessionRuntimeDto {
  return {
    id: 'a',
    projectId: 'p1',
    worktreeId: 'wt-1',
    terminalSessionId: 't1',
    orchestratorTaskId: null,
    orchestratorAttempt: null,
    providerId: 'openCodeVisible',
    phase: 'working',
    version: 1,
    startedAt: '2026-08-13T00:00:00.000Z',
    lastActivityAt: '2026-08-13T00:00:01.000Z',
    endedAt: null,
    outcomeCode: null,
    resumedFromAgentSessionId: null,
    isActive: true,
    ...partial,
  };
}

function snapshot(partial: Partial<AgentRuntimeSnapshot> = {}): AgentRuntimeSnapshot {
  return {
    ownerInstanceId: 'owner-1',
    asOfSequence: 10,
    projectId: null,
    sessions: [session({ phase: 'needsInput', version: 2 })],
    truncated: false,
    ...partial,
  };
}

function agentEvent(partial: Partial<AgentSessionRuntimeDto> & { sequence?: number } = {}) {
  const { sequence, ...rest } = partial;
  return {
    agentSession: session(rest),
    ownerInstanceId: 'owner-1',
    sequence: sequence ?? 11,
  };
}

function emit(eventName: string, payload: unknown): void {
  const handler = listenerHandlers.get(eventName);
  if (!handler) throw new Error(`no listener for ${eventName}`);
  handler({ payload });
}

describe('useWorkbenchAgentHints', () => {
  let store: WorkbenchAgentHintStore;

  beforeEach(() => {
    store = createWorkbenchAgentHintStore({ persist: persistFromLocalStorage() });
    listenerHandlers.clear();
    getSnapshotMock.mockReset();
    listSessionsMock.mockReset();
    listSessionsMock.mockResolvedValue([
      { id: 't1', projectId: 'p1', worktreeId: 'wt-1' },
    ]);
    listenMock.mockReset();
    window.localStorage.clear();
    (window as unknown as { __TAURI_INTERNALS__?: { transformCallback: unknown } }).__TAURI_INTERNALS__ =
      { transformCallback: () => undefined };
    listenMock.mockImplementation(async (eventName: string, handler: EventHandler) => {
      listenerHandlers.set(eventName, handler);
      return () => {
        listenerHandlers.delete(eventName);
      };
    });
  });

  afterEach(() => {
    cleanup();
    window.localStorage.clear();
    delete (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  });

  test('handshake 拉全量 snapshot(null) 并缓冲 live completed', async () => {
    let resolveSnapshot!: (value: AgentRuntimeSnapshot) => void;
    getSnapshotMock.mockImplementationOnce(
      () =>
        new Promise<AgentRuntimeSnapshot>((resolve) => {
          resolveSnapshot = resolve;
        }),
    );

    const { result } = renderHook(() => useWorkbenchAgentHints({ store }));

    await waitFor(() => {
      expect(listenerHandlers.has(WORKBENCH_AGENT_RUNTIME_EVENT)).toBe(true);
    });

    act(() => {
      emit(
        WORKBENCH_AGENT_RUNTIME_EVENT,
        agentEvent({
          id: 'done-1',
          projectId: 'p2',
          terminalSessionId: 't-done',
          phase: 'completed',
          version: 4,
          sequence: 12,
          isActive: false,
          endedAt: '2026-08-13T00:02:00.000Z',
        }),
      );
    });

    await act(async () => {
      resolveSnapshot(snapshot());
    });

    await waitFor(() => {
      expect(result.current.phase).toBe('live');
    });
    expect(getSnapshotMock).toHaveBeenCalledWith(null);
    expect(result.current.hintsForProject('p1').tone).toBe('wait');
    expect(result.current.hintsForProject('p2').tone).toBe('complete');
    expect(result.current.hintsForTerminal('t-done').completedCount).toBe(1);
  });

  test('ack 后 persist，刷新不再复活该 completed', async () => {
    getSnapshotMock.mockResolvedValue(snapshot({ sessions: [] }));
    const { result, unmount } = renderHook(() => useWorkbenchAgentHints({ store }));

    await waitFor(() => {
      expect(result.current.phase).toBe('live');
    });

    act(() => {
      emit(
        WORKBENCH_AGENT_RUNTIME_EVENT,
        agentEvent({
          id: 'done-keep',
          terminalSessionId: 't-ack',
          phase: 'completed',
          version: 8,
          sequence: 14,
          isActive: false,
          endedAt: '2026-08-13T00:03:00.000Z',
        }),
      );
    });

    expect(result.current.hintsForTerminal('t-ack').completedCount).toBe(1);
    act(() => {
      result.current.ackCompletedForTerminal('t-ack');
    });
    expect(result.current.hintsForTerminal('t-ack').count).toBe(0);
    expect(window.localStorage.getItem(ACKED_COMPLETED_STORAGE_KEY)).toContain('done-keep');

    unmount();
    const secondStore = createWorkbenchAgentHintStore({ persist: persistFromLocalStorage() });
    const second = renderHook(() => useWorkbenchAgentHints({ store: secondStore }));
    await waitFor(() => {
      expect(second.result.current.phase).toBe('live');
    });
    act(() => {
      emit(
        WORKBENCH_AGENT_RUNTIME_EVENT,
        agentEvent({
          id: 'done-keep',
          terminalSessionId: 't-ack',
          phase: 'completed',
          version: 8,
          sequence: 20,
          isActive: false,
          endedAt: '2026-08-13T00:03:00.000Z',
        }),
      );
    });
    expect(second.result.current.hintsForTerminal('t-ack').count).toBe(0);
    second.unmount();
  });

  test('persist 的未看 completed 在 snapshot 不含它时仍恢复', async () => {
    window.localStorage.setItem(ACKED_COMPLETED_STORAGE_KEY, '[]');
    window.localStorage.setItem(
      SEEN_COMPLETED_STORAGE_KEY,
      JSON.stringify([
        {
          agentSessionId: 'seen-1',
          terminalSessionId: 't-seen',
          projectId: 'p1',
          worktreeId: 'wt-1',
          version: 3,
          endedAt: '2026-08-13T00:04:00.000Z',
        },
      ]),
    );
    getSnapshotMock.mockResolvedValue(snapshot({ sessions: [] }));
    listSessionsMock.mockResolvedValue([
      { id: 't-seen', projectId: 'p1', worktreeId: 'wt-1' },
    ]);
    const { result } = renderHook(() => useWorkbenchAgentHints({ store }));
    await waitFor(() => {
      expect(result.current.phase).toBe('live');
    });
    expect(result.current.hintsForTerminal('t-seen').completedCount).toBe(1);
  });

  test('持久化 stopped 指向已删除 terminal 时握手会清理三层计数与本地边沿', async () => {
    window.localStorage.setItem(ACKED_COMPLETED_STORAGE_KEY, '[]');
    window.localStorage.setItem(
      SEEN_COMPLETED_STORAGE_KEY,
      JSON.stringify([
        {
          agentSessionId: 'seen-orphan',
          terminalSessionId: 't-orphan',
          projectId: 'p1',
          worktreeId: 'wt-1',
          version: 4,
          endedAt: '2026-08-13T00:04:00.000Z',
        },
      ]),
    );
    getSnapshotMock.mockResolvedValue(snapshot({ sessions: [] }));
    listSessionsMock.mockResolvedValue([]);

    const { result } = renderHook(() => useWorkbenchAgentHints({ store }));
    await waitFor(() => {
      expect(result.current.phase).toBe('live');
    });

    expect(result.current.hintsForProject('p1').count).toBe(0);
    expect(result.current.hintsForWorktree('p1', 'wt-1').count).toBe(0);
    expect(result.current.hintsForTerminal('t-orphan').count).toBe(0);
    expect(window.localStorage.getItem(SEEN_COMPLETED_STORAGE_KEY)).toBe('[]');
  });
});
