import { describe, expect, test } from 'vitest';
import {
  createOperationKey,
  isCurrentOperation,
  nextOperationSequence,
  type WorkbenchOperationKey,
} from './operationContext';

describe('isCurrentOperation', () => {
  const base: WorkbenchOperationKey = {
    projectId: 'p-a',
    worktreeId: 'wt-1',
    sequence: 3,
  };

  test('returns true when project, worktree and sequence match', () => {
    expect(
      isCurrentOperation(base, {
        projectId: 'p-a',
        worktreeId: 'wt-1',
        sequence: 3,
      }),
    ).toBe(true);
  });

  test('returns false when project differs', () => {
    expect(
      isCurrentOperation(base, {
        projectId: 'p-b',
        worktreeId: 'wt-1',
        sequence: 3,
      }),
    ).toBe(false);
  });

  test('returns false when worktree differs including null', () => {
    expect(
      isCurrentOperation(base, {
        projectId: 'p-a',
        worktreeId: 'wt-2',
        sequence: 3,
      }),
    ).toBe(false);
    expect(
      isCurrentOperation(
        { ...base, worktreeId: null },
        { projectId: 'p-a', worktreeId: 'wt-1', sequence: 3 },
      ),
    ).toBe(false);
  });

  test('returns false when sequence differs', () => {
    expect(
      isCurrentOperation(base, {
        projectId: 'p-a',
        worktreeId: 'wt-1',
        sequence: 4,
      }),
    ).toBe(false);
  });
});

describe('nextOperationSequence / createOperationKey', () => {
  test('increments sequence', () => {
    expect(nextOperationSequence(0)).toBe(1);
    expect(nextOperationSequence(41)).toBe(42);
  });

  test('createOperationKey assembles fields', () => {
    expect(createOperationKey('proj', null, 2)).toEqual({
      projectId: 'proj',
      worktreeId: null,
      sequence: 2,
    });
  });
});
