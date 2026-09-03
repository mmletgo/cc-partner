/**
 * 中转访问（跳板机）设备列表纯逻辑。
 *
 * Business Logic（为什么需要这个模块）:
 *   经跳板可见的「影子设备」与直连设备共用同一份设备列表（Device.viaDeviceId 标记）。
 *   Picker 渲染、跳板候选过滤、Settings 跳板行组装需要统一的中转判定/去重/统计规则，
 *   避免桌面 Picker、mobile Picker 与 Settings 卡片各自实现一套口径。
 *
 * Code Logic（这个模块做什么）:
 *   无 React / 无 API 依赖的纯函数集合：
 *   - isRelayShadowDevice：viaDeviceId 非空即影子设备
 *   - dedupeRelayShadowDevices：同一 device_id 直连+影子并存时只留直连（防御后端异常数据）
 *   - pickRelayAwarePickerDevices：Picker 展示列表（直连只留在线；影子离线也保留用于置灰提示）
 *   - filterRelayViaCandidates：Settings「添加跳板」候选（本机直连在线非本机设备）
 *   - buildRelayViaRows：Settings 跳板管理行（含每个跳板的影子清单与计数）
 */

import type { Device } from '@/lib/types';

/**
 * Business Logic（为什么需要这个函数）:
 *   设备是否经跳板可见决定 Picker 的中转 Pill、置灰与提示，以及跳板候选的排除。
 *
 * Code Logic（这个函数做什么）:
 *   viaDeviceId 为非空字符串即影子设备；直连设备（含字段缺失）返回 false。
 */
export function isRelayShadowDevice(device: Pick<Device, 'viaDeviceId'>): boolean {
  return typeof device.viaDeviceId === 'string' && device.viaDeviceId !== '';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   影子条目与直连条目可能因探测节奏短暂并存；直连永远优先，
 *   不去重会让同一台设备在 Picker 中出现两次且行为不一致。
 *
 * Code Logic（这个函数做什么）:
 *   一次遍历：先收集直连 id 集合，再丢弃 id 已有直连条目的影子条目；
 *   输出按「直连在前、影子在后」稳定排序，保持同组内原始顺序。
 */
export function dedupeRelayShadowDevices<T extends Device>(devices: readonly T[]): T[] {
  const directIds = new Set<string>();
  for (const device of devices) {
    if (!isRelayShadowDevice(device)) directIds.add(device.id);
  }
  const direct: T[] = [];
  const shadows: T[] = [];
  for (const device of devices) {
    if (isRelayShadowDevice(device)) {
      if (!directIds.has(device.id)) shadows.push(device);
      continue;
    }
    direct.push(device);
  }
  return [...direct, ...shadows];
}

/**
 * Business Logic（为什么需要这个函数）:
 *   远端项目 Picker 的设备列表需要同时呈现「可点的直连在线设备」与
 *   「经跳板可见的影子设备」；影子离线时不能静默消失——用户需要看到
 *   「中转设备不可达或目标已下线」的原因提示，而直连离线沿用现状过滤。
 *
 * Code Logic（这个函数做什么）:
 *   先按 dedupeRelayShadowDevices 去重；直连设备仅保留 online，
 *   影子设备全部保留（离线态由 UI 置灰 + 提示）。
 */
export function pickRelayAwarePickerDevices<T extends Device>(devices: readonly T[]): T[] {
  return dedupeRelayShadowDevices(devices).filter(
    (device) => isRelayShadowDevice(device) || device.status === 'online',
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Settings「添加跳板设备」候选必须是本机直连在线且非本机的设备——
 *   影子设备不能当跳板（单跳硬限制），本机与离线设备不能选。
 *
 * Code Logic（这个函数做什么）:
 *   过滤 status=online、无 viaDeviceId、id 不等于 selfDeviceId 的直连条目。
 */
export function filterRelayViaCandidates<T extends Device>(
  devices: readonly T[],
  selfDeviceId: string,
): T[] {
  return devices.filter(
    (device) =>
      !isRelayShadowDevice(device) &&
      device.status === 'online' &&
      device.id !== selfDeviceId,
  );
}

/** Settings 跳板管理行的影子清单条目。 */
export interface RelayShadowRow {
  id: string;
  name: string;
  status: Device['status'];
}

/** Settings 跳板管理行视图数据。 */
export interface RelayViaRow {
  deviceId: string;
  deviceName: string;
  address: string;
  status: Device['status'];
  /** 该跳板当前可见的影子设备数（在线 + 离线影子都计入展示） */
  shadowCount: number;
  /** 经该跳板可见的影子设备清单（展开区渲染） */
  shadows: RelayShadowRow[];
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Settings 中转卡片需要把「已配置的 viaDeviceIds」投影成用户可读的行：
 *   设备名/地址/在线状态来自直连设备表，「可见 N 台设备」与影子清单来自
 *   viaDeviceId 指向该跳板的影子条目；配置里的设备可能已离线甚至从直连表消失，
 *   行必须仍能渲染（用占位名），否则用户无法移除失效跳板。
 *
 * Code Logic（这个函数做什么）:
 *   对 viaDeviceIds 逐个查直连设备表（无 viaDeviceId 的条目）组装行；
 *   影子清单按传入顺序过滤 id 匹配的影子设备；离线跳板的影子按后端报告的
 *   online 字段投影（后端联动置 offline）。
 */
export function buildRelayViaRows(
  devices: readonly Device[],
  viaDeviceIds: readonly string[],
): RelayViaRow[] {
  const directById = new Map<string, Device>();
  for (const device of devices) {
    if (!isRelayShadowDevice(device) && !directById.has(device.id)) {
      directById.set(device.id, device);
    }
  }
  return viaDeviceIds.map((deviceId) => {
    const direct = directById.get(deviceId) ?? null;
    const shadows: RelayShadowRow[] = devices
      .filter((device) => isRelayShadowDevice(device) && device.viaDeviceId === deviceId)
      .map((device) => ({ id: device.id, name: device.name, status: device.status }));
    return {
      deviceId,
      deviceName: direct?.name ?? deviceId,
      address: direct?.address ?? '',
      status: direct?.status ?? 'offline',
      shadowCount: shadows.length,
      shadows,
    };
  });
}
