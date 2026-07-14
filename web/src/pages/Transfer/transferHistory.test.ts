/**
 * transferHistory 纯函数契约测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   分组与 resume/retry 判定是 UI 动作矩阵的权威源，不得漂移。
 *
 * Code Logic（这个测试做什么）:
 *   覆盖 classify/group/resumable/retryable/open-reveal 判定。
 */

import { describe, expect, test } from 'vitest';
import type { TransferTask } from '@/lib/types';
import {
  canOpenRevealTransfer,
  classifyTransferGroup,
  groupTransferTasks,
  isTransferResumable,
  isTransferRetryable,
} from './transferHistory';

/**
 * Business Logic（为什么需要这个函数）:
 *   纯函数用例共享最小合法 TransferTask。
 *
 * Code Logic（这个函数做什么）:
 *   返回可覆盖字段的 DTO。
 */
function task(overrides: Partial<TransferTask> = {}): TransferTask {
  return {
    id: 't1',
    fileName: 'a.bin',
    filePath: '/tmp/a.bin',
    fileSize: 100,
    direction: 'send',
    status: 'failed',
    progress: 0,
    startedAt: '2026-07-14T00:00:00.000Z',
    ...overrides,
  };
}

describe('transferHistory helpers', () => {
  test('classifies active/needsAttention/completed groups', () => {
    expect(classifyTransferGroup(task({ status: 'transferring' }), false)).toBe('active');
    expect(classifyTransferGroup(task({ status: 'failed' }), false)).toBe('needsAttention');
    expect(classifyTransferGroup(task({ status: 'completed' }), false)).toBe('completed');
    expect(classifyTransferGroup(task({ status: 'transferring' }), true)).toBe('needsAttention');
  });

  test('resumable requires send+failed+progress+peer resume capability', () => {
    const progressFailed = task({
      progress: 0.4,
      transferredBytes: 40,
      failure: { stage: 'transfer', code: 'x', retryable: true, message: 'x' },
    });
    // 无 peer 能力（默认 false）不得 resume
    expect(isTransferResumable(progressFailed)).toBe(false);
    expect(isTransferRetryable(progressFailed)).toBe(true);
    // 有能力且有进度 → resume
    expect(isTransferResumable(progressFailed, true)).toBe(true);
    expect(isTransferRetryable(progressFailed, true)).toBe(false);
    expect(
      isTransferResumable(
        task({
          progress: 0,
          transferredBytes: 0,
          failure: { stage: 'connect', code: 'x', retryable: true, message: 'x' },
        }),
        true,
      ),
    ).toBe(false);
    expect(
      isTransferRetryable(
        task({
          progress: 0,
          transferredBytes: 0,
          failure: { stage: 'connect', code: 'x', retryable: true, message: 'x' },
        }),
        true,
      ),
    ).toBe(true);
  });

  test('open/reveal only for receive completed', () => {
    expect(
      canOpenRevealTransfer(task({ status: 'completed', direction: 'receive' })),
    ).toBe(true);
    expect(canOpenRevealTransfer(task({ status: 'completed', direction: 'send' }))).toBe(false);
  });

  test('groupTransferTasks omits empty by leaving arrays empty', () => {
    const groups = groupTransferTasks(
      [
        task({ id: '1', status: 'transferring', fileName: 'a' }),
        task({ id: '2', status: 'failed', fileName: 'b' }),
      ],
      new Set(),
    );
    expect(groups.active.map((t) => t.id)).toEqual(['1']);
    expect(groups.needsAttention.map((t) => t.id)).toEqual(['2']);
    expect(groups.completed).toEqual([]);
  });
});
