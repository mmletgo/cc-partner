import { describe, expect, test } from 'vitest';
import { planTerminalBufferWrite } from '@/pages/Workbench/terminalReplay';
import {
  appendWorkbenchTerminalOutput,
  applyTerminalBaselineCutover,
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
    let state = createEmptySessionCutoverState();
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

    let state = beginHeldOverflowReplay(empty, 100).state;
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

    // owner B 以 lastSeq=1 到来：同 sessionId 但 authority 变更必须 accept
    expect(shouldAcceptTerminalCutover(state, 1, undefined, 'owner-b')).toBe(true);
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
    state = commitTerminalCutover(state, 1, undefined, 'owner-b');
    expect(store.getBuffer('s1')).toBe('B-BASE');
    expect(store.getLastSeq('s1')).toBe(1);
    expect(state.authorityId).toBe('owner-b');
    expect(state.committedBaselineLastSeq).toBe(1);

    // 后续 live 从新 authority 的 seq=2 起可写
    store.append('s1', 'b-live', 2, 'owner-b');
    expect(store.getBuffer('s1')).toBe('B-BASEb-live');
    expect(store.getLastSeq('s1')).toBe(2);
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
