/**
 * 工作台按 Git remote fingerprint 分组的纯函数合同。
 *
 * Business Logic（为什么需要这个测试）:
 *   刷新后同一仓库必须合成一项；无 remote、设备筛选、拖拽展开顺序都不能靠 UI 偶然正确。
 *
 * Code Logic（这个测试做什么）:
 *   覆盖 group / display member / 其他设备摘要 / 设备筛选 / 组序展开为 project id。
 */

import { describe, expect, test } from 'vitest';

import type { WorkbenchProject } from './types';
import { DEVICE_FILTER_ALL } from './workbenchProjectDeviceFilter';
import {
  expandGroupOrderToProjectIds,
  filterProjectGroupsByDevice,
  groupWorkbenchProjects,
  otherDeviceNames,
  pickDisplayMember,
  projectGroupKey,
} from './workbenchProjectGroups';

function project(overrides: Partial<WorkbenchProject>): WorkbenchProject {
  return {
    id: 'p1',
    name: 'cc-partner',
    path: '/Users/hans/cc-partner',
    kind: 'local',
    deviceId: 'mac',
    deviceName: 'MacBook',
    lastOpenedAt: '2026-09-01T00:00:00.000Z',
    createdAt: '2026-08-01T00:00:00.000Z',
    updatedAt: '2026-09-01T00:00:00.000Z',
    gitRemoteFingerprint: null,
    ...overrides,
  };
}

describe('workbenchProjectGroups', () => {
  test('projectGroupKey uses fingerprint when present, otherwise project id', () => {
    expect(
      projectGroupKey(project({ gitRemoteFingerprint: 'https://github.com/org/cc-partner' })),
    ).toBe('fp:https://github.com/org/cc-partner');
    expect(projectGroupKey(project({ id: 'solo', gitRemoteFingerprint: null }))).toBe('id:solo');
    expect(projectGroupKey(project({ id: 'empty', gitRemoteFingerprint: '  ' }))).toBe('id:empty');
  });

  test('groups same fingerprint across devices and same-device clones; keeps first-seen order', () => {
    const macMain = project({
      id: 'mac-main',
      path: '/Users/hans/cc-partner',
      gitRemoteFingerprint: 'https://github.com/org/cc-partner',
      lastOpenedAt: '2026-09-10T00:00:00.000Z',
    });
    const notes = project({
      id: 'notes',
      name: 'notes',
      path: '/Users/hans/notes',
      gitRemoteFingerprint: 'https://gitlab.com/hans/notes',
    });
    const ubuntu = project({
      id: 'ubuntu',
      kind: 'remote',
      deviceId: 'ubuntu',
      deviceName: 'Ubuntu',
      path: '/home/hans/cc-partner',
      gitRemoteFingerprint: 'https://github.com/org/cc-partner',
      lastOpenedAt: '2026-09-11T00:00:00.000Z',
    });
    const macClone = project({
      id: 'mac-clone',
      path: '/Users/hans/worktrees/cc-partner-fix',
      gitRemoteFingerprint: 'https://github.com/org/cc-partner',
      lastOpenedAt: '2026-09-09T00:00:00.000Z',
    });
    const scratch = project({
      id: 'scratch',
      name: 'scratch',
      path: '/tmp/scratch',
      gitRemoteFingerprint: null,
    });

    const groups = groupWorkbenchProjects([macMain, notes, ubuntu, macClone, scratch]);
    expect(groups.map((group) => group.key)).toEqual([
      'fp:https://github.com/org/cc-partner',
      'fp:https://gitlab.com/hans/notes',
      'id:scratch',
    ]);
    expect(groups[0]?.members.map((member) => member.id)).toEqual([
      'mac-main',
      'ubuntu',
      'mac-clone',
    ]);
  });

  test('pickDisplayMember prefers lastOpenedAt, then device filter pool', () => {
    const mac = project({
      id: 'mac',
      lastOpenedAt: '2026-09-10T00:00:00.000Z',
    });
    const ubuntu = project({
      id: 'ubuntu',
      kind: 'remote',
      deviceId: 'ubuntu',
      deviceName: 'Ubuntu',
      lastOpenedAt: '2026-09-12T00:00:00.000Z',
    });
    const members = [mac, ubuntu];
    expect(pickDisplayMember(members).id).toBe('ubuntu');
    expect(pickDisplayMember(members, { deviceFilterId: 'mac' }).id).toBe('mac');
    expect(pickDisplayMember(members, { preferredProjectId: 'mac' }).id).toBe('mac');
  });

  test('otherDeviceNames skips the display device and de-dupes', () => {
    const mac = project({ id: 'mac' });
    const ubuntu = project({
      id: 'ubuntu',
      deviceId: 'ubuntu',
      deviceName: 'Ubuntu',
    });
    const macClone = project({ id: 'mac-clone', path: '/tmp/other' });
    expect(otherDeviceNames([mac, ubuntu, macClone], mac)).toEqual(['Ubuntu']);
  });

  test('filterProjectGroupsByDevice keeps a group if any member matches', () => {
    const groups = groupWorkbenchProjects([
      project({
        id: 'mac',
        gitRemoteFingerprint: 'https://github.com/org/cc-partner',
      }),
      project({
        id: 'ubuntu',
        kind: 'remote',
        deviceId: 'ubuntu',
        deviceName: 'Ubuntu',
        gitRemoteFingerprint: 'https://github.com/org/cc-partner',
      }),
      project({
        id: 'notes',
        name: 'notes',
        deviceId: 'mac',
        gitRemoteFingerprint: 'https://gitlab.com/hans/notes',
      }),
    ]);
    expect(filterProjectGroupsByDevice(groups, DEVICE_FILTER_ALL)).toHaveLength(2);
    expect(filterProjectGroupsByDevice(groups, 'ubuntu').map((group) => group.key)).toEqual([
      'fp:https://github.com/org/cc-partner',
    ]);
  });

  test('expandGroupOrderToProjectIds keeps member relative order and hidden groups', () => {
    const fullIds = ['mac-a', 'ubuntu-a', 'notes', 'mac-b'];
    const membersByKey: Record<string, string[]> = {
      A: ['mac-a', 'ubuntu-a'],
      notes: ['notes'],
      B: ['mac-b'],
    };
    const fullGroupKeys = ['A', 'notes', 'B'];
    expect(
      expandGroupOrderToProjectIds({
        fullProjectIds: fullIds,
        fullGroupKeys,
        membersByKey,
        visibleGroupKeysNewOrder: ['B', 'A'],
      }),
    ).toEqual(['mac-b', 'notes', 'mac-a', 'ubuntu-a']);
  });
});
