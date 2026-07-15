/**
 * Transfer API - 文件传输任务（Tauri invoke 版本）
 *
 * Business Logic（为什么需要这个模块）:
 *   传输面板通过 invoke 列出任务、发起发送、取消任务、幂等 retry/resume、operation 对账，
 *   以及 same-device Open/Reveal 准备；返回值必须对齐后端真实 DTO，避免把 send 误当成完整
 *   TransferTask、把 cancel 当成 void，或把非法 phase/operation 写入 UI。
 *
 * Code Logic（这个模块做什么）:
 *   list → list_transfers → TransferTask[]（runtime decode）；
 *   send → send_transfer({deviceId,filePath,clientOperationId}) → SendTransferResult；
 *   cancel → cancel_transfer → CancelTransferResult；
 *   retry/resume → retry_transfer/resume_transfer → TransferTask；
 *   getOperation → get_transfer_operation → TransferOperationStatus；
 *   prepareOpen → prepare_transfer_open → LocalTransferOpenTarget；
 *   open/reveal → prepareOpen + Tauri plugin-opener（权限/平台失败映射稳定本地错误）。
 */

import { openPath, revealItemInDir } from '@tauri-apps/plugin-opener';
import {
  cancelTransferResultDecoder,
  sendTransferResultDecoder,
  transferOperationStatusDecoder,
  transferTaskDecoder,
  transferTasksDecoder,
} from '@/lib/schemas/transfer';
import type {
  CancelTransferResult,
  SendTransferResult,
  TransferOperationStatus,
  TransferTask,
} from '@/lib/types';
import {
  enumDecoder,
  objectDecoder,
  stringDecoder,
  type Decoder,
} from '@/lib/runtimeSchema';
import { invokeDecoded } from './client';

/** same-device Open/Reveal 动作。 */
export type TransferOpenAction = 'open' | 'reveal';

/**
 * prepare_transfer_open 返回的本机目标。
 *
 * Business Logic（为什么需要这个类型）:
 *   GUI 拿到 path 后才调用 opener；P2P/mobile 不得拿到该结构。
 *
 * Code Logic（字段说明）:
 *   对齐 Rust LocalTransferOpenTarget camelCase。
 */
export interface LocalTransferOpenTarget {
  taskId: string;
  action: TransferOpenAction;
  path: string;
}

/**
 * Business Logic（为什么需要这个 decoder）:
 *   prepareOpen 结果必须 fail-closed，不能把损坏 payload 当路径打开。
 *
 * Code Logic（这个 decoder 做什么）:
 *   taskId/action/path 必填；action 仅 open|reveal。
 */
export const localTransferOpenTargetDecoder: Decoder<LocalTransferOpenTarget> = objectDecoder(
  'LocalTransferOpenTarget',
  {
    taskId: stringDecoder,
    action: enumDecoder('TransferOpenAction', ['open', 'reveal'] as const),
    path: stringDecoder,
  },
);

/**
 * 将 plugin-opener 权限/平台失败映射为稳定本地错误文案。
 *
 * Business Logic（为什么需要这个函数）:
 *   opener 失败不得泄漏底层平台细节到远端；本地 UI 需要可识别的 code 前缀。
 *
 * Code Logic（这个函数做什么）:
 *   包装 Error.message 为 `transfer_opener_failed: ...`。
 */
export function mapOpenerError(err: unknown): Error {
  const message =
    err instanceof Error
      ? err.message
      : typeof err === 'string'
        ? err
        : 'opener failed';
  return new Error(`transfer_opener_failed: ${message}`);
}

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
   *   用户选定目标设备与文件路径后发起发送；稳定 clientOperationId 保证 lost ACK 不双发。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded send_transfer({ deviceId, filePath, clientOperationId })，返回 SendTransferResult。
   *   filePath 作为不透明 UTF-8 透传，不做分隔符改写或 URI decode。
   */
  send: (
    deviceId: string,
    filePath: string,
    clientOperationId: string,
  ): Promise<SendTransferResult> =>
    invokeDecoded(
      'send_transfer',
      { deviceId, filePath, clientOperationId },
      sendTransferResultDecoder,
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户取消进行中的传输任务。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded cancel_transfer({ taskId })，返回 CancelTransferResult。
   */
  cancel: (taskId: string): Promise<CancelTransferResult> =>
    invokeDecoded('cancel_transfer', { taskId }, cancelTransferResultDecoder),

  /**
   * Business Logic（为什么需要这个函数）:
   *   失败且可重试时用户点「重新传输」；同一 clientOperationId 幂等，禁止盲重放不同 payload。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded retry_transfer({ taskId, clientOperationId }) → TransferTask。
   */
  retry: (taskId: string, clientOperationId: string): Promise<TransferTask> =>
    invokeDecoded(
      'retry_transfer',
      { taskId, clientOperationId },
      transferTaskDecoder,
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   有 resume metadata 时用户点「继续传输」；稳定 clientOperationId 保证幂等 claim。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded resume_transfer({ taskId, clientOperationId }) → TransferTask。
   */
  resume: (taskId: string, clientOperationId: string): Promise<TransferTask> =>
    invokeDecoded(
      'resume_transfer',
      { taskId, clientOperationId },
      transferTaskDecoder,
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   transport timeout / lost ACK 后必须先对账，再决定是否 retry/resume。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded get_transfer_operation({ clientOperationId }) → TransferOperationStatus。
   */
  getOperation: (clientOperationId: string): Promise<TransferOperationStatus> =>
    invokeDecoded(
      'get_transfer_operation',
      { clientOperationId },
      transferOperationStatusDecoder,
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   Open/Reveal 前必须由 sidecar 校验 Receive+completed+path exists。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded prepare_transfer_open({ taskId, action }) → LocalTransferOpenTarget。
   */
  prepareOpen: (
    taskId: string,
    action: TransferOpenAction,
  ): Promise<LocalTransferOpenTarget> =>
    invokeDecoded(
      'prepare_transfer_open',
      { taskId, action },
      localTransferOpenTargetDecoder,
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户点击「打开」：先 prepare，再 openPath。
   *
   * Code Logic（这个函数做什么）:
   *   prepareOpen(taskId,'open') → openPath(path)；opener 失败 mapOpenerError。
   */
  open: async (taskId: string): Promise<LocalTransferOpenTarget> => {
    const target = await transferApi.prepareOpen(taskId, 'open');
    try {
      await openPath(target.path);
    } catch (err) {
      throw mapOpenerError(err);
    }
    return target;
  },

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户点击「在文件夹中显示」：先 prepare，再 revealItemInDir。
   *
   * Code Logic（这个函数做什么）:
   *   prepareOpen(taskId,'reveal') → revealItemInDir(path)；opener 失败 mapOpenerError。
   */
  reveal: async (taskId: string): Promise<LocalTransferOpenTarget> => {
    const target = await transferApi.prepareOpen(taskId, 'reveal');
    try {
      await revealItemInDir(target.path);
    } catch (err) {
      throw mapOpenerError(err);
    }
    return target;
  },
};
