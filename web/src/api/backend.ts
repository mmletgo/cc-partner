/**
 * Backend lifecycle API - 管理独立后端 sidecar。
 *
 * Business Logic（为什么需要这个模块）:
 *   桌面 GUI 现在只负责 UI 和 browse-only 发现，后台 HTTP/mDNS/自动化任务由 sidecar 负责；
 *   前端关闭弹窗需要调用统一的 status/start/stop/exit 命令。
 *
 * Code Logic（这个模块做什么）:
 *   封装四个 Tauri invoke 命令，组件层只消费类型化 Promise。
 */

import { invoke } from './client';
import type { BackendStatus } from '@/lib/types';

export const backendApi = {
  /** 读取 sidecar 当前状态。 */
  status: () => invoke<BackendStatus>('get_backend_status'),

  /** 启动 sidecar。 */
  start: () => invoke<BackendStatus>('start_backend_process'),

  /** 停止 sidecar。 */
  stop: () => invoke<BackendStatus>('stop_backend_process'),

  /** 退出 GUI 进程。 */
  exitGui: () => invoke<void>('exit_gui'),
};
