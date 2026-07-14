/**
 * Transfer schema fixtures。
 */

import { describe, expect, test } from 'vitest';
import { ContractDecodeError } from '../runtimeSchema';
import {
  cancelTransferResultDecoder,
  sendTransferResultDecoder,
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

  test('malformed status enum fails', () => {
    expect(() => transferTaskDecoder.decode({ ...validTask, status: 'running' })).toThrow(
      ContractDecodeError,
    );
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
