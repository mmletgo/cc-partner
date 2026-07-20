/**
 * Workbench 终端 per-session 有序输入泵。
 *
 * Business Logic（为什么需要这个模块）:
 *   xterm onData 在快速输入时会产生大量小字节片段；若每个片段直接并发 writeInput，
 *   会与 owner session mutex 竞态、放大 control HTTP 次数，且在失败时存在“是否应重放”
 *   的歧义。本泵按 session 保持 leading-edge FIFO：首批立即发送、in-flight 期间只拼接
 *   pending，settle 后整批发送；失败/未知结果后封锁 lane、清空 pending，永不自动重放。
 *
 * Code Logic（这个模块做什么）:
 *   维护 session → InputLane（generation/pending/running/blocked/idleWaiters）；enqueue 拼接并
 *   触发 drain；每个 lane 最多一个 in-flight write；write 失败立即 stop-on-failure；
 *   disposeSession / recoverSession 丢弃 pending、解除 blocked 并抬高 generation，但保留
 *   in-flight tombstone 直到 write settle；blocked 状态持续到 disposeSession 或 recoverSession。
 */

export interface TerminalInputPumpOptions {
  write: (sessionId: string, data: string) => Promise<unknown>;
  /**
   * Business Logic（为什么需要这个回调）:
   *   写失败后 controller 需要向用户展示断连/错误，并阻止在权威状态确认前继续输入。
   *
   * Code Logic（这个回调做什么）:
   *   drain 捕获到 write 异常后调用一次；不传 error 正文以外的终端 body。
   */
  onWriteError?: (sessionId: string, error: unknown) => void;
}

export interface TerminalInputPump {
  enqueue: (sessionId: string, data: string) => void;
  disposeSession: (sessionId: string) => void;
  /**
   * Business Logic（为什么需要这个函数）:
   *   write 失败后 lane 永久 blocked，enqueue 会静默 no-op；sessions.list 等权威刷新成功后
   *   需要重新开放输入，但绝不能自动重放失败批次（未知是否已执行）。
   *
   * Code Logic（这个函数做什么）:
   *   与 disposeSession 相同：generation++、清空 pending、解除 blocked、settle idle；
   *   不重放任何已失败或 pending 字节。
   */
  recoverSession: (sessionId: string) => void;
  /**
   * Business Logic（为什么需要这个函数）:
   *   UI / controller 需要查询 lane 是否仍处于 stop-on-write-failure 封锁，以便禁用 xterm 输入
   *   并避免“键盘黑洞”。
   *
   * Code Logic（这个函数做什么）:
   *   返回该 session 的 lane.blocked；无 lane 时为 false。
   */
  isBlocked: (sessionId: string) => boolean;
  dispose: () => void;
  whenIdle: (sessionId: string) => Promise<void>;
}

interface InputLane {
  generation: number;
  pending: string;
  running: boolean;
  /** 任一批次失败后封锁，直到 disposeSession / recoverSession 清掉，禁止继续 drain pending */
  blocked: boolean;
  idleWaiters: Set<() => void>;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   前端需要把终端输入从“每键一次 invoke”改为 per-session 有序批处理，
 *   同时保留字节顺序与“失败不重放、失败后不续发后缀”合同。
 *
 * Code Logic（这个函数做什么）:
 *   创建跨 session 的输入泵；返回 enqueue/disposeSession/recoverSession/isBlocked/dispose/whenIdle。
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
   *   dispose 后调用方不应再被 whenIdle 挂起，即使 in-flight write 仍在 settle。
   *
   * Code Logic（这个函数做什么）:
   *   无条件 resolve 并清空 idleWaiters。
   */
  const forceSettleIdle = (lane: InputLane): void => {
    lane.idleWaiters.forEach((resolve) => resolve());
    lane.idleWaiters.clear();
  };

