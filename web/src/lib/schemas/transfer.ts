/**
 * Transfer 任务 / 受理结果 / 进度事件 / recovery operation 运行时 schema。
 *
 * Business Logic（为什么需要这个模块）:
 *   传输页不得把 send 结果误当完整任务，也不得用损坏 progress 事件或非法 phase/failure/operation
 *   推进 UI；retry/resume/getOperation 边界必须 fail-closed。
 *
 * Code Logic（这个模块做什么）:
 *   解码 TransferTask（含 recovery 字段）、Send/Cancel 结果、progress/status 事件与
 *   TransferOperationStatus 判别联合；phase/failure/operation 闭集，未知值 reject。
 */

import type {
  CancelTransferResult,
  SendTransferResult,
  TransferDirection,
  TransferFailure,
  TransferFailureStage,
  TransferOperationStatus,
  TransferPhase,
  TransferStatus,
  TransferTask,
} from '../types/core';
import {
  arrayDecoder,
  booleanDecoder,
  enumDecoder,
  literalDecoder,
  nullableDecoder,
  numberDecoder,
  objectDecoder,
  optionalDecoder,
  stringDecoder,
  unionDecoder,
  type Decoder,
} from '../runtimeSchema';

const directionDecoder: Decoder<TransferDirection> = enumDecoder('TransferDirection', [
  'send',
  'receive',
] as const);

const statusDecoder: Decoder<TransferStatus> = enumDecoder('TransferStatus', [
  'pending',
  'transferring',
  'completed',
  'failed',
  'cancelled',
] as const);

/**
 * Business Logic（为什么需要这个 decoder）:
 *   phase 驱动动作矩阵；未知后端值不得映射 failed 或 silent 吞掉。
 *
 * Code Logic（这个 decoder 做什么）:
 *   闭集枚举：queued/connecting/transferring/finalizing/completed/cancelled/failed。
 */
export const transferPhaseDecoder: Decoder<TransferPhase> = enumDecoder('TransferPhase', [
  'queued',
  'connecting',
  'transferring',
  'finalizing',
  'completed',
  'cancelled',
  'failed',
] as const);

/**
 * Business Logic（为什么需要这个 decoder）:
 *   failure.stage 决定 retry/resume 展示；非法 stage 不得进入状态。
 *
 * Code Logic（这个 decoder 做什么）:
 *   闭集枚举：connect/transfer/finalize/source/protocol/local/unknown。
 */
export const transferFailureStageDecoder: Decoder<TransferFailureStage> = enumDecoder(
  'TransferFailureStage',
  ['connect', 'transfer', 'finalize', 'source', 'protocol', 'local', 'unknown'] as const,
);

/**
 * Business Logic（为什么需要这个 decoder）:
 *   结构化失败卡需要完整 stage/code/retryable/message。
 *
 * Code Logic（这个 decoder 做什么）:
 *   必填四字段；stage 闭集；retryable 必须是 boolean。
 */
export const transferFailureDecoder: Decoder<TransferFailure> = objectDecoder('TransferFailure', {
  stage: transferFailureStageDecoder,
  code: stringDecoder,
  retryable: booleanDecoder,
  message: stringDecoder,
});

/**
 * Business Logic（为什么需要这个 decoder）:
 *   timeout 后对账结果必须是闭集 status 联合，禁止把未知 tag 当 succeeded。
 *
 * Code Logic（这个 decoder 做什么）:
 *   tag=status 联合：notFound / pending / succeeded{taskId} / failed{code}。
 */
export const transferOperationStatusDecoder: Decoder<TransferOperationStatus> = unionDecoder<
  TransferOperationStatus
>('TransferOperationStatus', [
  objectDecoder('TransferOperationStatusNotFound', {
    status: literalDecoder('notFound'),
  }),
  objectDecoder('TransferOperationStatusPending', {
    status: literalDecoder('pending'),
  }),
  objectDecoder('TransferOperationStatusSucceeded', {
    status: literalDecoder('succeeded'),
    taskId: stringDecoder,
  }),
  objectDecoder('TransferOperationStatusFailed', {
    status: literalDecoder('failed'),
    code: stringDecoder,
  }),
]);

/**
 * Business Logic（为什么需要这个 decoder）:
 *   任务列表是传输页主状态；recovery 字段驱动历史动作。
 *
 * Code Logic（这个 decoder 做什么）:
 *   必填核心字段；可选 peer/speed/error/completedAt/recovery；
 *   phase/failure 存在时严格闭集解码（含 null failure）。
 */
