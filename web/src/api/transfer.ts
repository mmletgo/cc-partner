/**
 * Transfer API - 文件传输任务（Tauri invoke 版本）
 */

import { invoke } from './client';
import type { TransferTask } from '@/lib/types';

export const transferApi = {
  /** 列出传输任务（M5 实现） */
  list: () => invoke<TransferTask[]>('list_transfers'),

  /** 发起文件发送（M5 实现） */
  send: (deviceId: string, filePath: string) =>
    invoke<TransferTask>('send_transfer', { deviceId, filePath }),

  /** 取消传输任务（M5 实现） */
  cancel: (taskId: string) => invoke<void>('cancel_transfer', { taskId }),
};
