/**
 * Device API - 局域网设备列表与本机设备信息（Tauri invoke 版本）
 *
 * Business Logic（为什么需要这个模块）:
 *   前端需要获取局域网内其他设备列表，以及获取本机设备信息用于"本机信息"卡片展示。
 *
 * Code Logic（这个模块做什么）:
 *   - list: invoke list_devices，返回局域网内已发现的对端设备。
 *   - health: invoke get_local_device 获取本机设备信息，再组装为旧 health 响应形状，
 *     保持调用处 Devices 页的 toSelfDevice 映射零改动。
 */

import { invoke } from './client';
import type { Device } from '@/lib/types';

/** Rust list_devices 返回的原始 DTO（camelCase，由 serde rename_all 产生） */
interface DeviceDto {
  id: string;
  name: string;
  address: string;
  port: number;
  lastSeen?: string;
  online: boolean;
  isSelf?: boolean;
  /** mDNS 非权威能力提示 */
  capabilities?: string[];
  protoVersion?: number;
  /** 影子设备（经跳板可见）的中转来源设备 id；直连设备无该字段 */
  viaDeviceId?: string;
  /** 影子设备的中转来源设备名 */
  viaDeviceName?: string;
}

/** 对端 resume 能力 token（与 Rust CAPABILITY_TRANSFER_RESUME_V1 对齐）。 */
export const TRANSFER_RESUME_CAPABILITY_V1 = 'transfer.resume.v1';

/** 本机设备信息（对齐旧 /api/health 响应字段，snake_case，供 Devices 页 toSelfDevice 消费） */
export interface HealthResponse {
  ok: boolean;
  device_id: string;
  device_name: string;
  http_port: number;
  ts: number;
}

/**
 * 将 Rust 设备 DTO 归一化为前端 Device 类型。
 *
 * Business Logic（为什么需要）:
 *   Rust 后端用 online:boolean 表示 mDNS 发现状态，而页面和 DeviceCard 统一消费
 *   status:'online'|'offline'。在 API 边界完成转换，避免每个页面重复处理。
 *
 * Code Logic（做什么）:
 *   映射 id/name/address/port/lastSeen 原字段，把 online 布尔值转换为 status 枚举，
 *   并透传影子设备的 viaDeviceId/viaDeviceName 中转链路标记（直连设备为 undefined）。
 */
function toDevice(dto: DeviceDto): Device {
  return {
    id: dto.id,
    name: dto.name,
    address: dto.address,
    port: dto.port,
    status: dto.online ? 'online' : 'offline',
    lastSeen: dto.lastSeen,
    capabilities: Array.isArray(dto.capabilities) ? dto.capabilities : [],
    protoVersion: typeof dto.protoVersion === 'number' ? dto.protoVersion : 0,
    viaDeviceId: dto.viaDeviceId,
    viaDeviceName: dto.viaDeviceName,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Transfer UI「继续传输」必须在旧 peer 上回退为「重新传输」，不能显示假 resume。
 *
 * Code Logic（这个函数做什么）:
 *   capabilities 精确包含 transfer.resume.v1。
 */
export function deviceSupportsTransferResume(device: Pick<Device, 'capabilities'> | null | undefined): boolean {
  const caps = device?.capabilities;
  if (!Array.isArray(caps)) return false;
  return caps.includes(TRANSFER_RESUME_CAPABILITY_V1);
}

export const devicesApi = {
  /** 获取局域网内已发现的设备列表（M3 实现） */
  list: async (): Promise<Device[]> => {
    const list = await invoke<DeviceDto[]>('list_devices');
    return list.map(toDevice);
  },

  /**
   * 获取本机设备信息。
   * Tauri 下本地后端始终在线，复用 get_local_device 组装 HealthResponse。
   */
  health: async (): Promise<HealthResponse> => {
    const local = await invoke<DeviceDto>('get_local_device');
    return {
      ok: true,
      device_id: local.id,
      device_name: local.name,
      http_port: local.port,
      ts: Date.now(),
    };
  },
};
