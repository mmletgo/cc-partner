/**
 * Transfer API - 文件传输任务（Tauri invoke 版本）
 *
 * Business Logic（为什么需要这个模块）:
 *   传输面板通过 invoke 列出任务、发起发送、取消任务；返回值必须对齐后端真实 DTO，
 *   避免把 send 误当成完整 TransferTask 或把 cancel 当成 void。
 *
 * Code Logic（这个模块做什么）:
 *   list → list_transfers → TransferTask[]（runtime decode）；
 *   send → send_transfer → SendTransferResult；
 *   cancel → cancel_transfer → CancelTransferResult。
 */

import {
  cancelTransferResultDecoder,
  sendTransferResultDecoder,
  transferTasksDecoder,
} from '@/lib/schemas/transfer';
import type {
  CancelTransferResult,
  SendTransferResult,
  TransferTask,
} from '@/lib/types';
import { invokeDecoded } from './client';

export const transferApi = {
  /**
   * Business Logic（为什么需要这个函数）:
   *   传输页需要展示活跃任务与历史任务列表。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded list_transfers，返回 TransferTask[]。
   */
  list: (): Promise<TransferTask[]> =>
    invokeDecoded('list_transfers', undefined, transferTasksDecoder),

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户选定目标设备与文件路径后发起发送；后端 spawn 异步任务并立即受理。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded send_transfer({ deviceId, filePath })，返回 SendTransferResult。
   *   filePath 作为不透明 UTF-8 透传，不做分隔符改写或 URI decode。
   */
  send: (deviceId: string, filePath: string): Promise<SendTransferResult> =>
    invokeDecoded('send_transfer', { deviceId, filePath }, sendTransferResultDecoder),

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户取消进行中的传输任务。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded cancel_transfer({ taskId })，返回 CancelTransferResult。
   */
  cancel: (taskId: string): Promise<CancelTransferResult> =>
    invokeDecoded('cancel_transfer', { taskId }, cancelTransferResultDecoder),
};
