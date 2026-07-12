// @vitest-environment jsdom
/**
 * Attention invalidation 桥单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   业务动作成功路径依赖统一事件桥触发 Inbox 刷新；事件名/订阅/no-op 语义必须稳定。
 *
 * Code Logic（这个测试做什么）:
 *   覆盖 requestAttentionInvalidation 派发、subscribe 回调、unsubscribe 后不再触发。
 */

import { afterEach, describe, expect, test, vi } from 'vitest';

import {
  ATTENTION_INVALIDATION_EVENT,
  requestAttentionInvalidation,
  subscribeAttentionInvalidation,
} from './attentionInvalidation';

afterEach(() => {
  // 清理可能残留的监听器副作用（各用例自行 unsubscribe）。
});

describe('attentionInvalidation', () => {
  test('requestAttentionInvalidation dispatches custom event', () => {
    const handler = vi.fn();
    window.addEventListener(ATTENTION_INVALIDATION_EVENT, handler);
    requestAttentionInvalidation();
    expect(handler).toHaveBeenCalledTimes(1);
    window.removeEventListener(ATTENTION_INVALIDATION_EVENT, handler);
  });

  test('subscribeAttentionInvalidation receives and can unsubscribe', () => {
    const handler = vi.fn();
    const unsubscribe = subscribeAttentionInvalidation(handler);
    requestAttentionInvalidation();
    expect(handler).toHaveBeenCalledTimes(1);
    unsubscribe();
    requestAttentionInvalidation();
    expect(handler).toHaveBeenCalledTimes(1);
  });
});
