import { describe, expect, test } from 'vitest';
import { planTerminalBufferWrite } from '@/pages/Workbench/terminalReplay';
import { normalizeError } from '@/api/client';
import {
  appendWorkbenchTerminalOutput,
  applyTerminalBaselineCutover,
  beginAuthorityChangeReplay,
  beginStartupBaselineReplay,
  beginHeldOverflowReplay,
  commitTerminalCutover,
  createEmptySessionCutoverState,
  createWorkbenchTerminalBufferStore,
  isTerminalAuthorityChange,
  MAX_HELD_LIVE_TERMINAL_EVENTS,
  MAX_WORKBENCH_TERMINAL_BUFFER_CHARS,
  removeWorkbenchTerminalBuffer,
  resetWorkbenchTerminalBuffer,
  setTerminalCutoverReplayInFlight,
  shouldAcceptTerminalCutover,
  shouldClearTerminalNeedsReplay,
  shouldCollectHeldLiveTerminalEvent,
  shouldTriggerTerminalReplayRecovery,
  stopTerminalCutoverReplay,
  terminalHistorySyncFailureFromClass,
  terminalReplayRecoveryDelayMs,
  classifyTerminalReplayError,
  TERMINAL_REPLAY_IMMEDIATE_ATTEMPTS,
  TERMINAL_REPLAY_RECOVERY_BACKOFF_CAP_MS,
  type HeldLiveTerminalEvent,
  type TerminalBufferDelta,
  type TerminalFrameScheduler,
} from './workbenchTerminalBuffer';

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 页面切出后仍要由常驻 Provider 保留终端输出，切回页面时 xterm 能 replay 完整屏幕态。
 *
 * Code Logic（这个函数做什么）:
 *   condition 为 false 时抛错，让测试用例失败。
 */
function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Node/Vitest 没有真实 animation frame，需要确定性注入以验证批处理语义。
 *
 * Code Logic（这个函数做什么）:
 *   收集 schedule 的 callback，flush 时按入队顺序逐个执行；cancel 令对应 callback 失效。
 */
function createCollectingFrameScheduler(): {
  scheduler: TerminalFrameScheduler;
  pendingCount: () => number;
  flush: () => void;
} {
  const pending = new Map<number, () => void>();
  let nextId = 1;

  return {
    scheduler: {
      /**
       * Business Logic（为什么需要这个函数）:
       *   生产环境用 rAF 合并同帧通知；测试需要可控地推迟并手动 flush。
       *
       * Code Logic（这个函数做什么）:
       *   把 callback 记入 pending map，返回 cancel 函数删除该条目。
       */
      schedule(callback: () => void): () => void {
        const id = nextId;
        nextId += 1;
        pending.set(id, callback);
        return () => {
          pending.delete(id);
        };
      },
    },
    pendingCount: () => pending.size,
    /**
     * Business Logic（为什么需要这个函数）:
     *   测试断言需要在“帧到达”时一次性触发所有已调度通知。
     *
     * Code Logic（这个函数做什么）:
     *   复制当前 pending 后清空 map，再按顺序执行 callback（执行中新 schedule 的不在本轮）。
     */
    flush(): void {
      const callbacks = [...pending.values()];
      pending.clear();
      for (const callback of callbacks) {
        callback();
      }
    },
  };
}

