/**
 * 速记本多页面 autosave 队列状态机。
 *
 * Business Logic（为什么需要这个模块）:
 *   用户在多页速记本间编辑时，内容必须 debounce 后可靠落库；
 *   路由卸载或 GUI 关闭时仍能 await 全部 pending，不依赖 React effect cleanup。
 *
 * Code Logic（这个模块做什么）:
 *   工厂 createScratchpadAutosaveQueue(save, {delayMs})：按 pageId 维护
 *   {pendingVersion,savedVersion,content,inFlight,error,timer}；
 *   schedule 合并同页输入；flushPage 在 savedVersion < pendingVersion 时循环保存；
 *   同一页仅一个 in-flight；失败保留 pending/error，成功最新版本后清 error。
 */

/** 默认 debounce 间隔（毫秒），与设计/现网 500ms 一致。 */
export const SCRATCHPAD_AUTOSAVE_DELAY_MS = 500;

/**
 * 单页 autosave 对外快照。
 *
 * Business Logic（为什么需要这个类型）:
 *   Scratchpad UI 需要展示 saving/error，且不得被旧页保存回写覆盖。
 *
 * Code Logic（字段说明）:
 *   pendingVersion/savedVersion 比较判断是否仍有未落库内容；error 仅在失败后保留。
 */
export interface ScratchpadPageAutosaveSnapshot {
  pendingVersion: number;
  savedVersion: number;
  content: string;
  inFlight: boolean;
  error: string | null;
}

/**
 * 队列整体快照。
 *
 * Business Logic（为什么需要这个类型）:
 *   Provider 订阅者需要按 pageId 读取状态，并判断全局是否仍有 pending。
 *
 * Code Logic（字段说明）:
 *   pages 为 pageId → 页快照的浅拷贝。
 */
export interface ScratchpadAutosaveSnapshot {
  pages: Readonly<Record<string, ScratchpadPageAutosaveSnapshot>>;
}

/**
 * 多页面 autosave 队列合同。
 *
 * Business Logic（为什么需要这个接口）:
 *   Scratchpad 页与 AppShell 关闭路径共用同一队列，生命周期长于路由。
 *
 * Code Logic（方法说明）:
 *   schedule 只排队；flushPage/flushAll 强制落库；subscribe 通知快照变更。
 */
export interface ScratchpadAutosaveQueue {
  schedule(pageId: string, content: string): void;
  flushPage(pageId: string): Promise<void>;
  flushAll(): Promise<void>;
  getSnapshot(): ScratchpadAutosaveSnapshot;
  subscribe(listener: () => void): () => void;
}

/**
 * 可注入的保存函数。
 *
 * Business Logic（为什么需要这个类型）:
 *   生产走 scratchpadApi，单测注入 fake 以验证状态机。
 *
 * Code Logic（签名说明）:
 *   接收 pageId 与最新 content；成功 resolve，失败 reject。
 */
export type ScratchpadAutosaveSaveFn = (pageId: string, content: string) => Promise<void>;

/**
 * 队列配置。
 *
 * Business Logic（为什么需要这个类型）:
 *   debounce 间隔需可测可配，默认 500ms。
 *
 * Code Logic（字段说明）:
 *   delayMs 为 schedule 后触发 flush 的等待时间。
 */
export interface ScratchpadAutosaveQueueOptions {
  delayMs?: number;
}

/**
 * 内部页状态（含 timer，不暴露给 snapshot）。
 *
 * Business Logic（为什么需要这个类型）:
 *   状态机需要版本号、in-flight Promise 与 debounce timer 协同。
 *
 * Code Logic（字段说明）:
 *   timer 为 setTimeout 句柄；inFlight 为当前保存 Promise。
 */
