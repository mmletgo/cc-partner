// @vitest-environment jsdom
/**
 * WorkbenchTerminalBuffersProvider authority-change 回归测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   远端 owner 重启后 `/api/workbench/events` 是纯 broadcast，断线窗口输出不会进入本机 Gap。
 *   已绑定 authority 切换若 needsReplay=false 且只 append 首条 live，窗口输出永久缺失。
 *
 * Code Logic（这个测试做什么）:
 *   mock Tauri listen + workbenchApi.sessions.list/replay：先建立 A 高 seq baseline，
 *   再注入 B 首条 live（无 Gap resync），断言触发 sessions.replay 且最终 buffer 含 B baseline。
 *   测试不包含终端 body 明文日志；chunk 仅为可识别标记。
 */

import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, render, waitFor } from '@testing-library/react';
import type { ReactNode } from 'react';

const listMock = vi.fn();
const replayMock = vi.fn();
const listenMock = vi.fn();

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

import {
  WorkbenchTerminalBuffersProvider,
} from './useWorkbenchTerminalBuffers';
import {
  useWorkbenchTerminalBufferStore,
} from './workbenchTerminalBuffersContext';

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

/**
 * Business Logic（为什么需要这个组件）:
 *   测试需在 Provider 内读取 store 引用。
 *
 * Code Logic（这个组件做什么）:
 *   把 store 写入 ref 容器。
 */
function StoreProbe({
  storeRef,
}: {
  storeRef: { current: ReturnType<typeof useWorkbenchTerminalBufferStore> | null };
}) {
  storeRef.current = useWorkbenchTerminalBufferStore();
  return null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   渲染 Provider 并安装 Tauri internals 以便 canListenToTauriEvents 为真。
 *
 * Code Logic（这个函数做什么）:
 *   注入 transformCallback 后 render Provider + StoreProbe。
 */
function renderProvider(storeRef: {
  current: ReturnType<typeof useWorkbenchTerminalBufferStore> | null;
}) {
  (window as Window & {
    __TAURI_INTERNALS__?: { transformCallback?: unknown };
  }).__TAURI_INTERNALS__ = {
    transformCallback: () => undefined,
  };

  const wrapper = ({ children }: { children: ReactNode }) => (
    <WorkbenchTerminalBuffersProvider>{children}</WorkbenchTerminalBuffersProvider>
  );

  return render(<StoreProbe storeRef={storeRef} />, { wrapper });
}

describe('WorkbenchTerminalBuffersProvider authority change (R9 M1)', () => {
  beforeEach(() => {
    listenerHandlers.clear();
    listMock.mockReset();
    replayMock.mockReset();
    listenMock.mockReset();

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
    delete (window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  });

  test('bound authority live switch forces replay and settles held under new baseline', async () => {
    const sessionId = 'remote:peer:s1';
    // 与后端 unit separator 合成格式一致：localremote
    const authorityA = 'localremote-A';
    const authorityB = 'localremote-B';

    // 首次 launch baseline：A 高 seq
    listMock.mockResolvedValue([{ id: sessionId }]);
    const firstReplay = deferred<{
      sessionId: string;
      buffer: string;
      truncated: boolean;
      lastSeq: number;
      ownerInstanceId: string;
    }>();
    const secondReplay = deferred<{
      sessionId: string;
      buffer: string;
      truncated: boolean;
      lastSeq: number;
      ownerInstanceId: string;
    }>();
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