describe('workbenchTerminalBuffer', () => {
  test('truncates, resets, removes buffers and notifies only changed session subscribers', () => {
    let buffers = appendWorkbenchTerminalOutput({}, 'session-a', 'abc', 10);
    buffers = appendWorkbenchTerminalOutput(buffers, 'session-a', 'defghijk', 10);

    assert(buffers['session-a'] === 'bcdefghijk', 'buffer should keep latest max chars');

    const resetBuffers = resetWorkbenchTerminalBuffer(buffers, 'session-a');
    assert(resetBuffers['session-a'] === '', 'reset should keep session with empty buffer');

    const removedBuffers = removeWorkbenchTerminalBuffer(resetBuffers, 'session-a');
    assert(!('session-a' in removedBuffers), 'remove should delete session buffer');

    const frames = createCollectingFrameScheduler();
    const store = createWorkbenchTerminalBufferStore({
      maxChars: 20,
      frameScheduler: frames.scheduler,
    });
    let sessionANotifications = 0;
    let sessionBNotifications = 0;

    const unsubscribeA = store.subscribe('session-a', () => {
      sessionANotifications += 1;
    });
    const unsubscribeB = store.subscribe('session-b', () => {
      sessionBNotifications += 1;
    });

    store.append('session-a', 'hello');
    assert(sessionANotifications === 0, 'append should not notify before frame flush');
    assert(store.getRevision('session-a') === 0, 'revision should stay until frame flush');
    assert(store.getBuffer('session-a') === 'hello', 'store should cache appended session output before notify');

    frames.flush();

    assert(store.getBuffer('session-a') === 'hello', 'store should keep appended session output after flush');
    assert(store.getRevision('session-a') === 1, 'store should bump changed session revision after frame');
    assert(store.getRevision('session-b') === 0, 'store should not bump unrelated session revision');
    assert(sessionANotifications === 1, 'store should notify changed session subscribers once per frame');
    assert(sessionBNotifications === 0, 'store should not notify unrelated session subscribers');

    unsubscribeA();
    unsubscribeB();
  });

  test('batches many appends into one frame notification with exact order and trim', () => {
    const frames = createCollectingFrameScheduler();
    const store = createWorkbenchTerminalBufferStore({
      maxChars: MAX_WORKBENCH_TERMINAL_BUFFER_CHARS,
      frameScheduler: frames.scheduler,
    });

    let notifications = 0;
    store.subscribe('session-hot', () => {
      notifications += 1;
    });

    const chunkCount = 10_000;
    let expected = '';
    for (let index = 0; index < chunkCount; index += 1) {
      const chunk = `c${index}|`;
      expected += chunk;
      store.append('session-hot', chunk);
    }

    assert(notifications === 0, '10k appends must not notify before animation frame');
    assert(store.getRevision('session-hot') === 0, 'revision must stay 0 before frame');
    assert(frames.pendingCount() === 1, 'multiple appends should schedule a single pending frame');
    assert(
      store.getBuffer('session-hot') === expected,
      'getBuffer before flush must still materialize exact concatenation order',
    );

    frames.flush();

    assert(notifications === 1, 'one frame must produce exactly one notification');
    assert(store.getRevision('session-hot') === 1, 'one frame must bump revision once');
    assert(
      store.getBuffer('session-hot') === expected,
      'getBuffer after flush must keep exact content order',
    );
  });

  test('trims to maxChars from the head while preserving UTF-16 tail', () => {
    const frames = createCollectingFrameScheduler();
    const maxChars = 20;
    const store = createWorkbenchTerminalBufferStore({
      maxChars,
      frameScheduler: frames.scheduler,
    });

    store.append('session-trim', 'abcdefghij'); // 10
    store.append('session-trim', 'klmnopqrst'); // 20
    store.append('session-trim', 'uvwxyz'); // 26 -> keep last 20
    frames.flush();

    assert(
      store.getBuffer('session-trim') === 'ghijklmnopqrstuvwxyz',
      'trim should drop head chars and keep latest maxChars',
    );
    assert(store.getBuffer('session-trim').length === maxChars, 'trimmed buffer length must equal maxChars');

    // Cross-chunk boundary: first chunk partially retained
    const storeBoundary = createWorkbenchTerminalBufferStore({
      maxChars: 5,
      frameScheduler: frames.scheduler,
    });
    storeBoundary.append('s', 'abcd');
    storeBoundary.append('s', 'efgh');
    frames.flush();
    assert(storeBoundary.getBuffer('s') === 'defgh', 'trim should slice only the boundary chunk');
  });

  test('materializes snapshot once and caches until mutation', () => {
    const frames = createCollectingFrameScheduler();
    const store = createWorkbenchTerminalBufferStore({
      frameScheduler: frames.scheduler,
    });

    store.append('session-cache', 'alpha');
    store.append('session-cache', 'beta');

    const first = store.getBuffer('session-cache');
    const second = store.getBuffer('session-cache');
    assert(first === 'alphabeta', 'materialized snapshot should join chunks');
    assert(first === second, 'getBuffer should return the same cached string reference');

    store.append('session-cache', 'gamma');
    const third = store.getBuffer('session-cache');
    assert(third === 'alphabetagamma', 'mutation should invalidate cache and re-materialize');
    assert(third !== first, 'mutated buffer must not reuse stale materialized string');
  });

  test('isolates sessions for buffer, revision, and notifications', () => {
    const frames = createCollectingFrameScheduler();
    const store = createWorkbenchTerminalBufferStore({
      frameScheduler: frames.scheduler,
    });

    let aNotifications = 0;
    let bNotifications = 0;
    store.subscribe('a', () => {
      aNotifications += 1;
    });
    store.subscribe('b', () => {
      bNotifications += 1;
    });

    store.append('a', 'AAA');
    store.append('b', 'BBB');
    store.append('a', 'aaa');
    assert(aNotifications === 0 && bNotifications === 0, 'no notifications before frame');

    frames.flush();

    assert(store.getBuffer('a') === 'AAAaaa', 'session a content isolated');
    assert(store.getBuffer('b') === 'BBB', 'session b content isolated');
    assert(store.getRevision('a') === 1, 'session a revision bumped once');
    assert(store.getRevision('b') === 1, 'session b revision bumped once');
    assert(aNotifications === 1, 'session a notified once');
    assert(bNotifications === 1, 'session b notified once');
  });

  test('reset and remove cancel stale scheduled frames and notify immediately', () => {
    const frames = createCollectingFrameScheduler();
    const store = createWorkbenchTerminalBufferStore({
      frameScheduler: frames.scheduler,
    });

    let notifications = 0;
    store.subscribe('session-stale', () => {
      notifications += 1;
    });

    store.append('session-stale', 'pending-output');
    assert(frames.pendingCount() === 1, 'append should schedule a frame');
    assert(store.getBuffer('session-stale') === 'pending-output', 'append data visible before reset');

    store.reset('session-stale');
    assert(frames.pendingCount() === 0, 'reset must cancel the pending frame');
    assert(store.getBuffer('session-stale') === '', 'reset clears buffer immediately');
    assert(store.getRevision('session-stale') === 1, 'reset bumps revision immediately');
    assert(notifications === 1, 'reset notifies immediately without waiting for frame');

    // Stale flush (if any leftover) must not double-notify
    frames.flush();
    assert(notifications === 1, 'cancelled frame must not deliver a stale notification');
    assert(store.getRevision('session-stale') === 1, 'cancelled frame must not bump revision again');

    store.append('session-stale', 'again');
    store.remove('session-stale');
    assert(frames.pendingCount() === 0, 'remove must cancel the pending frame');
    assert(store.getBuffer('session-stale') === '', 'remove drops session buffer');
    assert(notifications === 2, 'remove notifies immediately');
    assert(store.getRevision('session-stale') === 2, 'remove bumps revision after reset baseline');

    frames.flush();
    assert(notifications === 2, 'cancelled remove-frame must not notify again');
  });

  test('enforces 200000 UTF-16 unit cap and keeps planTerminalBufferWrite append semantics', () => {
    const frames = createCollectingFrameScheduler();
    const store = createWorkbenchTerminalBufferStore({
      maxChars: MAX_WORKBENCH_TERMINAL_BUFFER_CHARS,
      frameScheduler: frames.scheduler,
    });

    // Build a buffer exactly at the cap, then append more to force head trim.
    const unit = 'x'.repeat(50_000);
    store.append('session-cap', unit);
    store.append('session-cap', unit);
    store.append('session-cap', unit);
    store.append('session-cap', unit); // 200_000
    frames.flush();

    const atCap = store.getBuffer('session-cap');
    assert(atCap.length === MAX_WORKBENCH_TERMINAL_BUFFER_CHARS, 'buffer should sit at 200k cap');

    const tail = 'NEW_TAIL_OUTPUT';
    store.append('session-cap', tail);
    frames.flush();

    const afterTrim = store.getBuffer('session-cap');
    assert(
      afterTrim.length === MAX_WORKBENCH_TERMINAL_BUFFER_CHARS,
      'buffer must remain at 200k after overflow append',
    );
    assert(afterTrim.endsWith(tail), 'trimmed buffer must keep the newest tail');

    // Replay diff: capped shift should plan append of only the live tail (not full replay).
    const plan = planTerminalBufferWrite(atCap, afterTrim);
    assert(plan.mode === 'append', 'capped ring-buffer shift must stay append-diff for xterm');
    assert(plan.data === tail, 'planTerminalBufferWrite should append only new output after trim');
  });

  test('accepts initialBuffers via options object', () => {
    const frames = createCollectingFrameScheduler();
    const store = createWorkbenchTerminalBufferStore({
      initialBuffers: { seeded: 'hello-seed' },
      frameScheduler: frames.scheduler,
    });

    assert(store.getBuffer('seeded') === 'hello-seed', 'initialBuffers should seed session content');
    assert(store.getRevision('seeded') === 0, 'seeded session starts at revision 0');
  });

  test('empty append does not schedule notify or bump revision', () => {
    const frames = createCollectingFrameScheduler();
    const store = createWorkbenchTerminalBufferStore({
      frameScheduler: frames.scheduler,
    });

    let notifications = 0;
    store.subscribe('session-empty', () => {
      notifications += 1;
    });

    store.append('session-empty', '');
    assert(frames.pendingCount() === 0, 'empty append must not schedule a frame');
    assert(store.getRevision('session-empty') === 0, 'empty append must not create/bump revision');
    assert(store.getBuffer('session-empty') === '', 'empty append must not seed buffer content');
    assert(notifications === 0, 'empty append must not notify subscribers');

    frames.flush();
    assert(frames.pendingCount() === 0, 'flush after empty append stays idle');
    assert(store.getRevision('session-empty') === 0, 'flush must not bump revision after empty append');
    assert(notifications === 0, 'flush must not notify after empty append');

    // Non-empty append still works after empty no-ops
    store.append('session-empty', 'data');
    assert(frames.pendingCount() === 1, 'non-empty append should schedule after empty no-op');
    frames.flush();
    assert(store.getBuffer('session-empty') === 'data', 'non-empty append should store content');
    assert(store.getRevision('session-empty') === 1, 'non-empty append should bump revision once');
    assert(notifications === 1, 'non-empty append should notify once');
  });

  test('synchronous frame scheduler still allows subsequent appends to notify', () => {
    /**
     * Business Logic（为什么需要这个 scheduler）:
     *   同步 schedule（callback 在返回 cancel 前已执行）会复现 scheduledCancel 粘住的回归：
     *   若赋值发生在 sync callback 清空之后，后续 append 永远跳过 schedule。
     *
     * Code Logic（这个 scheduler 做什么）:
     *   schedule 立即执行 callback 再返回 no-op cancel。
     */
    const syncScheduler: TerminalFrameScheduler = {
      schedule(callback: () => void): () => void {
        callback();
        return () => {};
      },
    };

    const store = createWorkbenchTerminalBufferStore({
      frameScheduler: syncScheduler,
    });

    let notifications = 0;
    store.subscribe('session-sync', () => {
      notifications += 1;
    });

    store.append('session-sync', 'first');
    assert(store.getBuffer('session-sync') === 'first', 'sync scheduler should apply first append content');
    assert(store.getRevision('session-sync') === 1, 'first sync append should bump revision immediately');
    assert(notifications === 1, 'first sync append should notify immediately');

    store.append('session-sync', 'second');
    assert(store.getBuffer('session-sync') === 'firstsecond', 'second append must concatenate after sync flush');
    assert(store.getRevision('session-sync') === 2, 'second sync append must bump revision again');
    assert(notifications === 2, 'second sync append must notify again (scheduledCancel must not stick)');
  });

  test('subscribe-before-snapshot dedupes queued deltas by generation and appendId', () => {
    const scheduler = createCollectingFrameScheduler();
    const store = createWorkbenchTerminalBufferStore({ frameScheduler: scheduler.scheduler });
    const deltas: TerminalBufferDelta[] = [];
    const unsubscribe = store.subscribeLive('s1', (delta) => deltas.push(delta));
    store.append('s1', 'a');
    const snapshot = store.getSnapshot('s1');
    store.append('s1', 'b');

    expect(snapshot.buffer).toBe('a');
    expect(snapshot.cursor).toEqual({ generation: 0, appendId: 1, lastSeq: 0 });
    expect(deltas.map((delta) => delta.chunk)).toEqual(['a', 'b']);
    expect(
      deltas.filter(
        (delta) =>
          delta.generation > snapshot.cursor.generation ||
          delta.appendId > snapshot.cursor.appendId,
      ),
    ).toHaveLength(1);
    unsubscribe();
  });

  test('live append never materializes the replay snapshot', () => {
    let materializeCalls = 0;
    const frames = createCollectingFrameScheduler();
    const store = createWorkbenchTerminalBufferStore({
      frameScheduler: frames.scheduler,
      onMaterializeForTest: () => {
        materializeCalls += 1;
      },
    });
    store.subscribeLive('s1', () => undefined);
    for (let index = 0; index < 1_000; index += 1) store.append('s1', 'x');
    expect(materializeCalls).toBe(0);
    expect(store.getSnapshot('s1').buffer.length).toBe(1_000);
    expect(materializeCalls).toBe(1);
  });

  test('full replay ring advances a head index instead of shifting every append', () => {
    let compactions = 0;
    const frames = createCollectingFrameScheduler();
    const store = createWorkbenchTerminalBufferStore({
      maxChars: 8,
      frameScheduler: frames.scheduler,
      onCompactForTest: () => {
        compactions += 1;
      },
    });
    for (let index = 0; index < 10_000; index += 1) store.append('s1', 'x');
    expect(store.getSnapshot('s1').buffer).toBe('xxxxxxxx');
    expect(compactions).toBeLessThan(12);
  });

  test('reset starts a new generation and remove invalidates old deltas', () => {
    const frames = createCollectingFrameScheduler();
    const store = createWorkbenchTerminalBufferStore({
      frameScheduler: frames.scheduler,
    });
    const deltas: TerminalBufferDelta[] = [];
    store.subscribeLive('s1', (delta) => deltas.push(delta));
    store.append('s1', 'old');
    store.reset('s1', 'new');
    store.append('s1', '!');
    expect(store.getSnapshot('s1').buffer).toBe('new!');
    expect(deltas.at(-1)?.generation).toBe(1);
  });

  test('lastSeq cutover drops duplicate stream events after reset/baseline', () => {
    const frames = createCollectingFrameScheduler();
    const store = createWorkbenchTerminalBufferStore({
      frameScheduler: frames.scheduler,
    });
    const deltas: TerminalBufferDelta[] = [];
    store.subscribeLive('s1', (delta) => deltas.push(delta));

    // 模拟 baseline replay：buffer 已含 seq=5 及之前的输出
    store.reset('s1', 'BASE', 5);
    expect(store.getLastSeq('s1')).toBe(5);
    expect(store.getSnapshot('s1').cursor.lastSeq).toBe(5);

    // stream 中仍排队的 <= lastSeq 事件必须丢弃
    store.append('s1', 'dup', 4);
    store.append('s1', 'dup5', 5);
    expect(store.getBuffer('s1')).toBe('BASE');
    expect(deltas).toHaveLength(0);

    // 严格更大的 seq 才能 append
    store.append('s1', 'live', 6);
    expect(store.getBuffer('s1')).toBe('BASElive');
    expect(store.getLastSeq('s1')).toBe(6);
    expect(deltas).toHaveLength(1);
    expect(deltas[0]?.chunk).toBe('live');
  });
});

