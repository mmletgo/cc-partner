/**
 * Device API - 局域网设备列表与本机设备信息（Tauri invoke 版本）
 *
 * Business Logic（为什么需要这个模块）:
 *   前端需要获取局域网内其他设备列表，以及获取本机设备信息用于"本机信息"卡片展示。
 *
 * Code Logic（这个模块做什么）:
 *   - list: invoke list_devices（M3 实现，暂未实现）。
 *   - health: Tauri 下本地后端必然可达，无需就绪探测；本方法用于取本机设备信息。
 *     M3 的 get_local_device 尚未实现，这里复用 get_config 返回的
 *     deviceId/deviceName/httpPort 组装出 HealthResponse（address 固定 127.0.0.1），
 *     保持调用处 Devices 页的 toSelfDevice 映射零改动。
 */

import { invoke } from './client';
import type { AppConfig, Device } from '@/lib/types';

/** 本机设备信息（对齐旧 /api/health 响应字段，snake_case，供 Devices 页 toSelfDevice 消费） */
export interface HealthResponse {
  ok: boolean;
  device_id: string;
  device_name: string;
  http_port: number;
  ts: number;
}

export const devicesApi = {
  /** 获取局域网内已发现的设备列表（M3 实现） */
  list: () => invoke<Device[]>('list_devices'),

  /**
   * 获取本机设备信息。
   * Tauri 下本地后端始终在线，复用 get_config 组装 HealthResponse。
   */
  health: async (): Promise<HealthResponse> => {
    const cfg = await invoke<AppConfig>('get_config');
    return {
      ok: true,
      device_id: cfg.deviceId,
      device_name: cfg.deviceName,
      http_port: cfg.httpPort,
      ts: Date.now(),
    };
  },
};
