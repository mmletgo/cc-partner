import { describe, expect, test } from 'vitest';
import { resolveOrchestratorFocusTarget } from './orchestratorFocus';

describe('resolveOrchestratorFocusTarget', () => {
  test('returns pending while loading', () => {
    expect(
      resolveOrchestratorFocusTarget({
        loading: true,
        focusTaskId: 't1',
        focusOutboxId: null,
        taskIds: [],
        outboxIds: [],
      }),
    ).toEqual({ status: 'pending', kind: 'task', id: 't1' });
  });

  test('returns found for task and outbox', () => {
    expect(
      resolveOrchestratorFocusTarget({
        loading: false,
        focusTaskId: 't1',
        focusOutboxId: null,
        taskIds: ['t1'],
        outboxIds: [],
      }),
    ).toEqual({ status: 'found', kind: 'task', id: 't1' });
    expect(
      resolveOrchestratorFocusTarget({
        loading: false,
        focusTaskId: null,
        focusOutboxId: 'o1',
        taskIds: [],
        outboxIds: ['o1'],
      }),
    ).toEqual({ status: 'found', kind: 'outbox', id: 'o1' });
  });

  test('returns not_found for missing targets', () => {
    expect(
      resolveOrchestratorFocusTarget({
        loading: false,
        focusTaskId: 'missing',
        focusOutboxId: null,
        taskIds: ['t1'],
        outboxIds: [],
      }),
    ).toEqual({ status: 'not_found', kind: 'task', id: 'missing' });
  });
});
