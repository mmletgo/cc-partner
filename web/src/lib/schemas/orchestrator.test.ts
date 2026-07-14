/**
 * Orchestrator runtime/task/outbox schema fixtures。
 */

import { describe, expect, test } from 'vitest';
import { ContractDecodeError } from '../runtimeSchema';
import {
  orchestratorEvidenceDecoder,
  orchestratorEvidenceListDecoder,
  orchestratorProjectRefreshResultDecoder,
  orchestratorRemoteOutboxItemDecoder,
  orchestratorReviewDiffDecoder,
  orchestratorReviewDiffResponseDecoder,
  orchestratorRuntimeSnapshotDecoder,
  orchestratorTaskDecoder,
  orchestratorTaskViewDecoder,
} from './orchestrator';

const validTask = {
  id: 't1',
  projectId: 'p1',
  title: 'Title',
  goal: 'Goal',
  acceptanceCriteria: 'AC',
  status: 'queued',
  workflowState: 'todo',
  runState: 'idle',
  attemptPhase: null,
  source: 'internal',
  externalId: null,
  externalIdentifier: null,
  externalUrl: null,
  externalState: null,
  externalLabels: null,
  runnerProvider: null,
  claudeSessionId: null,
  transcriptPath: null,
  runtimeStartedAt: null,
  lastActivityAt: null,
  lastRuntimeEvent: null,
  lastRuntimeMessage: null,
  priority: 0,
  branchName: null,
  worktreeId: null,
  sessionId: null,
  blockedReason: null,
  attempt: 0,
  createdAt: '2026-07-13T00:00:00.000Z',
  updatedAt: '2026-07-13T00:00:00.000Z',
  startedAt: null,
  finishedAt: null,
};

const validRuntime = {
  projectId: 'p1',
  projectKind: 'local',
  remoteStatus: 'local',
  generatedAt: '2026-07-13T00:00:00.000Z',
  latestTickAt: null,
  lastDispatchAt: null,
  lastDispatchedCount: 0,
  schedulerEnabled: true,
  workflowSource: 'builtin',
  workflowValid: true,
  workflowError: null,
  maxConcurrentTasks: 1,
  slotsUsed: 0,
  slotsAvailable: 1,
  latestError: null,
  runningTasks: [],
  retryingTasks: [],
  recentEvents: [],
};

const validOutbox = {
  id: 'o1',
  deviceId: 'd1',
  deviceName: 'Peer',
  remoteProjectPath: '/tmp/p',
  remoteProjectId: null,
  requestJson: '{}',
  status: 'failed',
  remoteTaskId: null,
  lastError: 'offline',
  createdAt: '2026-07-13T00:00:00.000Z',
  updatedAt: '2026-07-13T00:00:00.000Z',
  sentAt: null,
};

const validEvidence = {
  id: 'e1',
  taskId: 't1',
  kind: 'verifier',
  title: 'tests',
  summary: 'passed',
  content: 'ok',
  createdAt: '2026-07-13T00:00:00.000Z',
};

const validRefresh = {
  projectId: 'p1',
  dispatched: 2,
};

describe('orchestrator schemas', () => {
  test('decodes task/runtime/outbox/taskView/evidence/refresh', () => {
    expect(orchestratorTaskDecoder.decode(validTask).id).toBe('t1');
    expect(orchestratorRuntimeSnapshotDecoder.decode(validRuntime).remoteStatus).toBe('local');
    expect(orchestratorRemoteOutboxItemDecoder.decode(validOutbox).status).toBe('failed');
    expect(
      orchestratorTaskViewDecoder.decode({ origin: 'local', task: validTask }),
    ).toMatchObject({ origin: 'local' });
    expect(
      orchestratorTaskViewDecoder.decode({
        origin: 'pendingRemote',
        item: validOutbox,
      }),
    ).toMatchObject({ origin: 'pendingRemote' });
    expect(orchestratorEvidenceDecoder.decode(validEvidence).kind).toBe('verifier');
    expect(orchestratorEvidenceListDecoder.decode([validEvidence])).toHaveLength(1);
    expect(orchestratorProjectRefreshResultDecoder.decode(validRefresh).dispatched).toBe(2);
  });

  test('malformed workflowState fails', () => {
    expect(() =>
      orchestratorTaskDecoder.decode({ ...validTask, workflowState: 'doing' }),
    ).toThrow(ContractDecodeError);
  });

  test('malformed remoteStatus fails', () => {
    expect(() =>
      orchestratorRuntimeSnapshotDecoder.decode({ ...validRuntime, remoteStatus: 'online' }),
    ).toThrow(ContractDecodeError);
  });

  test('malformed evidence content fails', () => {
    expect(() =>
      orchestratorEvidenceDecoder.decode({ ...validEvidence, content: 12 }),
    ).toThrow(ContractDecodeError);
  });

  test('malformed refresh dispatched fails', () => {
    expect(() =>
      orchestratorProjectRefreshResultDecoder.decode({ ...validRefresh, dispatched: '2' }),
    ).toThrow(ContractDecodeError);
  });

  test('decodes review diff snapshot and response wrapper', () => {
    const validReviewDiff = {
      taskId: 't1',
      baseRef: 'main',
      headRef: 'worktree',
      files: [
        {
          path: 'src/a.ts',
          status: 'modified',
          additions: 2,
          deletions: 1,
          patch: '@@ -1 +1 @@\n-a\n+b\n',
          binary: false,
          truncated: false,
        },
        {
          path: 'img.bin',
          status: 'added',
          additions: 0,
          deletions: 0,
          patch: null,
          binary: true,
          truncated: false,
        },
      ],
      totalFiles: 2,
      truncated: false,
      reviewDigest: 'abc123',
    };
    expect(orchestratorReviewDiffDecoder.decode(validReviewDiff).reviewDigest).toBe('abc123');
    expect(
      orchestratorReviewDiffResponseDecoder.decode({ diff: validReviewDiff }).diff.files,
    ).toHaveLength(2);
  });

  test('malformed review digest fails closed', () => {
    expect(() =>
      orchestratorReviewDiffDecoder.decode({
        taskId: 't1',
        baseRef: 'main',
        headRef: 'wt',
        files: [],
        totalFiles: 0,
        truncated: false,
        reviewDigest: 12,
      }),
    ).toThrow(ContractDecodeError);
  });
});
