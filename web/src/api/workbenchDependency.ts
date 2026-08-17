/**
 * Workbench 依赖 API - 通过 Tauri invoke 管理 tmux 检测、安装和重新检测。
 *
 * Business Logic（为什么需要这个模块）:
 *   Workbench 的真实 window/pane 体验依赖 tmux，前端需要统一调用后端 dependency manager。
 *   选中远端项目时带 deviceId，Settings/Attention 不传以保持本机。
 *
 * Code Logic（这个模块做什么）:
 *   封装 check/install/status/cancel 四个命令；可选 deviceId 走对端。
 */

import { invoke } from './client';
import type { WorkbenchDependencyStatus } from '@/lib/types';

export const workbenchDependencyApi = {
  /** 检测 tmux 是否可用。 */
  check: (deviceId?: string) =>
    invoke<WorkbenchDependencyStatus>('check_workbench_dependency', {
      deviceId: deviceId ?? null,
    }),

  /** 启动 tmux 安装流程。 */
  install: (deviceId?: string) =>
    invoke<WorkbenchDependencyStatus>('install_workbench_dependency', {
      deviceId: deviceId ?? null,
    }),

  /** 读取当前安装/检测状态。 */
  status: (deviceId?: string) =>
    invoke<WorkbenchDependencyStatus>('get_workbench_dependency_install_status', {
      deviceId: deviceId ?? null,
    }),

  /** 取消正在进行的安装流程。 */
  cancel: (deviceId?: string) =>
    invoke<WorkbenchDependencyStatus>('cancel_workbench_dependency_install', {
      deviceId: deviceId ?? null,
    }),
};
