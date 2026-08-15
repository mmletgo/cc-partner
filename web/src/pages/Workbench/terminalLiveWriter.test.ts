import { describe, expect, test, vi } from 'vitest';
import type {
  TerminalBufferCursor,
  TerminalBufferDelta,
  TerminalBufferResetEvent,
  TerminalBufferSnapshot,
} from '@/hooks/workbenchTerminalBuffer';
import {
  createTerminalLiveWriter,
  type TerminalLiveSource,
  type TerminalLiveWriterTarget,
} from './terminalLiveWriter';

/**
 * Business Logic（为什么需要这个类）:
 *   live writer 测试需要可控的异步 write 完成时机，不能依赖真实 xterm。
 *
 * Code Logic（这个类做什么）:
 *   记录 write/clear，并把 callback 按索引保存供 completeWrite 触发。
 */
class FakeTerminalWriter implements TerminalLiveWriterTarget {
  writes: string[] = [];
  clearCalls = 0;
  private readonly callbacks: Array<(() => void) | undefined> = [];

  /**
   * Business Logic（为什么需要这个函数）:
   *   generation 变化后 writer 必须清空旧屏幕再 replay。
   *
   * Code Logic（这个函数做什么）:
   *   递增 clearCalls。
   */
  clear(): void {
    this.clearCalls += 1;
  }

  /**
   * Business Logic（为什么需要这个函数）:
   *   writer 用 write callback 表达 xterm 解析完成，测试需要手动推进。
   *
   * Code Logic（这个函数做什么）:
   *   记录 data 并保存 callback，不自动完成。
   */
  write(data: string, callback?: () => void): void {
    this.writes.push(data);
    this.callbacks.push(callback);
  }

  /**
   * Business Logic（为什么需要这个函数）:
   *   测试要按写入序号模拟 xterm 完成顺序。
   *
   * Code Logic（这个函数做什么）:
   *   触发对应 index 的 callback（若存在）。
   */
  completeWrite(index: number): void {
    this.callbacks[index]?.();
  }
}

/**
 * Business Logic（为什么需要这个类）:
 *   writer 握手依赖 source 的 snapshot 与 live/reset 事件顺序。
 *
 * Code Logic（这个类做什么）:
 *   内存维护 buffer/cursor，支持 emit/replace 与订阅。
 */
class FakeTerminalLiveSource implements TerminalLiveSource {
  private buffer: string;
  private cursor: TerminalBufferCursor;
  private readonly liveListeners = new Set<(delta: TerminalBufferDelta) => void>();
  private readonly resetListeners = new Set<(event: TerminalBufferResetEvent) => void>();

  constructor(buffer: string, cursor: TerminalBufferCursor) {
    this.buffer = buffer;
    this.cursor = cursor;
  }

  /**
   * Business Logic（为什么需要这个函数）:
   *   writer 在 subscribe 后立即读取 snapshot 做首屏 replay。
   *
   * Code Logic（这个函数做什么）:
   *   返回当前 buffer/cursor 快照。
   */
  getSnapshot(sessionId: string): TerminalBufferSnapshot {
    void sessionId;
    return {
      buffer: this.buffer,
      cursor: this.cursor,
      revision: 0,
    };
  }

  /**
   * Business Logic（为什么需要这个函数）:
   *   live append 必须同步推送给 writer。
   *
   * Code Logic（这个函数做什么）:
   *   注册 listener 并返回 unsubscribe。
   */
  subscribeLive(
    sessionId: string,
    listener: (delta: TerminalBufferDelta) => void,
  ): () => void {
    void sessionId;
    this.liveListeners.add(listener);
    return () => {
      this.liveListeners.delete(listener);
    };
  }

  /**
   * Business Logic（为什么需要这个函数）:
   *   resync/remove 需要通知 writer 作废旧队列。
   *
   * Code Logic（这个函数做什么）:
   *   注册 reset listener 并返回 unsubscribe。
   */
  subscribeReset(
    sessionId: string,
    listener: (event: TerminalBufferResetEvent) => void,
  ): () => void {
    void sessionId;
    this.resetListeners.add(listener);
    return () => {
      this.resetListeners.delete(listener);
    };
  }

