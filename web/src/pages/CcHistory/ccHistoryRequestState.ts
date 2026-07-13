/**
 * Claude History 请求世代守卫
 *
 * Business Logic（为什么需要这个模块）:
 *   项目切换、搜索词变更、刷新采集与同步会并发发出 listProjects/listPrompts。
 *   旧响应若逆序回写，会把错误项目或错误搜索词的结果/错误态盖到当前 UI。
 *   需要可单测的 latest-token + context 守卫，在 success/catch/finally 写状态前判定是否仍是当前请求。
 *
 * Code Logic（这个模块做什么）:
 *   - createLatestRequestGuard：begin 递增 token 并记录 context；isCurrent 校验 token+context；
 *     invalidate 使当前世代整体失效（切到无项目时丢弃 in-flight prompts）。
 *   - buildCcHistoryPromptContext：稳定编码 `${projectPath}\0${search ?? ''}` 作为 prompt 请求上下文键。
 */

/** 请求世代 token（单调递增整数） */
export type LatestRequestToken = number;

/**
 * 最新请求守卫接口
 *
 * Business Logic（为什么需要这个接口）:
 *   loadProjects / loadPrompts 需要同一套可注入、可单测的并发语义。
 *
 * Code Logic（这个接口做什么）:
 *   begin 开启新请求；isCurrent 在写状态前双重校验；invalidate 丢弃所有未完成请求。
 */
export interface LatestRequestGuard<TContext> {
  begin(context: TContext): LatestRequestToken;
  isCurrent(token: LatestRequestToken, context: TContext): boolean;
  invalidate(): void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Claude History 的项目列表与 prompt 列表各自需要独立的 latest-only 写屏障，
 *   避免 A→B / a→ab 逆序响应覆盖当前上下文。
 *
 * Code Logic（这个函数做什么）:
 *   返回闭包守卫：内部维护 latestToken、latestContext 与 valid 标志；
 *   begin 使 valid=true 并返回新 token；isCurrent 要求 valid 且 token/context 均匹配；
 *   invalidate 将 valid 置 false（不递增 token，直到下次 begin）。
 */
export function createLatestRequestGuard<TContext>(): LatestRequestGuard<TContext> {
  let latestToken = 0;
  let latestContext: TContext | undefined;
  let hasContext = false;
  let valid = false;

  return {
    /**
     * Business Logic（为什么需要这个方法）:
     *   每次发起异步加载都要声明“我是当前最新请求”，以便旧请求自动失效。
     *
     * Code Logic（这个方法做什么）:
     *   递增 token，记录 context，标记 valid，返回该 token 供后续 isCurrent 使用。
     */
    begin(context: TContext): LatestRequestToken {
      latestToken += 1;
      latestContext = context;
      hasContext = true;
      valid = true;
      return latestToken;
    },

    /**
     * Business Logic（为什么需要这个方法）:
     *   异步返回后写 UI 前必须确认仍是最新请求且上下文未变，否则静默丢弃。
     *
     * Code Logic（这个方法做什么）:
     *   要求 valid、token===latestToken，且 Object.is(context, latestContext)。
     */
    isCurrent(token: LatestRequestToken, context: TContext): boolean {
      if (!valid || !hasContext) return false;
      if (token !== latestToken) return false;
      return Object.is(context, latestContext);
    },

    /**
     * Business Logic（为什么需要这个方法）:
     *   选中项目变为 null 时，任何 in-flight prompt 响应都不得再写右栏。
     *
     * Code Logic（这个方法做什么）:
     *   将 valid 置 false；已发出请求的 isCurrent 一律 false，直到下次 begin。
     */
    invalidate(): void {
      valid = false;
    },
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   prompt 列表同时依赖 projectPath 与 search；两者任一变化都应使旧响应失效。
 *
 * Code Logic（这个函数做什么）:
 *   返回稳定字符串键 `${projectPath}\0${search ?? ''}`（search 缺省按空串）。
 */
export function buildCcHistoryPromptContext(projectPath: string, search?: string): string {
  return `${projectPath}\0${search ?? ''}`;
}