describe('applyTerminalBaselineCutover', () => {
  test('stale baseline lastSeq=N-1 re-applies held live seq=N after reset', () => {
    const frames = createCollectingFrameScheduler();
    const store = createWorkbenchTerminalBufferStore({
      frameScheduler: frames.scheduler,
    });

    // live seq=N 先到达并 append（模拟 race 中 held 侧也记录了该事件）
    store.append('s1', 'liveN', 10);
    expect(store.getBuffer('s1')).toBe('liveN');
    expect(store.getLastSeq('s1')).toBe(10);

    const held: HeldLiveTerminalEvent[] = [{ chunk: 'liveN', seq: 10 }];

    // 过期 baseline/replay 完成：lastSeq=N-1，若只 reset 会抹掉 liveN
    const pruned = applyTerminalBaselineCutover(
      store,
      's1',
      'BASE',
      9,
      held,
    );

    expect(store.getBuffer('s1')).toBe('BASEliveN');
    expect(store.getLastSeq('s1')).toBe(10);
    expect(pruned).toEqual([{ chunk: 'liveN', seq: 10 }]);
  });

  test('resync cutover drops held events with seq <= lastSeq and keeps newer ones', () => {
    const frames = createCollectingFrameScheduler();
    const store = createWorkbenchTerminalBufferStore({
      frameScheduler: frames.scheduler,
    });

    // 模拟 live 先入队：旧 seq 与更新 seq 均 held
    store.append('s1', 'old8', 8);
    store.append('s1', 'liveN', 10);

    const held: HeldLiveTerminalEvent[] = [
      { chunk: 'old8', seq: 8 },
      { chunk: 'dup9', seq: 9 },
      { chunk: 'liveN', seq: 10 },
      { chunk: 'live11', seq: 11 },
    ];

    // resync：buffer 覆盖到 lastSeq=9
    const pruned = applyTerminalBaselineCutover(
      store,
      's1',
      'BASE',
      9,
      held,
    );

    expect(store.getBuffer('s1')).toBe('BASEliveNlive11');
    expect(store.getLastSeq('s1')).toBe(11);
    expect(pruned).toEqual([
      { chunk: 'liveN', seq: 10 },
      { chunk: 'live11', seq: 11 },
    ]);
  });

  test('empty held list is pure reset cutover', () => {
    const frames = createCollectingFrameScheduler();
    const store = createWorkbenchTerminalBufferStore({
      frameScheduler: frames.scheduler,
    });
    store.append('s1', 'stale', 3);

    const pruned = applyTerminalBaselineCutover(store, 's1', 'BASE', 5, []);

    expect(store.getBuffer('s1')).toBe('BASE');
    expect(store.getLastSeq('s1')).toBe(5);
    expect(pruned).toEqual([]);
  });
});

