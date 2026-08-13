/**
 * Workbench schema fixtures。
 */

import { describe, expect, test } from 'vitest';
import { ContractDecodeError } from '../runtimeSchema';
import {
  workbenchFileNodeDecoder,
  workbenchFileNodesDecoder,
  workbenchOpenFileDecoder,
  workbenchPathInfoDecoder,
  workbenchProjectDecoder,
  workbenchProjectNoteDecoder,
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

const fileNode = {
  name: 'src',
  path: 'src',
  kind: 'dir',
  size: null,
  modifiedAt: '2026-07-13T00:00:00.000Z',
  children: [
    {
      name: 'a.ts',
      path: 'src/a.ts',
      kind: 'file',
      size: 10,
      modifiedAt: '2026-07-13T00:00:00.000Z',
    },
  ],
};

const openFile = {
  metadata: pathInfo,
  detectedType: 'code',
  capabilities: {
    canPreview: true,
    canEdit: true,
    canFormat: false,
    mustValidateBeforeSave: false,
    defaultMode: 'editor',
    availableModes: ['editor', 'viewer'],
  },
  text: {
    content: 'const x = 1;\n',
    baseHash: 'h1',
    baseModifiedAt: '2026-07-13T00:00:00.000Z',
  },
  image: null,
  csv: null,
  sqlite: null,
  truncated: false,
  notice: null,
};

describe('workbench schemas', () => {
  test('decodes project/worktree/session/path/save/fileNode/openFile', () => {
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
    expect(workbenchFileNodeDecoder.decode(fileNode).children?.[0]?.name).toBe('a.ts');
    expect(workbenchFileNodesDecoder.decode([fileNode])).toHaveLength(1);
    expect(workbenchOpenFileDecoder.decode(openFile).text?.baseHash).toBe('h1');
    expect(
      workbenchProjectNoteDecoder.decode({
        projectId: 'p1',
        content: '# hello',
        updatedAt: '2026-07-13T00:00:00.000Z',
      }).content,
    ).toBe('# hello');
  });

  test('malformed project note fails closed', () => {
    expect(() =>
      workbenchProjectNoteDecoder.decode({
        projectId: 'p1',
        content: 12,
        updatedAt: 't',
      }),
    ).toThrow(ContractDecodeError);
  });

  test('malformed canPush fails', () => {
    expect(() =>
      workbenchWorktreeDecoder.decode({
        ...worktree,
        status: { ...worktree.status, canPush: 'yes' },
      }),
    ).toThrow(ContractDecodeError);
  });

  test('malformed file node path fails', () => {
    expect(() => workbenchFileNodeDecoder.decode({ ...fileNode, path: 12 })).toThrow(
      ContractDecodeError,
    );
  });

  test('malformed open file capabilities fails', () => {
    expect(() =>
      workbenchOpenFileDecoder.decode({
        ...openFile,
        capabilities: { ...openFile.capabilities, canEdit: 'yes' },
      }),
    ).toThrow(ContractDecodeError);
  });
});
