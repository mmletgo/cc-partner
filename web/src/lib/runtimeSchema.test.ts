/**
 * runtimeSchema 原语契约测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   decoder 核心是所有域 schema 的底座，必须锁定 path 格式、fail closed 与 payload 脱敏。
 *
 * Code Logic（这个测试做什么）:
 *   覆盖 object/array/string/boolean/number/literal/nullable/optional/union、
 *   精确 path、数组深度/长度上限、ContractDecodeError 不泄露 fixture 正文。
 */

import { describe, expect, test } from 'vitest';
import {
  MAX_ARRAY_DEPTH,
  MAX_ARRAY_LENGTH,
  arrayDecoder,
  booleanDecoder,
  ContractDecodeError,
  enumDecoder,
  literalDecoder,
  nullableDecoder,
  numberDecoder,
  objectDecoder,
  optionalDecoder,
  stringDecoder,
  unionDecoder,
} from './runtimeSchema';

describe('runtimeSchema primitives', () => {
  test('decodes finite number and rejects non-finite', () => {
    expect(numberDecoder.decode(1.5)).toBe(1.5);
    expect(() => numberDecoder.decode(Number.NaN)).toThrow(ContractDecodeError);
    expect(() => numberDecoder.decode(Number.POSITIVE_INFINITY)).toThrow(ContractDecodeError);
    expect(() => numberDecoder.decode('1')).toThrow(ContractDecodeError);
  });

  test('decodes string/boolean and rejects wrong kinds', () => {
    expect(stringDecoder.decode('ok')).toBe('ok');
    expect(booleanDecoder.decode(false)).toBe(false);
    expect(() => stringDecoder.decode(1)).toThrow(ContractDecodeError);
    expect(() => booleanDecoder.decode('true')).toThrow(ContractDecodeError);
  });

  test('literal and enum are strict', () => {
    expect(literalDecoder('live').decode('live')).toBe('live');
    expect(() => literalDecoder('live').decode('cached')).toThrow(ContractDecodeError);
    const status = enumDecoder('status', ['a', 'b'] as const);
    expect(status.decode('a')).toBe('a');
    expect(() => status.decode('c')).toThrow(ContractDecodeError);
  });

  test('nullable accepts null; optional accepts undefined', () => {
    const n = nullableDecoder(stringDecoder);
    expect(n.decode(null)).toBe(null);
    expect(n.decode('x')).toBe('x');
    expect(() => n.decode(undefined)).toThrow(ContractDecodeError);

    const o = optionalDecoder(stringDecoder);
    expect(o.decode(undefined)).toBe(undefined);
    expect(o.decode('y')).toBe('y');
    expect(() => o.decode(null)).toThrow(ContractDecodeError);
  });

  test('object allows unknown extra fields and requires declared fields', () => {
    const dec = objectDecoder('Item', {
      id: stringDecoder,
      count: numberDecoder,
    });
    expect(dec.decode({ id: '1', count: 2, extra: 'ignored' })).toEqual({ id: '1', count: 2 });
    expect(() => dec.decode({ id: '1' })).toThrow(ContractDecodeError);
  });

  test('object defaults only apply when field is missing', () => {
    const dec = objectDecoder(
      'Legacy',
      {
        protocol_version: numberDecoder,
        capabilities: arrayDecoder(stringDecoder),
      },
      { defaults: { protocol_version: 0, capabilities: [] } },
    );
    expect(dec.decode({})).toEqual({ protocol_version: 0, capabilities: [] });
    expect(() => dec.decode({ protocol_version: null })).toThrow(ContractDecodeError);
  });

  test('reports exact path such as $.items[2].status', () => {
    const item = objectDecoder('Row', {
      status: enumDecoder('status', ['ok', 'bad'] as const),
    });
    const root = objectDecoder('Root', {
      items: arrayDecoder(item),
    });
    try {
      root.decode({
        items: [{ status: 'ok' }, { status: 'ok' }, { status: 3 }],
      });
      expect.unreachable('should throw');
    } catch (reason) {
      expect(reason).toBeInstanceOf(ContractDecodeError);
      const err = reason as ContractDecodeError;
      expect(err.path).toBe('$.items[2].status');
      expect(err.contract).toBe('status');
      expect(err.actualKind).toBe('primitive');
    }
  });

  test('enforces max array length and depth', () => {
    const shallow = arrayDecoder(numberDecoder, { maxLength: 2 });
    expect(() => shallow.decode([1, 2, 3])).toThrow(ContractDecodeError);

    const overDepth = arrayDecoder(numberDecoder, {
      depth: MAX_ARRAY_DEPTH + 1,
      maxDepth: MAX_ARRAY_DEPTH,
    });
    expect(() => overDepth.decode([1])).toThrow(ContractDecodeError);
    expect(MAX_ARRAY_LENGTH).toBeGreaterThan(0);
  });

  test('union picks first matching branch', () => {
    type U = { kind: 'a'; n: number } | { kind: 'b'; s: string };
    const dec = unionDecoder<U>('U', [
      objectDecoder('A', { kind: literalDecoder('a'), n: numberDecoder }),
      objectDecoder('B', { kind: literalDecoder('b'), s: stringDecoder }),
    ]);
    expect(dec.decode({ kind: 'b', s: 'x' })).toEqual({ kind: 'b', s: 'x' });
    expect(() => dec.decode({ kind: 'c' })).toThrow(ContractDecodeError);
  });

  test('ContractDecodeError never contains fixture secret/body', () => {
    const secret = 'SUPER_SECRET_TOKEN_should_not_leak';
    const dec = objectDecoder('SecretBag', {
      token: stringDecoder,
      nested: objectDecoder('Nested', { body: stringDecoder }),
    });
    try {
      dec.decode({ token: 123, nested: { body: secret } });
      expect.unreachable('should throw');
    } catch (reason) {
      expect(reason).toBeInstanceOf(ContractDecodeError);
      const err = reason as ContractDecodeError;
      const blob = `${err.message}\n${err.stack ?? ''}\n${err.contract}\n${err.path}\n${err.actualKind}`;
      expect(blob).not.toContain(secret);
      expect(blob).not.toContain('SUPER_SECRET');
    }
  });
});
