/**
 * Transfer API 契约单元测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   send/cancel 必须对齐后端真实 DTO；retry/resume 必须带稳定 clientOperationId；
 *   getOperation 解码 TransferOperationStatus；prepareOpen/open/reveal 必须调用
 *   prepare_transfer_open 且 opener 失败映射稳定本地错误。
 *
 * Code Logic（这个测试做什么）:
 *   mock invoke 与 plugin-opener；通过 invokeDecoded 走真实 decoder；锁定命令名、参数与返回形状。
 */

import { beforeEach, describe, expect, test, vi } from 'vitest';
import { readFileSync } from 'node:fs';
import type { Decoder } from '@/lib/runtimeSchema';
import type {
  CancelTransferResult,
  SendTransferResult,
  TransferOperationStatus,
  TransferTask,
} from '@/lib/types';

const mockInvoke = vi.fn();
const mockOpenPath = vi.fn();
const mockRevealItemInDir = vi.fn();

vi.mock('./client', () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
  invokeDecoded: async <T>(
    cmd: string,
    args: Record<string, unknown> | undefined,
    decoder: Decoder<T>,
  ): Promise<T> => {
    const raw = await mockInvoke(cmd, args);
    return decoder.decode(raw, '$');
  },
}));

vi.mock('@tauri-apps/plugin-opener', () => ({
  openPath: (...args: unknown[]) => mockOpenPath(...args),
  revealItemInDir: (...args: unknown[]) => mockRevealItemInDir(...args),
}));

import { mapOpenerError, transferApi } from './transfer';

const recoveryTask: TransferTask = {
  id: 't1',
  fileName: 'a.txt',
  filePath: '/tmp/a.txt',
  fileSize: 1,
  direction: 'send',
  status: 'pending',
  progress: 0,
  startedAt: '2026-07-13T00:00:00.000Z',
  phase: 'queued',
  attempt: 2,
  logicalTransferId: 'logical-1',
  attemptId: 'attempt-2',
  protocolTransferId: 'proto-1',
  clientOperationId: 'op1',
};

describe('transferApi', () => {
  beforeEach(() => {
    mockInvoke.mockReset();
    mockOpenPath.mockReset();
    mockRevealItemInDir.mockReset();
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
    mockInvoke.mockResolvedValueOnce(tasks);

    const result = await transferApi.list();

    expect(mockInvoke).toHaveBeenCalledWith('list_transfers', undefined);
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
    mockInvoke.mockResolvedValueOnce(payload);

    const result = await transferApi.send('device-a', windowsPath);

    expect(mockInvoke).toHaveBeenCalledWith('send_transfer', {
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
    mockInvoke.mockResolvedValueOnce(payload);

    const result = await transferApi.cancel('transfer-9');

    expect(mockInvoke).toHaveBeenCalledWith('cancel_transfer', { taskId: 'transfer-9' });
    expect(result).toEqual(payload);
    expect(result.ok).toBe(true);
    expect(result.id).toBe('transfer-9');
  });

  test('resume sends taskId and stable clientOperationId', async () => {
    mockInvoke.mockResolvedValueOnce(recoveryTask);

    await transferApi.resume('t1', 'op1');

    expect(mockInvoke).toHaveBeenCalledWith('resume_transfer', {
      taskId: 't1',
      clientOperationId: 'op1',
    });
  });

  test('retry sends taskId and stable clientOperationId', async () => {
    mockInvoke.mockResolvedValueOnce(recoveryTask);

    const result = await transferApi.retry('t1', 'op-retry');

    expect(mockInvoke).toHaveBeenCalledWith('retry_transfer', {
      taskId: 't1',
      clientOperationId: 'op-retry',
    });
    expect(result.clientOperationId).toBe('op1');
    expect(result.phase).toBe('queued');
  });

  test('getOperation decodes TransferOperationStatus', async () => {
    const payload: TransferOperationStatus = { status: 'succeeded', taskId: 't9' };
    mockInvoke.mockResolvedValueOnce(payload);

    const result = await transferApi.getOperation('op-ledger');

    expect(mockInvoke).toHaveBeenCalledWith('get_transfer_operation', {
      clientOperationId: 'op-ledger',
    });
    expect(result).toEqual(payload);
  });

  test('prepareOpen invokes prepare_transfer_open with action', async () => {
    const target = {
      taskId: 'recv-1',
      action: 'reveal' as const,
      path: '/tmp/received/a.bin',
    };
    mockInvoke.mockResolvedValueOnce(target);

    const result = await transferApi.prepareOpen('recv-1', 'reveal');

    expect(mockInvoke).toHaveBeenCalledWith('prepare_transfer_open', {
      taskId: 'recv-1',
      action: 'reveal',
    });
    expect(result).toEqual(target);
  });

  test('open prepares then openPath', async () => {
    const target = {
      taskId: 'recv-2',
      action: 'open' as const,
      path: '/tmp/received/b.bin',
    };
    mockInvoke.mockResolvedValueOnce(target);
    mockOpenPath.mockResolvedValueOnce(undefined);

    const result = await transferApi.open('recv-2');

    expect(mockInvoke).toHaveBeenCalledWith('prepare_transfer_open', {
      taskId: 'recv-2',
      action: 'open',
    });
    expect(mockOpenPath).toHaveBeenCalledWith('/tmp/received/b.bin');
    expect(result).toEqual(target);
  });

  test('reveal prepares then revealItemInDir', async () => {
    const target = {
      taskId: 'recv-3',
      action: 'reveal' as const,
      path: '/tmp/received/c.bin',
    };
    mockInvoke.mockResolvedValueOnce(target);
    mockRevealItemInDir.mockResolvedValueOnce(undefined);

    const result = await transferApi.reveal('recv-3');

    expect(mockInvoke).toHaveBeenCalledWith('prepare_transfer_open', {
      taskId: 'recv-3',
      action: 'reveal',
    });
    expect(mockRevealItemInDir).toHaveBeenCalledWith('/tmp/received/c.bin');
    expect(result).toEqual(target);
  });

  test('open maps opener permission failure to stable local error', async () => {
    mockInvoke.mockResolvedValueOnce({
      taskId: 'recv-4',
      action: 'open',
      path: '/tmp/received/d.bin',
    });
    mockOpenPath.mockRejectedValueOnce(new Error('permission denied'));

    await expect(transferApi.open('recv-4')).rejects.toThrow(
      'transfer_opener_failed: permission denied',
    );
  });

  test('mapOpenerError prefixes stable code', () => {
    expect(mapOpenerError(new Error('platform boom')).message).toBe(
      'transfer_opener_failed: platform boom',
    );
  });

  test('source wires invokeDecoded to recovery and open result DTOs', () => {
    const source = readFileSync(new URL('./transfer.ts', import.meta.url), 'utf8');
    expect(source).toContain("invokeDecoded('send_transfer'");
    expect(source).toContain("invokeDecoded('cancel_transfer'");
    expect(source).toContain("invokeDecoded('list_transfers'");
    expect(source).toContain("'retry_transfer'");
    expect(source).toContain("'resume_transfer'");
    expect(source).toContain("'get_transfer_operation'");
    expect(source).toContain("invokeDecoded(\n      'prepare_transfer_open'");
    expect(source).not.toContain("invoke<TransferTask>('send_transfer'");
    expect(source).not.toContain("invoke<void>('cancel_transfer'");
  });
});
