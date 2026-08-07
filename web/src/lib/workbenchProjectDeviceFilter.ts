import type { WorkbenchProject } from './types';

/** 设备筛选「全部」哨兵值（不是真实 deviceId）。 */
export const DEVICE_FILTER_ALL = '__all__';

/** localStorage key：侧栏按设备筛选偏好（本机 UI 记忆，不跨设备同步）。 */
export const DEVICE_FILTER_STORAGE_KEY = 'cp-workbench-project-device-filter';

export interface WorkbenchDeviceFilterOption {
  deviceId: string;
  deviceName: string;
  isLocal: boolean;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   侧栏设备筛选下拉需要从当前项目列表聚合去重的设备，本机优先便于默认扫视。
 *
 * Code Logic（这个函数做什么）:
 *   按 deviceId 去重；任一条 kind=local 则标 isLocal；本机优先，其余按 deviceName 字典序。
 */
export function collectDeviceFilterOptions(
  projects: WorkbenchProject[],
): WorkbenchDeviceFilterOption[] {
  const byId = new Map<string, WorkbenchDeviceFilterOption>();
  for (const project of projects) {
    const deviceId = project.deviceId?.trim();
    if (!deviceId) continue;
    const existing = byId.get(deviceId);
    if (!existing) {
      byId.set(deviceId, {
        deviceId,
        deviceName: project.deviceName?.trim() || deviceId,
        isLocal: project.kind === 'local',
      });
      continue;
    }
    if (project.kind === 'local') existing.isLocal = true;
    const name = project.deviceName?.trim();
    if (name && (!existing.deviceName || existing.deviceName === deviceId)) {
      existing.deviceName = name;
    }
  }
  return [...byId.values()].sort((a, b) => {
    if (a.isLocal !== b.isLocal) return a.isLocal ? -1 : 1;
    return a.deviceName.localeCompare(b.deviceName, undefined, { sensitivity: 'base' });
  });
}

/**
 * Business Logic（为什么需要这个函数）:
 *   持久化筛选 id 可能指向已消失的设备，必须安全回退到「全部」。
 *
 * Code Logic（这个函数做什么）:
 *   null/空/__all__/未知 deviceId → DEVICE_FILTER_ALL；否则返回原 id。
 */
export function resolveDeviceFilterId(
  stored: string | null | undefined,
  options: WorkbenchDeviceFilterOption[],
): string {
  const value = stored?.trim() ?? '';
  if (!value || value === DEVICE_FILTER_ALL) return DEVICE_FILTER_ALL;
  if (options.some((option) => option.deviceId === value)) return value;
  return DEVICE_FILTER_ALL;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   侧栏展示列表应按当前设备筛选收窄，不改源 projects 数组。
 *
 * Code Logic（这个函数做什么）:
 *   全部时原样返回；否则 filter deviceId 相等。
 */
export function filterProjectsByDevice(
  projects: WorkbenchProject[],
  deviceFilterId: string,
): WorkbenchProject[] {
  if (deviceFilterId === DEVICE_FILTER_ALL) return projects;
  return projects.filter((project) => project.deviceId === deviceFilterId);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   筛选视图下拖拽只重排可见项；其它设备项目相对位置必须保持，再写回全局 ordered_ids。
 *
 * Code Logic（这个函数做什么）:
 *   遍历 fullOrderIds，遇到属于 visible 子集的 id 时按 visibleNewOrderIds 依次替换。
 */
export function applyVisibleReorderToFullOrder(
  fullOrderIds: string[],
  visibleNewOrderIds: string[],
): string[] {
  if (visibleNewOrderIds.length === 0) return [...fullOrderIds];
  const visibleSet = new Set(visibleNewOrderIds);
  let index = 0;
  return fullOrderIds.map((id) => {
    if (!visibleSet.has(id)) return id;
    const next = visibleNewOrderIds[index];
    index += 1;
    return next ?? id;
  });
}

/**
 * Business Logic（为什么需要这个函数）:
 *   重启后恢复上次设备筛选，与 active project 偏好同一 localStorage 模式。
 *
 * Code Logic（这个函数做什么）:
 *   读 key；隐私模式/异常返回 null。
 */
export function readStoredDeviceFilterId(): string | null {
  try {
    return window.localStorage.getItem(DEVICE_FILTER_STORAGE_KEY);
  } catch {
    return null;
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用户切换设备筛选后跨刷新保持；「全部」清除 key 避免脏值。
 *
 * Code Logic（这个函数做什么）:
 *   全部/空 → removeItem；否则 setItem；异常静默。
 */
export function writeStoredDeviceFilterId(deviceFilterId: string | null): void {
  try {
    if (!deviceFilterId || deviceFilterId === DEVICE_FILTER_ALL) {
      window.localStorage.removeItem(DEVICE_FILTER_STORAGE_KEY);
      return;
    }
    window.localStorage.setItem(DEVICE_FILTER_STORAGE_KEY, deviceFilterId);
  } catch {
    // localStorage 不可用时只保留 React 内存态。
  }
}
