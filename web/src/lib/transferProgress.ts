/**
 * Transfer 进度 / 终态事件解码与列表合并。
 *
 * Business Logic（为什么需要这个模块）:
 *   传输页通过 Tauri listen 收到 progress/status 事件时，损坏 payload 不得部分写入任务列表。
 *
 * Code Logic（这个模块做什么）:
 *   用 schema decoder 解码 raw payload；解码失败返回 null（fail-closed）；
 *   成功则按 id 合并 progress 或 status 字段，生成新 tasks 数组。
 */

import type { TransferStatus, TransferTask } from '@/lib/types';
import {
  transferProgressEventDecoder,
  transferStatusEventDecoder,
  type TransferProgressEvent,
  type TransferStatusEvent,
} from '@/lib/schemas/transfer';
import { ContractDecodeError } from '@/lib/runtimeSchema';

/** 合法 TransferStatus 集合，用于事件层 string → 枚举收敛。 */
const TRANSFER_STATUSES: ReadonlySet<string> = new Set([
  'pending',
  'transferring',
  'completed',
  'failed',
  'cancelled',
]);

/**
 * Business Logic（为什么需要这个函数）:
 *   生产路径需要把 unknown 事件载荷收敛为类型安全的进度事件。
 *
 * Code Logic（这个函数做什么）:
 *   调用 transferProgressEventDecoder.decode；失败抛 ContractDecodeError。
 */
export function decodeTransferProgressEvent(raw: unknown): TransferProgressEvent {
  return transferProgressEventDecoder.decode(raw);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   生产路径需要把 unknown 事件载荷收敛为类型安全的终态事件。
 *
 * Code Logic（这个函数做什么）:
 *   调用 transferStatusEventDecoder.decode；失败抛 ContractDecodeError。
 */
export function decodeTransferStatusEvent(raw: unknown): TransferStatusEvent {
  return transferStatusEventDecoder.decode(raw);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   事件 status 是 string，只有合法 TransferStatus 才能写回任务列表。
 *
 * Code Logic（这个函数做什么）:
 *   命中枚举集合则收窄为 TransferStatus，否则返回 null。
 */
function asTransferStatus(status: string): TransferStatus | null {
  if (TRANSFER_STATUSES.has(status)) {
    return status as TransferStatus;
  }
  return null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   progress 事件需安全合并进现有任务列表，损坏载荷不得污染 UI。
 *
 * Code Logic（这个函数做什么）:
 *   解码 raw；失败返回 null。解码成功后按 id 查找任务：无匹配返回原数组引用；
 *   有匹配则返回浅拷贝后更新 progress 的新数组（不改 speed）。
 */
export function mergeTransferProgressEvent(
  tasks: TransferTask[],
  rawPayload: unknown,
): TransferTask[] | null {
  let event: TransferProgressEvent;
  try {
    event = decodeTransferProgressEvent(rawPayload);
  } catch (reason) {
    if (reason instanceof ContractDecodeError) {
      return null;
    }
    throw reason;
  }

  const index = tasks.findIndex((task) => task.id === event.id);
  if (index < 0) {
    return tasks;
  }

  const next = tasks.slice();
  const current = next[index]!;
  next[index] = {
    ...current,
    progress: event.progress,
  };
  return next;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   completed/failed/cancelled 等终态事件需安全合并 status/errorMessage。
 *
 * Code Logic（这个函数做什么）:
 *   解码 raw；失败返回 null。status 非合法 TransferStatus 时返回 null。
 *   无匹配 id 时返回原数组；有匹配则更新 status 与 errorMessage。
 */
export function mergeTransferStatusEvent(
  tasks: TransferTask[],
  rawPayload: unknown,
): TransferTask[] | null {
  let event: TransferStatusEvent;
  try {
    event = decodeTransferStatusEvent(rawPayload);
  } catch (reason) {
    if (reason instanceof ContractDecodeError) {
      return null;
    }
    throw reason;
  }

  const status = asTransferStatus(event.status);
  if (status == null) {
    return null;
  }

  const index = tasks.findIndex((task) => task.id === event.id);
  if (index < 0) {
    return tasks;
  }

  const next = tasks.slice();
  const current = next[index]!;
  next[index] = {
    ...current,
    status,
    errorMessage: event.errorMessage,
  };
  return next;
}
