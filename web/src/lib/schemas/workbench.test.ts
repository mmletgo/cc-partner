/**
 * Workbench schema fixtures。
 */

import { describe, expect, test } from 'vitest';
import { ContractDecodeError } from '../runtimeSchema';
import {
  workbenchPathInfoDecoder,
  workbenchProjectDecoder,
  workbenchSaveTextResultDecoder,
  workbenchSessionDecoder,
  workbenchWorktreeDecoder,
} from './workbench';

const project = {
  id: 'p1',
  name: 'proj',
  kind: 'local',
  deviceId: 'd1',
  deviceName: 'Mac',
  path: '/tmp/p',
  lastOpenedAt: '2026-07-13T00:00:00.000Z',
  createdAt: '2026-07-13T00:00:00.000Z',
  updatedAt: '2026-07-13T00:00:00.000Z',
};

const worktree = {
  id: 'w1',
  projectId: 'p1',
  name: 'main',
  branch: 'main',
  baseBranch: null,
  path: '/tmp/p',
  isMain: true,
  status: {
    branch: 'main',
    changed: 0,
    ahead: 0,
    behind: 0,
    conflicts: 0,
    clean: true,
    canPush: false,
  },
  createdAt: '2026-07-13T00:00:00.000Z',
  updatedAt: '2026-07-13T00:00:00.000Z',
};

const session = {
  id: 's1',
  projectId: 'p1',
  worktreeId: 'w1',
  name: 'term',
  command: '/bin/zsh',
  cwd: '/tmp/p',
  status: 'running',
  cols: 80,
  rows: 24,
  startedAt: '2026-07-13T00:00:00.000Z',
  exitedAt: null,
  exitCode: null,
  supportsPanes: true,
  paneCount: 1,
};

const pathInfo = {
  name: 'a.ts',
  path: 'a.ts',
  kind: 'file',
  size: 10,
  modifiedAt: '2026-07-13T00:00:00.000Z',
};

describe('workbench schemas', () => {
  test('decodes project/worktree/session/path/save', () => {
    expect(workbenchProjectDecoder.decode(project).id).toBe('p1');
    expect(workbenchWorktreeDecoder.decode(worktree).isMain).toBe(true);
    expect(workbenchSessionDecoder.decode(session).paneCount).toBe(1);
    expect(workbenchPathInfoDecoder.decode(pathInfo).kind).toBe('file');
    expect(
      workbenchSaveTextResultDecoder.decode({
        metadata: pathInfo,
        baseHash: 'h',
        baseModifiedAt: null,
      }).baseHash,
    ).toBe('h');
  });

  test('malformed canPush fails', () => {
    expect(() =>
      workbenchWorktreeDecoder.decode({
        ...worktree,
        status: { ...worktree.status, canPush: 'yes' },
      }),
    ).toThrow(ContractDecodeError);
  });
});
