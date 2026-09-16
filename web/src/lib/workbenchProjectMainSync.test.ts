/**
 * 跨设备主分支同步规划合同。
 *
 * Business Logic（为什么需要这个测试）:
 *   同步必须只打其他设备上的同一 Git 仓库主工作区，不能把无 remote 或同机 clone 算进去。
 *
 * Code Logic（这个测试做什么）:
 *   覆盖 sibling 过滤、主 worktree 选取与 canSync 门闩。
 */

import { describe, expect, test } from 'vitest';

import type { WorkbenchProject, WorkbenchWorktree } from './types';
import {
  canSyncProjectMain,
  pickMainWorktree,
  siblingProjectsOnOtherDevices,
} from './workbenchProjectMainSync';

function project(overrides: Partial<WorkbenchProject>): WorkbenchProject {
  return {
    id: 'mac',
    name: 'cc-partner',
    path: '/Users/hans/cc-partner',
    kind: 'local',
    deviceId: 'mac',
    deviceName: 'MacBook',
    lastOpenedAt: '2026-09-10T00:00:00.000Z',
    createdAt: '2026-09-01T00:00:00.000Z',
    updatedAt: '2026-09-10T00:00:00.000Z',
    gitRemoteFingerprint: 'github.com/org/cc-partner',
    ...overrides,
  };
}

function worktree(overrides: Partial<WorkbenchWorktree>): WorkbenchWorktree {
  return {
    id: 'wt-main',
    projectId: 'mac',
    name: 'main',
    branch: 'main',
    baseBranch: null,
    path: '/tmp/p',
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
      canPush: true,
    },
    createdAt: '2026-09-01T00:00:00.000Z',
    updatedAt: '2026-09-01T00:00:00.000Z',
    ...overrides,
  };
}

describe('workbenchProjectMainSync', () => {
  test('siblingProjectsOnOtherDevices keeps other devices with the same fingerprint', () => {
    const mac = project({ id: 'mac' });
    const macClone = project({
      id: 'mac-clone',
      path: '/tmp/clone',
      deviceId: 'mac',
    });
    const ubuntu = project({
      id: 'ubuntu',
      kind: 'remote',
      deviceId: 'ubuntu',
      deviceName: 'Ubuntu',
      path: '/home/hans/cc-partner',
    });
    const notes = project({
      id: 'notes',
      gitRemoteFingerprint: 'gitlab.com/hans/notes',
      deviceId: 'ubuntu',
    });
    expect(
      siblingProjectsOnOtherDevices(mac, [mac, macClone, ubuntu, notes]).map((item) => item.id),
    ).toEqual(['ubuntu']);
  });

  test('siblingProjectsOnOtherDevices is empty without fingerprint', () => {
    const local = project({ gitRemoteFingerprint: null });
    const remote = project({
      id: 'ubuntu',
      deviceId: 'ubuntu',
      gitRemoteFingerprint: null,
    });
    expect(siblingProjectsOnOtherDevices(local, [local, remote])).toEqual([]);
  });

  test('pickMainWorktree returns the main worktree', () => {
    const main = worktree({ id: 'main', isMain: true });
    const feat = worktree({ id: 'feat', isMain: false, branch: 'feat' });
    expect(pickMainWorktree([feat, main])?.id).toBe('main');
    expect(pickMainWorktree([feat])).toBeNull();
  });

  test('canSyncProjectMain requires siblings, pushable main, and idle lock', () => {
    const main = worktree({ status: { ...worktree({}).status, canPush: true } });
    expect(
      canSyncProjectMain({
        mainWorktree: main,
        siblingCount: 1,
        worktreeBusy: null,
        remoteWriteDisabled: false,
      }),
    ).toBe(true);
    expect(
      canSyncProjectMain({
        mainWorktree: main,
        siblingCount: 0,
        worktreeBusy: null,
        remoteWriteDisabled: false,
      }),
    ).toBe(false);
    expect(
      canSyncProjectMain({
        mainWorktree: { ...main, status: { ...main.status, canPush: false } },
        siblingCount: 1,
        worktreeBusy: null,
        remoteWriteDisabled: false,
      }),
    ).toBe(false);
    expect(
      canSyncProjectMain({
        mainWorktree: main,
        siblingCount: 1,
        worktreeBusy: 'commit',
        remoteWriteDisabled: false,
      }),
    ).toBe(false);
  });
});
