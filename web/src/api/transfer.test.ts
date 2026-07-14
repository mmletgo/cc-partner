/**
 * Transfer API 契约单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   send/cancel 必须对齐后端真实 DTO，不能再把 send 当 TransferTask、cancel 当 void。
 *
 * Code Logic（这个测试做什么）:
 *   mock invoke，锁定命令名、参数与 SendTransferResult / CancelTransferResult 返回形状。
 */

import { beforeEach, describe, expect, test, vi } from 'vitest';
import { readFileSync } from 'node:fs';

vi.mock('./client', () => ({
  invoke: vi.fn(),
}));

import { invoke } from './client';
import { transferApi } from './transfer';
import type { CancelTransferResult, SendTransferResult, TransferTask } from '@/lib/types';

const mockedInvoke = vi.mocked(invoke);

describe('transferApi', () => {
  beforeEach(() => {
    mockedInvoke.mockReset();
  });

  test('list invokes list_transfers and returns TransferTask[]', async () => {
    const tasks: TransferTask[] = [
      {
        id: 't1',
        fileName: 'a.txt',
        filePath: '/tmp/a.txt',
        fileSize: 1,
        direction: 'send',
        status: 'pending',
        progress: 0,
        startedAt: '2026-07-13T00:00:00.000Z',
      },
    ];
    mockedInvoke.mockResolvedValueOnce(tasks);

    const result = await transferApi.list();

    expect(mockedInvoke).toHaveBeenCalledWith('list_transfers');
    expect(result).toEqual(tasks);
  });

  test('send maps SendTransferResult shape and preserves opaque Windows path', async () => {
    const windowsPath = 'C:\\Users\\hans\\Desktop\\报告 1.txt';
    const payload: SendTransferResult = {
      accepted: true,
      deviceId: 'device-a',
      filePath: windowsPath,
      id: 'transfer-1',
    };
    mockedInvoke.mockResolvedValueOnce(payload);

    const result = await transferApi.send('device-a', windowsPath);

    expect(mockedInvoke).toHaveBeenCalledWith('send_transfer', {
      deviceId: 'device-a',
      filePath: windowsPath,
    });
    expect(result).toEqual(payload);
    expect(result.filePath).toBe(windowsPath);
    expect(result.accepted).toBe(true);
    expect(result.id).toBe('transfer-1');
  });

  test('cancel maps CancelTransferResult {ok,id} shape', async () => {
    const payload: CancelTransferResult = { ok: true, id: 'transfer-9' };
    mockedInvoke.mockResolvedValueOnce(payload);

    const result = await transferApi.cancel('transfer-9');

    expect(mockedInvoke).toHaveBeenCalledWith('cancel_transfer', { taskId: 'transfer-9' });
    expect(result).toEqual(payload);
    expect(result.ok).toBe(true);
    expect(result.id).toBe('transfer-9');
  });

  test('source wires invoke generics to send/cancel result DTOs', () => {
    const source = readFileSync(new URL('./transfer.ts', import.meta.url), 'utf8');
    expect(source).toContain("invoke<SendTransferResult>('send_transfer'");
    expect(source).toContain("invoke<CancelTransferResult>('cancel_transfer'");
    expect(source).not.toContain("invoke<TransferTask>('send_transfer'");
    expect(source).not.toContain("invoke<void>('cancel_transfer'");
  });
});
