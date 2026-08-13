// @vitest-environment jsdom
/**
 * WorkbenchTerminalBuffersProvider authority-change 回归测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   远端 owner 重启后 `/api/workbench/events` 是纯 broadcast，断线窗口输出不会进入本机 Gap。
 *   已绑定 authority 切换若 needsReplay=false 且只 append 首条 live，窗口输出永久缺失。
 *   启动 baseline 的 sessions.list 无 projectId 仅本机；首次远端 live 若 light rebind
 *   needsReplay=false，bridge 前 ring/TUI 历史永不补回（R10 M1）。
 *   authority replay 三次失败后若不再安排恢复，安静终端永久缺口（R10 M2）。
 *   永久 not-found / validation 若仍 3-burst+5s 循环，会静默刷 IPC（R11 M1）。
 *
 * Code Logic（这个测试做什么）:
 *   mock Tauri listen + workbenchApi.sessions.list/replay：
 *   - R9：先建立 A 高 seq baseline，再注入 B 首条 live，断言强制 replay 并 settle held。
 *   - R10 M1：启动 list 仅本地，随后远端首条 live → 强制 replay 并回填历史。
 *   - R10 M2：三次 replay 失败后服务恢复，live/cooldown 路径 settle held。
 *   - R11 M1：not-found / validation 终止自动重试并暴露 history sync failure；
 *     多轮瞬时失败后仍可恢复。
 *   测试不包含终端 body 明文日志；chunk 仅为可识别标记。
 */

import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, render, waitFor } from '@testing-library/react';
import { useEffect, type ReactNode } from 'react';

const listMock = vi.fn();
const replayMock = vi.fn();
const listenMock = vi.fn();
const windowLabelState = vi.hoisted(() => ({ label: 'main' }));

type EventHandler = (event: { payload: unknown }) => void;
const listenerHandlers = new Map<string, EventHandler>();

vi.mock('@/api/workbench', async () => {
  const actual = await vi.importActual<typeof import('@/api/workbench')>(
    '@/api/workbench',
  );
  return {
    ...actual,
    workbenchApi: {
      ...actual.workbenchApi,
      sessions: {
        ...actual.workbenchApi.sessions,
        list: (...args: unknown[]) => listMock(...args),
        replay: (...args: unknown[]) => replayMock(...args),
      },
    },
  };
});

vi.mock('@tauri-apps/api/event', () => ({
  listen: (...args: unknown[]) => listenMock(...args),
}));

vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => ({ label: windowLabelState.label }),
}));

import {
  WorkbenchTerminalBuffersProvider,
} from './useWorkbenchTerminalBuffers';
import {
  useStartupBaselineFailure,
  useWorkbenchTerminalBuffers,
  useWorkbenchTerminalBufferStore,
} from './workbenchTerminalBuffersContext';
import type { TerminalHistorySyncFailure } from './workbenchTerminalBuffer';
import { normalizeError } from '@/api/client';
import { useTerminalHistorySyncFailure } from './workbenchTerminalBuffersContext';

/**
 * Business Logic（为什么需要这个函数）:
 *   异步 listen / replay 测试需要手动 resolve。
 *
 * Code Logic（这个函数做什么）:
 *   返回 promise 与 resolve/reject 控制器。
 */
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

type ReplayDto = {
  sessionId: string;
  buffer: string;
  truncated: boolean;
  lastSeq: number;
  ownerInstanceId: string;
};

/**
 * Business Logic（为什么需要这个组件）:
 *   测试需在 Provider 内读取 store 与 history sync failure。
 *
 * Code Logic（这个组件做什么）:
 *   把 store / getHistorySyncFailure 写入 ref 容器。
 */
