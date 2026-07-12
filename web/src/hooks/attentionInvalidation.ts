/**
 * Attention 立即失效桥（单一入口）。
 *
 * Business Logic（为什么需要这个模块）:
 *   Deliver / Request Rework / task Retry/Refresh / outbox Retry/Discard /
 *   dependency install/recheck 成功后，Inbox badge 与列表必须立刻反映权威状态，
 *   不能只等 10 秒轮询；失败动作不得清空或抖动现有投影。
 *   调用方可能在 AttentionProvider 之外（如 WorkbenchDependencyProvider），
 *   因此用 window 事件做解耦桥，而不是强制 Provider 嵌套。
 *
 * Code Logic（这个模块做什么）:
 *   暴露 requestAttentionInvalidation 派发自定义事件；
 *   AttentionProvider 订阅后 await context.refresh()；
 *   仅成功路径调用，失败 catch 不得调用。
 */

/** 桌面与移动共享的 Attention 失效事件名。 */
export const ATTENTION_INVALIDATION_EVENT = 'cp-attention-invalidate';

/**
 * Business Logic（为什么需要这个函数）:
 *   业务动作成功后需要统一触发 Inbox 刷新，避免各页面直接耦合 Provider。
 *
 * Code Logic（这个函数做什么）:
 *   在浏览器环境派发 ATTENTION_INVALIDATION_EVENT；无 window 时 no-op。
 */
export function requestAttentionInvalidation(): void {
  if (typeof window === 'undefined') return;
  window.dispatchEvent(new CustomEvent(ATTENTION_INVALIDATION_EVENT));
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Provider 需要在挂载期订阅失效事件，卸载时清理，避免泄漏。
 *
 * Code Logic（这个函数做什么）:
 *   注册 window 监听；返回取消订阅函数。
 */
export function subscribeAttentionInvalidation(handler: () => void): () => void {
  if (typeof window === 'undefined') {
    return () => undefined;
  }
  const listener = (): void => {
    handler();
  };
  window.addEventListener(ATTENTION_INVALIDATION_EVENT, listener);
  return () => {
    window.removeEventListener(ATTENTION_INVALIDATION_EVENT, listener);
  };
}