  /**
   * Business Logic（为什么需要这个函数）:
   *   每个 session 必须串行写出；任一批次失败/未知后必须停泵，禁止把 Enter 等后缀
   *   发到可能已执行/可能丢失的命令前缀后面。
   *
   * Code Logic（这个函数做什么）:
   *   循环取出完整 pending 作为 batch 调用 options.write；catch 时若 disposed 或
   *   generation 已变则 break 不碰 pending/blocked/onWriteError，否则清空 pending、
   *   封锁 lane 并回调 onWriteError；generation 变化时保留 barrier 至本 drain 结束。
   */
  const drain = async (sessionId: string, lane: InputLane, generation: number): Promise<void> => {
    lane.running = true;
    while (
      !disposed &&
      !lane.blocked &&
      lane.generation === generation &&
      lane.pending.length > 0
    ) {
      const batch = lane.pending;
      lane.pending = '';
      try {
        await options.write(sessionId, batch);
      } catch (error) {
        // 旧 generation / 已 dispose 的 in-flight reject 不得清掉新 generation 的 pending，
        // 也不得 block 或上报错误；由下方 generation-mismatch 分支启动新 drain。
        if (disposed || lane.generation !== generation) {
          break;
        }
        // 当前 generation 失败批次永不自动重放；立刻清空尚未发送的后缀并封锁 lane。
        lane.pending = '';
        lane.blocked = true;
        options.onWriteError?.(sessionId, error);
        break;
      }
    }

    if (lane.generation !== generation) {
      // dispose 期间本 drain 作为 in-flight barrier；settle 后若有新 pending 则启动恰好一次新 drain。
      if (!disposed && !lane.blocked && lane.pending.length > 0) {
        void drain(sessionId, lane, lane.generation);
        return;
      }
      lane.running = false;
      settleIdle(lane);
      if (lane.pending.length === 0 && lane.idleWaiters.size === 0 && !lane.blocked) {
        lanes.delete(sessionId);
      }
      return;
    }

    lane.running = false;
    settleIdle(lane);
    if (lane.pending.length === 0 && lane.idleWaiters.size === 0 && !lane.blocked) {
      lanes.delete(sessionId);
    }
  };

  /**
   * Business Logic（为什么需要这个函数）:
   *   session close / offline / unmount / 写失败恢复前必须丢弃尚未提交的输入，且不得重发 in-flight。
   *   disposeSession 与 recoverSession 共享此实现：都抬 generation、清 pending、解 blocked，
   *   永不自动重放失败批次。
   *
   * Code Logic（这个函数做什么）:
   *   generation++、清空 pending、解除 blocked、强制 settle idle waiters；若无 in-flight 则删除 lane，
   *   若仍有 in-flight 则保留 tombstone（running 保持 true）直到旧 drain settle。
   */
  const resetSessionLane = (sessionId: string): void => {
    const lane = lanes.get(sessionId);
    if (!lane) return;
    lane.generation += 1;
    lane.pending = '';
    lane.blocked = false;
    forceSettleIdle(lane);
    if (!lane.running) {
      lanes.delete(sessionId);
    }
  };

  return {
    /**
     * Business Logic（为什么需要这个函数）:
     *   xterm onData 到达时需要尽快把字节交给 owner，同时遵守 per-session max in-flight=1。
     *
     * Code Logic（这个函数做什么）:
     *   空串、全局 dispose、blocked lane 后 no-op；否则拼接 pending，若无 in-flight 则启动 drain。
     */
    enqueue(sessionId, data) {
      if (disposed || data.length === 0) return;
      const lane = lanes.get(sessionId) ?? {
        generation: 0,
        pending: '',
        running: false,
        blocked: false,
        idleWaiters: new Set<() => void>(),
      };
      if (lane.blocked) return;
      lanes.set(sessionId, lane);
      lane.pending += data;
      if (!lane.running) void drain(sessionId, lane, lane.generation);
    },
    /**
     * Business Logic（为什么需要这个函数）:
     *   session close / offline / unmount 时必须丢弃尚未提交的输入，并解除可能残留的 blocked。
     *
     * Code Logic（这个函数做什么）:
     *   委托 resetSessionLane：generation++、清 pending、解 blocked；保留 in-flight tombstone。
     */
    disposeSession: resetSessionLane,
    /**
     * Business Logic（为什么需要这个函数）:
     *   write 失败后 lane 永久 blocked，enqueue 静默 no-op 会造成键盘黑洞；权威 sessions.list
     *   刷新成功后需要重新开放输入，但绝不能自动重放失败批次。
     *
     * Code Logic（这个函数做什么）:
     *   与 disposeSession 共享 resetSessionLane：抬 generation、清 pending/blocked，不重放任何字节。
     */
    recoverSession: resetSessionLane,
    /**
     * Business Logic（为什么需要这个函数）:
     *   UI / controller 需要知道 session 是否因写失败被封锁，以便禁用 xterm 输入并展示错误。
     *
     * Code Logic（这个函数做什么）:
     *   读取 lane.blocked；无 lane 时返回 false。
     */
    isBlocked(sessionId) {
      return lanes.get(sessionId)?.blocked === true;
    },
    /**
     * Business Logic（为什么需要这个函数）:
     *   controller unmount 时必须清理全部 lane，避免卸载后仍继续写。
     *
     * Code Logic（这个函数做什么）:
     *   标记全局 disposed，并对所有 session 调用 disposeSession。
     */
    dispose() {
      disposed = true;
      for (const sessionId of [...lanes.keys()]) resetSessionLane(sessionId);
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
