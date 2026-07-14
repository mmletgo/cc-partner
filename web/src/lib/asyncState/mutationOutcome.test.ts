import { describe, expect, test } from 'vitest';
import {
  isMutationSucceeded,
  isMutationUnknown,
  mutationSucceeded,
  mutationUnknown,
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
