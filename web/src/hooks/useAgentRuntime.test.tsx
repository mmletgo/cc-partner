// @vitest-environment jsdom
/**
 * useAgentRuntime handshake 单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   buffer→baseline drain、Gap 重拉、旧 version 拒绝是 A2 投影正确性核心。
 *
 * Code Logic（这个测试做什么）:
 *   mock listen/getSnapshot，覆盖 listener-first、gap refetch、selector。
 */

import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, renderHook, waitFor } from '@testing-library/react';

import type { AgentRuntimeSnapshot, AgentSessionRuntimeDto } from '@/lib/types/agentRuntime';

const getSnapshotMock = vi.fn();
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
    },
  };
});

vi.mock('@tauri-apps/api/event', () => ({
  listen: (...args: unknown[]) => listenMock(...args),
}));

import { useAgentRuntime } from './useAgentRuntime';
import {
  BACKEND_RUNTIME_GAP_EVENT,
  WORKBENCH_AGENT_RUNTIME_EVENT,
} from '@/api/workbench';

/**
 * Business Logic（为什么需要这个工厂）:
 *   测试只需改 phase/version/sequence 等字段。
 *
 * Code Logic（这个函数做什么）:
 *   合并 partial 到默认 DTO。
 */
function session(partial: Partial<AgentSessionRuntimeDto> = {}): AgentSessionRuntimeDto {
  return {
    id: 'a',
    projectId: 'p1',
    worktreeId: null,
    terminalSessionId: 't',
    orchestratorTaskId: null,
    orchestratorAttempt: null,
    providerId: 'claudeCodeVisible',
    phase: 'working',
    version: 1,
    startedAt: '2026-07-15T00:00:00.000Z',
    lastActivityAt: '2026-07-15T00:00:01.000Z',
    endedAt: null,
    outcomeCode: null,
    resumedFromAgentSessionId: null,
    isActive: true,
    ...partial,
  };
}

/**
 * Business Logic（为什么需要这个工厂）:
 *   handshake 测试需要可控 asOfSequence 与 sessions。
 *
 * Code Logic（这个函数做什么）:
 *   构造合法 snapshot。
 */
function snapshot(partial: Partial<AgentRuntimeSnapshot> = {}): AgentRuntimeSnapshot {
  return {
    ownerInstanceId: 'owner-1',
    asOfSequence: 10,
    projectId: 'p1',
    sessions: [session({ version: 1, phase: 'working' })],
    truncated: false,
    ...partial,
  };
}

/**
 * Business Logic（为什么需要这个工厂）:
 *   模拟 Tauri live 事件形状。
 *
 * Code Logic（这个函数做什么）:
 *   构造 agentSession + owner/sequence。
 */