  /**
   * Business Logic（为什么需要这个函数）:
   *   测试模拟 store.append 发布的 live delta。
   *
   * Code Logic（这个函数做什么）:
   *   同步调用全部 live listeners。
   */
  emit(delta: TerminalBufferDelta): void {
    this.liveListeners.forEach((listener) => listener(delta));
  }

  /**
   * Business Logic（为什么需要这个函数）:
   *   测试模拟 store.reset 后的 generation 变化。
   *
   * Code Logic（这个函数做什么）:
   *   更新 buffer/cursor 并同步通知 reset listeners。
   */
  replace(
    buffer: string,
    cursor: TerminalBufferCursor,
    reason: TerminalBufferResetEvent['reason'] = 'snapshotReplace',
  ): void {
    this.buffer = buffer;
    this.cursor = cursor;
    this.resetListeners.forEach((listener) => listener({ sessionId: 's1', reason }));
  }
}

describe('terminalLiveWriter', () => {
  test('notifies only after the latest reset snapshot write completes', () => {
    const terminal = new FakeTerminalWriter();
    const source = new FakeTerminalLiveSource('old', { generation: 0, appendId: 1 });
    const onSnapshotComplete = vi.fn();
    createTerminalLiveWriter({
      terminal,
      source,
      sessionId: 's1',
      onSnapshotComplete,
    });

    source.replace('hydrated', { generation: 1, appendId: 0 }, 'snapshotReplace');
    terminal.completeWrite(0);
    expect(onSnapshotComplete).not.toHaveBeenCalled();
    terminal.completeWrite(1);
    expect(onSnapshotComplete).toHaveBeenCalledTimes(1);
  });

  test('replays snapshot once then drains only newer deltas in exact order', () => {
    const terminal = new FakeTerminalWriter();
    const source = new FakeTerminalLiveSource('history', { generation: 0, appendId: 2 });
    const writer = createTerminalLiveWriter({ terminal, source, sessionId: 's1' });
    source.emit({ sessionId: 's1', generation: 0, appendId: 2, chunk: 'duplicate' });
    source.emit({ sessionId: 's1', generation: 0, appendId: 3, chunk: 'a' });
    source.emit({ sessionId: 's1', generation: 0, appendId: 4, chunk: 'b' });
    terminal.completeWrite(0);
    terminal.completeWrite(1);
    expect(terminal.writes).toEqual(['history', 'ab']);
    writer.dispose();
  });

  test('generation change waits for in-flight write before clear+replay', () => {
    const terminal = new FakeTerminalWriter();
    const source = new FakeTerminalLiveSource('old', { generation: 0, appendId: 1 });
    createTerminalLiveWriter({ terminal, source, sessionId: 's1' });
    // snapshot write still in flight when reset arrives
    source.replace('new', { generation: 1, appendId: 0 }, 'authorityChange');
    // clear must not run until old write completes
    expect(terminal.clearCalls).toBe(0);
    expect(terminal.writes).toEqual(['old']);
    terminal.completeWrite(0);
    expect(terminal.clearCalls).toBe(1);
    expect(terminal.writes).toEqual(['old', 'new']);
    source.emit({ sessionId: 's1', generation: 1, appendId: 1, chunk: '!' });
    terminal.completeWrite(1);
    terminal.completeWrite(2);
    expect(terminal.writes).toEqual(['old', 'new', '!']);
  });

  test('preserveScrollback reset keeps history and drains only new-generation deltas', () => {
    const terminal = new FakeTerminalWriter();
    const source = new FakeTerminalLiveSource('history', { generation: 0, appendId: 1 });
    createTerminalLiveWriter({
      terminal,
      source,
      sessionId: 's1',
      resetStrategy: 'preserveScrollback',
    });

    // 初始历史仍在 write 中时发生短快照 reset；移动端不能 clear 已有 scrollback。
    source.replace('short-snapshot', { generation: 1, appendId: 0 });
    source.emit({ sessionId: 's1', generation: 1, appendId: 1, chunk: 'fresh' });
    terminal.completeWrite(0);

    expect(terminal.clearCalls).toBe(0);
    expect(terminal.writes).toEqual(['history', 'fresh']);
    terminal.completeWrite(1);
  });

  test('preserveScrollback still replaces the screen when terminal authority changes', () => {
    const terminal = new FakeTerminalWriter();
    const source = new FakeTerminalLiveSource('owner-a', { generation: 0, appendId: 1 });
    createTerminalLiveWriter({
      terminal,
      source,
      sessionId: 's1',
      resetStrategy: 'preserveScrollback',
    });

    source.replace(
      'owner-b',
      { generation: 1, appendId: 0 },
      'authorityChange',
    );
    terminal.completeWrite(0);

    expect(terminal.clearCalls).toBe(1);
    expect(terminal.writes).toEqual(['owner-a', 'owner-b']);
    terminal.completeWrite(1);
  });

  test('preserveScrollback fully replays an explicit history hydration snapshot', () => {
    const terminal = new FakeTerminalWriter();
    const source = new FakeTerminalLiveSource('last-screen', { generation: 0, appendId: 1 });
    createTerminalLiveWriter({
      terminal,
      source,
      sessionId: 's1',
      resetStrategy: 'preserveScrollback',
    });

    source.replace(
      'captured-history',
      { generation: 1, appendId: 0 },
      'historyHydration',
    );
    terminal.completeWrite(0);

    expect(terminal.clearCalls).toBe(1);
    expect(terminal.writes).toEqual(['last-screen', 'captured-history']);
    terminal.completeWrite(1);
  });

  test('coalesces high-rate deltas into a single next batch while writing', () => {
    const terminal = new FakeTerminalWriter();
    const source = new FakeTerminalLiveSource('seed', { generation: 0, appendId: 0 });
    createTerminalLiveWriter({ terminal, source, sessionId: 's1' });
    // 完成 snapshot write，进入 idle。
    terminal.completeWrite(0);
    expect(terminal.writes).toEqual(['seed']);

    source.emit({ sessionId: 's1', generation: 0, appendId: 1, chunk: 'a' });
    expect(terminal.writes).toEqual(['seed', 'a']);
    // 保持 in-flight，连续大量 delta 必须合并为有界 next-buffer，不得每个 delta 一次 write。
    for (let i = 2; i <= 50; i += 1) {
      source.emit({
        sessionId: 's1',
        generation: 0,
        appendId: i,
        chunk: String(i % 10),
      });
    }
    expect(terminal.writes).toHaveLength(2);
    terminal.completeWrite(1);
    expect(terminal.writes).toHaveLength(3);
    const expectedTail = Array.from({ length: 49 }, (_, idx) => String((idx + 2) % 10)).join('');
    expect(terminal.writes[2]).toBe(expectedTail);
    terminal.completeWrite(2);
    expect(terminal.writes).toHaveLength(3);
  });

  test('overflow pending under stalled write switches to bounded snapshot replay', () => {
    const terminal = new FakeTerminalWriter();
    const source = new FakeTerminalLiveSource('seed', { generation: 0, appendId: 0 });
    createTerminalLiveWriter({
      terminal,
      source,
      sessionId: 's1',
      maxPendingChars: 8,
    });
    terminal.completeWrite(0);
    source.emit({ sessionId: 's1', generation: 0, appendId: 1, chunk: 'aaaa' });
    expect(terminal.writes).toEqual(['seed', 'aaaa']);
    // keep write in-flight and push over the hard cap with many large chunks
    for (let i = 2; i <= 20; i += 1) {
      source.emit({
        sessionId: 's1',
        generation: 0,
        appendId: i,
        chunk: 'xxxxxxxxxx',
      });
    }
    // pending overflow must not schedule unbounded extra writes while stalled
    expect(terminal.writes).toHaveLength(2);
    // advance authoritative snapshot while write is still open
    source.replace('SNAP', { generation: 1, appendId: 0 });
    terminal.completeWrite(1);
    // after stalled write completes: clear + replay latest bounded snapshot
    expect(terminal.clearCalls).toBeGreaterThanOrEqual(1);
    expect(terminal.writes[terminal.writes.length - 1]).toBe('SNAP');
    // no third live pending batch for the overflowed x's
    expect(terminal.writes.filter((w) => w.includes('xxxx')).length).toBeLessThanOrEqual(1);
  });
});
