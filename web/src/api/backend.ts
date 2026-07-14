/**
 * Backend lifecycle API - 管理独立后端 sidecar 与 LAN 披露启动。
 *
 * Business Logic（为什么需要这个模块）:
 *   桌面 GUI 现在只负责 UI 和 browse-only 发现，后台 HTTP/mDNS/自动化任务由 sidecar 负责；
 *   前端关闭弹窗需要调用统一的 status/start/stop/exit 命令；
 *   首次启动须经版本化 LAN 风险确认后才 ensure sidecar。
 *
 * Code Logic（这个模块做什么）:
 *   封装 status/start/stop/exit 与 disclosure 两个 invoke 命令，组件层只消费类型化 Promise。
 */

import { invoke } from './client';
import type { BackendStatus } from '@/lib/types';

/** LAN 披露状态（camelCase，对齐 Rust LanDisclosureStatus）。 */
export type LanDisclosureStatus = {
  required: boolean;
  version: number;
  localAddresses: string[];
  preferredPort: number;
  mdnsPort: number;
  alreadyRunning: boolean;
  actualHttpPort: number | null;
};

/** 确认并启动后的访问信息。 */
export type LanDisclosureStartResult = {
  actualHttpPort: number;
  localAddresses: string[];
  reusedExisting: boolean;
  version: number;
};

export const backendApi = {
  /** 读取 sidecar 当前状态。 */
  status: () => invoke<BackendStatus>('get_backend_status'),

  /** 启动 sidecar。 */
  start: () => invoke<BackendStatus>('start_backend_process'),

  /** 停止 sidecar。 */
  stop: () => invoke<BackendStatus>('stop_backend_process'),

  /** 退出 GUI 进程。 */
  exitGui: () => invoke<void>('exit_gui'),

  /**
   * Business Logic（为什么需要这个函数）:
   *   App 级 gate 挂载时需判断是否展示 LAN 风险确认页。
   *
   * Code Logic（这个函数做什么）:
   *   invoke get_lan_disclosure_status。
   */
  getLanDisclosureStatus: () =>
    invoke<LanDisclosureStatus>('get_lan_disclosure_status'),

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户确认后才 ensure sidecar 并启动 GUI browse 服务。
   *
   * Code Logic（这个函数做什么）:
   *   invoke acknowledge_lan_disclosure_and_start_backend。
   */
  acknowledgeLanDisclosureAndStartBackend: () =>
    invoke<LanDisclosureStartResult>('acknowledge_lan_disclosure_and_start_backend'),
};