function StoreProbe({
  storeRef,
  historySyncRef,
}: {
  storeRef: { current: ReturnType<typeof useWorkbenchTerminalBufferStore> | null };
  historySyncRef?: {
    current: ((sessionId: string) => TerminalHistorySyncFailure | null) | null;
  };
}) {
  const ctx = useWorkbenchTerminalBuffers();
  // refs 只能在 effect 中写入，禁止在 render 阶段赋值（react-hooks/refs）。
  useEffect(() => {
    storeRef.current = ctx.store;
    if (historySyncRef) {
      historySyncRef.current = ctx.getHistorySyncFailure;
    }
  });
  return null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   渲染 Provider 并安装 Tauri internals 以便 canListenToTauriEvents 为真。
 *
 * Code Logic（这个函数做什么）:
 *   注入 transformCallback 后 render Provider + StoreProbe。
 */
function renderProvider(
  storeRef: {
    current: ReturnType<typeof useWorkbenchTerminalBufferStore> | null;
  },
  historySyncRef?: {
    current: ((sessionId: string) => TerminalHistorySyncFailure | null) | null;
  },
) {
  (window as Window & {
    __TAURI_INTERNALS__?: { transformCallback?: unknown };
  }).__TAURI_INTERNALS__ = {
    transformCallback: () => undefined,
  };

  const wrapper = ({ children }: { children: ReactNode }) => (
    <WorkbenchTerminalBuffersProvider>{children}</WorkbenchTerminalBuffersProvider>
  );

  return render(
    <StoreProbe storeRef={storeRef} historySyncRef={historySyncRef} />,
    { wrapper },
  );
}

describe('WorkbenchTerminalBuffersProvider authority change (R9 M1)', () => {
  beforeEach(() => {
    listenerHandlers.clear();
    listMock.mockReset();
    replayMock.mockReset();
    listenMock.mockReset();
    vi.useRealTimers();

    listenMock.mockImplementation(
      async (eventName: string, handler: EventHandler) => {
        listenerHandlers.set(eventName, handler);
        return () => {
          listenerHandlers.delete(eventName);
        };
      },
    );
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    delete (window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  });

  test('bound authority live switch forces replay and settles held under new baseline', async () => {
    const sessionId = 'remote:peer:s1';
    // 与后端 unit separator 合成格式一致：localremote
    const authorityA = 'localremote-A';
    const authorityB = 'localremote-B';

    // 首次 launch baseline：A 高 seq
    listMock.mockResolvedValue([{ id: sessionId, status: 'running' }]);
    const firstReplay = deferred<ReplayDto>();
    const secondReplay = deferred<ReplayDto>();
    let replayCalls = 0;
    replayMock.mockImplementation(() => {
      replayCalls += 1;
      if (replayCalls === 1) return firstReplay.promise;
      return secondReplay.promise;
    });

    const storeRef: {
      current: ReturnType<typeof useWorkbenchTerminalBufferStore> | null;
    } = { current: null };

    renderProvider(storeRef);

    // 等待 listener-first 完成注册
    await waitFor(() => {
      expect(listenerHandlers.has('workbench:terminal-output')).toBe(true);
      expect(listenerHandlers.has('workbench:terminal-resync')).toBe(true);
    });

    // launch baseline A：高 lastSeq
    await act(async () => {
      firstReplay.resolve({
        sessionId,
        buffer: 'A-BASE',
        truncated: false,
        lastSeq: 50,
        ownerInstanceId: authorityA,
      });
      await firstReplay.promise;
    });

    await waitFor(() => {
      expect(storeRef.current?.getBuffer(sessionId)).toBe('A-BASE');
      expect(storeRef.current?.getLastSeq(sessionId)).toBe(50);
    });

    // A 稳态 live 推进
    await act(async () => {
      listenerHandlers.get('workbench:terminal-output')?.({
        payload: {
          sessionId,
          chunk: 'A51',
          seq: 51,
          ownerInstanceId: authorityA,
        },
      });
    });
    expect(storeRef.current?.getBuffer(sessionId)).toBe('A-BASEA51');
    expect(storeRef.current?.getLastSeq(sessionId)).toBe(51);

    // 断线窗口：B 已产生输出但本机无 Gap；首条 B live 到达 → 强制 re-baseline
    await act(async () => {
      listenerHandlers.get('workbench:terminal-output')?.({
        payload: {
          sessionId,
          chunk: 'B-LIVE-2',
          seq: 2,
          ownerInstanceId: authorityB,
        },
      });
    });

    // 首条 B live 先 append（store 会因 authority 切换重置 lastSeq），并请求 replay
    await waitFor(() => {
      expect(replayCalls).toBeGreaterThanOrEqual(2);
    });
    expect(storeRef.current?.getLastSeq(sessionId)).toBe(2);

    // B baseline 含断线窗口内容（seq<=1）；held live seq=2 应在 cutover 后 settle
    await act(async () => {
      secondReplay.resolve({
        sessionId,
        buffer: 'B0B1',
        truncated: false,
        lastSeq: 1,
        ownerInstanceId: authorityB,
      });
      await secondReplay.promise;
    });

    await waitFor(() => {
      // baseline + held live（seq>lastSeq）——断线窗口 B0/B1 必须在，不能只剩首条 live
      expect(storeRef.current?.getBuffer(sessionId)).toBe('B0B1B-LIVE-2');
      expect(storeRef.current?.getLastSeq(sessionId)).toBe(2);
    });

    // 同 authority 下重复 seq 不双写
    await act(async () => {
      listenerHandlers.get('workbench:terminal-output')?.({
        payload: {
          sessionId,
          chunk: 'DUP2',
          seq: 2,
          ownerInstanceId: authorityB,
        },
      });
    });
    expect(storeRef.current?.getBuffer(sessionId)).toBe('B0B1B-LIVE-2');
    expect(storeRef.current?.getLastSeq(sessionId)).toBe(2);

    // 后续 B live 正常追加
    await act(async () => {
      listenerHandlers.get('workbench:terminal-output')?.({
        payload: {
          sessionId,
          chunk: 'B3',
          seq: 3,
          ownerInstanceId: authorityB,
        },
      });
    });
    expect(storeRef.current?.getBuffer(sessionId)).toBe('B0B1B-LIVE-2B3');
    expect(storeRef.current?.getLastSeq(sessionId)).toBe(3);
  });
});

describe('WorkbenchTerminalBuffersProvider first remote bind (R10 M1)', () => {
  beforeEach(() => {
    listenerHandlers.clear();
    listMock.mockReset();
    replayMock.mockReset();
    listenMock.mockReset();
    vi.useRealTimers();

    listenMock.mockImplementation(
      async (eventName: string, handler: EventHandler) => {
        listenerHandlers.set(eventName, handler);
        return () => {
          listenerHandlers.delete(eventName);
        };
      },
    );
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    delete (window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  });

  test('startup baseline skips disconnected session rows that have no live runtime', async () => {
    listMock.mockResolvedValue([
      { id: 'disconnected-s1', status: 'disconnected' },
      { id: 'running-s1', status: 'running' },
    ]);
    replayMock.mockResolvedValue({
      sessionId: 'running-s1',
      buffer: 'RUNNING',
      truncated: false,
      lastSeq: 1,
      ownerInstanceId: 'owner-running',
    });

    const storeRef: {
      current: ReturnType<typeof useWorkbenchTerminalBufferStore> | null;
    } = { current: null };
    renderProvider(storeRef);

    await waitFor(() => {
      expect(replayMock).toHaveBeenCalledWith('running-s1');
    });
    expect(replayMock).not.toHaveBeenCalledWith('disconnected-s1');
  });

  test('startup list local-only then first remote live forces replay and settles history', async () => {
    const localSessionId = 'local-s1';
    const remoteSessionId = 'remote:peer:s-remote';
    const localAuthority = 'local-owner';
    // 与后端 unit separator 合成格式一致
    const remoteAuthority = 'localremote-R1';

    // 真实生产合同：启动 list 无 projectId → 仅本机会话
    listMock.mockResolvedValue([{ id: localSessionId, status: 'running' }]);

    const localBaseline = deferred<ReplayDto>();
    const remoteReplay = deferred<ReplayDto>();
    let replayCalls = 0;
    const replayArgs: string[] = [];
    replayMock.mockImplementation((sessionId: string) => {
      replayCalls += 1;
      replayArgs.push(sessionId);
      if (sessionId === localSessionId) return localBaseline.promise;
      return remoteReplay.promise;
    });

    const storeRef: {
      current: ReturnType<typeof useWorkbenchTerminalBufferStore> | null;
    } = { current: null };

    renderProvider(storeRef);

    await waitFor(() => {
      expect(listenerHandlers.has('workbench:terminal-output')).toBe(true);
    });

    // 本机 baseline 完成
    await act(async () => {
      localBaseline.resolve({
        sessionId: localSessionId,
        buffer: 'L-BASE',
        truncated: false,
        lastSeq: 3,
        ownerInstanceId: localAuthority,
      });
      await localBaseline.promise;
    });

    await waitFor(() => {
      expect(storeRef.current?.getBuffer(localSessionId)).toBe('L-BASE');
    });
    // 启动阶段不得对远端 session 发起 baseline（list 没有它）
    expect(replayArgs).toEqual([localSessionId]);
    expect(replayCalls).toBe(1);

    // 用户打开远端项目后首条 remote live 到达（无 prior baseline）
    await act(async () => {
      listenerHandlers.get('workbench:terminal-output')?.({
        payload: {
          sessionId: remoteSessionId,
          chunk: 'R-LIVE-5',
          seq: 5,
          ownerInstanceId: remoteAuthority,
        },
      });
    });

    // 必须强制 sessions.replay（禁止 light rebind needsReplay=false）
    await waitFor(() => {
      expect(replayCalls).toBeGreaterThanOrEqual(2);
      expect(replayArgs).toContain(remoteSessionId);
    });

    // remote baseline 含 bridge 前历史；held live seq=5 在 cutover 后 settle
    await act(async () => {
      remoteReplay.resolve({
        sessionId: remoteSessionId,
        buffer: 'R0R1R2R3R4',
        truncated: false,
        lastSeq: 4,
        ownerInstanceId: remoteAuthority,
      });
      await remoteReplay.promise;
    });

    await waitFor(() => {
      expect(storeRef.current?.getBuffer(remoteSessionId)).toBe('R0R1R2R3R4R-LIVE-5');
      expect(storeRef.current?.getLastSeq(remoteSessionId)).toBe(5);
    });

    // 本机 buffer 不受影响
    expect(storeRef.current?.getBuffer(localSessionId)).toBe('L-BASE');
  });
});

describe('WorkbenchTerminalBuffersProvider recoverable replay (R10 M2)', () => {
  beforeEach(() => {
    listenerHandlers.clear();
    listMock.mockReset();
    replayMock.mockReset();
    listenMock.mockReset();
    // 先用 real timers 完成 listener/baseline 注册，再切 fake timers 控制退避。
    vi.useRealTimers();

    listenMock.mockImplementation(
      async (eventName: string, handler: EventHandler) => {
        listenerHandlers.set(eventName, handler);
        return () => {
          listenerHandlers.delete(eventName);
        };
      },
    );
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    delete (window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  });

  test('three replay failures then recovery settles held via live cooldown path', async () => {
    const sessionId = 'remote:peer:s-recover';
    const authorityA = 'localremote-A';
    const authorityB = 'localremote-B';

    listMock.mockResolvedValue([{ id: sessionId, status: 'running' }]);

    let replayCalls = 0;
    const pendingReplays: Array<ReturnType<typeof deferred<ReplayDto>>> = [];
    replayMock.mockImplementation(() => {
      replayCalls += 1;
      const d = deferred<ReplayDto>();
      pendingReplays.push(d);
      return d.promise;
    });

    const storeRef: {
      current: ReturnType<typeof useWorkbenchTerminalBufferStore> | null;
    } = { current: null };

    renderProvider(storeRef);

    await waitFor(() => {
      expect(listenerHandlers.has('workbench:terminal-output')).toBe(true);
    });

    // launch baseline A 成功
    await act(async () => {
      expect(pendingReplays.length).toBeGreaterThanOrEqual(1);
      pendingReplays[0]!.resolve({
        sessionId,
        buffer: 'A-BASE',
        truncated: false,
        lastSeq: 10,
        ownerInstanceId: authorityA,
      });
      await pendingReplays[0]!.promise;
    });

    await waitFor(() => {
      expect(storeRef.current?.getBuffer(sessionId)).toBe('A-BASE');
    });
    const baselineCalls = replayCalls;

    // 之后的失败/退避路径用 fake timers 精确控制
    vi.useFakeTimers();

    // A→B 切换触发 replay
    await act(async () => {
      listenerHandlers.get('workbench:terminal-output')?.({
        payload: {
          sessionId,
          chunk: 'B-HELD-3',
          seq: 3,
          ownerInstanceId: authorityB,
        },
      });
    });
    expect(replayCalls).toBe(baselineCalls + 1);

    // 三次立即失败（attempt 1/2/3）
    for (let i = 0; i < 3; i += 1) {
      const idx = pendingReplays.length - 1;
      await act(async () => {
        pendingReplays[idx]!.reject(new Error('replay_unavailable'));
        await pendingReplays[idx]!.promise.catch(() => undefined);
      });
      if (i < 2) {
        await act(async () => {
          await vi.advanceTimersByTimeAsync(i === 0 ? 50 : 100);
        });
        expect(replayCalls).toBe(baselineCalls + 2 + i);
      }
    }

    // 立即耗尽后进入 recoverable backoff（failures=3 → 1000ms）
    const callsAfterImmediate = replayCalls;
    expect(callsAfterImmediate).toBe(baselineCalls + 3);

    // 在 cooldown 内的 live 不得再触发额外请求
    await act(async () => {
      listenerHandlers.get('workbench:terminal-output')?.({
        payload: {
          sessionId,
          chunk: 'B-HELD-4',
          seq: 4,
          ownerInstanceId: authorityB,
        },
      });
    });
    expect(replayCalls).toBe(callsAfterImmediate);

    // 推进到 recovery backoff，timer 触发第 4 次请求
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1_000);
    });
    expect(replayCalls).toBe(callsAfterImmediate + 1);

    // 服务恢复：第 4 次 replay 成功，settle held
    const successIdx = pendingReplays.length - 1;
    await act(async () => {
      pendingReplays[successIdx]!.resolve({
        sessionId,
        buffer: 'B0B1B2',
        truncated: false,
        lastSeq: 2,
        ownerInstanceId: authorityB,
      });
      await pendingReplays[successIdx]!.promise;
    });

    expect(storeRef.current?.getBuffer(sessionId)).toBe('B0B1B2B-HELD-3B-HELD-4');
    expect(storeRef.current?.getLastSeq(sessionId)).toBe(4);
  });
});

describe('WorkbenchTerminalBuffersProvider permanent replay errors (R11 M1)', () => {
  beforeEach(() => {
    listenerHandlers.clear();
    listMock.mockReset();
    replayMock.mockReset();
    listenMock.mockReset();
    vi.useRealTimers();

    listenMock.mockImplementation(
      async (eventName: string, handler: EventHandler) => {
        listenerHandlers.set(eventName, handler);
        return () => {
          listenerHandlers.delete(eventName);
        };
      },
    );
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    delete (window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  });

  test('permanent not-found stops auto-retry without infinite recovery loop', async () => {
    const sessionId = 'remote:peer:s-gone';
    const authorityA = 'localremote-A';
    const authorityB = 'localremote-B';

    listMock.mockResolvedValue([{ id: sessionId, status: 'running' }]);

    let replayCalls = 0;
    const pendingReplays: Array<ReturnType<typeof deferred<ReplayDto>>> = [];
    replayMock.mockImplementation(() => {
      replayCalls += 1;
      const d = deferred<ReplayDto>();
      pendingReplays.push(d);
      return d.promise;
    });

    const storeRef: {
      current: ReturnType<typeof useWorkbenchTerminalBufferStore> | null;
    } = { current: null };
    const historySyncRef: {
      current: ((sessionId: string) => TerminalHistorySyncFailure | null) | null;
    } = { current: null };

    renderProvider(storeRef, historySyncRef);

    await waitFor(() => {
      expect(listenerHandlers.has('workbench:terminal-output')).toBe(true);
    });

    await act(async () => {
      expect(pendingReplays.length).toBeGreaterThanOrEqual(1);
      pendingReplays[0]!.resolve({
        sessionId,
        buffer: 'A-BASE',
        truncated: false,
        lastSeq: 8,
        ownerInstanceId: authorityA,
      });
      await pendingReplays[0]!.promise;
    });

    await waitFor(() => {
      expect(storeRef.current?.getBuffer(sessionId)).toBe('A-BASE');
    });
    const baselineCalls = replayCalls;

    vi.useFakeTimers();

    await act(async () => {
      listenerHandlers.get('workbench:terminal-output')?.({
        payload: {
          sessionId,
          chunk: 'B-HELD-1',
          seq: 1,
          ownerInstanceId: authorityB,
        },
      });
    });
    expect(replayCalls).toBe(baselineCalls + 1);

    // 首次即 not-found：不得进入 3-burst 或 5s recovery
    await act(async () => {
      const idx = pendingReplays.length - 1;
      pendingReplays[idx]!.reject(normalizeError({ error: '工作台会话不存在', code: 'not_found' }));
      await pendingReplays[idx]!.promise.catch(() => undefined);
    });

    expect(historySyncRef.current?.(sessionId)).toEqual({ kind: 'not_found' });

    await act(async () => {
      await vi.advanceTimersByTimeAsync(50);
    });
    expect(replayCalls).toBe(baselineCalls + 1);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(10_000);
    });
    expect(replayCalls).toBe(baselineCalls + 1);

    // cooldown 后 live 也不得再触发
    await act(async () => {
      listenerHandlers.get('workbench:terminal-output')?.({
        payload: {
          sessionId,
          chunk: 'B-HELD-2',
          seq: 2,
          ownerInstanceId: authorityB,
        },
      });
    });
    expect(replayCalls).toBe(baselineCalls + 1);
    expect(historySyncRef.current?.(sessionId)).toEqual({ kind: 'not_found' });
  });

  test('permanent validation stops auto-retry and exposes history_sync_failed', async () => {
    const sessionId = 'remote:peer:s-decode';
    const authorityA = 'localremote-A';
    const authorityB = 'localremote-B';

    listMock.mockResolvedValue([{ id: sessionId, status: 'running' }]);

    let replayCalls = 0;
    const pendingReplays: Array<ReturnType<typeof deferred<ReplayDto>>> = [];
    replayMock.mockImplementation(() => {
      replayCalls += 1;
      const d = deferred<ReplayDto>();
      pendingReplays.push(d);
      return d.promise;
    });

    const storeRef: {
      current: ReturnType<typeof useWorkbenchTerminalBufferStore> | null;
    } = { current: null };
    const historySyncRef: {
      current: ((sessionId: string) => TerminalHistorySyncFailure | null) | null;
    } = { current: null };

    renderProvider(storeRef, historySyncRef);

    await waitFor(() => {
      expect(listenerHandlers.has('workbench:terminal-output')).toBe(true);
    });

    await act(async () => {
      expect(pendingReplays.length).toBeGreaterThanOrEqual(1);
      pendingReplays[0]!.resolve({
        sessionId,
        buffer: 'A-BASE',
        truncated: false,
        lastSeq: 5,
        ownerInstanceId: authorityA,
      });
      await pendingReplays[0]!.promise;
    });

    await waitFor(() => {
      expect(storeRef.current?.getBuffer(sessionId)).toBe('A-BASE');
    });
    const baselineCalls = replayCalls;

    vi.useFakeTimers();

    await act(async () => {
      listenerHandlers.get('workbench:terminal-output')?.({
        payload: {
          sessionId,
          chunk: 'B-HELD-1',
          seq: 1,
          ownerInstanceId: authorityB,
        },
      });
    });
    expect(replayCalls).toBe(baselineCalls + 1);

    const decodeErr = new Error(
      'Contract "WorkbenchSessionReplay" failed at $.lastSeq: got primitive',
    );
    decodeErr.name = 'ContractDecodeError';
    await act(async () => {
      const idx = pendingReplays.length - 1;
      pendingReplays[idx]!.reject(decodeErr);
      await pendingReplays[idx]!.promise.catch(() => undefined);
    });

    expect(historySyncRef.current?.(sessionId)).toEqual({
      kind: 'history_sync_failed',
    });

    await act(async () => {
      await vi.advanceTimersByTimeAsync(15_000);
    });
    expect(replayCalls).toBe(baselineCalls + 1);

    await act(async () => {
      listenerHandlers.get('workbench:terminal-output')?.({
        payload: {
          sessionId,
          chunk: 'B-HELD-9',
          seq: 9,
          ownerInstanceId: authorityB,
        },
      });
    });
    expect(replayCalls).toBe(baselineCalls + 1);
  });

  test('multi-round recoverable failures then recover still works', async () => {
    const sessionId = 'remote:peer:s-multi';
    const authorityA = 'localremote-A';
    const authorityB = 'localremote-B';

    listMock.mockResolvedValue([{ id: sessionId, status: 'running' }]);

    let replayCalls = 0;
    const pendingReplays: Array<ReturnType<typeof deferred<ReplayDto>>> = [];
    replayMock.mockImplementation(() => {
      replayCalls += 1;
      const d = deferred<ReplayDto>();
      pendingReplays.push(d);
      return d.promise;
    });

    const storeRef: {
      current: ReturnType<typeof useWorkbenchTerminalBufferStore> | null;
    } = { current: null };
    const historySyncRef: {
      current: ((sessionId: string) => TerminalHistorySyncFailure | null) | null;
    } = { current: null };

    renderProvider(storeRef, historySyncRef);

    await waitFor(() => {
      expect(listenerHandlers.has('workbench:terminal-output')).toBe(true);
    });

    await act(async () => {
      expect(pendingReplays.length).toBeGreaterThanOrEqual(1);
      pendingReplays[0]!.resolve({
        sessionId,
        buffer: 'A-BASE',
        truncated: false,
        lastSeq: 10,
        ownerInstanceId: authorityA,
      });
      await pendingReplays[0]!.promise;
    });

    await waitFor(() => {
      expect(storeRef.current?.getBuffer(sessionId)).toBe('A-BASE');
    });
    const baselineCalls = replayCalls;

    vi.useFakeTimers();

    await act(async () => {
      listenerHandlers.get('workbench:terminal-output')?.({
        payload: {
          sessionId,
          chunk: 'B-HELD-3',
          seq: 3,
          ownerInstanceId: authorityB,
        },
      });
    });
    expect(replayCalls).toBe(baselineCalls + 1);

    // 第一波：3 次 timeout 立即失败
    for (let i = 0; i < 3; i += 1) {
      const idx = pendingReplays.length - 1;
      await act(async () => {
        pendingReplays[idx]!.reject(new Error('request timeout'));
        await pendingReplays[idx]!.promise.catch(() => undefined);
      });
      if (i < 2) {
        await act(async () => {
          await vi.advanceTimersByTimeAsync(i === 0 ? 50 : 100);
        });
      }
    }
    expect(replayCalls).toBe(baselineCalls + 3);
    expect(historySyncRef.current?.(sessionId)).toBeNull();

    // 第二波：recovery timer 再失败一次
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1_000);
    });
    expect(replayCalls).toBe(baselineCalls + 4);
    await act(async () => {
      const idx = pendingReplays.length - 1;
      pendingReplays[idx]!.reject(new Error('replay_unavailable'));
      await pendingReplays[idx]!.promise.catch(() => undefined);
    });

    // 第三波：再等 backoff 后成功
    await act(async () => {
      await vi.advanceTimersByTimeAsync(2_000);
    });
    expect(replayCalls).toBe(baselineCalls + 5);

    const successIdx = pendingReplays.length - 1;
    await act(async () => {
      pendingReplays[successIdx]!.resolve({
        sessionId,
        buffer: 'B0B1B2',
        truncated: false,
        lastSeq: 2,
        ownerInstanceId: authorityB,
      });
      await pendingReplays[successIdx]!.promise;
    });

    expect(storeRef.current?.getBuffer(sessionId)).toBe('B0B1B2B-HELD-3');
    expect(storeRef.current?.getLastSeq(sessionId)).toBe(3);
    expect(historySyncRef.current?.(sessionId)).toBeNull();
  });
});


