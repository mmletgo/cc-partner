/**
 * Mobile Attention HTTP loader（capability-gated）。
 *
 * Business Logic（为什么需要这个模块）:
 *   `/mobile` 浏览器无法 invoke，必须通过同源 HTTP 拉取 Inbox 快照；
 *   旧后端无 attention.v1 时必须明确 unsupported，禁止猜测旧接口。
 *
 * Code Logic（这个模块做什么）:
 *   GET /api/health 检查 capabilities 含 attention.v1 且 protocol_version>=1，
 *   通过后 GET /api/mobile/attention；否则抛出带 kind=unsupported 的错误。
 */

import type { AttentionCategory, AttentionSnapshot } from '@/lib/types';
import { attentionSnapshotDecoder } from '@/lib/schemas/attention';
import { protocolHealthInfoDecoder } from '@/lib/schemas/protocol';
import { getJson, postJson } from './workbenchHttp';

/** P2P capability token：与后端 CAPABILITY_ATTENTION_V1 一致。 */
export const ATTENTION_CAPABILITY_V1 = 'attention.v1' as const;

/** P2P capability token：与后端 CAPABILITY_ATTENTION_V2 一致（含 Agent 投影）。 */
export const ATTENTION_CAPABILITY_V2 = 'attention.v2' as const;

/** Mobile Attention HTTP 路径（v1）。 */
export const ATTENTION_MOBILE_HTTP_PATH = '/api/mobile/attention' as const;

/** Mobile Attention HTTP 路径（v2）。 */
export const ATTENTION_MOBILE_HTTP_PATH_V2 = '/api/mobile/attention/v2' as const;

/** Mobile 标记指定条目已读。 */
export const ATTENTION_MOBILE_MARK_READ_PATH = '/api/mobile/attention/mark-read' as const;

/** Mobile 撤销指定条目已读。 */
export const ATTENTION_MOBILE_MARK_UNREAD_PATH = '/api/mobile/attention/mark-unread' as const;

/** Mobile 全部已读。 */
export const ATTENTION_MOBILE_MARK_ALL_READ_PATH = '/api/mobile/attention/mark-all-read' as const;

/** Mobile 按分类已读。 */
export const ATTENTION_MOBILE_MARK_CATEGORY_READ_PATH =
  '/api/mobile/attention/mark-category-read' as const;

/** 同源 health 路径，用于能力探测。 */
export const ATTENTION_HEALTH_PATH = '/api/health' as const;

/**
 * Attention HTTP GET 默认超时（毫秒）。
 *
 * Business Logic（为什么需要这个常量）:
 *   半开连接或无响应后端会让 mobile Inbox 永久 loading；与 Provider 层超时形成双保险。
 *
 * Code Logic（这个常量做什么）:
 *   传给 getJson 的 timeoutMs，内部 AbortController.abort。
 */
export const ATTENTION_HTTP_TIMEOUT_MS = 30_000;

/**
 * health 响应中与能力探测相关的字段（snake_case，对齐 P2P health）。
 *
 * Business Logic（为什么需要这个类型）:
 *   Mobile loader 只关心 protocol_version 与 capabilities，不必绑定完整 Devices HealthResponse。
 *
 * Code Logic（字段说明）:
 *   protocol_version 缺省 0；capabilities 缺省空数组。
 */
export interface AttentionHealthProtocolInfo {
  protocol_version?: number;
  capabilities?: string[];
}

/**
 * Attention Mobile 加载错误种类。
 *
 * Business Logic（为什么需要这个类型）:
 *   页面需区分 unsupported 与普通网络/协议失败，展示明确 unsupported 态。
 *
 * Code Logic（这个类型做什么）:
 *   unsupported | network | protocol。
 */
export type AttentionHttpErrorKind = 'unsupported' | 'network' | 'protocol';

/**
 * Business Logic（为什么需要这个类）:
 *   调用方不能靠 Error.message 猜 unsupported，需要稳定 kind 字段。
 *
 * Code Logic（这个类做什么）:
 *   扩展 Error，附带 kind 与可选 capability。
 */
export class AttentionHttpError extends Error {
  readonly kind: AttentionHttpErrorKind;
  readonly capability: string | null;

