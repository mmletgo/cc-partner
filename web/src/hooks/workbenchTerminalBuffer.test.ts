import { describe, test } from 'vitest';
import { planTerminalBufferWrite } from '@/pages/Workbench/terminalReplay';
import {
  appendWorkbenchTerminalOutput,
  createWorkbenchTerminalBufferStore,
  MAX_WORKBENCH_TERMINAL_BUFFER_CHARS,
  removeWorkbenchTerminalBuffer,
  resetWorkbenchTerminalBuffer,
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
});
