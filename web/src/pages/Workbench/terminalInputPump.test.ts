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
    pump.enqueue("s1", "\u007f");
    expect(writes).toHaveLength(1);
    first.resolve();
    await pump.whenIdle("s1");
    expect(writes).toEqual([["s1", "a"], ["s1", "b\u007f"]]);
  });

  test("isolates sessions and never replays a failed batch", async () => {
    const calls: Array<[string, string]> = [];
    const write = vi.fn(async (sessionId: string, data: string) => {
      calls.push([sessionId, data]);
      if (data === "a") throw new Error("uncertain");
    });
    const pump = createTerminalInputPump({ write });
    pump.enqueue("s1", "a");
    pump.enqueue("s1", "b");
    pump.enqueue("s2", "x");
    await Promise.all([pump.whenIdle("s1"), pump.whenIdle("s2")]);
    expect(calls.filter(([id]) => id === "s1")).toEqual([["s1", "a"], ["s1", "b"]]);
    expect(calls.filter(([, data]) => data === "a")).toHaveLength(1);
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
    expect(write).toHaveBeenCalledTimes(1);
  });
});
