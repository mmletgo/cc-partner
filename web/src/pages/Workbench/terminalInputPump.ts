/**
 * Workbench 终端 per-session 有序输入泵。
 *
 * Business Logic（为什么需要这个模块）:
 *   xterm onData 在快速输入时会产生大量小字节片段；若每个片段直接并发 writeInput，
 *   会与 owner session mutex 竞态、放大 control HTTP 次数，且在失败时存在“是否应重放”
 *   的歧义。本泵按 session 保持 leading-edge FIFO：首批立即发送、in-flight 期间只拼接
 *   pending，settle 后整批发送；失败批次永不自动重放，只继续尚未发送的后续字节。
 *
 * Code Logic（这个模块做什么）:
 *   维护 session → InputLane（generation/pending/running/idleWaiters）；enqueue 拼接并
 *   触发 drain；每个 lane 最多一个 in-flight write；disposeSession/dispose 丢弃 pending
 *   并抬高 generation，不伪取消 in-flight。
 */

export interface TerminalInputPumpOptions {
  write: (sessionId: string, data: string) => Promise<unknown>;
}

export interface TerminalInputPump {
  enqueue: (sessionId: string, data: string) => void;
  disposeSession: (sessionId: string) => void;
  dispose: () => void;
  whenIdle: (sessionId: string) => Promise<void>;
}

interface InputLane {
  generation: number;
  pending: string;
  running: boolean;
  idleWaiters: Set<() => void>;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   前端需要把终端输入从“每键一次 invoke”改为 per-session 有序批处理，
 *   同时保留字节顺序与“失败不重放”合同。
 *
 * Code Logic（这个函数做什么）:
 *   创建跨 session 的输入泵；返回 enqueue/disposeSession/dispose/whenIdle 接口。
 */
export function createTerminalInputPump(options: TerminalInputPumpOptions): TerminalInputPump {
  const lanes = new Map<string, InputLane>();
  let disposed = false;

  /**
   * Business Logic（为什么需要这个函数）:
   *   whenIdle 调用方需要在 lane 真正空闲时继续断言；drain 结束时要唤醒所有等待者。
   *
   * Code Logic（这个函数做什么）:
   *   仅当 running=false 且 pending 为空时 resolve 并清空 idleWaiters。
   */
  const settleIdle = (lane: InputLane): void => {
    if (lane.running || lane.pending.length > 0) return;
    lane.idleWaiters.forEach((resolve) => resolve());
    lane.idleWaiters.clear();
  };

  /**
   * Business Logic（为什么需要这个函数）:
   *   每个 session 必须串行写出，且 in-flight 期间到达的字节要在 settle 后作为下一批发送。
   *
   * Code Logic（这个函数做什么）:
   *   循环取出完整 pending 作为 batch 调用 options.write；catch 后不重放本批；
   *   generation 变化或 dispose 后停止。
   */
  const drain = async (sessionId: string, lane: InputLane, generation: number): Promise<void> => {
    lane.running = true;
    while (!disposed && lane.generation === generation && lane.pending.length > 0) {
      const batch = lane.pending;
      lane.pending = '';
      try {
        await options.write(sessionId, batch);
      } catch {
        // Mutation 不重放；后续已排队批次仍按原顺序继续。
      }
    }
    if (lane.generation === generation) {
      lane.running = false;
      settleIdle(lane);
      if (lane.pending.length === 0 && lane.idleWaiters.size === 0) lanes.delete(sessionId);
    }
  };

  /**
   * Business Logic（为什么需要这个函数）:
   *   session close / offline / unmount 时必须丢弃尚未提交的输入，且不得重发 in-flight。
   *
   * Code Logic（这个函数做什么）:
   *   generation++、清空 pending、标记非 running、settle idle waiters 并从 map 删除。
   */
  const disposeSession = (sessionId: string): void => {
    const lane = lanes.get(sessionId);
    if (!lane) return;
    lane.generation += 1;
    lane.pending = '';
    lane.running = false;
    settleIdle(lane);
    lanes.delete(sessionId);
  };

  return {
    /**
     * Business Logic（为什么需要这个函数）:
     *   xterm onData 到达时需要尽快把字节交给 owner，同时遵守 per-session max in-flight=1。
     *
     * Code Logic（这个函数做什么）:
     *   空串与全局 dispose 后 no-op；否则拼接 pending，若无 in-flight 则启动 drain。
     */
    enqueue(sessionId, data) {
      if (disposed || data.length === 0) return;
      const lane = lanes.get(sessionId) ?? {
        generation: 0,
        pending: '',
        running: false,
        idleWaiters: new Set<() => void>(),
      };
      lanes.set(sessionId, lane);
      lane.pending += data;
      if (!lane.running) void drain(sessionId, lane, lane.generation);
    },
    disposeSession,
    /**
     * Business Logic（为什么需要这个函数）:
     *   controller unmount 时必须清理全部 lane，避免卸载后仍继续写。
     *
     * Code Logic（这个函数做什么）:
     *   标记全局 disposed，并对所有 session 调用 disposeSession。
     */
    dispose() {
      disposed = true;
      for (const sessionId of [...lanes.keys()]) disposeSession(sessionId);
    },
    /**
     * Business Logic（为什么需要这个函数）:
     *   测试需要等待某个 session 的输入链路完全 idle 后再断言写调用序列。
     *
     * Code Logic（这个函数做什么）:
     *   无 lane 或已 idle 时立即 resolve；否则把 resolve 放入 idleWaiters。
     */
    whenIdle(sessionId) {
      const lane = lanes.get(sessionId);
      if (!lane || (!lane.running && lane.pending.length === 0)) return Promise.resolve();
      return new Promise<void>((resolve) => lane.idleWaiters.add(resolve));
    },
  };
}