describe('session cutover epoch / committed baseline', () => {
  test('shouldAccept rejects lastSeq older than committed baseline', () => {
    let state = createEmptySessionCutoverState();
    state = commitTerminalCutover(state, 20);

    expect(shouldAcceptTerminalCutover(state, 20)).toBe(true);
    expect(shouldAcceptTerminalCutover(state, 21)).toBe(true);
    expect(shouldAcceptTerminalCutover(state, 19)).toBe(false);
    expect(shouldAcceptTerminalCutover(state, Number.NaN)).toBe(false);
  });

  test('shouldAccept rejects requestEpoch older than cutoverEpoch', () => {
    let state = createEmptySessionCutoverState();
    const overflow = beginHeldOverflowReplay(state);
    state = overflow.state;
    // overflow 把 epoch 抬到 1
    expect(state.cutoverEpoch).toBe(1);
    expect(state.needsReplay).toBe(true);

    expect(shouldAcceptTerminalCutover(state, 50, 1)).toBe(true);
    expect(shouldAcceptTerminalCutover(state, 50, 0)).toBe(false);
    // 未提供 requestEpoch 时只按 lastSeq 判定
    expect(shouldAcceptTerminalCutover(state, 50)).toBe(true);
  });

  test('out-of-order baseline B then A: A rejected; store keeps B + later live', () => {
    const frames = createCollectingFrameScheduler();
    const store = createWorkbenchTerminalBufferStore({
      frameScheduler: frames.scheduler,
    });
    let state = createEmptySessionCutoverState();
    const heldAfterB: HeldLiveTerminalEvent[] = [{ chunk: 'liveB', seq: 21 }];

    // 先 apply 较新 B（lastSeq=20）
    expect(shouldAcceptTerminalCutover(state, 20)).toBe(true);
    applyTerminalBaselineCutover(store, 's1', 'BASE-B', 20, heldAfterB);
    state = commitTerminalCutover(state, 20);
    store.append('s1', 'afterB', 22);

    expect(store.getBuffer('s1')).toBe('BASE-BliveBafterB');
    expect(store.getLastSeq('s1')).toBe(22);
    expect(state.committedBaselineLastSeq).toBe(20);

    // 晚到的旧 baseline A（lastSeq=10）必须 reject，且不得 clobber store
    expect(shouldAcceptTerminalCutover(state, 10)).toBe(false);
    // 模拟 Provider 拒绝后不调用 apply
    expect(store.getBuffer('s1')).toBe('BASE-BliveBafterB');
    expect(store.getLastSeq('s1')).toBe(22);
  });

  test('held overflow: beginHeldOverflowReplay bumps epoch and marks needsReplay', () => {
    let state = createEmptySessionCutoverState();
    state = commitTerminalCutover(state, 5);
    expect(state.needsReplay).toBe(false);
    expect(state.overflowHighWaterSeq).toBe(0);

    const first = beginHeldOverflowReplay(state, 40);
    expect(first.requestEpoch).toBe(1);
    expect(first.state.cutoverEpoch).toBe(1);
    expect(first.state.needsReplay).toBe(true);
    expect(first.state.committedBaselineLastSeq).toBe(5);
    expect(first.state.overflowHighWaterSeq).toBe(40);

    // 并发再次 overflow：抬 epoch，旧 in-flight 的 requestEpoch=1 应失效；high water 取 max
    const second = beginHeldOverflowReplay(first.state, 30);
    expect(second.requestEpoch).toBe(2);
    expect(second.state.cutoverEpoch).toBe(2);
    expect(second.state.overflowHighWaterSeq).toBe(40);
    expect(shouldAcceptTerminalCutover(second.state, 100, 1)).toBe(false);
    expect(shouldAcceptTerminalCutover(second.state, 100, 2)).toBe(true);
  });

  test('beginHeldOverflowReplay records max high water across successive overflows', () => {
    const state = createEmptySessionCutoverState();
    const first = beginHeldOverflowReplay(state, 10);
    expect(first.state.overflowHighWaterSeq).toBe(10);

    const second = beginHeldOverflowReplay(first.state, 25);
    expect(second.state.overflowHighWaterSeq).toBe(25);

    const third = beginHeldOverflowReplay(second.state, 12);
    expect(third.state.overflowHighWaterSeq).toBe(25);
    expect(third.state.needsReplay).toBe(true);
    expect(third.requestEpoch).toBe(3);
  });

  test('held overflow decision: >MAX_HELD requires clear+replay, not drop-oldest', () => {
    // 纯决策：超过上限时 Provider 必须清空 held 并 beginHeldOverflowReplay，
    // 而不是 splice(0, held.length - MAX) 后继续。
    const held: HeldLiveTerminalEvent[] = [];
    for (let i = 1; i <= MAX_HELD_LIVE_TERMINAL_EVENTS; i += 1) {
      held.push({ chunk: `c${i}`, seq: i });
    }
    expect(held.length).toBe(MAX_HELD_LIVE_TERMINAL_EVENTS);

    // 再 push 1 条 → 溢出
    held.push({ chunk: 'overflow', seq: MAX_HELD_LIVE_TERMINAL_EVENTS + 1 });
    const overflowed = held.length > MAX_HELD_LIVE_TERMINAL_EVENTS;
    expect(overflowed).toBe(true);

    // 正确语义：清空 held 前算 highWater + 抬 epoch
    let highWater = 0;
    for (const event of held) {
      if (typeof event.seq === 'number' && Number.isFinite(event.seq)) {
        highWater = Math.max(highWater, event.seq);
      }
    }
    const cleared: HeldLiveTerminalEvent[] = [];
    let state = createEmptySessionCutoverState();
    const { state: next, requestEpoch } = beginHeldOverflowReplay(state, highWater);
    state = setTerminalCutoverReplayInFlight(next, true);

    expect(cleared).toEqual([]);
    expect(requestEpoch).toBe(1);
    expect(state.needsReplay).toBe(true);
    expect(state.replayInFlight).toBe(true);
    expect(state.cutoverEpoch).toBe(1);
    expect(state.overflowHighWaterSeq).toBe(MAX_HELD_LIVE_TERMINAL_EVENTS + 1);

    // 禁止 drop-oldest：若错误 splice 会保留尾部 256 条并丢前缀，语义不可接受。
    const wrongDropOldest = held.slice(-(MAX_HELD_LIVE_TERMINAL_EVENTS));
    expect(wrongDropOldest[0]?.seq).toBe(2); // 静默丢了 seq=1
    expect(cleared.length).toBe(0); // 正确路径不保留任何 held 缺口
  });

  test('commit with matching requestEpoch clears needsReplay after overflow', () => {
    let state = createEmptySessionCutoverState();
    const overflow = beginHeldOverflowReplay(state, 100);
    state = overflow.state;
    expect(state.needsReplay).toBe(true);
    expect(state.overflowHighWaterSeq).toBe(100);

    // matching epoch 即使 lastSeq < highWater 也清 needsReplay
    state = commitTerminalCutover(state, 42, overflow.requestEpoch);
    expect(state.committedBaselineLastSeq).toBe(42);
    expect(state.needsReplay).toBe(false);
    expect(state.overflowHighWaterSeq).toBe(0);
    expect(state.cutoverEpoch).toBe(1); // epoch 不回退
  });

  test('overflow then epoch-less commit with lastSeq < highWater keeps needsReplay', () => {
    let state = createEmptySessionCutoverState();
    const overflow = beginHeldOverflowReplay(state, 100);
    state = overflow.state;
    expect(state.needsReplay).toBe(true);
    expect(state.overflowHighWaterSeq).toBe(100);

    // launch baseline / terminal-resync 无 epoch；lastSeq 未盖住 high-water 不得清 needsReplay
    state = commitTerminalCutover(state, 50);
    expect(state.committedBaselineLastSeq).toBe(50);
    expect(state.needsReplay).toBe(true);
    expect(state.overflowHighWaterSeq).toBe(100);
    expect(state.cutoverEpoch).toBe(1);
  });

  test('epoch-less commit with lastSeq >= highWater clears needsReplay', () => {
    let state = createEmptySessionCutoverState();
    const overflow = beginHeldOverflowReplay(state, 80);
    state = overflow.state;
    expect(state.needsReplay).toBe(true);

    state = commitTerminalCutover(state, 80);
    expect(state.committedBaselineLastSeq).toBe(80);
    expect(state.needsReplay).toBe(false);
    expect(state.overflowHighWaterSeq).toBe(0);
  });

  test('shouldClearTerminalNeedsReplay: epoch match / highWater / nothing-to-protect', () => {
    const empty = createEmptySessionCutoverState();
    expect(shouldClearTerminalNeedsReplay(empty, 0)).toBe(true);

    const state = beginHeldOverflowReplay(empty, 100).state;
    expect(shouldClearTerminalNeedsReplay(state, 50)).toBe(false);
    expect(shouldClearTerminalNeedsReplay(state, 100)).toBe(true);
    expect(shouldClearTerminalNeedsReplay(state, 50, 1)).toBe(true);
    expect(shouldClearTerminalNeedsReplay(state, 50, 0)).toBe(false);
  });

  test('stale epoch-less baseline after overflow does not clear needsReplay', () => {
    // Provider 决策：overflow 后 launch baseline 无 epoch 且 lastSeq < highWater
    // 可 accept+commit 抬 committed，但 needsReplay 必须保留直至匹配 epoch 或盖住 highWater。
    let state = createEmptySessionCutoverState();
    state = commitTerminalCutover(state, 10);
    const overflow = beginHeldOverflowReplay(state, 60);
    state = overflow.state;

    expect(shouldAcceptTerminalCutover(state, 30)).toBe(true);
    state = commitTerminalCutover(state, 30); // epoch-less
    expect(state.committedBaselineLastSeq).toBe(30);
    expect(state.needsReplay).toBe(true);
    expect(state.overflowHighWaterSeq).toBe(60);

    // 当前 epoch replay 成功才真正清闸
    expect(shouldAcceptTerminalCutover(state, 70, overflow.requestEpoch)).toBe(true);
    state = commitTerminalCutover(state, 70, overflow.requestEpoch);
    expect(state.needsReplay).toBe(false);
    expect(state.overflowHighWaterSeq).toBe(0);
  });

  test('beginAuthorityChangeReplay forces re-baseline for first bind and already-bound switch', () => {
    const unbound = createEmptySessionCutoverState();
    // R10 M1：首次绑定也必须强制 needsReplay（禁止 light rebind）。
    const firstBind = beginAuthorityChangeReplay(unbound, 'owner-a');
    expect(firstBind).not.toBeNull();
    expect(firstBind!.state.authorityId).toBe('owner-a');
    expect(firstBind!.state.needsReplay).toBe(true);
    expect(firstBind!.state.committedBaselineLastSeq).toBe(0);
    expect(firstBind!.state.cutoverEpoch).toBe(1);
    expect(firstBind!.requestEpoch).toBe(1);
    expect(shouldCollectHeldLiveTerminalEvent(firstBind!.state, true)).toBe(true);

    expect(beginAuthorityChangeReplay(unbound, null)).toBeNull();
    expect(beginAuthorityChangeReplay(unbound, '')).toBeNull();

    const state = commitTerminalCutover(unbound, 100, undefined, 'owner-a');
    expect(state.authorityId).toBe('owner-a');
    expect(beginAuthorityChangeReplay(state, 'owner-a')).toBeNull();

    const switched = beginAuthorityChangeReplay(state, 'owner-b');
    expect(switched).not.toBeNull();
    expect(switched!.requestEpoch).toBe(state.cutoverEpoch + 1);
    expect(switched!.state.authorityId).toBe('owner-b');
    expect(switched!.state.committedBaselineLastSeq).toBe(0);
    expect(switched!.state.overflowHighWaterSeq).toBe(0);
    expect(switched!.state.needsReplay).toBe(true);
    // R13 H1：authority 切换同样保留 inFlight（无在途时为 false）。
    expect(switched!.state.replayInFlight).toBe(false);
    expect(switched!.state.cutoverEpoch).toBe(state.cutoverEpoch + 1);
    // 切换后必须 held 新 authority live，直到匹配 epoch 的 replay 成功。
    expect(shouldCollectHeldLiveTerminalEvent(switched!.state, true)).toBe(true);

    const inFlightSwitch = beginAuthorityChangeReplay(
      {
        ...state,
        authorityId: 'owner-a',
        replayInFlight: true,
      },
      'owner-c',
    );
    expect(inFlightSwitch).not.toBeNull();
    expect(inFlightSwitch!.state.replayInFlight).toBe(true);
    expect(inFlightSwitch!.state.needsReplay).toBe(true);
  });

  test('terminalReplayRecoveryDelayMs caps backoff and shouldTrigger respects cooldown', () => {
    expect(TERMINAL_REPLAY_IMMEDIATE_ATTEMPTS).toBe(3);
    expect(terminalReplayRecoveryDelayMs(1)).toBe(50);
    expect(terminalReplayRecoveryDelayMs(2)).toBe(100);
    // 第 3 次失败后进入恢复波：1s、2s、4s… 封顶
    expect(terminalReplayRecoveryDelayMs(3)).toBe(1_000);
    expect(terminalReplayRecoveryDelayMs(4)).toBe(2_000);
    expect(terminalReplayRecoveryDelayMs(5)).toBe(4_000);
    expect(terminalReplayRecoveryDelayMs(6)).toBe(TERMINAL_REPLAY_RECOVERY_BACKOFF_CAP_MS);
    expect(terminalReplayRecoveryDelayMs(20)).toBe(TERMINAL_REPLAY_RECOVERY_BACKOFF_CAP_MS);

    expect(shouldTriggerTerminalReplayRecovery(0, 1000)).toBe(true);
    expect(shouldTriggerTerminalReplayRecovery(500, 1000)).toBe(true);
    expect(shouldTriggerTerminalReplayRecovery(1500, 1000)).toBe(false);
  });

  test('classifyTerminalReplayError uses stable code / ContractDecodeError only (R12 M2)', () => {
    // 无 code 且非 ContractDecodeError → 默认 recoverable（禁止 message 子串判定）
    expect(classifyTerminalReplayError(new Error('replay_unavailable'))).toBe('recoverable');
    expect(classifyTerminalReplayError(new Error('request timeout'))).toBe('recoverable');
    expect(classifyTerminalReplayError(new Error('network offline'))).toBe('recoverable');
    expect(classifyTerminalReplayError(new Error('unknown boom'))).toBe('recoverable');
    expect(classifyTerminalReplayError(null)).toBe('recoverable');
    // 中文/英文 not-found 文案不再触发 not_found
    expect(classifyTerminalReplayError(new Error('工作台会话不存在'))).toBe('recoverable');
    expect(classifyTerminalReplayError(new Error('session not found'))).toBe('recoverable');
    expect(classifyTerminalReplayError(new Error('validation failed'))).toBe('recoverable');
    expect(classifyTerminalReplayError(new Error('malformed dto'))).toBe('recoverable');

    // 稳定 code 路径（含 normalizeError 透传）
    expect(
      classifyTerminalReplayError(
        normalizeError({ error: '远端 Workbench 网关只接受对端本机项目', code: 'validation' }),
      ),
    ).toBe('permanent');
    expect(
      classifyTerminalReplayError(normalizeError({ error: 'missing', code: 'validation_error' })),
    ).toBe('permanent');
    expect(
      classifyTerminalReplayError(normalizeError({ error: 'gone', code: 'not_found' })),
    ).toBe('not_found');
    expect(
      classifyTerminalReplayError(normalizeError({ error: 'gone', code: 'session_not_found' })),
    ).toBe('not_found');
    expect(
      classifyTerminalReplayError(normalizeError({ error: 'busy', code: 'unavailable' })),
    ).toBe('recoverable');
    expect(
      classifyTerminalReplayError(normalizeError({ error: 'slow', code: 'timeout' })),
    ).toBe('recoverable');
    expect(
      classifyTerminalReplayError(normalizeError({ error: 'race', code: 'conflict' })),
    ).toBe('recoverable');
    expect(
      classifyTerminalReplayError(normalizeError({ error: 'boom', code: 'internal' })),
    ).toBe('recoverable');

    const decodeErr = new Error('Contract "WorkbenchSessionReplay" failed at $.lastSeq: got primitive');
    decodeErr.name = 'ContractDecodeError';
    expect(classifyTerminalReplayError(decodeErr)).toBe('permanent');

    expect(terminalHistorySyncFailureFromClass('not_found')).toEqual({ kind: 'not_found' });
    expect(terminalHistorySyncFailureFromClass('permanent')).toEqual({
      kind: 'history_sync_failed',
    });

    const stopped = stopTerminalCutoverReplay({
      ...createEmptySessionCutoverState(),
      needsReplay: true,
      replayInFlight: true,
      cutoverEpoch: 2,
    });
    expect(stopped.needsReplay).toBe(false);
    expect(stopped.replayInFlight).toBe(false);
    expect(stopped.cutoverEpoch).toBe(2);

    // beginStartupBaselineReplay：抬 epoch + needsReplay，不重置 authority/seq；
    // R13 H1：保留既有 replayInFlight，禁止 list 路径清门闩制造并发 cutover。
    const base = {
      ...createEmptySessionCutoverState(),
      authorityId: 'auth-A',
      committedBaselineLastSeq: 9,
      overflowHighWaterSeq: 3,
      cutoverEpoch: 4,
      needsReplay: false,
      replayInFlight: true,
    };
    const started = beginStartupBaselineReplay(base);
    expect(started.requestEpoch).toBe(5);
    expect(started.state.cutoverEpoch).toBe(5);
    expect(started.state.needsReplay).toBe(true);
    expect(started.state.replayInFlight).toBe(true);
    expect(started.state.authorityId).toBe('auth-A');
    expect(started.state.committedBaselineLastSeq).toBe(9);
    expect(started.state.overflowHighWaterSeq).toBe(3);

    const idle = beginStartupBaselineReplay({
      ...base,
      replayInFlight: false,
    });
    expect(idle.state.replayInFlight).toBe(false);
  });

  test('owner authority change accepts lower lastSeq and resets store baseline', () => {
    const frames = createCollectingFrameScheduler();
    const store = createWorkbenchTerminalBufferStore({
      frameScheduler: frames.scheduler,
    });
    let state = createEmptySessionCutoverState();

    // owner A: lastSeq 推进到 100
    expect(shouldAcceptTerminalCutover(state, 100, undefined, 'owner-a')).toBe(true);
    applyTerminalBaselineCutover(store, 's1', 'A-BASE', 100, [], 'owner-a', true);
    state = commitTerminalCutover(state, 100, undefined, 'owner-a');
    store.append('s1', 'a-live', 101, 'owner-a');
    expect(store.getLastSeq('s1')).toBe(101);
    expect(state.authorityId).toBe('owner-a');
    expect(isTerminalAuthorityChange(state, 'owner-b')).toBe(true);

    // 已绑定 A 时，直接用 B 调 shouldAccept 必须拒绝（防 delayed A/B clobber）。
    expect(shouldAcceptTerminalCutover(state, 1, undefined, 'owner-b')).toBe(false);

    // R9 M1：已绑定 authority 切换强制 re-baseline（beginAuthorityChangeReplay）。
    const switched = beginAuthorityChangeReplay(state, 'owner-b');
    expect(switched).not.toBeNull();
    state = switched!.state;
    expect(state.needsReplay).toBe(true);
    expect(shouldAcceptTerminalCutover(state, 1, switched!.requestEpoch, 'owner-b')).toBe(
      true,
    );
    // authority 已 rebind：authorityChanged=false；旧 held 在 Provider 侧先清空，
    // 这里传入的旧 seq held 也不得写回（调用方应丢弃或 authorityChanged=true）。
    const pruned = applyTerminalBaselineCutover(
      store,
      's1',
      'B-BASE',
      1,
      [{ chunk: 'stale-a', seq: 102 }],
      'owner-b',
      true,
    );
    expect(pruned).toEqual([]);
    state = commitTerminalCutover(state, 1, switched!.requestEpoch, 'owner-b');
    expect(store.getBuffer('s1')).toBe('B-BASE');
    expect(store.getLastSeq('s1')).toBe(1);
    expect(state.authorityId).toBe('owner-b');
    expect(state.committedBaselineLastSeq).toBe(1);
    expect(state.needsReplay).toBe(false);

    // 后续 live 从新 authority 的 seq=2 起可写
    store.append('s1', 'b-live', 2, 'owner-b');
    expect(store.getBuffer('s1')).toBe('B-BASEb-live');
    expect(store.getLastSeq('s1')).toBe(2);

    // 迟到的 owner-A 高 lastSeq replay 不得 clobber B
    expect(shouldAcceptTerminalCutover(state, 999, undefined, 'owner-a')).toBe(false);
  });

  test('delayed owner-A replay after B live must not clobber B', () => {
    const frames = createCollectingFrameScheduler();
    const store = createWorkbenchTerminalBufferStore({
      frameScheduler: frames.scheduler,
    });
    let state = createEmptySessionCutoverState();

    // A baseline 请求已发出，但尚未返回。期间 B live rebind + cutover 建立权威。
    state = {
      ...state,
      authorityId: 'owner-b',
      committedBaselineLastSeq: 0,
      overflowHighWaterSeq: 0,
      needsReplay: false,
    };
    expect(shouldAcceptTerminalCutover(state, 5, 1, 'owner-b')).toBe(true);
    applyTerminalBaselineCutover(store, 's1', 'B', 5, [], 'owner-b', true);
    state = commitTerminalCutover(state, 5, 1, 'owner-b');
    store.append('s1', 'b2', 6, 'owner-b');
    expect(store.getBuffer('s1')).toBe('Bb2');

    // A 的 delayed replay（高 lastSeq、旧 authority）必须 reject
    expect(shouldAcceptTerminalCutover(state, 100, 0, 'owner-a')).toBe(false);
    // 无 authority 的旧 baseline 也不得用高 lastSeq 清 B（同 authority 比较 lastSeq 时可接受，
    // 但这里模拟 replay 未带 owner：不得改变已绑定 authority，且 lastSeq 比较仍可接受高水位）。
    // 关键：带 owner-a 的路径已 reject。
    expect(store.getBuffer('s1')).toBe('Bb2');
    expect(store.getLastSeq('s1')).toBe(6);
    expect(state.authorityId).toBe('owner-b');
  });

  test('remote bridged live-before-replay accepts same local bus owner history', () => {
    // R7 H1：bridged live 与 replay 统一本机 event_bus owner。
    // live-first 绑定 local-owner 后，stamp 过的 replay 必须能补上挂载前历史；
    // 若仍带 remote owner，则永久丢失 history。
    const frames = createCollectingFrameScheduler();
    const store = createWorkbenchTerminalBufferStore({
      frameScheduler: frames.scheduler,
    });
    let state = createEmptySessionCutoverState();
    const localOwner = 'local-bus-owner';
    const remoteOwner = 'remote-peer-owner';
    const sessionId = 'remote:peer:s1';

    // live 先到：GUI enrichment 绑定本机 bus owner
    store.append(sessionId, 'live-2', 2, localOwner);
    state = commitTerminalCutover(state, 0, undefined, localOwner);
    expect(state.authorityId).toBe(localOwner);
    expect(store.getLastSeq(sessionId)).toBe(2);

    // 错误路径：远端 raw owner 的 replay 必须 reject（证明 authority 冲突后果）
    expect(shouldAcceptTerminalCutover(state, 2, undefined, remoteOwner)).toBe(false);

    // 正确路径：后端无条件 stamp 本机 owner 后，replay 可补历史并 cutover
    expect(shouldAcceptTerminalCutover(state, 2, undefined, localOwner)).toBe(true);
    applyTerminalBaselineCutover(store, sessionId, 'history-12', 2, [], localOwner, false);
    state = commitTerminalCutover(state, 2, undefined, localOwner);
    expect(store.getBuffer(sessionId)).toBe('history-12');
    expect(store.getLastSeq(sessionId)).toBe(2);
    expect(state.authorityId).toBe(localOwner);

    // 后续 live seq=3 继续同 authority
    store.append(sessionId, 'live-3', 3, localOwner);
    expect(store.getBuffer(sessionId)).toBe('history-12live-3');
    expect(store.getLastSeq(sessionId)).toBe(3);
  });

  test('remote bridged replay-before-live keeps single local bus authority without double-apply', () => {
    // R7 H1：replay-first 时 replay/live 同 local owner，live 不得切换 authority 重置 lastSeq。
    const frames = createCollectingFrameScheduler();
    const store = createWorkbenchTerminalBufferStore({
      frameScheduler: frames.scheduler,
    });
    let state = createEmptySessionCutoverState();
    const localOwner = 'local-bus-owner';
    const sessionId = 'remote:peer:s1';

    // replay 先到（已 stamp 本机 owner），建立 baseline lastSeq=2
    expect(shouldAcceptTerminalCutover(state, 2, undefined, localOwner)).toBe(true);
    applyTerminalBaselineCutover(store, sessionId, 'history-12', 2, [], localOwner, true);
    state = commitTerminalCutover(state, 2, undefined, localOwner);
    expect(store.getBuffer(sessionId)).toBe('history-12');
    expect(store.getLastSeq(sessionId)).toBe(2);
    expect(state.authorityId).toBe(localOwner);

    // live 同 authority：seq<=lastSeq 的竞态 chunk 被 no-op（不双写）
    store.append(sessionId, 'dup-2', 2, localOwner);
    expect(store.getBuffer(sessionId)).toBe('history-12');
    expect(store.getLastSeq(sessionId)).toBe(2);

    // 新 live seq=3 追加；authority 不变
    store.append(sessionId, 'live-3', 3, localOwner);
    expect(store.getBuffer(sessionId)).toBe('history-12live-3');
    expect(store.getLastSeq(sessionId)).toBe(3);
    expect(isTerminalAuthorityChange(state, localOwner)).toBe(false);
  });

  test('remote composite authority cutover accepts seq restart after remote backend restart', () => {
    // R8 H1：同 local bus owner，remote stream owner 变化（远端 backend 重启）必须 cutover，
    // 否则 lastSeq 高位永久丢弃新低 seq，终端静默冻结。
    const frames = createCollectingFrameScheduler();
    const store = createWorkbenchTerminalBufferStore({
      frameScheduler: frames.scheduler,
    });
    let state = createEmptySessionCutoverState();
    const sessionId = 'remote:peer:s1';
    // 与后端 unit separator 合成格式一致：localremote
    const authorityA = 'localremote-A';
    const authorityB = 'localremote-B';

    // generation A：高 lastSeq 已提交
    expect(shouldAcceptTerminalCutover(state, 50, undefined, authorityA)).toBe(true);
    applyTerminalBaselineCutover(store, sessionId, 'a', 50, [], authorityA, true);
    state = commitTerminalCutover(state, 50, undefined, authorityA);
    store.append(sessionId, 'a', 51, authorityA);
    expect(store.getLastSeq(sessionId)).toBe(51);
    expect(state.authorityId).toBe(authorityA);

    // 已绑定 A 时，直接用 B 的 cutover 会被 reject（防 delayed clobber）
    expect(shouldAcceptTerminalCutover(state, 1, undefined, authorityB)).toBe(false);

    // R9 M1：已绑定 A→B 必须 beginAuthorityChangeReplay（needsReplay + 抬 epoch）
    const switched = beginAuthorityChangeReplay(state, authorityB);
    expect(switched).not.toBeNull();
    state = switched!.state;
    expect(state.needsReplay).toBe(true);
    expect(state.authorityId).toBe(authorityB);
    expect(shouldAcceptTerminalCutover(state, 1, switched!.requestEpoch, authorityB)).toBe(
      true,
    );
    // 模拟成功 sessions.replay：baseline 含断线窗口内容 'b0'+'b1'，lastSeq=1
    // held 在 replay 期间收集了首条 B live（seq=2）
    const heldAfterLive: HeldLiveTerminalEvent[] = [{ chunk: 'b2', seq: 2 }];
    applyTerminalBaselineCutover(
      store,
      sessionId,
      'b0b1',
      1,
      heldAfterLive,
      authorityB,
      false,
    );
    state = commitTerminalCutover(state, 1, switched!.requestEpoch, authorityB);
    expect(store.getBuffer(sessionId)).toBe('b0b1b2');
    expect(store.getLastSeq(sessionId)).toBe(2);
    expect(state.authorityId).toBe(authorityB);
    expect(state.needsReplay).toBe(false);

    // 同 authority 下重复 seq 不双写
    store.append(sessionId, 'x', 2, authorityB);
    expect(store.getBuffer(sessionId)).toBe('b0b1b2');
    expect(store.getLastSeq(sessionId)).toBe(2);

    // 新 live seq=3 追加
    store.append(sessionId, 'b3', 3, authorityB);
    expect(store.getBuffer(sessionId)).toBe('b0b1b2b3');
    expect(store.getLastSeq(sessionId)).toBe(3);
  });

  test('shouldCollectHeldLive only during baseline window or replay', () => {
    const idle = createEmptySessionCutoverState();
    expect(shouldCollectHeldLiveTerminalEvent(idle, false)).toBe(true);
    expect(shouldCollectHeldLiveTerminalEvent(idle, true)).toBe(false);

    let replaying = createEmptySessionCutoverState();
    replaying = beginHeldOverflowReplay(replaying, 10).state;
    expect(shouldCollectHeldLiveTerminalEvent(replaying, true)).toBe(true);

    let inFlight = createEmptySessionCutoverState();
    inFlight = setTerminalCutoverReplayInFlight(inFlight, true);
    expect(shouldCollectHeldLiveTerminalEvent(inFlight, true)).toBe(true);
  });

  test('steady-state append does not require held growth beyond 256 without replay', () => {
    // Provider 决策合同：baselineSettled 且 !needsReplay && !replayInFlight 时
    // 不得把 live 写入 held；因此 >256 稳态 chunk 不会触发 overflow re-baseline。
    const frames = createCollectingFrameScheduler();
    const store = createWorkbenchTerminalBufferStore({
      frameScheduler: frames.scheduler,
    });
    let state = createEmptySessionCutoverState();
    state = commitTerminalCutover(state, 0, undefined, 'owner-a');
    expect(shouldCollectHeldLiveTerminalEvent(state, true)).toBe(false);

    const total = MAX_HELD_LIVE_TERMINAL_EVENTS + 20;
    for (let i = 1; i <= total; i += 1) {
      store.append('s1', `c${i}`, i, 'owner-a');
    }
    expect(store.getLastSeq('s1')).toBe(total);
    // 无 needsReplay：不需要 re-baseline
    expect(state.needsReplay).toBe(false);
    expect(state.cutoverEpoch).toBe(0);
  });
});