  /**
   * Business Logic（为什么需要这个构造函数）:
   *   统一构造带 kind 的 Attention 加载错误。
   *
   * Code Logic（这个函数做什么）:
   *   设置 message/name/kind/capability。
   */
  constructor(message: string, kind: AttentionHttpErrorKind, capability: string | null = null) {
    super(message);
    this.name = 'AttentionHttpError';
    this.kind = kind;
    this.capability = capability;
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   旧后端可能缺 protocol_version/capabilities；gate 必须安全回落为不支持。
 *
 * Code Logic（这个函数做什么）:
 *   protocol_version>=1 且 capabilities 含 attention.v1 精确匹配时返回 true。
 */
export function supportsAttentionV1(info: AttentionHealthProtocolInfo | null | undefined): boolean {
  if (!info) return false;
  const version = typeof info.protocol_version === 'number' ? info.protocol_version : 0;
  if (version < 1) return false;
  const caps = Array.isArray(info.capabilities) ? info.capabilities : [];
  return caps.includes(ATTENTION_CAPABILITY_V1);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Mobile 优先走 v2 以获取 Agent 投影；无 v2 时回落 v1。
 *
 * Code Logic（这个函数做什么）:
 *   protocol_version>=1 且 capabilities 含 attention.v2。
 */
export function supportsAttentionV2(info: AttentionHealthProtocolInfo | null | undefined): boolean {
  if (!info) return false;
  const version = typeof info.protocol_version === 'number' ? info.protocol_version : 0;
  if (version < 1) return false;
  const caps = Array.isArray(info.capabilities) ? info.capabilities : [];
  return caps.includes(ATTENTION_CAPABILITY_V2);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Mobile Provider 需要在请求 Attention 前探测本机 backend 是否宣告 attention.v1。
 *
 * Code Logic（这个函数做什么）:
 *   GET /api/health；解析 protocol_version/capabilities；不支持则抛 AttentionHttpError(unsupported)。
 */
export async function assertAttentionCapability(
  fetchHealth: () => Promise<AttentionHealthProtocolInfo> = () =>
    getJson<AttentionHealthProtocolInfo>(ATTENTION_HEALTH_PATH, {
      timeoutMs: ATTENTION_HTTP_TIMEOUT_MS,
      decoder: protocolHealthInfoDecoder,
    }),
): Promise<void> {
  let health: AttentionHealthProtocolInfo;
  try {
    health = await fetchHealth();
  } catch (reason) {
    const message = reason instanceof Error ? reason.message : String(reason);
    throw new AttentionHttpError(message, 'network');
  }
  if (!supportsAttentionV1(health)) {
    throw new AttentionHttpError(
      '当前后端不支持 attention.v1',
      'unsupported',
      ATTENTION_CAPABILITY_V1,
    );
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Mobile Inbox 在能力就绪后加载完整 Attention 快照。
 *
 * Code Logic（这个函数做什么）:
 *   先 assertAttentionCapability，再 GET /api/mobile/attention（默认带超时 abort）。
 */
export async function listAttentionSnapshotHttp(
  options: {
    fetchHealth?: () => Promise<AttentionHealthProtocolInfo>;
    fetchSnapshot?: () => Promise<AttentionSnapshot>;
  } = {},
): Promise<AttentionSnapshot> {
  let health: AttentionHealthProtocolInfo | null = null;
  if (options.fetchHealth) {
    await assertAttentionCapability(options.fetchHealth);
  } else {
    try {
      health = await getJson<AttentionHealthProtocolInfo>(ATTENTION_HEALTH_PATH, {
        timeoutMs: ATTENTION_HTTP_TIMEOUT_MS,
        decoder: protocolHealthInfoDecoder,
      });
    } catch (reason) {
      const message = reason instanceof Error ? reason.message : String(reason);
      throw new AttentionHttpError(message, 'network');
    }
    if (!supportsAttentionV1(health) && !supportsAttentionV2(health)) {
      throw new AttentionHttpError(
        '当前后端不支持 attention.v1',
        'unsupported',
        ATTENTION_CAPABILITY_V1,
      );
    }
  }

  const preferV2 = health ? supportsAttentionV2(health) : true;
  const fetchSnapshot =
    options.fetchSnapshot ??
    (async () => {
      if (preferV2) {
        try {
          return await getJson<AttentionSnapshot>(ATTENTION_MOBILE_HTTP_PATH_V2, {
            timeoutMs: ATTENTION_HTTP_TIMEOUT_MS,
            decoder: attentionSnapshotDecoder,
          });
        } catch {
          // 回落 v1
        }
      }
      return getJson<AttentionSnapshot>(ATTENTION_MOBILE_HTTP_PATH, {
        timeoutMs: ATTENTION_HTTP_TIMEOUT_MS,
        decoder: attentionSnapshotDecoder,
      });
    });
  try {
    return await fetchSnapshot();
  } catch (reason) {
    if (reason instanceof AttentionHttpError) throw reason;
    const message = reason instanceof Error ? reason.message : String(reason);
    // workbenchHttp 的 OrchestratorRuntimeTransportError 也带 kind，但这里统一为 protocol/network 语义。
    const kind =
      reason &&
      typeof reason === 'object' &&
      'kind' in reason &&
      (reason as { kind?: string }).kind === 'network'
        ? 'network'
        : 'protocol';
    throw new AttentionHttpError(message, kind);
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   把 mark HTTP 失败统一成 AttentionHttpError，页面不靠裸 Error.message 分支。
 *
 * Code Logic（这个函数做什么）:
 *   已是 AttentionHttpError 原样抛；其余按 kind 映射 network/protocol。
 */
function wrapAttentionHttpMutation(reason: unknown): never {
  if (reason instanceof AttentionHttpError) throw reason;
  const message = reason instanceof Error ? reason.message : String(reason);
  const kind =
    reason &&
    typeof reason === 'object' &&
    'kind' in reason &&
    (reason as { kind?: string }).kind === 'network'
      ? 'network'
      : 'protocol';
  throw new AttentionHttpError(message, kind);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   手机 Inbox 标已读必须写本机仓储，不能只改前端状态。
 *
 * Code Logic（这个函数做什么）:
 *   POST /api/mobile/attention/mark-read，body `{itemIds}`。
 */
export async function markAttentionItemsReadHttp(itemIds: string[]): Promise<AttentionSnapshot> {
  try {
    return await postJson<AttentionSnapshot>(
      ATTENTION_MOBILE_MARK_READ_PATH,
      { itemIds },
      { policy: { kind: 'mutation' }, decoder: attentionSnapshotDecoder },
    );
  } catch (reason) {
    wrapAttentionHttpMutation(reason);
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   误标已读时手机也要能单条撤销。
 *
 * Code Logic（这个函数做什么）:
 *   POST /api/mobile/attention/mark-unread。
 */
export async function markAttentionItemsUnreadHttp(itemIds: string[]): Promise<AttentionSnapshot> {
  try {
    return await postJson<AttentionSnapshot>(
      ATTENTION_MOBILE_MARK_UNREAD_PATH,
      { itemIds },
      { policy: { kind: 'mutation' }, decoder: attentionSnapshotDecoder },
    );
  } catch (reason) {
    wrapAttentionHttpMutation(reason);
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   手机顶部「全部已读」与桌面同语义。
 *
 * Code Logic（这个函数做什么）:
 *   POST /api/mobile/attention/mark-all-read。
 */
export async function markAllAttentionItemsReadHttp(): Promise<AttentionSnapshot> {
  try {
    return await postJson<AttentionSnapshot>(
      ATTENTION_MOBILE_MARK_ALL_READ_PATH,
      {},
      { policy: { kind: 'mutation' }, decoder: attentionSnapshotDecoder },
    );
  } catch (reason) {
    wrapAttentionHttpMutation(reason);
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   按分类清未读，避免窄屏上逐条点。
 *
 * Code Logic（这个函数做什么）:
 *   POST /api/mobile/attention/mark-category-read，body `{category}`。
 */
export async function markAttentionCategoryReadHttp(
  category: AttentionCategory,
): Promise<AttentionSnapshot> {
  try {
    return await postJson<AttentionSnapshot>(
      ATTENTION_MOBILE_MARK_CATEGORY_READ_PATH,
      { category },
      { policy: { kind: 'mutation' }, decoder: attentionSnapshotDecoder },
    );
  } catch (reason) {
    wrapAttentionHttpMutation(reason);
  }
}

/**
 * Mobile Attention API 入口对象。
 *
 * Business Logic（为什么需要这个对象）:
 *   与桌面 attentionApi 形状对齐，Provider 可注入 loadSnapshot 与 mark mutations。
 *
 * Code Logic（这个对象做什么）:
 *   listSnapshot 委托 listAttentionSnapshotHttp；四个 mark 走同源 POST。
 */
export const attentionHttpApi = {
  listSnapshot: (): Promise<AttentionSnapshot> => listAttentionSnapshotHttp(),
  markRead: markAttentionItemsReadHttp,
  markUnread: markAttentionItemsUnreadHttp,
  markAllRead: markAllAttentionItemsReadHttp,
  markCategoryRead: markAttentionCategoryReadHttp,
};
