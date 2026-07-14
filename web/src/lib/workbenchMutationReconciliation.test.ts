/**
 * Workbench mutation 对账矩阵表驱动测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   对账错误会导致盲重放或把未完成操作当成功；必须锁定与 Rust confirm_mutation 一致的后置条件，
 *   以及 ledger 终态优先合同。
 *
 * Code Logic（这个测试做什么）:
 *   用 table 覆盖 commit/push/merge/remove 的成功与 unknown 分支，以及 ledger 终态/缺失/pending。
 */

import { describe, expect, test } from 'vitest';
import {
  buildMergeRemoveAuthority,
  reconcileWorkbenchMutation,
  type WorkbenchMutationReconcileResult,
} from './workbenchMutationReconciliation';
import type {
  MutationAuthoritySnapshot,
  MutationIntent,
  WorkbenchMutationOperation,
} from '@/lib/types';

function buildLedger(
  overrides: Partial<WorkbenchMutationOperation> & { intent: MutationIntent },
): WorkbenchMutationOperation {
  const { intent, ...rest } = overrides;
  return {
    clientOperationId: 'op-1',
    kind: intent.kind,
    payloadHash: 'hash',
    state: 'running',
    outcome: null,
    errorMessage: null,
    projectId: 'proj',
    worktreeId: 'wt',
    createdAt: '2026-07-14T00:00:00.000Z',
    updatedAt: '2026-07-14T00:00:00.000Z',
    ...rest,
    intent,
  };
}

const commitIntent: MutationIntent = {
  kind: 'commit',
  projectId: 'proj',
  worktreeId: 'wt',
  beforeHead: 'aaa',
  expectedTree: 'tree-1',
};

const pushIntent: MutationIntent = {
  kind: 'push',
  projectId: 'proj',
  worktreeId: 'wt',
  localRef: 'refs/heads/feature',
  remoteRef: 'refs/remotes/origin/feature',
  localHead: 'bbb',
};

const mergeIntent: MutationIntent = {
  kind: 'merge',
  projectId: 'proj',
  sourceWorktreeId: 'wt-feat',
  sourceHead: 'ccc',
  mainHead: 'ddd',
};

const removeIntent: MutationIntent = {
  kind: 'remove',
  projectId: 'proj',
  worktreeId: 'wt-feat',
  path: '/tmp/wt-feat',
  branch: 'feature/x',
};

