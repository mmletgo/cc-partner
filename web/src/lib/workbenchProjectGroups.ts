/**
 * 工作台项目按 Git remote fingerprint 分组。
 *
 * Business Logic（为什么需要这个模块）:
 *   同一仓库在多设备/多路径上各有一行；侧栏要合成一项，工作区内再切换成员。
 *
 * Code Logic（这个模块做什么）:
 *   纯函数：分组键、按列表顺序归组、选取展示成员、设备筛选、组序展开为 project id。
 */

import type { WorkbenchProject } from './types';
import {
  applyVisibleReorderToFullOrder,
  DEVICE_FILTER_ALL,
} from './workbenchProjectDeviceFilter';

/** 一组同一 Git remote（或无 remote 的单独项目）。 */
export interface WorkbenchProjectGroup {
  key: string;
  fingerprint: string | null;
  members: WorkbenchProject[];
}

/**
 * Business Logic（为什么需要这个函数）:
 *   非空 fingerprint 相同才合并；空/缺失必须用项目 id，避免无 remote 目录互并。
 *
 * Code Logic（这个函数做什么）:
 *   trim fingerprint；非空 → `fp:{fingerprint}`，否则 `id:{project.id}`。
 */
export function projectGroupKey(project: WorkbenchProject): string {
  const fingerprint = project.gitRemoteFingerprint?.trim() ?? '';
  if (fingerprint) return `fp:${fingerprint}`;
  return `id:${project.id}`;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   侧栏顺序应保持用户拖拽/添加顺序：第一次见到某组时占位，后续成员并入该组。
 *
 * Code Logic（这个函数做什么）:
 *   按 projects 顺序扫描，Map 保序输出 WorkbenchProjectGroup[]。
 */
export function groupWorkbenchProjects(projects: WorkbenchProject[]): WorkbenchProjectGroup[] {
  const groups: WorkbenchProjectGroup[] = [];
  const indexByKey = new Map<string, number>();
  for (const project of projects) {
    const key = projectGroupKey(project);
    const existing = indexByKey.get(key);
    if (existing === undefined) {
      indexByKey.set(key, groups.length);
      const fingerprint = project.gitRemoteFingerprint?.trim() || null;
      groups.push({ key, fingerprint, members: [project] });
      continue;
    }
    groups[existing]?.members.push(project);
  }
  return groups;
}

export interface PickDisplayMemberOptions {
  preferredProjectId?: string | null;
  deviceFilterId?: string | null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   列表项展示上次打开的那条路径；设备筛选时改用该设备上最近打开的成员。
 *
 * Code Logic（这个函数做什么）:
 *   先按 deviceFilterId 收窄（无匹配则回退全员）；preferredId 在池中则用之，否则 lastOpenedAt 最大。
 */
export function pickDisplayMember(
  members: WorkbenchProject[],
  options: PickDisplayMemberOptions = {},
): WorkbenchProject {
  const first = members[0];
  if (!first) {
    throw new Error('pickDisplayMember requires at least one member');
  }
  const deviceFilterId = options.deviceFilterId?.trim() ?? '';
  let pool = members;
  if (deviceFilterId && deviceFilterId !== DEVICE_FILTER_ALL) {
    const filtered = members.filter((member) => member.deviceId === deviceFilterId);
    if (filtered.length > 0) pool = filtered;
  }
  const preferredId = options.preferredProjectId?.trim() ?? '';
  if (preferredId) {
    const preferred = pool.find((member) => member.id === preferredId);
    if (preferred) return preferred;
  }
  return pool.reduce((latest, member) =>
    member.lastOpenedAt >= latest.lastOpenedAt ? member : latest,
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   列表项要提示「另有 Ubuntu」，不含当前展示设备，同设备多 clone 不算其他设备。
 *
 * Code Logic（这个函数做什么）:
 *   按成员出现顺序收集 deviceName，跳过与 display.deviceId 相同的设备。
 */
export function otherDeviceNames(
  members: WorkbenchProject[],
  display: WorkbenchProject,
): string[] {
  const names: string[] = [];
  const seen = new Set<string>();
  seen.add(display.deviceId);
  for (const member of members) {
    if (seen.has(member.deviceId)) continue;
    seen.add(member.deviceId);
    const name = member.deviceName.trim() || member.deviceId;
    names.push(name);
  }
  return names;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   设备筛选只收窄「哪些仓库出现」，组内仍保留全部成员供下拉切换。
 *
 * Code Logic（这个函数做什么）:
 *   全部设备原样返回；否则保留至少有一名成员 deviceId 匹配的组。
 */
export function filterProjectGroupsByDevice(
  groups: WorkbenchProjectGroup[],
  deviceFilterId: string,
): WorkbenchProjectGroup[] {
  if (deviceFilterId === DEVICE_FILTER_ALL) return groups;
  return groups.filter((group) =>
    group.members.some((member) => member.deviceId === deviceFilterId),
  );
}

export interface ExpandGroupOrderInput {
  fullProjectIds: string[];
  fullGroupKeys: string[];
  membersByKey: Record<string, string[]>;
  visibleGroupKeysNewOrder: string[];
}

/**
 * Business Logic（为什么需要这个函数）:
 *   拖拽的是组，持久化仍是 project id 列表；隐藏设备的组相对位置必须保持。
 *
 * Code Logic（这个函数做什么）:
 *   用 applyVisibleReorderToFullOrder 重排组键，再按组成员原相对序展开。
 */
export function expandGroupOrderToProjectIds(input: ExpandGroupOrderInput): string[] {
  const nextGroupKeys = applyVisibleReorderToFullOrder(
    input.fullGroupKeys,
    input.visibleGroupKeysNewOrder,
  );
  const used = new Set<string>();
  const result: string[] = [];
  for (const key of nextGroupKeys) {
    const members = input.membersByKey[key] ?? [];
    for (const id of members) {
      if (used.has(id)) continue;
      used.add(id);
      result.push(id);
    }
  }
  for (const id of input.fullProjectIds) {
    if (used.has(id)) continue;
    used.add(id);
    result.push(id);
  }
  return result;
}
