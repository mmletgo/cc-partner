/**
 * Transfer schema fixtures（含 recovery phase/failure/operation）。
 */

import { describe, expect, test } from 'vitest';
import { ContractDecodeError } from '../runtimeSchema';
import {
  cancelTransferResultDecoder,
  sendTransferResultDecoder,
  transferFailureDecoder,
  transferOperationStatusDecoder,
  transferPhaseDecoder,
  transferProgressEventDecoder,
  transferStatusEventDecoder,
  transferTaskDecoder,
  transferTasksDecoder,
} from './transfer';

const validTask = {
  id: 't1',
  fileName: 'a.txt',
  filePath: '/tmp/a.txt',
  fileSize: 1,
  direction: 'send',
  status: 'pending',
  progress: 0,
  startedAt: '2026-07-13T00:00:00.000Z',
};

const recoveryTask = {
  ...validTask,
  status: 'failed',
  progress: 0.3,
  transferredBytes: 3,
  phase: 'failed',
  failure: {
    stage: 'transfer',
    code: 'peer_timeout',
    retryable: true,
    message: '超时',
  },
  attempt: 2,
  logicalTransferId: 'logical-1',
  attemptId: 'attempt-2',
  protocolTransferId: 'proto-1',
  clientOperationId: 'op-1',
  operationPayloadHash: 'hash-1',
};

describe('transfer schemas', () => {
  test('decodes task list and send/cancel results', () => {
    expect(transferTasksDecoder.decode([validTask])).toHaveLength(1);
    expect(
      sendTransferResultDecoder.decode({
        accepted: true,
        deviceId: 'd',
        filePath: 'C:\\x',
        id: 'id1',
      }).id,
    ).toBe('id1');
    expect(cancelTransferResultDecoder.decode({ ok: true, id: 'id1' }).ok).toBe(true);
    expect(
      transferProgressEventDecoder.decode({
        id: 'id1',
        transferredBytes: 10,
        size: 100,
        progress: 0.1,
      }).progress,
    ).toBe(0.1);
  });

  test('decodes recovery fields on TransferTask', () => {
    const decoded = transferTaskDecoder.decode(recoveryTask);
    expect(decoded.phase).toBe('failed');
    expect(decoded.failure).toEqual({
      stage: 'transfer',
      code: 'peer_timeout',
      retryable: true,
      message: '超时',
    });
    expect(decoded.attempt).toBe(2);
    expect(decoded.logicalTransferId).toBe('logical-1');
    expect(decoded.clientOperationId).toBe('op-1');
    expect(decoded.transferredBytes).toBe(3);
  });

  test('malformed status enum fails', () => {
    expect(() => transferTaskDecoder.decode({ ...validTask, status: 'running' })).toThrow(
      ContractDecodeError,
    );
  });

  test('malformed phase enum fails closed', () => {
    expect(() => transferPhaseDecoder.decode('brand_new_phase')).toThrow(ContractDecodeError);
    expect(() =>
      transferTaskDecoder.decode({ ...validTask, phase: 'running' }),
    ).toThrow(ContractDecodeError);
  });

  test('malformed failure stage fails closed', () => {
    expect(() =>
      transferFailureDecoder.decode({
        stage: 'network',
        code: 'x',
        retryable: true,
        message: 'm',
      }),
    ).toThrow(ContractDecodeError);
  });

  test('null failure is accepted on task', () => {
    expect(transferTaskDecoder.decode({ ...validTask, failure: null }).failure).toBeNull();
  });

  test('send accepted must be literal true', () => {
    expect(() =>
      sendTransferResultDecoder.decode({
        accepted: false,
        deviceId: 'd',
        filePath: 'p',
        id: 'i',
      }),
    ).toThrow(ContractDecodeError);
  });

  test('decodes operation status union exhaustively', () => {
    expect(transferOperationStatusDecoder.decode({ status: 'notFound' })).toEqual({
      status: 'notFound',
    });
    expect(transferOperationStatusDecoder.decode({ status: 'pending' })).toEqual({
      status: 'pending',
    });
    expect(
      transferOperationStatusDecoder.decode({ status: 'succeeded', taskId: 't9' }),
    ).toEqual({ status: 'succeeded', taskId: 't9' });
    expect(transferOperationStatusDecoder.decode({ status: 'failed', code: 'source_changed' })).toEqual(
      {
        status: 'failed',
        code: 'source_changed',
      },
    );
  });

  test('invalid operation status fails closed', () => {
    expect(() => transferOperationStatusDecoder.decode({ status: 'unknown' })).toThrow(
      ContractDecodeError,
    );
    expect(() => transferOperationStatusDecoder.decode({ status: 'succeeded' })).toThrow(
      ContractDecodeError,
    );
    expect(() =>
      transferOperationStatusDecoder.decode({ status: 'failed', code: 1 }),
    ).toThrow(ContractDecodeError);
  });

  test('decodes status event with optional errorMessage', () => {
    expect(
      transferStatusEventDecoder.decode({
        id: 'id1',
        status: 'failed',
        errorMessage: 'boom',
      }),
    ).toEqual({
      id: 'id1',
      status: 'failed',
      errorMessage: 'boom',
    });
    expect(
      transferStatusEventDecoder.decode({
        id: 'id1',
        status: 'completed',
      }),
    ).toEqual({
      id: 'id1',
      status: 'completed',
      errorMessage: undefined,
    });
  });

  test('malformed status event fails closed', () => {
    expect(() => transferStatusEventDecoder.decode({ id: 'id1' })).toThrow(ContractDecodeError);
    expect(() =>
      transferStatusEventDecoder.decode({ id: 1, status: 'completed' }),
    ).toThrow(ContractDecodeError);
  });
});