describe('reconcileWorkbenchMutation', () => {
  test.each([
    {
      name: 'commit: new commit parent+tree match',
      intent: commitIntent,
      authority: {
        head: 'eee',
        headTree: 'tree-1',
        headParent: 'aaa',
      } satisfies MutationAuthoritySnapshot,
      expected: 'confirmedSucceeded' as WorkbenchMutationReconcileResult,
    },
    {
      name: 'commit: no-op same head + tree',
      intent: commitIntent,
      authority: {
        head: 'aaa',
        headTree: 'tree-1',
        headParent: null,
      },
      expected: 'confirmedSucceeded',
    },
    {
      name: 'commit: tree mismatch stays unknown',
      intent: commitIntent,
      authority: {
        head: 'eee',
        headTree: 'other-tree',
        headParent: 'aaa',
      },
      expected: 'unknown',
    },
    {
      name: 'push: remote matches local head',
      intent: pushIntent,
      authority: { remoteRefHead: 'bbb' },
      expected: 'confirmedSucceeded',
    },
    {
      name: 'push: remote mismatch stays unknown',
      intent: pushIntent,
      authority: { remoteRefHead: 'zzz' },
      expected: 'unknown',
    },
    {
      name: 'merge: main contains source and source gone',
      intent: mergeIntent,
      authority: {
        mainContainsSourceHead: true,
        sourceWorktreePresent: false,
      },
      expected: 'confirmedSucceeded',
    },
    {
      name: 'merge: source still present stays unknown',
      intent: mergeIntent,
      authority: {
        mainContainsSourceHead: true,
        sourceWorktreePresent: true,
      },
      expected: 'unknown',
    },
    {
      name: 'merge: missing mainContainsSourceHead stays unknown',
      intent: mergeIntent,
      authority: {
        mainContainsSourceHead: false,
        sourceWorktreePresent: false,
      },
      expected: 'unknown',
    },
    {
      name: 'remove: identity absent',
      intent: removeIntent,
      authority: { worktreeIdentityPresent: false },
      expected: 'confirmedSucceeded',
    },
    {
      name: 'remove: identity still present',
      intent: removeIntent,
      authority: { worktreeIdentityPresent: true },
      expected: 'unknown',
    },
    {
      name: 'remove: identity unknown field',
      intent: removeIntent,
      authority: {},
      expected: 'unknown',
    },
  ])('$name', ({ intent, authority, expected }) => {
    expect(reconcileWorkbenchMutation(intent, null, authority)).toBe(expected);
  });

  test('ledger null/pending does not block pure matrix confirmation', () => {
    const authority: MutationAuthoritySnapshot = {
      head: 'eee',
      headTree: 'tree-1',
      headParent: 'aaa',
    };
    expect(reconcileWorkbenchMutation(commitIntent, null, authority)).toBe(
      'confirmedSucceeded',
    );
    expect(reconcileWorkbenchMutation(commitIntent, undefined, authority)).toBe(
      'confirmedSucceeded',
    );
    const pending = buildLedger({ intent: commitIntent, state: 'running' });
    expect(reconcileWorkbenchMutation(commitIntent, pending, authority)).toBe(
      'confirmedSucceeded',
    );
    const claimed = buildLedger({ intent: commitIntent, state: 'claimed' });
    expect(reconcileWorkbenchMutation(commitIntent, claimed, authority)).toBe(
      'confirmedSucceeded',
    );
  });

  test('ledger terminal succeeded wins even without authority fields', () => {
    const ledger = buildLedger({ intent: commitIntent, state: 'succeeded' });
    expect(reconcileWorkbenchMutation(commitIntent, ledger, {})).toBe(
      'confirmedSucceeded',
    );
  });

  test('ledger terminal failed is definitive without authority matrix', () => {
    const ledger = buildLedger({
      intent: pushIntent,
      state: 'failed',
      errorMessage: 'rejected',
    });
    expect(reconcileWorkbenchMutation(pushIntent, ledger, { remoteRefHead: 'bbb' })).toBe(
      'confirmedFailed',
    );
  });

  test('intent mismatch still honors terminal ledger state first', () => {
    const ledger = buildLedger({ intent: pushIntent, state: 'succeeded' });
    expect(
      reconcileWorkbenchMutation(commitIntent, ledger, {
        head: 'eee',
        headTree: 'tree-1',
        headParent: 'aaa',
      }),
    ).toBe('confirmedSucceeded');
  });
});

describe('buildMergeRemoveAuthority', () => {
  test('merge populates source presence and mainContainsSourceHead from hashes', () => {
    expect(
      buildMergeRemoveAuthority(
        mergeIntent,
        [{ id: 'wt-main' }],
        { mainCommitHashes: ['ccc', 'ddd'] },
      ),
    ).toEqual({
      sourceWorktreePresent: false,
      mainContainsSourceHead: true,
    });
  });

  test('merge sets mainContainsSourceHead false when source head not in list', () => {
    expect(
      buildMergeRemoveAuthority(
        mergeIntent,
        [{ id: 'wt-feat' }, { id: 'wt-main' }],
        { mainCommitHashes: ['zzz'] },
      ),
    ).toEqual({
      sourceWorktreePresent: true,
      mainContainsSourceHead: false,
    });
  });

  test('merge omits mainContainsSourceHead when hashes not provided', () => {
    expect(buildMergeRemoveAuthority(mergeIntent, [{ id: 'wt-main' }])).toEqual({
      sourceWorktreePresent: false,
      mainContainsSourceHead: undefined,
    });
  });

  test('remove populates identity presence', () => {
    expect(buildMergeRemoveAuthority(removeIntent, [{ id: 'wt-other' }])).toEqual({
      worktreeIdentityPresent: false,
    });
    expect(buildMergeRemoveAuthority(removeIntent, [{ id: 'wt-feat' }])).toEqual({
      worktreeIdentityPresent: true,
    });
  });
});
