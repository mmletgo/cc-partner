/**
 * Attention reducer / request sequence 单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   Provider 依赖可预测状态机：初次失败无 badge 数字、刷新失败保 stale、成功清 stale。
 *
 * Code Logic（这个测试做什么）:
 *   覆盖 loadStarted/Succeeded/Failed 与 requestId 辅助函数。
 */

import { describe, expect, test } from 'vitest';

import {
  attentionReducer,
  createInitialAttentionState,
  isCurrentAttentionRequest,
  nextAttentionRequestId,
} from './attentionState';
import type { AttentionSnapshot } from '@/lib/types';

/**
 * Business Logic（为什么需要这个函数）:
 *   reducer 测试需要最小合法快照 fixture。
 *
 * Code Logic（这个函数做什么）:
 *   返回空 items 的 AttentionSnapshot。
 */
function emptySnapshot(generatedAt = '2026-07-11T10:00:00.000Z'): AttentionSnapshot {
  return {
    generatedAt,
    counts: { total: 0, decision: 0, blocked: 0, environment: 0 },
    items: [],
  };
}

describe('attentionReducer', () => {
  test('initial load sets loading without snapshot', () => {
    const next = attentionReducer(createInitialAttentionState(), {
      type: 'loadStarted',
      hasSnapshot: false,
    });
    expect(next.loading).toBe(true);
    expect(next.refreshing).toBe(false);
    expect(next.snapshot).toBeNull();
  });

  test('successful snapshot clears loading and stores lastSucceededAt', () => {
    const loading = attentionReducer(createInitialAttentionState(), {
      type: 'loadStarted',
      hasSnapshot: false,
    });
    const next = attentionReducer(loading, {
      type: 'loadSucceeded',
      snapshot: emptySnapshot('2026-07-11T10:05:00.000Z'),
      receivedAt: '2026-07-11T10:05:01.000Z',
    });
    expect(next.loading).toBe(false);
    expect(next.refreshing).toBe(false);
    expect(next.stale).toBe(false);
    expect(next.error).toBeNull();
    expect(next.snapshot?.generatedAt).toBe('2026-07-11T10:05:00.000Z');
    expect(next.lastSucceededAt).toBe('2026-07-11T10:05:01.000Z');
  });

  test('first load failure keeps null snapshot and no stale badge source', () => {
    const loading = attentionReducer(createInitialAttentionState(), {
      type: 'loadStarted',
      hasSnapshot: false,
    });
    const next = attentionReducer(loading, {
      type: 'loadFailed',
      error: new Error('network down'),
      hasSnapshot: false,
    });
    expect(next.snapshot).toBeNull();
    expect(next.loading).toBe(false);
    expect(next.stale).toBe(false);
    expect(next.error?.message).toBe('network down');
    expect(next.lastSucceededAt).toBeNull();
  });

  test('refresh failure keeps snapshot and marks stale', () => {
    const withSnapshot = attentionReducer(createInitialAttentionState(), {
      type: 'loadSucceeded',
      snapshot: emptySnapshot('2026-07-11T09:00:00.000Z'),
      receivedAt: '2026-07-11T09:00:01.000Z',
    });
    const refreshing = attentionReducer(withSnapshot, {
      type: 'loadStarted',
      hasSnapshot: true,
    });
    expect(refreshing.refreshing).toBe(true);
    expect(refreshing.snapshot?.generatedAt).toBe('2026-07-11T09:00:00.000Z');

    const failed = attentionReducer(refreshing, {
      type: 'loadFailed',
      error: new Error('timeout'),
      hasSnapshot: true,
    });
    expect(failed.snapshot?.generatedAt).toBe('2026-07-11T09:00:00.000Z');
    expect(failed.stale).toBe(true);
    expect(failed.refreshing).toBe(false);
    expect(failed.error?.message).toBe('timeout');
    expect(failed.lastSucceededAt).toBe('2026-07-11T09:00:01.000Z');
  });

  test('success after stale clears stale flag', () => {
    const stale = attentionReducer(
      {
        snapshot: emptySnapshot('2026-07-11T08:00:00.000Z'),
        loading: false,
        refreshing: false,
        stale: true,
        error: new Error('old'),
        lastSucceededAt: '2026-07-11T08:00:01.000Z',
      },
      {
        type: 'loadSucceeded',
        snapshot: emptySnapshot('2026-07-11T10:00:00.000Z'),
        receivedAt: '2026-07-11T10:00:02.000Z',
      },
    );
    expect(stale.stale).toBe(false);
    expect(stale.error).toBeNull();
    expect(stale.snapshot?.generatedAt).toBe('2026-07-11T10:00:00.000Z');
  });
});

describe('attention request sequence helpers', () => {
  test('nextAttentionRequestId increments', () => {
    expect(nextAttentionRequestId(0)).toBe(1);
    expect(nextAttentionRequestId(7)).toBe(8);
  });

  test('isCurrentAttentionRequest only accepts latest id', () => {
    expect(isCurrentAttentionRequest(2, 2)).toBe(true);
    expect(isCurrentAttentionRequest(1, 2)).toBe(false);
  });
});
