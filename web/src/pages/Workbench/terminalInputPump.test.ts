import { describe, expect, test, vi } from "vitest";
import { createTerminalInputPump } from "./terminalInputPump";

function deferred(): { promise: Promise<void>; resolve: () => void; reject: (error: Error) => void } {
  let resolve!: () => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<void>((ok, fail) => {
    resolve = ok;
    reject = fail;
  });
  return { promise, resolve, reject };
}

describe("terminalInputPump", () => {
  test("sends the leading batch immediately and coalesces only while in flight", async () => {
    const first = deferred();
    const writes: Array<[string, string]> = [];
    const write = vi.fn((sessionId: string, data: string) => {
      writes.push([sessionId, data]);
      return writes.length === 1 ? first.promise : Promise.resolve();
    });
    const pump = createTerminalInputPump({ write });

    pump.enqueue("s1", "a");
    expect(writes).toEqual([["s1", "a"]]);
    pump.enqueue("s1", "b");
    pump.enqueue("s1", "c");
    expect(writes).toHaveLength(1);
    first.resolve();
    await pump.whenIdle("s1");
    expect(writes).toEqual([["s1", "a"], ["s1", "bc"]]);
  });

  test("stops lane after failure and never sends pending suffix or replays the failed batch", async () => {
    const calls: Array<[string, string]> = [];
    const errors: string[] = [];
    const write = vi.fn(async (sessionId: string, data: string) => {
      calls.push([sessionId, data]);
      if (data === "a") throw new Error("uncertain");
    });
    const pump = createTerminalInputPump({
      write,
      onWriteError: (sessionId) => {
        errors.push(sessionId);
      },
    });
    pump.enqueue("s1", "a");
    // 失败前缀后的 Enter/后缀不得发送
    pump.enqueue("s1", "b\n");
    pump.enqueue("s2", "x");
    await Promise.all([pump.whenIdle("s1"), pump.whenIdle("s2")]);
    expect(calls.filter(([id]) => id === "s1")).toEqual([["s1", "a"]]);
    expect(calls.filter(([, data]) => data === "a")).toHaveLength(1);
    expect(calls.some(([, data]) => data.includes("b"))).toBe(false);
    expect(calls.filter(([id]) => id === "s2")).toEqual([["s2", "x"]]);
    expect(errors).toEqual(["s1"]);
    // blocked lane 继续 enqueue 必须 no-op
    pump.enqueue("s1", "more");
    await pump.whenIdle("s1");
    expect(calls.filter(([id]) => id === "s1")).toEqual([["s1", "a"]]);
  });

  test("dispose drops pending bytes without cancelling or replaying the in-flight write", async () => {
    const first = deferred();
    const write = vi.fn(() => first.promise);
    const pump = createTerminalInputPump({ write });
    pump.enqueue("s1", "a");
    pump.enqueue("s1", "b");
    pump.disposeSession("s1");
    first.resolve();
    await Promise.resolve();
    await Promise.resolve();
    expect(write).toHaveBeenCalledTimes(1);
  });

  test("dispose during in-flight then re-enqueue keeps max concurrency 1 and ordered batches", async () => {
    const first = deferred();
    let inFlight = 0;
    let maxInFlight = 0;
    const writes: Array<[string, string]> = [];
    const write = vi.fn(async (sessionId: string, data: string) => {
      inFlight += 1;
      maxInFlight = Math.max(maxInFlight, inFlight);
      writes.push([sessionId, data]);
      try {
        if (writes.length === 1) {
          await first.promise;
        }
      } finally {
        inFlight -= 1;
      }
    });
    const pump = createTerminalInputPump({ write });

    pump.enqueue("s1", "a");
    expect(writes).toEqual([["s1", "a"]]);
    pump.disposeSession("s1");
    // dispose 后、旧 write 仍 pending 时 re-enqueue 必须排队，不得并发第二 write。
    pump.enqueue("s1", "b");
    pump.enqueue("s1", "c");
    expect(write).toHaveBeenCalledTimes(1);
    expect(maxInFlight).toBe(1);

    first.resolve();
    await pump.whenIdle("s1");
    expect(maxInFlight).toBe(1);
    expect(writes).toEqual([["s1", "a"], ["s1", "bc"]]);
  });

  test("disposeSession after failure unblocks a fresh generation of input", async () => {
    const write = vi.fn(async (_sessionId: string, data: string) => {
      if (data === "bad") throw new Error("fail");
    });
    const pump = createTerminalInputPump({ write });
    pump.enqueue("s1", "bad");
    await pump.whenIdle("s1");
    expect(write).toHaveBeenCalledTimes(1);
    pump.disposeSession("s1");
    pump.enqueue("s1", "ok");
    await pump.whenIdle("s1");
    expect(write).toHaveBeenCalledTimes(2);
    expect(write.mock.calls[1]?.[1]).toBe("ok");
  });
});