export const transferTaskDecoder: Decoder<TransferTask> = objectDecoder('TransferTask', {
  id: stringDecoder,
  fileName: stringDecoder,
  filePath: stringDecoder,
  fileSize: numberDecoder,
  direction: directionDecoder,
  status: statusDecoder,
  progress: numberDecoder,
  peerDeviceId: optionalDecoder(stringDecoder),
  peerDeviceName: optionalDecoder(stringDecoder),
  speed: optionalDecoder(numberDecoder),
  errorMessage: optionalDecoder(stringDecoder),
  startedAt: stringDecoder,
  completedAt: optionalDecoder(stringDecoder),
  transferredBytes: optionalDecoder(numberDecoder),
  phase: optionalDecoder(transferPhaseDecoder),
  failure: optionalDecoder(nullableDecoder(transferFailureDecoder)),
  attempt: optionalDecoder(numberDecoder),
  logicalTransferId: optionalDecoder(stringDecoder),
  attemptId: optionalDecoder(stringDecoder),
  protocolTransferId: optionalDecoder(stringDecoder),
  clientOperationId: optionalDecoder(nullableDecoder(stringDecoder)),
  operationPayloadHash: optionalDecoder(nullableDecoder(stringDecoder)),
});

/** TransferTask[] decoder。 */
export const transferTasksDecoder: Decoder<TransferTask[]> = arrayDecoder(transferTaskDecoder);

/**
 * Business Logic（为什么需要这个 decoder）:
 *   send 受理结果必须是 accepted:true + id，不能当 TransferTask。
 *
 * Code Logic（这个 decoder 做什么）:
 *   accepted 字面量 true + deviceId/filePath/id。
 */
export const sendTransferResultDecoder: Decoder<SendTransferResult> = objectDecoder(
  'SendTransferResult',
  {
    accepted: literalDecoder(true),
    deviceId: stringDecoder,
    filePath: stringDecoder,
    id: stringDecoder,
  },
);

/**
 * Business Logic（为什么需要这个 decoder）:
 *   cancel 成功确认需 ok:true + id。
 *
 * Code Logic（这个 decoder 做什么）:
 *   ok 字面量 true + id。
 */
export const cancelTransferResultDecoder: Decoder<CancelTransferResult> = objectDecoder(
  'CancelTransferResult',
  {
    ok: literalDecoder(true),
    id: stringDecoder,
  },
);

/**
 * 传输进度事件（listen transfer:progress）。
 *
 * Business Logic（为什么需要这个类型）:
 *   进度条只消费字节与比例字段。
 *
 * Code Logic（字段说明）:
 *   对齐 Rust ProgressPayload camelCase。
 */
export interface TransferProgressEvent {
  id: string;
  transferredBytes: number;
  size: number;
  progress: number;
}

/**
 * 传输终态事件（completed/failed/cancelled）。
 *
 * Business Logic（为什么需要这个类型）:
 *   终态更新任务 status，可选 errorMessage。
 *
 * Code Logic（字段说明）:
 *   对齐 Rust StatusPayload camelCase。
 */
export interface TransferStatusEvent {
  id: string;
  status: string;
  errorMessage?: string;
}

/**
 * Business Logic（为什么需要这个 decoder）:
 *   progress 事件损坏不得驱动进度条。
 *
 * Code Logic（这个 decoder 做什么）:
 *   解码 id/transferredBytes/size/progress。
 */
export const transferProgressEventDecoder: Decoder<TransferProgressEvent> = objectDecoder(
  'TransferProgressEvent',
  {
    id: stringDecoder,
    transferredBytes: numberDecoder,
    size: numberDecoder,
    progress: numberDecoder,
  },
);

/**
 * Business Logic（为什么需要这个 decoder）:
 *   终态事件需至少有 id/status。
 *
 * Code Logic（这个 decoder 做什么）:
 *   status 保持 string（事件层可能扩展）；errorMessage optional。
 */
export const transferStatusEventDecoder: Decoder<TransferStatusEvent> = objectDecoder(
  'TransferStatusEvent',
  {
    id: stringDecoder,
    status: stringDecoder,
    errorMessage: optionalDecoder(stringDecoder),
  },
);