describe('WorkbenchTerminalBuffersProvider startup baseline (R12 M1/M3)', () => {
  beforeEach(() => {
    listenerHandlers.clear();
    listMock.mockReset();
    replayMock.mockReset();
    listenMock.mockReset();
    vi.useRealTimers();

    listenMock.mockImplementation(
      async (eventName: string, handler: EventHandler) => {
        listenerHandlers.set(eventName, handler);
        return () => {
          listenerHandlers.delete(eventName);
        };
      },
    );
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    delete (window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  });

  test('startup recoverable failure then success settles buffer', async () => {
    const sessionId = 'local-startup-s1';
    const authority = 'owner-local-1';
    listMock.mockResolvedValue([{ id: sessionId, status: 'running' }]);

    let replayCalls = 0;
    const pendingReplays: Array<ReturnType<typeof deferred<ReplayDto>>> = [];
    replayMock.mockImplementation(() => {
      replayCalls += 1;
      const d = deferred<ReplayDto>();
      pendingReplays.push(d);
      return d.promise;
    });

    const storeRef: {
      current: ReturnType<typeof useWorkbenchTerminalBufferStore> | null;
    } = { current: null };
    const historySyncRef: {
      current: ((sessionId: string) => TerminalHistorySyncFailure | null) | null;
    } = { current: null };

    renderProvider(storeRef, historySyncRef);

    await waitFor(() => {
      expect(listenerHandlers.has('workbench:terminal-output')).toBe(true);
    });
    await waitFor(() => {
      expect(pendingReplays.length).toBeGreaterThanOrEqual(1);
    });

    vi.useFakeTimers();

    // 首次 startup replay: timeout（可恢复）
    await act(async () => {
      pendingReplays[0]!.reject(normalizeError({ error: 'request timeout', code: 'timeout' }));
      await pendingReplays[0]!.promise.catch(() => undefined);
    });
    expect(historySyncRef.current?.(sessionId)).toBeNull();

    // 立即重试窗口
    await act(async () => {
      await vi.advanceTimersByTimeAsync(50);
    });
    expect(replayCalls).toBeGreaterThanOrEqual(2);

    const successIdx = pendingReplays.length - 1;
    await act(async () => {
      pendingReplays[successIdx]!.resolve({
        sessionId,
        buffer: 'STARTUP-OK',
        truncated: false,
        lastSeq: 2,
        ownerInstanceId: authority,
      });
      await pendingReplays[successIdx]!.promise;
    });

    expect(storeRef.current?.getBuffer(sessionId)).toBe('STARTUP-OK');
    expect(storeRef.current?.getLastSeq(sessionId)).toBe(2);
    expect(historySyncRef.current?.(sessionId)).toBeNull();
  });

  test('startup permanent validation sets observable historySyncFailure', async () => {
    const sessionId = 'local-startup-perm';
    listMock.mockResolvedValue([{ id: sessionId, status: 'running' }]);

    let replayCalls = 0;
    const pendingReplays: Array<ReturnType<typeof deferred<ReplayDto>>> = [];
    replayMock.mockImplementation(() => {
      replayCalls += 1;
      const d = deferred<ReplayDto>();
      pendingReplays.push(d);
      return d.promise;
    });

    const storeRef: {
      current: ReturnType<typeof useWorkbenchTerminalBufferStore> | null;
    } = { current: null };

    function FailureProbe() {
      const failure = useTerminalHistorySyncFailure(sessionId);
      return (
        <div data-testid="history-sync-probe">
          {failure ? failure.kind : 'none'}
        </div>
      );
    }

    (window as Window & {
      __TAURI_INTERNALS__?: { transformCallback?: unknown };
    }).__TAURI_INTERNALS__ = {
      transformCallback: () => undefined,
    };

    const { findByTestId } = render(
      <WorkbenchTerminalBuffersProvider>
        <StoreProbe storeRef={storeRef} />
        <FailureProbe />
      </WorkbenchTerminalBuffersProvider>,
    );

    await waitFor(() => {
      expect(listenerHandlers.has('workbench:terminal-output')).toBe(true);
    });
    await waitFor(() => {
      expect(pendingReplays.length).toBeGreaterThanOrEqual(1);
    });

    const probeBefore = await findByTestId('history-sync-probe');
    expect(probeBefore.textContent).toBe('none');

    await act(async () => {
      pendingReplays[0]!.reject(
        normalizeError({
          error: '远端 Workbench 网关只接受对端本机项目',
          code: 'validation',
        }),
      );
      await pendingReplays[0]!.promise.catch(() => undefined);
    });

    await waitFor(async () => {
      const probe = await findByTestId('history-sync-probe');
      expect(probe.textContent).toBe('history_sync_failed');
    });

    const callsAfterFailure = replayCalls;
    vi.useFakeTimers();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(15_000);
    });
    // 永久错误不得进入无限 3-burst
    expect(replayCalls).toBe(callsAfterFailure);
  });

  test('R13 H1 live-first replay preserves inFlight; list does not spawn concurrent cutover', async () => {
    const sessionId = 'local-live-first-s1';
    const authority = 'owner-live-first';

    // list 故意 defer：让 live-first authority replay 先起飞。
    const listDeferred = deferred<Array<{ id: string; status: string }>>();
    listMock.mockImplementation(() => listDeferred.promise);

    const pendingReplays: Array<ReturnType<typeof deferred<ReplayDto>>> = [];
    replayMock.mockImplementation(() => {
      const d = deferred<ReplayDto>();
      pendingReplays.push(d);
      return d.promise;
    });

    const storeRef: {
      current: ReturnType<typeof useWorkbenchTerminalBufferStore> | null;
    } = { current: null };
    renderProvider(storeRef);

    await waitFor(() => {
      expect(listenerHandlers.has('workbench:terminal-output')).toBe(true);
    });

    // live-first：首条 live 绑定 authority 并启动 epoch N replay。
    await act(async () => {
      listenerHandlers.get('workbench:terminal-output')?.({
        payload: {
          sessionId,
          chunk: 'LIVE-1',
          seq: 1,
          ownerInstanceId: authority,
        },
      });
    });

    await waitFor(() => {
      expect(pendingReplays.length).toBe(1);
    });

    // list 完成：beginStartupBaselineReplay 应保留 inFlight，不得再起第二个 flight。
    await act(async () => {
      listDeferred.resolve([{ id: sessionId, status: 'running' }]);
      await listDeferred.promise;
    });

    // 给 list 调度路径一个 microtask 窗口；仍应只有 1 个 in-flight replay。
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(pendingReplays.length).toBe(1);

    // 旧 epoch snapshot 与更高 seq live 交错：先注入更高 seq live，再 settle 旧请求。
    await act(async () => {
      listenerHandlers.get('workbench:terminal-output')?.({
        payload: {
          sessionId,
          chunk: 'LIVE-5',
          seq: 5,
          ownerInstanceId: authority,
        },
      });
    });

    // 旧 owner（epoch N）结束：apply 被 reject（epoch 已抬），串行启动 epoch N+1 唯一替代。
    await act(async () => {
      pendingReplays[0]!.resolve({
        sessionId,
        buffer: 'STALE-BASE',
        truncated: false,
        lastSeq: 1,
        ownerInstanceId: authority,
      });
      await pendingReplays[0]!.promise;
    });

    await waitFor(() => {
      expect(pendingReplays.length).toBe(2);
    });

    // 唯一有效 cutover：新 epoch baseline + held live seq=5。
    await act(async () => {
      pendingReplays[1]!.resolve({
        sessionId,
        buffer: 'BASE-1-4',
        truncated: false,
        lastSeq: 4,
        ownerInstanceId: authority,
      });
      await pendingReplays[1]!.promise;
    });

    await waitFor(() => {
      expect(storeRef.current?.getBuffer(sessionId)).toBe('BASE-1-4LIVE-5');
      expect(storeRef.current?.getLastSeq(sessionId)).toBe(5);
    });

    // 不得再起第三个同 session 并发 replay。
    expect(pendingReplays.length).toBe(2);
  });
});

