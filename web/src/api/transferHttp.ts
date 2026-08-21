/**
 * 移动端文件传输 HTTP（主机中转）。
 *
 * Business Logic（为什么需要这个模块）:
 *   `/mobile` 不能走 Tauri send_transfer(filePath)；手机把文件分块上传到主机 staging，
 *   再由主机对本机落盘或对局域网对端 start_sending。任务 JSON 不得携带主机 path。
 *
 * Code Logic（这个模块做什么）:
 *   GET/POST JSON 走 workbenchHttp helpers；chunk 用 fetch + application/octet-stream
 *   与 X-Chunk-Offset；download 用 blob / a[download]，永不把 path 写入 UI。
 */

import {
  cancelTransferResultDecoder,
  transferOperationStatusDecoder,
  transferTaskDecoder,
} from '@/lib/schemas/transfer';
import type {
  CancelTransferResult,
  Device,
  TransferOperationStatus,
  TransferTask,
} from '@/lib/types';
import {
  arrayDecoder,
  booleanDecoder,
  enumDecoder,
  numberDecoder,
  objectDecoder,
  optionalDecoder,
  stringDecoder,
  type Decoder,
} from '@/lib/runtimeSchema';
import { basenameFromPath } from '@/pages/Transfer/transferPageUtils';
import { OrchestratorRuntimeTransportError } from './orchestratorRuntimeTransportError';
import { getJson, postJson } from './workbenchHttp';

/** 与后端 `transfer::CHUNK_SIZE` 对齐：960KiB。 */
export const MOBILE_TRANSFER_CHUNK_SIZE = 960 * 1024;

/** 移动端设备列表（含合成「这台电脑」）。 */
export const MOBILE_TRANSFER_DEVICES_PATH = '/api/mobile/devices' as const;

/** 移动端传输任务列表。 */
export const MOBILE_TRANSFER_TASKS_PATH = '/api/mobile/transfer/tasks' as const;

/** 分块上传初始化。 */
export const MOBILE_TRANSFER_UPLOAD_INIT_PATH = '/api/mobile/transfer/upload/init' as const;

/** 取消进行中任务。 */
export const MOBILE_TRANSFER_CANCEL_PATH = '/api/mobile/transfer/cancel' as const;

/** 失败后重新传输。 */
export const MOBILE_TRANSFER_RETRY_PATH = '/api/mobile/transfer/retry' as const;

/** 失败后继续传输。 */
export const MOBILE_TRANSFER_RESUME_PATH = '/api/mobile/transfer/resume' as const;

/** clientOperationId 对账。 */
export const MOBILE_TRANSFER_GET_OPERATION_PATH = '/api/mobile/transfer/get-operation' as const;

/**
 * 移动端目标设备（主机对端 + 合成本机）。
 *
 * Business Logic（为什么需要这个类型）:
 *   手机必须能选「这台电脑」；isSelf 由主机合成，不能靠 UI 猜 loopback。
 *
 * Code Logic（字段说明）:
 *   复用 Device 核心字段；isSelf=true 表示当前 /mobile 主机。
 */
export interface MobileTransferDevice extends Device {
  isSelf: boolean;
}

/**
 * 上传初始化结果。
 *
 * Business Logic（为什么需要这个类型）:
 *   后续 chunk 必须按 staging id 续传，并从 receivedBytes 对齐 offset。
 *
 * Code Logic（字段说明）:
 *   id 为 staging/upload id；receivedBytes 为已确认字节。
 */
export interface MobileUploadInitResult {
  id: string;
  receivedBytes: number;
}

/**
 * 单块上传结果。
 *
 * Business Logic（为什么需要这个类型）:
 *   下一块 offset 必须以服务端确认字节为准，不能只信本地累加。
 *
 * Code Logic（字段说明）:
 *   receivedBytes 为 staging 已收字节。
 */
export interface MobileUploadChunkResult {
  receivedBytes: number;
}

const deviceStatusDecoder: Decoder<Device['status']> = enumDecoder('DeviceStatus', [
  'online',
  'offline',
] as const);

const mobileTransferDeviceDtoDecoder = objectDecoder('MobileTransferDeviceDto', {
  id: stringDecoder,
  name: stringDecoder,
  address: stringDecoder,
  port: numberDecoder,
  isSelf: booleanDecoder,
  lastSeen: optionalDecoder(stringDecoder),
  capabilities: optionalDecoder(arrayDecoder(stringDecoder)),
  protoVersion: optionalDecoder(numberDecoder),
  status: optionalDecoder(deviceStatusDecoder),
  online: optionalDecoder(booleanDecoder),
});

