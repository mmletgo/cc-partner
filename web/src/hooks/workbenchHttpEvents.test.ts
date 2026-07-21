import { afterEach, describe, expect, test, vi } from 'vitest';
import {
  advanceWorkbenchHttpStreamCursor,
  buildWorkbenchEventsUrl,
  canOpenWorkbenchEventsRequest,
  consumeWorkbenchHttpEvent,
  isWorkbenchHttpGapPayload,
  parseWorkbenchNdjsonChunk,
  parseWorkbenchNdjsonFrame,
  parseWorkbenchNdjsonFrames,
  resolveCursorAfterGap,
  resolveRemoteProjectDeviceId,
  resyncWorkbenchSessionsAfterGap,
  WORKBENCH_HTTP_EVENT_WATCHDOG_MS,
  type WorkbenchNdjsonParserState,
} from './useWorkbenchHttpEvents';
import type { WorkbenchTerminalBufferStore } from './workbenchTerminalBuffer';

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端 Workbench 事件流测试需要在断言失败时让用例失败，方便 TDD 和 CI 捕获回归。
 *
 * Code Logic（这个函数做什么）:
 *   接收布尔条件和错误消息；条件为 false 时抛出 Error，条件为 true 时帮助 TypeScript 缩窄类型。
 */
function assert(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   parser 测试需要断言 pending 和输出 chunk 的字符串值，同时不希望 TypeScript 把可变 state 缩窄成字面量。
 *
 * Code Logic（这个函数做什么）:
 *   比较 actual/expected；不相等时抛出包含上下文的 Error。
 */
function assertStringEqual(actual: string, expected: string, message: string): void {
  if (actual !== expected) throw new Error(`${message}: expected ${expected}, got ${actual}`);
}

describe('workbenchHttpEvents', () => {
  /**
   * Business Logic（为什么需要这个测试）:
   *   手机浏览器通过 NDJSON 长连接接收 Workbench terminal/status 事件，网络 chunk 可能切在任意 JSON 行中间。
   *
   * Code Logic（这个测试做什么）:
   *   构造一个 parser pending state，先喂入不完整中文输出行，再补齐 terminalOutput 和 terminalStatus 两行，
   *   断言 parser 只解析完整换行结尾事件并正确维护 pending。
   */
  test('parses NDJSON across chunk boundaries', () => {
    const state: WorkbenchNdjsonParserState = { pending: '' };
    const firstChunk = '{"type":"terminalOutput","payload":{"sessionId":"session-a","chunk":"你好';

    const firstEvents = parseWorkbenchNdjsonChunk(state, firstChunk);

    assert(firstEvents.length === 0, 'incomplete JSON line should not emit events');
    assertStringEqual(state.pending, firstChunk, 'incomplete JSON line should remain pending');

    const secondChunk =
      '，移动端","seq":7,"ts":100}}\n' +
      '{"type":"terminalStatus","payload":{"sessionId":"session-a","status":"running","exitCode":null,"ts":101}}\n';

    const secondEvents = parseWorkbenchNdjsonChunk(state, secondChunk);

    assert(secondEvents.length === 2, 'completed chunk should emit two events');
    assertStringEqual(state.pending, '', 'complete newline-delimited chunks should clear pending');
    const outputEvent = secondEvents[0];
    assert(outputEvent?.type === 'terminalOutput', 'first event should be terminalOutput');
    assertStringEqual(
      outputEvent.payload.chunk,
      '你好，移动端',
      'terminalOutput chunk should preserve split Chinese text',
    );
    assert(secondEvents[1]?.type === 'terminalStatus', 'second event should be terminalStatus');
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   typed heartbeat 必须在业务解码前识别，否则 client watchdog 永不重置或抛不支持事件。
   *
   * Code Logic（这个测试做什么）:
   *   解析 heartbeat 与 terminalOutput 混合帧，断言 frames 含 heartbeat 且业务 chunk 仍过滤掉 heartbeat。
   */
  test('parses typed heartbeat frames before business events', () => {
    const state: WorkbenchNdjsonParserState = { pending: '' };
    const chunk =
      '{"type":"heartbeat","sentAt":"2026-07-15T00:00:00Z"}\n' +
      '{"type":"terminalOutput","payload":{"sessionId":"s1","chunk":"x","seq":1,"ts":1}}\n';

    const frames = parseWorkbenchNdjsonFrames(state, chunk);
    expect(frames).toHaveLength(2);
    expect(frames[0]).toEqual({
      kind: 'heartbeat',
      sentAt: '2026-07-15T00:00:00Z',
    });
    expect(frames[1]?.kind).toBe('event');

    const state2: WorkbenchNdjsonParserState = { pending: '' };
    const eventsOnly = parseWorkbenchNdjsonChunk(state2, chunk);
    expect(eventsOnly).toHaveLength(1);
    expect(eventsOnly[0]?.type).toBe('terminalOutput');
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   旧 mobile 遇到新 event type 不得断开 stream；agentRuntime 合法 payload 必须可解析。
   *
   * Code Logic（这个测试做什么）:
   *   未知 type → ignored；合法 agentRuntime → event；畸形 agentRuntime → throw。
   */
  test('ignores unknown event types and parses agentRuntime', () => {
    const state: WorkbenchNdjsonParserState = { pending: '' };
    const unknownChunk = '{"type":"futureFeature","payload":{"x":1}}\n';
    const frames = parseWorkbenchNdjsonFrames(state, unknownChunk);
    expect(frames).toEqual([{ kind: 'ignored', type: 'futureFeature' }]);

    const agentChunk =
      '{"type":"agentRuntime","payload":{"agentSession":{"id":"a1","projectId":"p1","terminalSessionId":"t1","providerId":"claudeCodeVisible","phase":"needsInput","version":2,"startedAt":"2026-07-15T00:00:00.000Z","lastActivityAt":"2026-07-15T00:01:00.000Z","isActive":true}}}\n';
    const agentFrames = parseWorkbenchNdjsonFrames({ pending: '' }, agentChunk);
    expect(agentFrames).toHaveLength(1);
    expect(agentFrames[0]?.kind).toBe('event');
    if (agentFrames[0]?.kind === 'event') {
      expect(agentFrames[0].event.type).toBe('agentRuntime');
      if (agentFrames[0].event.type === 'agentRuntime') {
        expect(agentFrames[0].event.payload.agentSession.phase).toBe('needsInput');
      }
    }

    expect(() =>
      parseWorkbenchNdjsonFrames(
        { pending: '' },
        '{"type":"agentRuntime","payload":{"agentSession":{"id":"a"}}}\n',
      ),
    ).toThrow(/agentRuntime/);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   Gap 是 ring lag / owner 切换后的一等恢复帧；mobile 不得当 ignored 或继续 live。
   *
   * Code Logic（这个测试做什么）:
   *   解析三种协议形态（lag / ring truncate / owner switch）的 gap 帧，断言 kind=gap 与游标字段；
   *   畸形 gap 抛错；业务 chunk 过滤掉 gap。
   */
  test('parses gap frames as first-class (lag / ring truncate / owner switch)', () => {
    const lag = parseWorkbenchNdjsonFrame({
      type: 'gap',
      payload: { ownerInstanceId: 'owner-a', oldestAvailable: 12, latest: 40 },
    });
    expect(lag).toEqual({
      kind: 'gap',
      ownerInstanceId: 'owner-a',
      oldestAvailable: 12,
      latest: 40,
    });

    const ringTruncate = parseWorkbenchNdjsonFrames(
      { pending: '' },
      '{"type":"gap","payload":{"ownerInstanceId":"owner-a","oldestAvailable":3,"latest":9}}\n',
    );
    expect(ringTruncate).toEqual([
      { kind: 'gap', ownerInstanceId: 'owner-a', oldestAvailable: 3, latest: 9 },
    ]);

    const ownerSwitch = parseWorkbenchNdjsonFrame({
      type: 'gap',
      payload: { ownerInstanceId: 'owner-b', oldestAvailable: 0, latest: 1 },
    });
    expect(ownerSwitch).toEqual({
      kind: 'gap',
      ownerInstanceId: 'owner-b',
      oldestAvailable: 0,
      latest: 1,
    });

    expect(isWorkbenchHttpGapPayload({ ownerInstanceId: 'o', oldestAvailable: 1, latest: 2 })).toBe(
      true,
    );
    expect(isWorkbenchHttpGapPayload({ oldestAvailable: 1, latest: 2 })).toBe(false);

    expect(() =>
      parseWorkbenchNdjsonFrame({ type: 'gap', payload: { ownerInstanceId: 'x' } }),
    ).toThrow(/gap/);

    const mixed = parseWorkbenchNdjsonChunk(
      { pending: '' },
      '{"type":"gap","payload":{"ownerInstanceId":"o","oldestAvailable":1,"latest":2}}\n' +
        '{"type":"terminalStatus","payload":{"sessionId":"s","status":"running","exitCode":null,"ts":1}}\n',
    );
    expect(mixed).toHaveLength(1);
    expect(mixed[0]?.type).toBe('terminalStatus');
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   live 业务帧 envelope 的 owner+sequence 必须可解析，供 stream cursor 推进。
   *
   * Code Logic（这个测试做什么）:
   *   解析带 top-level ownerInstanceId/sequence 的 terminalOutput，断言 frame 附带 envelope。
   */
  test('parses envelope ownerInstanceId and sequence on event frames', () => {
    const frame = parseWorkbenchNdjsonFrame({
      type: 'terminalOutput',
      ownerInstanceId: 'owner-a',
      sequence: 7,
      payload: { sessionId: 's1', chunk: 'x', seq: 3, ts: 1 },
    });
    expect(frame.kind).toBe('event');
    if (frame.kind === 'event') {
      expect(frame.ownerInstanceId).toBe('owner-a');
      expect(frame.sequence).toBe(7);
      expect(frame.event.type).toBe('terminalOutput');
    }
  });
});

describe('workbenchHttpEvents stream cursor helpers', () => {
  /**
   * Business Logic（为什么需要这个测试）:
   *   after 游标必须在同 owner 单调推进，owner 切换时重置，禁止 brand-new None 丢 recovery。
   *
   * Code Logic（这个测试做什么）:
   *   覆盖 advance / resolveCursorAfterGap / buildWorkbenchEventsUrl 的核心合同。
   */
  test('advances cursor, resolves gap attach vs recovery, and builds after URL', () => {
    expect(advanceWorkbenchHttpStreamCursor(null, 'owner-a', 5)).toEqual({
      ownerInstanceId: 'owner-a',
      sequence: 5,
    });
    expect(
      advanceWorkbenchHttpStreamCursor(
        { ownerInstanceId: 'owner-a', sequence: 5 },
        'owner-a',
        9,
      ),
    ).toEqual({ ownerInstanceId: 'owner-a', sequence: 9 });
    expect(
      advanceWorkbenchHttpStreamCursor(
        { ownerInstanceId: 'owner-a', sequence: 9 },
        'owner-a',
        4,
      ),
    ).toEqual({ ownerInstanceId: 'owner-a', sequence: 9 });
    expect(
      advanceWorkbenchHttpStreamCursor(
        { ownerInstanceId: 'owner-a', sequence: 9 },
        'owner-b',
        1,
      ),
    ).toEqual({ ownerInstanceId: 'owner-b', sequence: 1 });
    expect(advanceWorkbenchHttpStreamCursor({ ownerInstanceId: 'owner-a', sequence: 2 }, '', 3)).toEqual(
      { ownerInstanceId: 'owner-a', sequence: 2 },
    );

    const pre = { ownerInstanceId: 'owner-a', sequence: 8 };
    const gap = { ownerInstanceId: 'owner-a', oldestAvailable: 12, latest: 40 };
    expect(resolveCursorAfterGap(pre, gap, true)).toEqual({
      ownerInstanceId: 'owner-a',
      sequence: 40,
    });
    expect(resolveCursorAfterGap(pre, gap, false)).toEqual(pre);
    // R35 M1：成功 resync 后即使 latest=0 也必须 cutover 到新 owner，禁止保留旧 pre cursor。
    expect(
      resolveCursorAfterGap(pre, { ownerInstanceId: 'owner-b', oldestAvailable: 0, latest: 0 }, true),
    ).toEqual({ ownerInstanceId: 'owner-b', sequence: 0 });
    // 旧 owner pre + 成功 gap 新 owner latest=0 → 新 owner/0。
    expect(
      resolveCursorAfterGap(
        { ownerInstanceId: 'owner-old', sequence: 12 },
        { ownerInstanceId: 'owner-new', oldestAvailable: 0, latest: 0 },
        true,
      ),
    ).toEqual({ ownerInstanceId: 'owner-new', sequence: 0 });

    // R32 M1：first-frame Gap + incomplete resync → gap owner + sequence 0，禁止 null。
    expect(resolveCursorAfterGap(null, gap, false)).toEqual({
      ownerInstanceId: 'owner-a',
      sequence: 0,
    });
    // first-frame Gap + success attaches latest（含 latest>0）。
    expect(resolveCursorAfterGap(null, gap, true)).toEqual({
      ownerInstanceId: 'owner-a',
      sequence: 40,
    });
    // first-frame Gap + success latest=0 → 直接 attach owner/0 并应清 recovery。
    expect(
      resolveCursorAfterGap(null, { ownerInstanceId: 'owner-z', oldestAvailable: 0, latest: 0 }, true),
    ).toEqual({ ownerInstanceId: 'owner-z', sequence: 0 });

    expect(buildWorkbenchEventsUrl(null)).toBe('/api/workbench/events');
    expect(buildWorkbenchEventsUrl({ ownerInstanceId: 'owner-a', sequence: 9 })).toBe(
      '/api/workbench/events?afterOwnerInstanceId=owner-a&afterSequence=9',
    );
    // sequence 0 recovery cursor 必须生成 after query（非 bare）。
    expect(buildWorkbenchEventsUrl({ ownerInstanceId: 'owner-a', sequence: 0 })).toBe(
      '/api/workbench/events?afterOwnerInstanceId=owner-a&afterSequence=0',
    );

    // recoveryPending 门闩：无 cursor 时禁止 open；有 recovery cursor（含 seq 0）允许。
    expect(canOpenWorkbenchEventsRequest(null, false)).toBe(true);
    expect(canOpenWorkbenchEventsRequest(null, true)).toBe(false);
    expect(
      canOpenWorkbenchEventsRequest({ ownerInstanceId: 'owner-a', sequence: 0 }, true),
    ).toBe(true);
  });

  /**
   * Business Logic（R32 M1: 为什么需要这个测试）:
   *   首帧 Gap 后 resync 失败不得以 bare events URL 重连，否则变成 brand-new consumer 掩盖 gap。
   *
   * Code Logic（这个测试做什么）:
   *   pre-gap cursor=null → incomplete resolve → recovery={owner,0} → URL 含 afterSequence=0；
   *   recoveryPending 时 canOpen 拒绝 null。
   */
  test('first-frame gap recovery never reconnects bare without after cursor', () => {
    let streamCursor: { ownerInstanceId: string; sequence: number } | null = null;
    let recoveryPending = false;
    const gap = {
      ownerInstanceId: 'owner-first',
      oldestAvailable: 5,
      latest: 20,
    };

    // 模拟 first-frame Gap + resync 失败。
    streamCursor = resolveCursorAfterGap(streamCursor, gap, false);
    recoveryPending = true;

    expect(streamCursor).toEqual({ ownerInstanceId: 'owner-first', sequence: 0 });
    expect(canOpenWorkbenchEventsRequest(streamCursor, recoveryPending)).toBe(true);
    const url = buildWorkbenchEventsUrl(streamCursor, {
      recoveryPending,
      recoveryFallback: streamCursor,
    });
    expect(url).toContain('afterOwnerInstanceId=owner-first');
    expect(url).toContain('afterSequence=0');
    expect(url).not.toBe('/api/workbench/events');

    // 若错误地清空 cursor，门闩必须拒绝 bare open。
    expect(canOpenWorkbenchEventsRequest(null, true)).toBe(false);
  });

  /**
   * Business Logic（R35 M1: 为什么需要这个测试）:
   *   新 owner latest=0 成功 resync 后必须 cutover 并清 recoveryPending，否则会无限 Gap。
   *
   * Code Logic（这个测试做什么）:
   *   复刻 handleGap recovery 决策：success + attach gap owner（含 seq 0）→ recoveryPending=false。
   */
  test('successful latest=0 owner cutover clears recoveryPending', () => {
    let streamCursor: { ownerInstanceId: string; sequence: number } | null = {
      ownerInstanceId: 'owner-old',
      sequence: 8,
    };
    let recoveryPending = true;
    const gap = { ownerInstanceId: 'owner-new', oldestAvailable: 0, latest: 0 };
    const resyncSucceeded = true;

    streamCursor = resolveCursorAfterGap(streamCursor, gap, resyncSucceeded);
    if (
      resyncSucceeded &&
      streamCursor &&
      streamCursor.ownerInstanceId === gap.ownerInstanceId
    ) {
      recoveryPending = false;
    } else {
      recoveryPending = true;
    }

    expect(streamCursor).toEqual({ ownerInstanceId: 'owner-new', sequence: 0 });
    expect(recoveryPending).toBe(false);
    expect(canOpenWorkbenchEventsRequest(streamCursor, recoveryPending)).toBe(true);
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   Gap resync 只对 running session replay，并用 replay 的 lastSeq/owner 权威 reset buffer。
   *
   * Code Logic（这个测试做什么）:
   *   注入 list/replay 与 store.reset spy；断言只 reset running、带 lastSeq/owner；失败上抛。
   *   不断言任何 terminal I/O body 内容。
   */
  test('resyncs only running sessions and resets store with replay authority', async () => {
    const reset = vi.fn();
    const store = { reset } as unknown as WorkbenchTerminalBufferStore;
    const sessions = {
      list: vi.fn(async () => [
        { id: 'run-1', status: 'running' },
        { id: 'exit-1', status: 'exited' },
        { id: 'run-2', status: 'running' },
      ]),
      replay: vi.fn(async (sessionId: string) => ({
        sessionId,
        buffer: '',
        truncated: false,
        lastSeq: sessionId === 'run-1' ? 11 : 22,
        ownerInstanceId: 'owner-x',
      })),
    };

    await resyncWorkbenchSessionsAfterGap(store, sessions);

    expect(sessions.list).toHaveBeenCalledTimes(1);
    expect(sessions.replay).toHaveBeenCalledTimes(2);
    expect(sessions.replay).toHaveBeenCalledWith('run-1');
    expect(sessions.replay).toHaveBeenCalledWith('run-2');
    expect(reset).toHaveBeenCalledTimes(2);
    expect(reset).toHaveBeenNthCalledWith(1, 'run-1', '', 11, 'owner-x');
    expect(reset).toHaveBeenNthCalledWith(2, 'run-2', '', 22, 'owner-x');

    const failing = {
      list: vi.fn(async () => [{ id: 'run-1', status: 'running' }]),
      replay: vi.fn(async () => {
        throw new Error('network');
      }),
    };
    await expect(resyncWorkbenchSessionsAfterGap(store, failing)).rejects.toThrow(/network/);
  });

  /**
   * Business Logic（R38 M1: 为什么需要这个测试）:
   *   Mobile Gap inventory 必须覆盖 remote shortcut running sessions，
   *   但默认 offline remote 不得 fail-closed 挡本机恢复；仅 active bridge 子集 fail-closed。
   *
   * Code Logic（这个测试做什么）:
   *   1) 成功 inventory：local+remote union 去重后只 replay running。
   *   2) 默认（无 listActiveBridgeDevices）：remote list reject 不 throw，本机 running 仍 resync。
   *   3) 注入 listActiveBridgeDevices 且含该 device：remote list 失败 throw。
   *   4) resolveRemoteProjectDeviceId 优先 deviceId，否则解析 remote:device:...。
   */
  test('resync inventories remote sessions; offline remote skips unless active bridge', async () => {
    const reset = vi.fn();
    const store = { reset } as unknown as WorkbenchTerminalBufferStore;
    const sessions = {
      list: vi.fn(async (projectId?: string | null) => {
        if (!projectId) {
          return [{ id: 'local-run', status: 'running' }];
        }
        if (projectId === 'remote:dev:shortcut') {
          return [
            { id: 'remote:dev:s1', status: 'running' },
            { id: 'remote:dev:s2', status: 'exited' },
          ];
        }
        return [];
      }),
      listProjects: vi.fn(async () => [
        { id: 'local-p', kind: 'local' },
        { id: 'remote:dev:shortcut', kind: 'remote' },
      ]),
      replay: vi.fn(async (sessionId: string) => ({
        sessionId,
        buffer: 'buf',
        truncated: false,
        lastSeq: sessionId === 'local-run' ? 1 : 9,
        ownerInstanceId: 'own',
      })),
    };

    await resyncWorkbenchSessionsAfterGap(store, sessions);

    expect(sessions.listProjects).toHaveBeenCalledTimes(1);
    expect(sessions.list).toHaveBeenCalledWith();
    expect(sessions.list).toHaveBeenCalledWith('remote:dev:shortcut');
    expect(sessions.replay).toHaveBeenCalledWith('local-run');
    expect(sessions.replay).toHaveBeenCalledWith('remote:dev:s1');
    expect(sessions.replay).not.toHaveBeenCalledWith('remote:dev:s2');
    expect(reset).toHaveBeenCalledWith('local-run', 'buf', 1, 'own');
    expect(reset).toHaveBeenCalledWith('remote:dev:s1', 'buf', 9, 'own');

    // R38 M1 默认 skip-offline：remote reject 不得 throw，本机 running 仍完成 resync。
    const localReset = vi.fn();
    const localStore = { reset: localReset } as unknown as WorkbenchTerminalBufferStore;
    const offlineRemote = {
      list: vi.fn(async (projectId?: string | null) => {
        if (!projectId) return [{ id: 'local-run', status: 'running' }];
        throw new Error('remote offline');
      }),
      listProjects: vi.fn(async () => [{ id: 'remote:dev:shortcut', kind: 'remote' }]),
      replay: vi.fn(async (sessionId: string) => ({
        sessionId,
        buffer: 'local-buf',
        truncated: false,
        lastSeq: 3,
        ownerInstanceId: 'local-own',
      })),
    };
    await expect(resyncWorkbenchSessionsAfterGap(localStore, offlineRemote)).resolves.toBeUndefined();
    expect(offlineRemote.list).toHaveBeenCalledWith();
    expect(offlineRemote.list).toHaveBeenCalledWith('remote:dev:shortcut');
    expect(offlineRemote.replay).toHaveBeenCalledWith('local-run');
    expect(localReset).toHaveBeenCalledWith('local-run', 'local-buf', 3, 'local-own');

    // 注入非空 active bridge → 该 device 上 remote list 失败 fail-closed。
    const activeBridgeFail = {
      list: vi.fn(async (projectId?: string | null) => {
        if (!projectId) return [{ id: 'local-run', status: 'running' }];
        throw new Error('remote offline');
      }),
      listProjects: vi.fn(async () => [
        { id: 'remote:dev:shortcut', kind: 'remote', deviceId: 'dev' },
        { id: 'remote:other:shortcut', kind: 'remote' },
      ]),
      listActiveBridgeDevices: vi.fn(async () => ['dev']),
      replay: vi.fn(async () => ({
        sessionId: 'x',
        buffer: '',
        truncated: false,
        lastSeq: 0,
      })),
    };
    await expect(resyncWorkbenchSessionsAfterGap(store, activeBridgeFail)).rejects.toThrow(
      /remote offline/,
    );
    expect(activeBridgeFail.listActiveBridgeDevices).toHaveBeenCalledTimes(1);
    expect(activeBridgeFail.list).toHaveBeenCalledWith('remote:dev:shortcut');
    expect(activeBridgeFail.list).not.toHaveBeenCalledWith('remote:other:shortcut');

    // resolveRemoteProjectDeviceId：deviceId 优先，否则 parse remote:device:...
    expect(resolveRemoteProjectDeviceId({ id: 'remote:dev:p1', deviceId: 'explicit' })).toBe(
      'explicit',
    );
    expect(resolveRemoteProjectDeviceId({ id: 'remote:dev:p1' })).toBe('dev');
    expect(resolveRemoteProjectDeviceId({ id: 'local-p' })).toBeNull();
  });
});

describe('workbenchHttpEvents terminalResync', () => {
  /**
   * Business Logic（为什么需要这个测试）:
   *   R37 H2：bridge Gap resync 发布 terminalResync 后 Mobile 必须 parse 并 reset buffer。
   *
   * Code Logic（这个测试做什么）:
   *   parse terminalResync frame；畸形 payload 抛错；未知 type 仍 ignored。
   */
  test('parses terminalResync frames for store reset', () => {
    const frame = parseWorkbenchNdjsonFrame({
      type: 'terminalResync',
      ownerInstanceId: 'bus-owner',
      sequence: 15,
      payload: {
        sessionId: 'remote:dev:s1',
        buffer: 'history',
        truncated: true,
        lastSeq: 42,
        ownerInstanceId: 'comp-owner',
      },
    });
    expect(frame.kind).toBe('event');
    if (frame.kind === 'event') {
      expect(frame.ownerInstanceId).toBe('bus-owner');
      expect(frame.sequence).toBe(15);
      expect(frame.event.type).toBe('terminalResync');
      if (frame.event.type === 'terminalResync') {
        expect(frame.event.payload).toEqual({
          sessionId: 'remote:dev:s1',
          buffer: 'history',
          truncated: true,
          lastSeq: 42,
          ownerInstanceId: 'comp-owner',
        });
      }
    }

    expect(() =>
      parseWorkbenchNdjsonFrame({
        type: 'terminalResync',
        payload: { sessionId: 's', buffer: 'b' },
      }),
    ).toThrow(/terminalResync payload 非法/);

    const reset = vi.fn();
    const store = { reset } as unknown as WorkbenchTerminalBufferStore;
    consumeWorkbenchHttpEvent(store, {
      type: 'terminalResync',
      payload: {
        sessionId: 'remote:dev:s1',
        buffer: 'history',
        truncated: true,
        lastSeq: 42,
        ownerInstanceId: 'comp-owner',
      },
    });
    expect(reset).toHaveBeenCalledWith('remote:dev:s1', 'history', 42, 'comp-owner');
  });
});

describe('workbenchHttpEvents gap handling contract', () => {
  /**
   * Business Logic（为什么需要这个测试）:
   *   Gap 后不得继续应用本连接 live 尾部；resync 成功 attach latest，失败保留 pre-gap recovery。
   *
   * Code Logic（这个测试做什么）:
   *   纯函数复刻 hook 内 gap 合同：pause 后丢弃后续 event；成功/失败两种 cursor 决策。
   *   不包含 terminal I/O body 断言。
   */
  test('pauses live after gap and never brand-new None after incomplete resync', () => {
    let livePaused = false;
    let streamCursor: { ownerInstanceId: string; sequence: number } | null = {
      ownerInstanceId: 'owner-a',
      sequence: 8,
    };
    const appliedEvents: string[] = [];

    const frames = parseWorkbenchNdjsonFrames(
      { pending: '' },
      [
        '{"type":"terminalStatus","payload":{"sessionId":"s","status":"running","exitCode":null,"ts":1},"ownerInstanceId":"owner-a","sequence":8}\n',
        '{"type":"gap","payload":{"ownerInstanceId":"owner-a","oldestAvailable":12,"latest":40}}\n',
        '{"type":"terminalStatus","payload":{"sessionId":"s","status":"running","exitCode":null,"ts":2},"ownerInstanceId":"owner-a","sequence":41}\n',
      ].join(''),
    );

    for (const frame of frames) {
      if (livePaused) break;
      if (frame.kind === 'gap') {
        livePaused = true;
        const pre = streamCursor;
        // 模拟 incomplete resync
        streamCursor = resolveCursorAfterGap(pre, frame, false);
        break;
      }
      if (frame.kind === 'event') {
        streamCursor = advanceWorkbenchHttpStreamCursor(
          streamCursor,
          frame.ownerInstanceId,
          frame.sequence,
        );
        appliedEvents.push(frame.event.type);
      }
    }

    expect(livePaused).toBe(true);
    expect(appliedEvents).toEqual(['terminalStatus']);
    expect(streamCursor).toEqual({ ownerInstanceId: 'owner-a', sequence: 8 });
    expect(buildWorkbenchEventsUrl(streamCursor)).toContain('afterSequence=8');

    // 成功 resync 才 attach latest
    const afterSuccess = resolveCursorAfterGap(
      { ownerInstanceId: 'owner-a', sequence: 8 },
      { ownerInstanceId: 'owner-a', oldestAvailable: 12, latest: 40 },
      true,
    );
    expect(afterSuccess).toEqual({ ownerInstanceId: 'owner-a', sequence: 40 });
    expect(buildWorkbenchEventsUrl(afterSuccess)).toBe(
      '/api/workbench/events?afterOwnerInstanceId=owner-a&afterSequence=40',
    );
  });
});

describe('workbenchHttpEvents watchdog contract', () => {
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  /**
   * Business Logic（为什么需要这个测试）:
   *   半开连接 35s 无帧必须只 abort 当前连接并在 lifecycle 仍活跃时重连；
   *   heartbeat 与业务帧都要重置 watchdog。
   *
   * Code Logic（这个测试做什么）:
   *   用可控 ReadableStream + fake timers 模拟 hook 内 connect 语义：
   *   先发 heartbeat，推进 34s 不 abort；再静默 35s 触发 child abort 并二次 fetch。
   */
  test('watchdog aborts child after silence and reconnects while lifecycle active', async () => {
    vi.useFakeTimers();

    type StreamController = ReadableStreamDefaultController<Uint8Array>;
    const controllers: StreamController[] = [];
    let fetchCount = 0;

    const mockFetch = vi.fn((_input: RequestInfo | URL, init?: RequestInit) => {
      fetchCount += 1;
      const stream = new ReadableStream<Uint8Array>({
        start(controller) {
          controllers.push(controller);
          init?.signal?.addEventListener('abort', () => {
            try {
              controller.error(new DOMException('The operation was aborted.', 'AbortError'));
            } catch {
              // already closed
            }
          });
        },
      });
      return Promise.resolve({
        ok: true,
        body: stream,
      } as Response);
    });
    vi.stubGlobal('fetch', mockFetch);

    // 内联复刻 hook 的 lifecycle/child/watchdog 合同，避免把 React hook 绑进 node 环境。
    const lifecycle = new AbortController();
    const reconnectDelayMs = 10;
    const watchdogMs = WORKBENCH_HTTP_EVENT_WATCHDOG_MS;
    let stopped = false;
    let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
    const encoder = new TextEncoder();

    /**
     * Business Logic（为什么需要这个函数）:
     *   测试需要与生产 hook 相同的重连调度语义。
     *
     * Code Logic（这个函数做什么）:
     *   lifecycle 未结束时延迟调用 connect。
     */
    function scheduleReconnect(): void {
      if (stopped || lifecycle.signal.aborted) return;
      reconnectTimer = setTimeout(() => {
        void connect();
      }, reconnectDelayMs);
    }

    /**
     * Business Logic（为什么需要这个函数）:
     *   测试连接体必须与 production：child abort + watchdog 重置一致。
     *
     * Code Logic（这个函数做什么）:
     *   fetch events，读 frames，heartbeat/业务都 resetWatchdog；结束 scheduleReconnect。
     */
    async function connect(): Promise<void> {
      if (stopped || lifecycle.signal.aborted) return;
      const connection = new AbortController();
      const onLife = (): void => connection.abort();
      lifecycle.signal.addEventListener('abort', onLife);
      let watchdog: ReturnType<typeof setTimeout> | null = null;
      /**
       * Business Logic（为什么需要这个函数）:
       *   任何完整 frame 都证明连接存活。
       *
       * Code Logic（这个函数做什么）:
       *   重置 watchdog；触发时仅 abort child。
       */
      const resetWatchdog = (): void => {
        if (watchdog) clearTimeout(watchdog);
        watchdog = setTimeout(() => connection.abort(), watchdogMs);
      };
      resetWatchdog();
      try {
        const response = await fetch('/api/workbench/events', {
          method: 'GET',
          signal: connection.signal,
        });
        if (!response.body) throw new Error('no body');
        const reader = response.body.getReader();
        const decoder = new TextDecoder();
        const state: WorkbenchNdjsonParserState = { pending: '' };
        while (!stopped && !lifecycle.signal.aborted) {
          const { done, value } = await reader.read();
          if (done) break;
          if (!value) continue;
          const chunk = decoder.decode(value, { stream: true });
          parseWorkbenchNdjsonFrames(state, chunk).forEach(() => {
            resetWatchdog();
          });
        }
      } catch {
        if (lifecycle.signal.aborted) return;
      } finally {
        if (watchdog) clearTimeout(watchdog);
        lifecycle.signal.removeEventListener('abort', onLife);
        if (!lifecycle.signal.aborted && !stopped) scheduleReconnect();
      }
    }

    void connect();
    await vi.advanceTimersByTimeAsync(0);
    expect(fetchCount).toBe(1);

    // heartbeat 重置 watchdog
    controllers[0]?.enqueue(
      encoder.encode('{"type":"heartbeat","sentAt":"2026-07-15T00:00:00Z"}\n'),
    );
    await vi.advanceTimersByTimeAsync(watchdogMs - 1_000);
    expect(fetchCount).toBe(1);

    // 业务帧也重置
    controllers[0]?.enqueue(
      encoder.encode(
        '{"type":"terminalOutput","payload":{"sessionId":"s","chunk":"a","seq":1,"ts":1}}\n',
      ),
    );
    await vi.advanceTimersByTimeAsync(watchdogMs - 1_000);
    expect(fetchCount).toBe(1);

    // 静默满 watchdog → child abort → reconnect
    await vi.advanceTimersByTimeAsync(watchdogMs + reconnectDelayMs + 5);
    expect(fetchCount).toBe(2);

    stopped = true;
    lifecycle.abort();
    if (reconnectTimer) clearTimeout(reconnectTimer);
  });
});
