/**
 * createLatestRequestGuard / buildCcHistoryPromptContext 单元测试
 *
 * Business Logic（为什么需要这个测试）:
 *   Claude History 依赖 latest-token + context 守卫丢弃逆序响应；守卫语义必须独立可测。
 *
 * Code Logic（这个测试做什么）:
 *   覆盖 begin 递增、isCurrent 双条件、invalidate 失效、prompt context 键编码。
 */

import { describe, expect, test } from 'vitest';
import {
  buildCcHistoryPromptContext,
  createLatestRequestGuard,
} from './ccHistoryRequestState';

describe('createLatestRequestGuard', () => {
  test('begin 返回递增 token，且仅最新 token+匹配 context 为 current', () => {
    const guard = createLatestRequestGuard<string>();
    const t1 = guard.begin('A');
    expect(t1).toBe(1);
    expect(guard.isCurrent(t1, 'A')).toBe(true);
    expect(guard.isCurrent(t1, 'B')).toBe(false);

    const t2 = guard.begin('B');
    expect(t2).toBe(2);
    expect(guard.isCurrent(t1, 'A')).toBe(false);
    expect(guard.isCurrent(t2, 'B')).toBe(true);
    expect(guard.isCurrent(t2, 'A')).toBe(false);
  });

  test('invalidate 后所有 token 均非 current，直到下次 begin', () => {
    const guard = createLatestRequestGuard<string>();
    const token = guard.begin('/proj\0');
    expect(guard.isCurrent(token, '/proj\0')).toBe(true);

    guard.invalidate();
    expect(guard.isCurrent(token, '/proj\0')).toBe(false);

    const next = guard.begin('/other\0ab');
    expect(guard.isCurrent(token, '/proj\0')).toBe(false);
    expect(guard.isCurrent(next, '/other\0ab')).toBe(true);
  });

  test('支持 null 项目列表上下文', () => {
    const guard = createLatestRequestGuard<null>();
    const token = guard.begin(null);
    expect(guard.isCurrent(token, null)).toBe(true);
    guard.begin(null);
    expect(guard.isCurrent(token, null)).toBe(false);
  });
});

describe('buildCcHistoryPromptContext', () => {
  test('编码 projectPath 与 search，缺省 search 为空串', () => {
    expect(buildCcHistoryPromptContext('/a/b')).toBe('/a/b\0');
    expect(buildCcHistoryPromptContext('/a/b', undefined)).toBe('/a/b\0');
    expect(buildCcHistoryPromptContext('/a/b', '')).toBe('/a/b\0');
    expect(buildCcHistoryPromptContext('/a/b', 'ab')).toBe('/a/b\0ab');
  });
});
