import { describe, expect, test } from 'vitest';
import {
  getUnknownMutationClientOperationId,
  isMutationSucceeded,
  isMutationUnknown,
  isWorkbenchMutationUnknownError,
  MUTATION_UNKNOWN_ERROR_CODE,
  mutationSucceeded,
  mutationUnknown,
  WorkbenchMutationUnknownError,
  type WorkbenchMutationEnvelope,
} from './mutationOutcome';

describe('WorkbenchMutationEnvelope helpers', () => {
  test('mutationSucceeded builds typed success envelope', () => {
    const envelope = mutationSucceeded({ head: 'abc' }, 'op-1');
    expect(envelope).toEqual({
      kind: 'succeeded',
      value: { head: 'abc' },
      clientOperationId: 'op-1',
    });
    expect(isMutationSucceeded(envelope)).toBe(true);
    expect(isMutationUnknown(envelope)).toBe(false);
  });

  test('mutationUnknown carries only id and optional transport class', () => {
    const timeout: WorkbenchMutationEnvelope<never> = mutationUnknown('op-2', 'timeout');
    expect(timeout).toEqual({
      kind: 'unknown',
      clientOperationId: 'op-2',
      transportClass: 'timeout',
    });
    expect(isMutationUnknown(timeout)).toBe(true);
    expect(isMutationSucceeded(timeout)).toBe(false);

    const bare = mutationUnknown('op-3');
    expect(bare).toEqual({
      kind: 'unknown',
      clientOperationId: 'op-3',
    });
  });

  test('unknown envelope never invents reconciliation intent fields', () => {
    const envelope = mutationUnknown('op-x', 'network') as Record<string, unknown>;
    expect(envelope).not.toHaveProperty('value');
    expect(envelope).not.toHaveProperty('intent');
    expect(envelope).not.toHaveProperty('beforeHead');
    expect(envelope).not.toHaveProperty('expectedTree');
  });
});

describe('WorkbenchMutationUnknownError typed detection', () => {
  test('instance is detected without reading localized message', () => {
    const err = new WorkbenchMutationUnknownError(
      'op-en',
      'Result unknown. Refresh and verify manually.',
    );
    expect(isWorkbenchMutationUnknownError(err)).toBe(true);
    expect(err.code).toBe(MUTATION_UNKNOWN_ERROR_CODE);
    expect(getUnknownMutationClientOperationId(err)).toBe('op-en');
  });

  test('duck-typed code is accepted for cross-bundle errors', () => {
    const duck = Object.assign(new Error('任意本地化文案'), {
      code: MUTATION_UNKNOWN_ERROR_CODE,
      clientOperationId: 'op-duck',
    });
    expect(isWorkbenchMutationUnknownError(duck)).toBe(true);
    expect(getUnknownMutationClientOperationId(duck)).toBe('op-duck');
  });

  test('Chinese substring alone does not classify as unknown', () => {
    const plain = new Error('操作结果未知，请刷新后人工核对');
    expect(isWorkbenchMutationUnknownError(plain)).toBe(false);
    expect(getUnknownMutationClientOperationId(plain)).toBeNull();
  });

  test('English mutationUnknown substring alone does not classify as unknown', () => {
    const plain = new Error('mutationUnknown');
    expect(isWorkbenchMutationUnknownError(plain)).toBe(false);
  });
});