interface PageAutosaveState {
  pendingVersion: number;
  savedVersion: number;
  content: string;
  inFlight: Promise<void> | null;
  error: string | null;
  timer: ReturnType<typeof setTimeout> | null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   失败信息需要稳定字符串，避免 UI 出现 [object Object]。
 *
 * Code Logic（这个函数做什么）:
 *   Error 取 message；其它值 String()。
 */
function toErrorMessage(error: unknown): string {
  if (error instanceof Error && error.message) return error.message;
  return String(error);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   flushAll 需要把多页失败聚合成一次 reject。
 *
 * Code Logic（这个函数做什么）:
 *   构造 AggregateError 并带可读 message。
 */
function aggregateFlushErrors(errors: unknown[]): AggregateError {
  const normalized = errors.map((error) =>
    error instanceof Error ? error : new Error(String(error)),
  );
  const message =
    normalized.length === 1
      ? (normalized[0]?.message ?? 'Scratchpad autosave flush failed')
      : normalized.map((error) => error.message).join('; ');
  return new AggregateError(normalized, message);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   AppShell 需要常驻队列；单测需要独立实例与可注入 save。
 *
 * Code Logic（这个函数做什么）:
 *   创建按 pageId 隔离的 debounce + versioned flush 状态机。
 */
export function createScratchpadAutosaveQueue(
  save: ScratchpadAutosaveSaveFn,
  options: ScratchpadAutosaveQueueOptions = {},
): ScratchpadAutosaveQueue {
  const delayMs = options.delayMs ?? SCRATCHPAD_AUTOSAVE_DELAY_MS;
  const pages = new Map<string, PageAutosaveState>();
  const listeners = new Set<() => void>();
  let cachedSnapshot: ScratchpadAutosaveSnapshot = { pages: {} };
  let snapshotDirty = true;

  /**
   * Business Logic（为什么需要这个函数）:
   *   useSyncExternalStore 要求 getSnapshot 在数据未变时返回同一引用。
   *
   * Code Logic（这个函数做什么）:
   *   在 pages 状态变化后标记 dirty，首次读取时重建只读快照并缓存。
   */
  function rebuildSnapshotIfNeeded(): ScratchpadAutosaveSnapshot {
    if (!snapshotDirty) return cachedSnapshot;
    const snapshotPages: Record<string, ScratchpadPageAutosaveSnapshot> = {};
    for (const [pageId, state] of pages) {
      snapshotPages[pageId] = {
        pendingVersion: state.pendingVersion,
        savedVersion: state.savedVersion,
        content: state.content,
        inFlight: state.inFlight !== null,
        error: state.error,
      };
    }
    cachedSnapshot = { pages: snapshotPages };
    snapshotDirty = false;
    return cachedSnapshot;
  }

  /**
   * Business Logic（为什么需要这个函数）:
   *   UI 与测试通过 subscribe 感知 saving/error 变化。
   *
   * Code Logic（这个函数做什么）:
   *   标记快照 dirty 后同步调用全部 listener；listener 抛错不影响其它订阅者。
   */
  function emit(): void {
    snapshotDirty = true;
    for (const listener of listeners) {
      try {
        listener();
      } catch {
        // 订阅者异常不得破坏队列
      }
    }
  }

  /**
   * Business Logic（为什么需要这个函数）:
   *   schedule/flush 都需要拿到可变页状态。
   *
   * Code Logic（这个函数做什么）:
   *   不存在则初始化空状态并写入 Map。
   */
  function ensurePage(pageId: string): PageAutosaveState {
    let state = pages.get(pageId);
    if (!state) {
      state = {
        pendingVersion: 0,
        savedVersion: 0,
        content: '',
        inFlight: null,
        error: null,
        timer: null,
      };
      pages.set(pageId, state);
    }
    return state;
  }

  /**
   * Business Logic（为什么需要这个函数）:
   *   flush 前必须取消 debounce，避免重复保存。
   *
   * Code Logic（这个函数做什么）:
   *   clearTimeout 并清空 timer 字段。
   */
  function clearTimer(state: PageAutosaveState): void {
    if (state.timer !== null) {
      clearTimeout(state.timer);
      state.timer = null;
    }
  }

  /**
   * Business Logic（为什么需要这个函数）:
   *   实际落库循环：同页仅一个 in-flight，保存期间的新编辑在完成后继续 flush。
   *
   * Code Logic（这个函数做什么）:
   *   若已有 inFlight 则 await 后递归；否则 while pending>saved 调用 save；
   *   成功最新版本后清 error；失败写 error 并停止循环。
   */
  async function runFlush(pageId: string): Promise<void> {
    const state = ensurePage(pageId);
    clearTimer(state);

    // 同页 single-flight：先等待既有 inFlight，再决定是否续 flush
    const existing = state.inFlight;
    if (existing) {
      try {
        await existing;
      } catch {
        // 前一次失败后仍可能有更高 pending，下面继续检查
      }
      if (state.savedVersion >= state.pendingVersion) {
        return;
      }
      // 既有链结束后可能已有其它调用方启动了新 inFlight，递归汇合
      return runFlush(pageId);
    }

    if (state.savedVersion >= state.pendingVersion) {
      return;
    }

    const work = (async () => {
      while (state.savedVersion < state.pendingVersion) {
        const version = state.pendingVersion;
        const content = state.content;
        try {
          await save(pageId, content);
          // 保存开始时的 version 已成功；若期间 schedule 了更高版本，while 继续。
          if (version > state.savedVersion) {
            state.savedVersion = version;
          }
          if (state.savedVersion >= state.pendingVersion) {
            state.error = null;
          }
        } catch (error) {
          state.error = toErrorMessage(error);
          emit();
          throw error;
        }
      }
    })();

    state.inFlight = work;
    emit();
    try {
      await work;
    } finally {
      if (state.inFlight === work) {
        state.inFlight = null;
      }
      emit();
    }
  }

  return {
    /**
     * Business Logic（为什么需要这个方法）:
     *   输入时只排队最新正文，避免每次按键打后端。
     *
     * Code Logic（这个方法做什么）:
     *   提升 pendingVersion、覆盖 content、重置 debounce timer。
     */
    schedule(pageId: string, content: string): void {
      const state = ensurePage(pageId);
      state.pendingVersion += 1;
      state.content = content;
      clearTimer(state);
      state.timer = setTimeout(() => {
        state.timer = null;
        void runFlush(pageId).catch(() => undefined);
      }, delayMs);
      emit();
    },

    /**
     * Business Logic（为什么需要这个方法）:
     *   切页/同步/卸载前必须立即落库当前页，不能再等 debounce。
     *
     * Code Logic（这个方法做什么）:
     *   取消 timer 并 await runFlush。
     */
    async flushPage(pageId: string): Promise<void> {
      const state = pages.get(pageId);
      if (!state) return;
      clearTimer(state);
      if (state.savedVersion >= state.pendingVersion && !state.inFlight) {
        return;
      }
      await runFlush(pageId);
    },

    /**
     * Business Logic（为什么需要这个方法）:
     *   路由卸载与 GUI 关闭需要一次 flush 全部页面。
     *
     * Code Logic（这个方法做什么）:
     *   并行 flush 所有页；聚合失败为 AggregateError。
     */
    async flushAll(): Promise<void> {
      const pageIds = Array.from(pages.keys());
      const results = await Promise.allSettled(
        pageIds.map((pageId) => {
          const state = pages.get(pageId);
          if (!state) return Promise.resolve();
          clearTimer(state);
          if (state.savedVersion >= state.pendingVersion && !state.inFlight) {
            return Promise.resolve();
          }
          return runFlush(pageId);
        }),
      );
      const failures: unknown[] = [];
      for (const result of results) {
        if (result.status === 'rejected') {
          failures.push(result.reason);
        }
      }
      if (failures.length > 0) {
        throw aggregateFlushErrors(failures);
      }
    },

    /**
     * Business Logic（为什么需要这个方法）:
     *   UI 需要同步读取各页 saving/error。
     *
     * Code Logic（这个方法做什么）:
     *   返回 pages 的只读浅拷贝快照（不含 timer）。
     */
    getSnapshot(): ScratchpadAutosaveSnapshot {
      return rebuildSnapshotIfNeeded();
    },

    /**
     * Business Logic（为什么需要这个方法）:
     *   React 订阅需要在状态变化时重渲染。
     *
     * Code Logic（这个方法做什么）:
     *   登记 listener，返回 unsubscribe。
     */
    subscribe(listener: () => void): () => void {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
  };
}