describe('WorkbenchTerminalBuffersProvider startup list recovery (R13 M1)', () => {
  beforeEach(() => {
    listenerHandlers.clear();
    listMock.mockReset();
    replayMock.mockReset();
    listenMock.mockReset();
    vi.useRealTimers();

    listenMock.mockImplementation(
      async (eventName: string, handler: EventHandler) => {
        listenerHandlers.set(eventName, handler);
        return () => {
          listenerHandlers.delete(eventName);
        };
      },
    );
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    delete (window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  });

  test('startup list first reject then success recovers history without live', async () => {
    const sessionId = 'local-list-retry-s1';
    const authority = 'owner-list-retry';

    let listCalls = 0;
    listMock.mockImplementation(() => {
      listCalls += 1;
      if (listCalls === 1) {
        return Promise.reject(
          normalizeError({ error: 'sidecar unavailable', code: 'unavailable' }),
        );
      }
      return Promise.resolve([{ id: sessionId, status: 'running' }]);
    });

    const pendingReplays: Array<ReturnType<typeof deferred<ReplayDto>>> = [];
    replayMock.mockImplementation(() => {
      const d = deferred<ReplayDto>();
      pendingReplays.push(d);
      return d.promise;
    });

    const storeRef: {
      current: ReturnType<typeof useWorkbenchTerminalBufferStore> | null;
    } = { current: null };
    const historySyncRef: {
      current: ((sessionId: string) => TerminalHistorySyncFailure | null) | null;
    } = { current: null };

    function StartupProbe() {
      const failure = useStartupBaselineFailure();
      return (
        <div data-testid="startup-baseline-probe">
          {failure ? failure.kind : 'none'}
        </div>
      );
    }

    (window as Window & {
      __TAURI_INTERNALS__?: { transformCallback?: unknown };
    }).__TAURI_INTERNALS__ = {
      transformCallback: () => undefined,
    };

    const { findByTestId } = render(
      <WorkbenchTerminalBuffersProvider>
        <StoreProbe storeRef={storeRef} historySyncRef={historySyncRef} />
        <StartupProbe />
      </WorkbenchTerminalBuffersProvider>,
    );

    await waitFor(() => {
      expect(listenerHandlers.has('workbench:terminal-output')).toBe(true);
    });
    await waitFor(() => {
      expect(listCalls).toBeGreaterThanOrEqual(1);
    });

    // 失败期间不得误报 permanent failure（仍在有界重试窗口）。
    expect((await findByTestId('startup-baseline-probe')).textContent).toBe('none');

    vi.useFakeTimers();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(200);
    });
    vi.useRealTimers();

    await waitFor(() => {
      expect(listCalls).toBeGreaterThanOrEqual(2);
      expect(pendingReplays.length).toBeGreaterThanOrEqual(1);
    });

    // 无任何 live：仅靠 list 恢复后的 schedule replay 回填历史。
    await act(async () => {
      pendingReplays[0]!.resolve({
        sessionId,
        buffer: 'LIST-RECOVERED',
        truncated: false,
        lastSeq: 3,
        ownerInstanceId: authority,
      });
      await pendingReplays[0]!.promise;
    });

    await waitFor(() => {
      expect(storeRef.current?.getBuffer(sessionId)).toBe('LIST-RECOVERED');
      expect(storeRef.current?.getLastSeq(sessionId)).toBe(3);
    });
    expect((await findByTestId('startup-baseline-probe')).textContent).toBe('none');
    expect(historySyncRef.current?.(sessionId)).toBeNull();
  });

  test('startup list permanent failure is observable and manual retry recovers', async () => {
    const sessionId = 'local-list-perm-s1';
    const authority = 'owner-list-perm';

    // 每 attempt 一个 deferred，显式 reject 以绕过 fake/real timer 混用竞态。
    const listDeferreds: Array<ReturnType<typeof deferred<Array<{ id: string }>>>> = [];
    let listCalls = 0;
    listMock.mockImplementation(() => {
      listCalls += 1;
      const d = deferred<Array<{ id: string }>>();
      listDeferreds.push(d);
      return d.promise;
    });

    const pendingReplays: Array<ReturnType<typeof deferred<ReplayDto>>> = [];
    replayMock.mockImplementation(() => {
      const d = deferred<ReplayDto>();
      pendingReplays.push(d);
      return d.promise;
    });

    const storeRef: {
      current: ReturnType<typeof useWorkbenchTerminalBufferStore> | null;
    } = { current: null };

    function StartupProbe() {
      const failure = useStartupBaselineFailure();
      const { retryStartupBaseline } = useWorkbenchTerminalBuffers();
      return (
        <div>
          <div data-testid="startup-baseline-probe">
            {failure?.kind ?? 'none'}
          </div>
          <button
            type="button"
            data-testid="startup-baseline-retry"
            onClick={() => retryStartupBaseline()}
          >
            retry
          </button>
        </div>
      );
    }

    (window as Window & {
      __TAURI_INTERNALS__?: { transformCallback?: unknown };
    }).__TAURI_INTERNALS__ = {
      transformCallback: () => undefined,
    };

    const { findByTestId } = render(
      <WorkbenchTerminalBuffersProvider>
        <StoreProbe storeRef={storeRef} />
        <StartupProbe />
      </WorkbenchTerminalBuffersProvider>,
    );

    await waitFor(() => {
      expect(listenerHandlers.has('workbench:terminal-output')).toBe(true);
    });
    await waitFor(() => {
      expect(listDeferreds.length).toBe(1);
    });

    vi.useFakeTimers();

    // 有界 5 次 list：reject → advance 退避 → 下一 attempt；避免 waitFor+fake timer 混用。
    for (let attempt = 1; attempt <= 5; attempt += 1) {
      expect(listDeferreds.length).toBe(attempt);
      await act(async () => {
        listDeferreds[attempt - 1]!.reject(
          normalizeError({ error: 'control plane down', code: 'unavailable' }),
        );
        await listDeferreds[attempt - 1]!.promise.catch(() => undefined);
      });
      if (attempt < 5) {
        await act(async () => {
          await vi.advanceTimersByTimeAsync(5_000);
        });
        expect(listDeferreds.length).toBe(attempt + 1);
      }
    }

    // 第 5 次失败后应写入 permanent failure（仍在 fake timer 下 flush microtasks）。
    await act(async () => {
      await Promise.resolve();
    });
    expect(listCalls).toBe(5);
    expect(pendingReplays.length).toBe(0);

    vi.useRealTimers();

    await waitFor(async () => {
      const probe = await findByTestId('startup-baseline-probe');
      expect(probe.textContent).toBe('startup_list_failed');
    });

    // 手动重试：list 恢复后 schedule replay。
    const retryList = deferred<Array<{ id: string; status: string }>>();
    listMock.mockImplementation(() => {
      listCalls += 1;
      return retryList.promise;
    });
    await act(async () => {
      (await findByTestId('startup-baseline-retry')).click();
    });
    await act(async () => {
      retryList.resolve([{ id: sessionId, status: 'running' }]);
      await retryList.promise;
    });

    await waitFor(() => {
      expect(pendingReplays.length).toBeGreaterThanOrEqual(1);
    });

    await act(async () => {
      pendingReplays[0]!.resolve({
        sessionId,
        buffer: 'MANUAL-RETRY-OK',
        truncated: false,
        lastSeq: 1,
        ownerInstanceId: authority,
      });
      await pendingReplays[0]!.promise;
    });

    await waitFor(() => {
      expect(storeRef.current?.getBuffer(sessionId)).toBe('MANUAL-RETRY-OK');
    });
    await waitFor(async () => {
      const probe = await findByTestId('startup-baseline-probe');
      expect(probe.textContent).toBe('none');
    });
  });
});

