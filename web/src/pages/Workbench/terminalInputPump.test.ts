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

  test("recoverSession unblocks after failure without replaying failed batch or pending", async () => {
    const write = vi.fn(async (_sessionId: string, data: string) => {
      if (data === "bad") throw new Error("fail");
    });
    const pump = createTerminalInputPump({ write });
    pump.enqueue("s1", "bad");
    await pump.whenIdle("s1");
    expect(write).toHaveBeenCalledTimes(1);
    expect(pump.isBlocked("s1")).toBe(true);

    // blocked 期间 enqueue 必须 no-op，且 recover 不得重放这些字节。
    pump.enqueue("s1", "should-not-send");
    await pump.whenIdle("s1");
    expect(write).toHaveBeenCalledTimes(1);

    pump.recoverSession("s1");
    expect(pump.isBlocked("s1")).toBe(false);
    // recover 后不得自动重放 failed batch 或 blocked 期间的 pending。
    await pump.whenIdle("s1");
    expect(write).toHaveBeenCalledTimes(1);

    pump.enqueue("s1", "ok");
    await pump.whenIdle("s1");
    expect(write).toHaveBeenCalledTimes(2);
    expect(write.mock.calls[1]?.[1]).toBe("ok");
  });

  test("stale in-flight write reject after recover preserves and drains new generation pending once", async () => {
    const first = deferred();
    const writes: Array<[string, string]> = [];
    const errors: string[] = [];
    const write = vi.fn((sessionId: string, data: string) => {
      writes.push([sessionId, data]);
      if (writes.length === 1) {
        return first.promise;
      }
      return Promise.resolve();
    });
    const pump = createTerminalInputPump({
      write,
      onWriteError: (sessionId) => {
        errors.push(sessionId);
      },
    });

    pump.enqueue("s1", "old-in-flight");
    expect(writes).toEqual([["s1", "old-in-flight"]]);

    // recover 抬 generation 并清 pending；随后新 generation 入队。
    pump.recoverSession("s1");
    pump.enqueue("s1", "new-gen");
    // 旧 write 仍 in-flight：新 generation 必须排队，不得并发第二 write。
    expect(write).toHaveBeenCalledTimes(1);

    // 旧 generation write reject：不得清 new-gen pending、不得 block、不得 onWriteError。
    first.reject(new Error("stale-fail"));
    await pump.whenIdle("s1");

    expect(errors).toEqual([]);
    expect(pump.isBlocked("s1")).toBe(false);
    expect(writes).toEqual([
      ["s1", "old-in-flight"],
      ["s1", "new-gen"],
    ]);
    // 新 generation 只 drain 一次，且绝不重放旧失败批次。
    expect(write).toHaveBeenCalledTimes(2);
  });
});
