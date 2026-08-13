/**
 * Workbench Git mutation API 契约测试。
 *
 * Business Logic（为什么需要这个测试）:
 *   commit/push/merge/remove 必须传 clientOperationId 并解码 typed envelope；
 *   getMutationOperation 是 unknown 后对账入口。
 *
 * Code Logic（这个测试做什么）:
 *   mock invoke + 真实 invokeDecoded/decoder；锁定命令名、参数与 envelope 形状。
 */

import { beforeEach, describe, expect, test, vi } from 'vitest';
import { readFileSync } from 'node:fs';
import type { Decoder } from '@/lib/runtimeSchema';
import type {
  WorkbenchMergeResult,
  WorkbenchMutationEnvelope,
  WorkbenchMutationOperation,
  WorkbenchWorktree,
} from '@/lib/types';

const mockInvoke = vi.fn();

vi.mock('./client', () => ({
  invoke: (...args: unknown[]) => mockInvoke(...args),
  invokeDecoded: async <T>(
    cmd: string,
    args: Record<string, unknown> | undefined,
    decoder: Decoder<T>,
  ): Promise<T> => {
    const raw = await mockInvoke(cmd, args);
    return decoder.decode(raw, '$');
  },
}));

import { workbenchApi } from './workbench';

const sampleWorktree: WorkbenchWorktree = {
  id: 'wt-1',
  projectId: 'proj-1',
  name: 'main',
  branch: 'main',
  baseBranch: null,
  path: '/tmp/proj',
  isMain: true,
  canCollectMerge: false,
  homeBranch: null,
  collectibleBranches: [],
  status: {
    branch: 'main',
    changed: 0,
    ahead: 0,
    behind: 0,
    conflicts: 0,
    clean: true,
    canPush: false,
  },
  createdAt: '2026-07-14T00:00:00.000Z',
  updatedAt: '2026-07-14T00:00:00.000Z',
};

const sampleMerge: WorkbenchMergeResult = {
  ok: true,
  worktreeId: 'wt-feat',
  stages: [
    { id: 'checkSource', status: 'completed', message: 'ok' },
    { id: 'closeSessions', status: 'completed', message: 'ok' },
    { id: 'mergeMain', status: 'completed', message: 'ok' },
    { id: 'resolveConflicts', status: 'skipped', message: 'n/a' },
    { id: 'cleanup', status: 'completed', message: 'ok' },
  ],
};

