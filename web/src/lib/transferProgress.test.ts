/**
 * transferProgress 合并 helper 合同测试。
 */

import { describe, expect, test } from 'vitest';
import type { TransferTask } from '@/lib/types';
import {
  decodeTransferProgressEvent,
  decodeTransferStatusEvent,
  mergeTransferProgressEvent,
  mergeTransferStatusEvent,
} from './transferProgress';
import { ContractDecodeError } from './runtimeSchema';

function baseTask(overrides: Partial<TransferTask> = {}): TransferTask {
  return {
    id: 't1',
    fileName: 'a.txt',
    filePath: '/tmp/a.txt',
    fileSize: 100,
    direction: 'send',
    status: 'transferring',
    progress: 0.1,
    startedAt: '2026-07-13T00:00:00.000Z',
    ...overrides,
  };
}

describe('decodeTransferProgressEvent / decodeTransferStatusEvent', () => {
  test('decodes valid progress and status payloads', () => {
    expect(
      decodeTransferProgressEvent({
        id: 't1',
        transferredBytes: 10,
        size: 100,
        progress: 0.1,
      }),
    ).toEqual({
      id: 't1',
      transferredBytes: 10,
      size: 100,
      progress: 0.1,
    });

    expect(
      decodeTransferStatusEvent({
        id: 't1',
        status: 'completed',
        errorMessage: undefined,
      }),
    ).toMatchObject({ id: 't1', status: 'completed' });
  });

  test('malformed progress throws ContractDecodeError', () => {
    expect(() =>
      decodeTransferProgressEvent({
        id: 't1',
        transferredBytes: 'nope',
        size: 100,
        progress: 0.1,
      }),
    ).toThrow(ContractDecodeError);
  });
});

describe('mergeTransferProgressEvent', () => {
  test('updates matching task progress', () => {
    const tasks = [baseTask({ id: 't1', progress: 0.1 }), baseTask({ id: 't2', progress: 0 })];
    const next = mergeTransferProgressEvent(tasks, {
      id: 't1',
      transferredBytes: 50,
      size: 100,
      progress: 0.5,
    });
    expect(next).not.toBeNull();
    expect(next).not.toBe(tasks);
    expect(next![0]!.progress).toBe(0.5);
    expect(next![1]!.progress).toBe(0);
    // 原数组不被原地修改
    expect(tasks[0]!.progress).toBe(0.1);
  });

  test('returns same array reference when id has no match after successful decode', () => {
    const tasks = [baseTask({ id: 't1' })];
    const next = mergeTransferProgressEvent(tasks, {
      id: 'missing',
      transferredBytes: 1,
      size: 10,
      progress: 0.1,
    });
    expect(next).toBe(tasks);
  });

  test('malformed progress returns null and does not invent state', () => {
    const tasks = [baseTask()];
    const next = mergeTransferProgressEvent(tasks, {
      id: 't1',
      // progress 缺失 → 解码失败
      transferredBytes: 1,
      size: 10,
    });
    expect(next).toBeNull();
    expect(tasks[0]!.progress).toBe(0.1);
  });

  test('NaN progress fails closed', () => {
    const tasks = [baseTask()];
    const next = mergeTransferProgressEvent(tasks, {
      id: 't1',
      transferredBytes: 1,
      size: 10,
      progress: Number.NaN,
    });
    expect(next).toBeNull();
  });
});

describe('mergeTransferStatusEvent', () => {
  test('merges completed status', () => {
    const tasks = [baseTask({ id: 't1', status: 'transferring' })];
    const next = mergeTransferStatusEvent(tasks, {
      id: 't1',
      status: 'completed',
    });
    expect(next).not.toBeNull();
    expect(next![0]!.status).toBe('completed');
  });

  test('merges failed status with errorMessage', () => {
    const tasks = [baseTask({ id: 't1', status: 'transferring' })];
    const next = mergeTransferStatusEvent(tasks, {
      id: 't1',
      status: 'failed',
      errorMessage: 'disk full',
    });
    expect(next).not.toBeNull();
    expect(next![0]!.status).toBe('failed');
    expect(next![0]!.errorMessage).toBe('disk full');
  });

  test('merges cancelled status', () => {
    const tasks = [baseTask({ id: 't1', status: 'transferring' })];
    const next = mergeTransferStatusEvent(tasks, {
      id: 't1',
      status: 'cancelled',
    });
    expect(next).not.toBeNull();
    expect(next![0]!.status).toBe('cancelled');
  });

  test('unknown status returns null', () => {
    const tasks = [baseTask({ id: 't1', status: 'transferring' })];
    const next = mergeTransferStatusEvent(tasks, {
      id: 't1',
      status: 'running',
    });
    expect(next).toBeNull();
    expect(tasks[0]!.status).toBe('transferring');
  });

  test('malformed status payload returns null', () => {
    const tasks = [baseTask()];
    const next = mergeTransferStatusEvent(tasks, { id: 1, status: 'completed' });
    expect(next).toBeNull();
  });

  test('no matching id returns same array after successful decode', () => {
    const tasks = [baseTask({ id: 't1' })];
    const next = mergeTransferStatusEvent(tasks, {
      id: 'missing',
      status: 'completed',
    });
    expect(next).toBe(tasks);
  });
});
