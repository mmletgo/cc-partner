/**
 * App 关闭前 pending write 注册表。
 *
 * Business Logic（为什么需要这个模块）:
 *   速记本等编辑器在 GUI 关闭前必须把未落库内容 flush 到后端；
 *   不能只依赖 React effect cleanup（cleanup 等不了 Promise）。
 *
 * Code Logic（这个模块做什么）:
 *   提供单例 PendingWriteRegistry：register 登记 flush 函数并返回 unregister；
 *   flushAll 并行执行当前登记的全部 flush，并聚合失败。
 */

/**
 * 可注册的待落库写入器集合。
 *
 * Business Logic（为什么需要这个接口）:
 *   AppShell 关闭路径与多个编辑域（速记本等）需要统一的 flush 合同。
 *
 * Code Logic（字段说明）:
 *   register 返回 unregister；flushAll 等待所有仍注册的 writer。
 */
export interface PendingWriteRegistry {
  register(id: string, flush: () => Promise<void>): () => void;
  flushAll(): Promise<void>;
}

/**
 * Business Logic（为什么需要这个类型）:
 *   关闭 GUI 时若多个 writer 同时失败，需要一次性把错误交给关闭对话框。
 *
 * Code Logic（这个类型做什么）:
 *   继承 AggregateError，errors 为各 writer 的原始失败。
 */
export class PendingWriteFlushError extends AggregateError {
  /**
   * Business Logic（为什么需要这个构造函数）:
   *   调用方需要统一 Error 类型与可读 message。
   *
   * Code Logic（这个函数做什么）:
   *   用 errors 构造 AggregateError，并固定 name。
   */
  constructor(errors: unknown[]) {
    const normalized = errors.map((error) =>
      error instanceof Error ? error : new Error(String(error)),
    );
    super(normalized, PendingWriteFlushError.buildMessage(normalized));
    this.name = 'PendingWriteFlushError';
  }

  /**
   * Business Logic（为什么需要这个函数）:
   *   关闭对话框需要可读摘要，而不是空 message。
   *
   * Code Logic（这个函数做什么）:
   *   拼接各 Error.message。
   */
  private static buildMessage(errors: Error[]): string {
    if (errors.length === 0) return 'Pending write flush failed';
    if (errors.length === 1) return errors[0]?.message ?? 'Pending write flush failed';
    return errors.map((error) => error.message).join('; ');
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   测试与生产需要同一套 registry 行为；工厂便于单测隔离。
 *
 * Code Logic（这个函数做什么）:
 *   创建内存 Map 实现的 PendingWriteRegistry。
 */
export function createPendingWriteRegistry(): PendingWriteRegistry {
  const writers = new Map<string, () => Promise<void>>();

  return {
    /**
     * Business Logic（为什么需要这个方法）:
     *   各编辑域在挂载时登记，卸载时注销，避免关闭时 flush 已销毁状态。
     *
     * Code Logic（这个方法做什么）:
     *   以 id 覆盖登记 flush；返回的 unregister 仅在仍是同一 flush 时删除。
     */
    register(id: string, flush: () => Promise<void>): () => void {
      writers.set(id, flush);
      return () => {
        if (writers.get(id) === flush) {
          writers.delete(id);
        }
      };
    },

    /**
     * Business Logic（为什么需要这个方法）:
     *   GUI 关闭前必须等待全部 pending write 落库，任一失败应阻断关闭。
     *
     * Code Logic（这个方法做什么）:
     *   快照当前 writers 后 Promise.allSettled；有 rejected 则抛 PendingWriteFlushError。
     */
    async flushAll(): Promise<void> {
      const flushes = Array.from(writers.values());
      if (flushes.length === 0) return;

      const results = await Promise.allSettled(flushes.map((flush) => flush()));
      const failures: unknown[] = [];
      for (const result of results) {
        if (result.status === 'rejected') {
          failures.push(result.reason);
        }
      }
      if (failures.length > 0) {
        throw new PendingWriteFlushError(failures);
      }
    },
  };
}

/** AppShell 生命周期使用的全局 pending write 注册表。 */
export const pendingWrites: PendingWriteRegistry = createPendingWriteRegistry();
