/**
 * protocol schema fixtures。
 *
 * Business Logic（为什么需要这个测试）:
 *   health/capabilities/error 是首批强制边界。
 *
 * Code Logic（这个测试做什么）:
 *   正常/legacy/malformed 各一条；malformed 只改一字段且不泄露正文。
 */

import { describe, expect, test } from 'vitest';
import { ContractDecodeError } from '../runtimeSchema';
import {
  protocolErrorEnvelopeDecoder,
  protocolHealthInfoDecoder,
  protocolHealthResponseDecoder,
} from './protocol';

describe('protocol schemas', () => {
  test('decodes full health response', () => {
    const raw = {
      ok: true,
      device_id: 'd1',
      device_name: 'Mac',
      http_port: 62116,
      ts: 1700000000,
      protocol_version: 1,
      capabilities: ['errors.envelope.v1', 'attention.v1'],
      extra: 'ok',
    };
    expect(protocolHealthResponseDecoder.decode(raw)).toMatchObject({
      ok: true,
      device_id: 'd1',
      protocol_version: 1,
      capabilities: ['errors.envelope.v1', 'attention.v1'],
    });
  });

  test('legacy health defaults protocol_version and capabilities', () => {
    expect(protocolHealthInfoDecoder.decode({})).toEqual({
      protocol_version: 0,
      capabilities: [],
    });
  });

  test('malformed health fails on wrong ok kind without leaking body', () => {
    const secret = 'health-secret-body-xyz';
    try {
      protocolHealthResponseDecoder.decode({
        ok: 'yes',
        device_id: 'd1',
        device_name: secret,
        http_port: 1,
        ts: 1,
      });
      expect.unreachable('should throw');
    } catch (reason) {
      expect(reason).toBeInstanceOf(ContractDecodeError);
      const err = reason as ContractDecodeError;
      expect(err.path).toBe('$.ok');
      expect(`${err.message}${err.stack ?? ''}`).not.toContain(secret);
    }
  });

  test('decodes error envelope with default details', () => {
    expect(
      protocolErrorEnvelopeDecoder.decode({
        error: 'x',
        code: 'not_found',
        request_id: 'r1',
        retryable: false,
      }),
    ).toEqual({
      error: 'x',
      code: 'not_found',
      request_id: 'r1',
      retryable: false,
      details: {},
    });
  });

  test('malformed envelope fails when code is not string', () => {
    expect(() =>
      protocolErrorEnvelopeDecoder.decode({
        error: 'x',
        code: 404,
        request_id: 'r1',
        retryable: false,
      }),
    ).toThrow(ContractDecodeError);
  });
});