describe('WorkbenchTerminalBuffersProvider satellite startup list', () => {
  const originalUrl = `${window.location.pathname}${window.location.search}`;

  beforeEach(() => {
    listenerHandlers.clear();
    listMock.mockReset();
    replayMock.mockReset();
    listenMock.mockReset();
    windowLabelState.label = 'main';
    vi.useRealTimers();

    listenMock.mockImplementation(
      async (eventName: string, handler: EventHandler) => {
        listenerHandlers.set(eventName, handler);
        return () => {
          listenerHandlers.delete(eventName);
        };
      },
    );
  });

  afterEach(() => {
    cleanup();
    windowLabelState.label = 'main';
    window.history.replaceState({}, '', originalUrl);
    delete (window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
    vi.useRealTimers();
  });

  test('satellite window lists sessions for the URL projectId', async () => {
    windowLabelState.label = 'workbench-1';
    window.history.replaceState({}, '', '/workbench?projectId=sat-project');
    listMock.mockResolvedValue([]);

    const storeRef: {
      current: ReturnType<typeof useWorkbenchTerminalBufferStore> | null;
    } = { current: null };
    renderProvider(storeRef);

    await waitFor(() => {
      expect(listMock).toHaveBeenCalledWith('sat-project');
    });
    expect(listMock).not.toHaveBeenCalledWith();
  });

  test('satellite window without projectId does not list sessions', async () => {
    windowLabelState.label = 'workbench-2';
    window.history.replaceState({}, '', '/workbench');
    listMock.mockResolvedValue([]);

    const storeRef: {
      current: ReturnType<typeof useWorkbenchTerminalBufferStore> | null;
    } = { current: null };
    renderProvider(storeRef);

    await waitFor(() => {
      expect(listenerHandlers.has('workbench:terminal-output')).toBe(true);
    });
    expect(listMock).not.toHaveBeenCalled();
  });
});
