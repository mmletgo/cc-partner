/**
 * GUI 关闭前 pending write flush 门闩。
 *
 * Business Logic（为什么需要这个模块）:
 *   关闭 GUI 前必须先落库未保存正文；失败时禁止 exit，避免静默丢数据。
 *   从 App.tsx 抽出以满足 react-refresh 仅导出组件的约定。
 *
 * Code Logic（这个模块做什么）:
 *   提供 flushPendingWritesThenClose：先 flushAll，full 模式再 stop，最后 exitGui。
 */

/**
 * 关闭 GUI 前的 pending write 执行依赖。
 *
 * Business Logic（为什么需要这个类型）:
 *   关闭路径需要可注入 flush/stop/exit，便于单测验证时序与失败门闩。
 *
 * Code Logic（字段说明）:
 *   flushAll 必须先完成；mode=full 时再 stop；最后 exitGui。
 */
export interface CloseFlushDeps {
  flushAll: () => Promise<void>;
  stop: () => Promise<unknown>;
  exitGui: () => Promise<void>;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   “仅关闭 GUI”与“前后端都关闭”都必须先落库未保存正文，失败时禁止 exit。
 *
 * Code Logic（这个函数做什么）:
 *   先 await flushAll；full 模式再 await stop；最后 await exitGui。
 *   flush 抛错时不调用 stop/exit，错误向上抛给调用方展示。
 */
export async function flushPendingWritesThenClose(
  mode: 'gui' | 'full',
  deps: CloseFlushDeps,
): Promise<void> {
  await deps.flushAll();
  if (mode === 'full') {
    await deps.stop();
  }
  await deps.exitGui();
}
