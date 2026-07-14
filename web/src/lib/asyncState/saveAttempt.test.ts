import { describe, expect, test } from 'vitest';
import {
  createSaveAttempt,
  resolveSaveFailure,
  resolveSaveSuccess,
} from './saveAttempt';

describe('createSaveAttempt', () => {
  test('captures seq, snapshot and edit version', () => {
    expect(createSaveAttempt(3, 'hello', 7)).toEqual({
      requestSeq: 3,
      submittedSnapshot: 'hello',
      submittedEditVersion: 7,
    });
  });
});

describe('resolveSaveSuccess', () => {
  test('success updates baseline without replacing newer draft', () => {
    const result = resolveSaveSuccess({
      attempt: { requestSeq: 1, submittedSnapshot: 'A', submittedEditVersion: 1 },
      currentRequestSeq: 1,
      currentDraft: 'B',
      currentEditVersion: 2,
      serverValue: 'A',
    });
    expect(result).toEqual({
      baseline: 'A',
      draft: 'B',
      dirty: true,
      applied: true,
    });
  });

  test('hydrates draft when no edits occurred during save', () => {
    const result = resolveSaveSuccess({
      attempt: { requestSeq: 2, submittedSnapshot: 'A', submittedEditVersion: 4 },
      currentRequestSeq: 2,
      currentDraft: 'A',
      currentEditVersion: 4,
      serverValue: 'A-server',
    });
    expect(result).toEqual({
      baseline: 'A-server',
      draft: 'A-server',
      dirty: false,
      applied: true,
    });
  });

  test('stale seq does not apply and preserves current draft', () => {
    const result = resolveSaveSuccess({
      attempt: { requestSeq: 1, submittedSnapshot: 'A', submittedEditVersion: 1 },
      currentRequestSeq: 2,
      currentDraft: 'C',
      currentEditVersion: 5,
      serverValue: 'A',
      currentBaseline: 'base',
    });
    expect(result).toEqual({
      baseline: 'base',
      draft: 'C',
      dirty: true,
      applied: false,
    });
  });

  test('object snapshot keeps draft when nested fields diverge during save', () => {
    const submitted = { name: 'desk', host: '10.0.0.1' };
    const currentDraft = { name: 'desk', host: '10.0.0.7' };
    const serverValue = { name: 'desk', host: '10.0.0.1' };
    const result = resolveSaveSuccess({
      attempt: {
        requestSeq: 1,
        submittedSnapshot: submitted,
        submittedEditVersion: 1,
      },
      currentRequestSeq: 1,
      currentDraft,
      currentEditVersion: 2,
      serverValue,
    });
    expect(result.applied).toBe(true);
    expect(result.baseline).toEqual(serverValue);
    expect(result.draft).toEqual(currentDraft);
    expect(result.dirty).toBe(true);
  });
});

describe('resolveSaveFailure', () => {
  test('current failure keeps draft and marks applied', () => {
    const result = resolveSaveFailure({
      attempt: { requestSeq: 1, submittedSnapshot: 'A', submittedEditVersion: 1 },
      currentRequestSeq: 1,
      currentDraft: 'AB',
      currentBaseline: 'A',
    });
    expect(result).toEqual({
      baseline: 'A',
      draft: 'AB',
      dirty: true,
      applied: true,
    });
  });

  test('stale failure is not applied', () => {
    const result = resolveSaveFailure({
      attempt: { requestSeq: 1, submittedSnapshot: 'A', submittedEditVersion: 1 },
      currentRequestSeq: 9,
      currentDraft: 'Z',
      currentBaseline: 'base',
    });
    expect(result.applied).toBe(false);
    expect(result.draft).toBe('Z');
    expect(result.baseline).toBe('base');
  });
});