const mobileUploadInitDecoder: Decoder<MobileUploadInitResult> = objectDecoder(
  'MobileUploadInitResult',
  {
    id: stringDecoder,
    receivedBytes: numberDecoder,
  },
);

const mobileUploadChunkDecoder: Decoder<MobileUploadChunkResult> = objectDecoder(
  'MobileUploadChunkResult',
  {
    receivedBytes: numberDecoder,
  },
);

/**
 * Business Logic（为什么需要这个函数）:
 *   设备下拉需要稳定 isSelf；Rust 可能给 status 或 online 布尔。
 *
 * Code Logic（这个函数做什么）:
 *   解码 DTO 并映射为 MobileTransferDevice；缺省 status 由 online 推导。
 */
export function decodeMobileTransferDevice(raw: unknown): MobileTransferDevice {
  const dto = mobileTransferDeviceDtoDecoder.decode(raw, '$');
  const status: Device['status'] =
    dto.status ?? (dto.online === false ? 'offline' : 'online');
  return {
    id: dto.id,
    name: dto.name,
    address: dto.address,
    port: dto.port,
    status,
    lastSeen: dto.lastSeen,
    capabilities: Array.isArray(dto.capabilities) ? dto.capabilities : [],
    protoVersion: typeof dto.protoVersion === 'number' ? dto.protoVersion : 0,
    isSelf: dto.isSelf,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   列表刷新必须 fail-closed，不能把损坏设备项写进选择器。
 *
 * Code Logic（这个函数做什么）:
 *   解码数组并逐项映射。
 */
export function decodeMobileTransferDevices(raw: unknown): MobileTransferDevice[] {
  if (!Array.isArray(raw)) {
    throw new OrchestratorRuntimeTransportError('设备列表不是数组', 'decode');
  }
  return raw.map((item) => decodeMobileTransferDevice(item));
}

/**
 * Business Logic（为什么需要这个函数）:
 *   任务卡只展示 basename；即便后端误带 path 也不得进入 React state。
 *
 * Code Logic（这个函数做什么）:
 *   复用桌面 TransferTask decoder（接受 serde Option 的 JSON null）；
 *   解码前把 filePath 固定成空串，避免主机 path 进入 React state。
 */
export function decodeMobileTransferTask(raw: unknown): TransferTask {
  const source =
    raw !== null && typeof raw === 'object' && !Array.isArray(raw)
      ? { ...(raw as Record<string, unknown>), filePath: '' }
      : raw;
  const dto = transferTaskDecoder.decode(source, '$');
  return {
    ...dto,
    filePath: '',
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   任务列表是看板权威源，必须整表 fail-closed。
 *
 * Code Logic（这个函数做什么）:
 *   解码数组并 strip path。
 */
export function decodeMobileTransferTasks(raw: unknown): TransferTask[] {
  if (!Array.isArray(raw)) {
    throw new OrchestratorRuntimeTransportError('任务列表不是数组', 'decode');
  }
  return raw.map((item) => decodeMobileTransferTask(item));
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Content-Disposition 只允许取 basename，禁止把主机目录当下载名。
 *
 * Code Logic（这个函数做什么）:
 *   解析 filename / filename*；再经 basenameFromPath 去掉任意分隔符。
 */
export function parseDownloadFileName(
  contentDisposition: string | null,
  fallbackName: string,
): string {
  const rawHeader = contentDisposition ?? '';
  const starMatch = rawHeader.match(/filename\*\s*=\s*(?:UTF-8''|utf-8'')([^;]+)/i);
  const quotedMatch = rawHeader.match(/filename\s*=\s*"([^"]+)"/i);
  const plainMatch = rawHeader.match(/filename\s*=\s*([^;]+)/i);
  const encoded = starMatch?.[1]?.trim();
  const candidate = encoded
    ? safeDecodeUriComponent(encoded)
    : quotedMatch?.[1] ?? plainMatch?.[1]?.trim().replace(/^"+|"+$/g, '') ?? fallbackName;
  const base = basenameFromPath(candidate);
  return base.length > 0 ? base : 'download';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   filename* 可能是百分号编码；解码失败时不得抛到 UI 成 path。
 *
 * Code Logic（这个函数做什么）:
 *   decodeURIComponent，失败则原样返回。
 */
function safeDecodeUriComponent(value: string): string {
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   浏览器下载必须用 blob URL + a[download]，不能打开主机 file://。
 *
 * Code Logic（这个函数做什么）:
 *   创建 object URL，点击隐藏锚点，随后 revoke。
 */
export function triggerBlobDownload(blob: Blob, fileName: string): void {
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement('a');
  anchor.href = url;
  anchor.download = parseDownloadFileName(null, fileName);
  anchor.rel = 'noopener';
  document.body.appendChild(anchor);
  anchor.click();
  anchor.remove();
  window.setTimeout(() => {
    URL.revokeObjectURL(url);
  }, 0);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   chunk/download 的非 JSON fetch 仍要映射 timeout/network/protocol。
 *
 * Code Logic（这个函数做什么）:
 *   AbortError → timeout；其它 TypeError → network。
 */
function transportErrorFromFetch(reason: unknown): OrchestratorRuntimeTransportError {
  if (reason instanceof OrchestratorRuntimeTransportError) return reason;
  const name = reason instanceof Error ? reason.name : '';
  if (name === 'AbortError') {
    return new OrchestratorRuntimeTransportError('请求超时', 'timeout');
  }
  if (reason instanceof TypeError) {
    return new OrchestratorRuntimeTransportError(
      reason.message || '网络不可用',
      'network',
    );
  }
  const message = reason instanceof Error ? reason.message : String(reason);
  return new OrchestratorRuntimeTransportError(message || '请求失败', 'unknown');
}

/**
 * Business Logic（为什么需要这个函数）:
 *   octet-stream 失败体可能是 JSON 信封，需要可读 message。
 *
 * Code Logic（这个函数做什么）:
 *   读 text，优先 JSON error/message，否则回退 status。
 */
async function readFetchErrorMessage(response: Response): Promise<string> {
  const fallback = response.statusText || `HTTP ${response.status}`;
  const text = await response.text().catch(() => '');
  const trimmed = text.trim();
  if (!trimmed) return fallback;
  try {
    const parsed = JSON.parse(trimmed) as { error?: unknown; message?: unknown };
    if (typeof parsed.error === 'string' && parsed.error.trim()) return parsed.error;
    if (typeof parsed.message === 'string' && parsed.message.trim()) return parsed.message;
  } catch {
    return trimmed.slice(0, 240);
  }
  return fallback;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   构造带 overall timeout 的 AbortSignal，避免 chunk/download 挂死。
 *
 * Code Logic（这个函数做什么）:
 *   返回 controller + 清理函数。
 */
function createTimeoutSignal(timeoutMs: number): {
  signal: AbortSignal;
  dispose: () => void;
} {
  const controller = new AbortController();
  const timer = window.setTimeout(() => {
    controller.abort();
  }, timeoutMs);
  return {
    signal: controller.signal,
    dispose: () => {
      window.clearTimeout(timer);
    },
  };
}

export const transferHttp = {
  /**
   * Business Logic（为什么需要这个函数）:
   *   发送目标 = 主机 mDNS 对端 + 合成「这台电脑」。
   *
   * Code Logic（这个函数做什么）:
   *   GET /api/mobile/devices，解码 MobileTransferDevice[]。
   */
  listDevices: (): Promise<MobileTransferDevice[]> =>
    getJson(MOBILE_TRANSFER_DEVICES_PATH, {
      policy: { kind: 'query' },
      decoder: {
        name: 'MobileTransferDevices',
        decode: (value) => decodeMobileTransferDevices(value),
      },
    }),

  /**
   * Business Logic（为什么需要这个函数）:
   *   任务列表是进度/取消/续传的权威源；JSON 不含 path。
   *
   * Code Logic（这个函数做什么）:
   *   GET /api/mobile/transfer/tasks，解码并 strip filePath。
   */
  listTasks: (): Promise<TransferTask[]> =>
    getJson(MOBILE_TRANSFER_TASKS_PATH, {
      policy: { kind: 'query' },
      decoder: {
        name: 'MobileTransferTasks',
        decode: (value) => decodeMobileTransferTasks(value),
      },
    }),

  /**
   * Business Logic（为什么需要这个函数）:
   *   上传前必须拿到 staging id 与已收字节，才能按 offset 续传。
   *
   * Code Logic（这个函数做什么）:
   *   POST init `{filename,size,deviceId,clientOperationId}`。
   */
  initUpload: (input: {
    filename: string;
    size: number;
    deviceId: string;
    clientOperationId: string;
  }): Promise<MobileUploadInitResult> =>
    postJson(MOBILE_TRANSFER_UPLOAD_INIT_PATH, input, {
      policy: { kind: 'mutation' },
      decoder: mobileUploadInitDecoder,
    }),

  /**
   * Business Logic（为什么需要这个函数）:
   *   浏览器只能 file.slice 分块；单块不得超过 CHUNK_SIZE，且禁止 transport 盲重放。
   *
   * Code Logic（这个函数做什么）:
   *   POST octet-stream 到 `/upload/chunk/:id`，带 X-Chunk-Offset。
   */
  uploadChunk: async (
    uploadId: string,
    offset: number,
    body: Blob,
  ): Promise<MobileUploadChunkResult> => {
    const timeout = createTimeoutSignal(30_000);
    try {
      const response = await fetch(
        `/api/mobile/transfer/upload/chunk/${encodeURIComponent(uploadId)}`,
        {
          method: 'POST',
          headers: {
            Accept: 'application/json',
            'Content-Type': 'application/octet-stream',
            'X-Chunk-Offset': String(offset),
          },
          body,
          signal: timeout.signal,
        },
      );
      if (!response.ok) {
        throw new OrchestratorRuntimeTransportError(
          await readFetchErrorMessage(response),
          'protocol',
        );
      }
      const raw: unknown = await response.json();
      return mobileUploadChunkDecoder.decode(raw, '$');
    } catch (reason) {
      throw transportErrorFromFetch(reason);
    } finally {
      timeout.dispose();
    }
  },

  /**
   * Business Logic（为什么需要这个函数）:
   *   分块结束后主机校验并对本机落盘或对端 start_sending。
   *
   * Code Logic（这个函数做什么）:
   *   POST complete/:id，空 JSON 体；响应是剥离 path 的任务 DTO（协议约定，不是 `{accepted}`）。
   */
  completeUpload: (uploadId: string): Promise<TransferTask> =>
    postJson(
      `/api/mobile/transfer/upload/complete/${encodeURIComponent(uploadId)}`,
      {},
      {
        policy: { kind: 'mutation' },
        decoder: {
          name: 'MobileUploadCompleteResult',
          decode: (value) => decodeMobileTransferTask(value),
        },
      },
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   进行中任务只允许取消，与桌面动作矩阵一致。
   *
   * Code Logic（这个函数做什么）:
   *   POST /cancel `{taskId}`。
   */
  cancel: (taskId: string): Promise<CancelTransferResult> =>
    postJson(
      MOBILE_TRANSFER_CANCEL_PATH,
      { taskId },
      { policy: { kind: 'mutation' }, decoder: cancelTransferResultDecoder },
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   失败/取消后重新传输必须带稳定 clientOperationId。
   *
   * Code Logic（这个函数做什么）:
   *   POST /retry，解码任务并 strip path。
   */
  retry: (taskId: string, clientOperationId: string): Promise<TransferTask> =>
    postJson(
      MOBILE_TRANSFER_RETRY_PATH,
      { taskId, clientOperationId },
      {
        policy: { kind: 'mutation' },
        decoder: {
          name: 'MobileTransferRetryResult',
          decode: (value) => decodeMobileTransferTask(value),
        },
      },
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   有 resume metadata 时继续传输，禁止 mint 新 id 盲重放。
   *
   * Code Logic（这个函数做什么）:
   *   POST /resume，解码任务并 strip path。
   */
  resume: (taskId: string, clientOperationId: string): Promise<TransferTask> =>
    postJson(
      MOBILE_TRANSFER_RESUME_PATH,
      { taskId, clientOperationId },
      {
        policy: { kind: 'mutation' },
        decoder: {
          name: 'MobileTransferResumeResult',
          decode: (value) => decodeMobileTransferTask(value),
        },
      },
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   timeout/network 后必须先对账，禁止 blind retry。
   *
   * Code Logic（这个函数做什么）:
   *   POST /get-operation `{clientOperationId}`。
   */
  getOperation: (clientOperationId: string): Promise<TransferOperationStatus> =>
    postJson(
      MOBILE_TRANSFER_GET_OPERATION_PATH,
      { clientOperationId },
      { policy: { kind: 'query' }, decoder: transferOperationStatusDecoder },
    ),

  /**
   * Business Logic（为什么需要这个函数）:
   *   已完成 Receive 只能下载字节流，不能 Open/Reveal 主机路径。
   *
   * Code Logic（这个函数做什么）:
   *   GET download/:taskId → blob → a[download]；文件名来自 Content-Disposition 或调用方 basename。
   */
  download: async (taskId: string, fallbackFileName: string): Promise<void> => {
    const timeout = createTimeoutSignal(180_000);
    try {
      const response = await fetch(
        `/api/mobile/transfer/download/${encodeURIComponent(taskId)}`,
        {
          method: 'GET',
          headers: {
            Accept: 'application/octet-stream',
          },
          signal: timeout.signal,
        },
      );
      if (!response.ok) {
        throw new OrchestratorRuntimeTransportError(
          await readFetchErrorMessage(response),
          'protocol',
        );
      }
      const blob = await response.blob();
      const fileName = parseDownloadFileName(
        response.headers.get('Content-Disposition'),
        fallbackFileName,
      );
      triggerBlobDownload(blob, fileName);
    } catch (reason) {
      throw transportErrorFromFetch(reason);
    } finally {
      timeout.dispose();
    }
  },
};
