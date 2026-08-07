/**
 * 工作台侧栏按设备筛选纯函数合同。
 *
 * Business Logic（为什么需要这个测试）:
 *   设备聚合、筛选回退、可见子集重排投影决定侧栏可用性，不能靠 UI 偶然通过。
 *
 * Code Logic（这个测试做什么）:
 *   覆盖 collect/resolve/filter/applyVisibleReorder 与 storage key 常量。
 */

import { describe, expect, test } from 'vitest';

import type { WorkbenchProject } from './types';
import {
  DEVICE_FILTER_ALL,
  DEVICE_FILTER_STORAGE_KEY,
  applyVisibleReorderToFullOrder,
  collectDeviceFilterOptions,
  filterProjectsByDevice,
  resolveDeviceFilterId,
} from './workbenchProjectDeviceFilter';

function project(overrides: Partial<WorkbenchProject>): WorkbenchProject {
  return {
    id: 'p1',
    name: 'demo',
    path: '/tmp/demo',
    kind: 'local',
    deviceId: 'local',
    deviceName: '本机',
    lastOpenedAt: '2026-07-13T00:00:00.000Z',
    createdAt: '2026-07-13T00:00:00.000Z',
    updatedAt: '2026-07-13T00:00:00.000Z',
    ...overrides,
  };
}

describe('workbenchProjectDeviceFilter', () => {
  test('exports stable all sentinel and storage key', () => {
    expect(DEVICE_FILTER_ALL).toBe('__all__');
    expect(DEVICE_FILTER_STORAGE_KEY).toBe('cp-workbench-project-device-filter');
  });

  test('collectDeviceFilterOptions dedupes and puts local first', () => {
    const options = collectDeviceFilterOptions([
      project({ id: 'r1', kind: 'remote', deviceId: 'dev-b', deviceName: 'Beta' }),
      project({ id: 'l1', kind: 'local', deviceId: 'local', deviceName: '本机' }),
      project({ id: 'r2', kind: 'remote', deviceId: 'dev-a', deviceName: 'Alpha' }),
      project({ id: 'r3', kind: 'remote', deviceId: 'dev-b', deviceName: 'Beta' }),
    ]);
    expect(options.map((o) => o.deviceId)).toEqual(['local', 'dev-a', 'dev-b']);
    expect(options[0]?.isLocal).toBe(true);
  });

  test('resolveDeviceFilterId falls back when device disappears', () => {
    const options = collectDeviceFilterOptions([
      project({ deviceId: 'local', deviceName: '本机' }),
    ]);
    expect(resolveDeviceFilterId(null, options)).toBe(DEVICE_FILTER_ALL);
    expect(resolveDeviceFilterId(DEVICE_FILTER_ALL, options)).toBe(DEVICE_FILTER_ALL);
    expect(resolveDeviceFilterId('local', options)).toBe('local');
    expect(resolveDeviceFilterId('gone', options)).toBe(DEVICE_FILTER_ALL);
  });

  test('filterProjectsByDevice keeps relative order', () => {
    const projects = [
      project({ id: 'a', deviceId: 'local' }),
      project({ id: 'b', kind: 'remote', deviceId: 'dev-a', deviceName: 'A' }),
      project({ id: 'c', deviceId: 'local' }),
    ];
    expect(filterProjectsByDevice(projects, DEVICE_FILTER_ALL).map((p) => p.id)).toEqual([
      'a',
      'b',
      'c',
    ]);
    expect(filterProjectsByDevice(projects, 'local').map((p) => p.id)).toEqual(['a', 'c']);
    expect(filterProjectsByDevice(projects, 'dev-a').map((p) => p.id)).toEqual(['b']);
  });

  test('applyVisibleReorderToFullOrder only rewrites visible slots', () => {
    // full: L1, R1, L2, R2  →  filter local visible L1,L2 reorder to L2,L1
    const full = ['L1', 'R1', 'L2', 'R2'];
    const next = applyVisibleReorderToFullOrder(full, ['L2', 'L1']);
    expect(next).toEqual(['L2', 'R1', 'L1', 'R2']);
  });
});
