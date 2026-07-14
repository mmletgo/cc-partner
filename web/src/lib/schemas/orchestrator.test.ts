/**
 * Orchestrator runtime/task/outbox schema fixtures。
 */

import { describe, expect, test } from 'vitest';
import { ContractDecodeError } from '../runtimeSchema';
import {
  orchestratorRemoteOutboxItemDecoder,
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

describe('orchestrator schemas', () => {
  test('decodes task/runtime/outbox/taskView', () => {
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
});