function agentEvent(partial: Partial<AgentSessionRuntimeDto> & { sequence?: number } = {}) {
  const { sequence, ...rest } = partial;
  return {
    agentSession: session(rest),
    ownerInstanceId: 'owner-1',
    sequence: sequence ?? 11,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   测试需要向已注册 listener 投递事件。
 *
 * Code Logic（这个函数做什么）:
 *   调用对应 event name 的 handler。
 */
function emit(eventName: string, payload: unknown): void {
  const handler = listenerHandlers.get(eventName);
  if (!handler) throw new Error(`no listener for ${eventName}`);
  handler({ payload });
}

describe('useAgentRuntime', () => {
  beforeEach(() => {
    listenerHandlers.clear();
    getSnapshotMock.mockReset();
    listenMock.mockReset();

    // 让 canListenToTauriEvents 为 true
    (window as unknown as { __TAURI_INTERNALS__?: { transformCallback: unknown } }).__TAURI_INTERNALS__ =
      {
        transformCallback: () => undefined,
      };

    listenMock.mockImplementation(async (eventName: string, handler: EventHandler) => {
      listenerHandlers.set(eventName, handler);
      return () => {
        listenerHandlers.delete(eventName);
      };
    });
  });

  afterEach(() => {
    cleanup();
    delete (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  });

  test('buffers events until snapshot baseline and refetches on gap', async () => {
    let resolveSnapshot!: (value: AgentRuntimeSnapshot) => void;
    const firstSnapshot = new Promise<AgentRuntimeSnapshot>((resolve) => {
      resolveSnapshot = resolve;
    });
    getSnapshotMock.mockImplementationOnce(() => firstSnapshot);
    getSnapshotMock.mockResolvedValueOnce(
      snapshot({
        asOfSequence: 20,
        sessions: [session({ id: 'a', version: 5, phase: 'working' })],
      }),
    );

    const { result } = renderHook(() =>
      useAgentRuntime('p1', { getSnapshot: (pid) => getSnapshotMock(pid) }),
    );

    // listener 已注册后、snapshot 未到：缓冲 needsInput
    await waitFor(() => {
      expect(listenerHandlers.has(WORKBENCH_AGENT_RUNTIME_EVENT)).toBe(true);
    });

    act(() => {
      emit(
        WORKBENCH_AGENT_RUNTIME_EVENT,
        agentEvent({ id: 'a', sequence: 12, phase: 'needsInput', version: 2 }),
      );
    });

    await act(async () => {
      resolveSnapshot(snapshot({ asOfSequence: 10, sessions: [session({ version: 1 })] }));
    });

    await waitFor(() => {
      expect(result.current.phase).toBe('live');
      expect(result.current.latestAgentForTerminal('t')?.phase).toBe('needsInput');
      expect(result.current.latestAgentForTerminal('t')?.version).toBe(2);
    });

    // Gap → 第二次 snapshot
    act(() => {
      emit(BACKEND_RUNTIME_GAP_EVENT, { ownerInstanceId: 'owner-1' });
    });

    await waitFor(() => {
      expect(getSnapshotMock).toHaveBeenCalledTimes(2);
    });

    await waitFor(() => {
      expect(result.current.latestAgentForTerminal('t')?.version).toBe(5);
    });
  });

  test('discards buffered events with sequence <= asOfSequence', async () => {
    let resolveSnapshot!: (value: AgentRuntimeSnapshot) => void;
    getSnapshotMock.mockImplementationOnce(
      () =>
        new Promise<AgentRuntimeSnapshot>((resolve) => {
          resolveSnapshot = resolve;
        }),
    );

    const { result } = renderHook(() =>
      useAgentRuntime('p1', { getSnapshot: (pid) => getSnapshotMock(pid) }),
    );

    await waitFor(() => {
      expect(listenerHandlers.has(WORKBENCH_AGENT_RUNTIME_EVENT)).toBe(true);
    });

    act(() => {
      emit(
        WORKBENCH_AGENT_RUNTIME_EVENT,
        agentEvent({ sequence: 8, phase: 'failed', version: 9 }),
      );
      emit(
        WORKBENCH_AGENT_RUNTIME_EVENT,
        agentEvent({ sequence: 15, phase: 'needsInput', version: 3 }),
      );
    });

    await act(async () => {
      resolveSnapshot(
        snapshot({
          asOfSequence: 10,
          sessions: [session({ version: 2, phase: 'working' })],
        }),
      );
    });

    await waitFor(() => {
      expect(result.current.phase).toBe('live');
    });

    // seq 8 丢弃；seq 15 应用 version 3 needsInput
    expect(result.current.latestAgentForTerminal('t')?.phase).toBe('needsInput');
    expect(result.current.latestAgentForTerminal('t')?.version).toBe(3);
  });

  test('rejects older version after live apply', async () => {
    getSnapshotMock.mockResolvedValue(
      snapshot({
        asOfSequence: 10,
        sessions: [session({ id: 'a', version: 3, phase: 'working' })],
      }),
    );

    const { result } = renderHook(() =>
      useAgentRuntime('p1', { getSnapshot: (pid) => getSnapshotMock(pid) }),
    );

    await waitFor(() => {
      expect(result.current.phase).toBe('live');
    });

    act(() => {
      emit(
        WORKBENCH_AGENT_RUNTIME_EVENT,
        agentEvent({ id: 'a', sequence: 11, version: 2, phase: 'failed' }),
      );
    });

    expect(result.current.latestAgentForTerminal('t')?.version).toBe(3);
    expect(result.current.latestAgentForTerminal('t')?.phase).toBe('working');
  });
});