describe('workbenchApi mutation ledger envelope', () => {
  beforeEach(() => {
    mockInvoke.mockReset();
  });

  test('commit passes clientOperationId and decodes succeeded envelope', async () => {
    const envelope: WorkbenchMutationEnvelope<WorkbenchWorktree> = {
      kind: 'succeeded',
      value: sampleWorktree,
      clientOperationId: 'op-commit-1',
    };
    mockInvoke.mockResolvedValueOnce(envelope);

    const result = await workbenchApi.worktrees.commit('wt-1', null, 'op-commit-1');

    expect(mockInvoke).toHaveBeenCalledWith('commit_workbench_worktree', {
      worktreeId: 'wt-1',
      message: null,
      clientOperationId: 'op-commit-1',
    });
    expect(result).toEqual(envelope);
  });

  test('push/merge/remove require clientOperationId and decode envelopes', async () => {
    mockInvoke
      .mockResolvedValueOnce({
        kind: 'unknown',
        clientOperationId: 'op-push',
        transportClass: 'timeout',
      })
      .mockResolvedValueOnce({
        kind: 'succeeded',
        value: sampleMerge,
        clientOperationId: 'op-merge',
      })
      .mockResolvedValueOnce({
        kind: 'succeeded',
        value: { ok: true, worktreeId: 'wt-feat' },
        clientOperationId: 'op-remove',
      });

    await expect(workbenchApi.worktrees.push('wt-1', 'op-push')).resolves.toMatchObject({
      kind: 'unknown',
      clientOperationId: 'op-push',
      transportClass: 'timeout',
    });
    expect(mockInvoke).toHaveBeenNthCalledWith(1, 'push_workbench_worktree', {
      worktreeId: 'wt-1',
      clientOperationId: 'op-push',
    });

    await expect(workbenchApi.worktrees.merge('wt-feat', 'op-merge')).resolves.toMatchObject({
      kind: 'succeeded',
      clientOperationId: 'op-merge',
    });
    expect(mockInvoke).toHaveBeenNthCalledWith(2, 'merge_workbench_worktree', {
      worktreeId: 'wt-feat',
      clientOperationId: 'op-merge',
    });

    await expect(
      workbenchApi.worktrees.remove('wt-feat', false, 'op-remove'),
    ).resolves.toMatchObject({
      kind: 'succeeded',
      value: { ok: true, worktreeId: 'wt-feat' },
    });
    expect(mockInvoke).toHaveBeenNthCalledWith(3, 'remove_workbench_worktree', {
      worktreeId: 'wt-feat',
      force: false,
      clientOperationId: 'op-remove',
    });
  });

  test('getMutationOperation decodes ledger row or null', async () => {
    const op: WorkbenchMutationOperation = {
      clientOperationId: 'op-1',
      kind: 'commit',
      payloadHash: 'abc',
      intent: {
        kind: 'commit',
        projectId: 'proj-1',
        worktreeId: 'wt-1',
        beforeHead: 'h0',
        expectedTree: 't0',
      },
      state: 'running',
      outcome: null,
      errorMessage: null,
      projectId: 'proj-1',
      worktreeId: 'wt-1',
      createdAt: '2026-07-14T00:00:00.000Z',
      updatedAt: '2026-07-14T00:00:01.000Z',
    };
    mockInvoke.mockResolvedValueOnce(op);
    await expect(workbenchApi.worktrees.getMutationOperation('op-1')).resolves.toEqual(op);
    expect(mockInvoke).toHaveBeenCalledWith('get_workbench_mutation_operation', {
      clientOperationId: 'op-1',
    });

    mockInvoke.mockResolvedValueOnce(null);
    await expect(workbenchApi.worktrees.getMutationOperation('missing')).resolves.toBeNull();
  });

  test('source wires mutation commands with clientOperationId and envelope decoders', () => {
    const source = readFileSync(new URL('./workbench.ts', import.meta.url), 'utf8');
    expect(source).toContain("invokeDecoded(\n        'commit_workbench_worktree'");
    expect(source).toContain('clientOperationId');
    expect(source).toContain('workbenchMutationEnvelopeDecoder');
    expect(source).toContain("get_workbench_mutation_operation");
  });

  test('getLaunchSummary invokes get_workbench_launch_summary and decodes sections', async () => {
    const wire = {
      projects: {
        kind: 'ready',
        value: [
          {
            id: 'p1',
            name: 'demo',
            kind: 'local',
            deviceId: 'self',
            deviceName: 'Mac',
            path: '/tmp/demo',
            lastOpenedAt: '2026-07-14T00:00:00.000Z',
          },
        ],
      },
      sessions: { kind: 'ready', value: [] },
      tasks: { kind: 'error', message: 'tasks down' },
      transfers: { kind: 'ready', value: [] },
      generatedAt: '2026-07-14T12:00:00.000Z',
    };
    mockInvoke.mockResolvedValueOnce(wire);

    const result = await workbenchApi.getLaunchSummary();

    expect(mockInvoke).toHaveBeenCalledWith('get_workbench_launch_summary', undefined);
    expect(result.projects.kind).toBe('ready');
    expect(result.tasks).toEqual({ kind: 'error', message: 'tasks down' });
    expect(result.generatedAt).toBe('2026-07-14T12:00:00.000Z');
  });

  test('getLaunchSummary rejects malformed launch payload fail-closed', async () => {
    mockInvoke.mockResolvedValueOnce({ projects: { kind: 'ready' } });
    await expect(workbenchApi.getLaunchSummary()).rejects.toBeTruthy();
  });
});
