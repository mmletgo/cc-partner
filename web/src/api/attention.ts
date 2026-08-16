/**
 * Desktop Attention API。
 *
 * Business Logic（为什么需要这个模块）:
 *   桌面端 Inbox 与侧栏 badge 需要通过 Tauri invoke 拉取本机聚合快照，
 *   并把已读/未读写回本机仓储；优先 v2（含 Agent 投影），旧后端回落 v1。
 *
 * Code Logic（这个模块做什么）:
 *   封装 list / mark-read 系列 invoke，返回 runtime decode 后的 AttentionSnapshot。
 */

import type { AttentionCategory, AttentionSnapshot } from '@/lib/types';
import { attentionSnapshotDecoder } from '@/lib/schemas/attention';
import { invokeDecoded } from './client';

/** 桌面 Attention v1 命令名（无 Agent 变体）。 */
export const ATTENTION_DESKTOP_COMMAND = 'list_attention_items' as const;

/** 桌面 Attention v2 命令名（含 Agent needsInput/failed）。 */
export const ATTENTION_DESKTOP_COMMAND_V2 = 'list_attention_items_v2' as const;

/** 标记指定条目已读。 */
export const ATTENTION_MARK_READ_COMMAND = 'mark_attention_items_read' as const;

/** 撤销指定条目已读。 */
export const ATTENTION_MARK_UNREAD_COMMAND = 'mark_attention_items_unread' as const;

/** 标记当前快照全部条目已读。 */
export const ATTENTION_MARK_ALL_READ_COMMAND = 'mark_all_attention_items_read' as const;

/** 标记某一分类条目已读。 */
export const ATTENTION_MARK_CATEGORY_READ_COMMAND = 'mark_attention_category_read' as const;

/**
 * Business Logic（为什么需要这个对象）:
 *   桌面页面与 AttentionProvider 通过统一 API 入口加载与改已读，避免散落 invoke 字符串。
 *
 * Code Logic（这个对象做什么）:
 *   listSnapshot 优先 v2，失败回落 v1；四个 mark 命令返回新 snapshot。
 */
export const attentionApi = {
  /**
   * Business Logic（为什么需要这个函数）:
   *   Provider 首次挂载与轮询都需要拉取完整 Attention 快照；新版本含 Agent 异常。
   *
   * Code Logic（这个函数做什么）:
   *   先 invoke list_attention_items_v2；若命令不存在/失败则回落 list_attention_items。
   */
  listSnapshot: async (): Promise<AttentionSnapshot> => {
    try {
      return await invokeDecoded(
        ATTENTION_DESKTOP_COMMAND_V2,
        undefined,
        attentionSnapshotDecoder,
      );
    } catch {
      return invokeDecoded(ATTENTION_DESKTOP_COMMAND, undefined, attentionSnapshotDecoder);
    }
  },

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户打开条目或点「标为已读」后，本设备 unread 徽章必须立刻下降。
   *
   * Code Logic（这个函数做什么）:
   *   invoke mark_attention_items_read，解码返回的新 snapshot。
   */
  markRead: (itemIds: string[]): Promise<AttentionSnapshot> =>
    invokeDecoded(ATTENTION_MARK_READ_COMMAND, { itemIds }, attentionSnapshotDecoder),

  /**
   * Business Logic（为什么需要这个函数）:
   *   误标已读时需要单条撤销，不能提供一键全部未读。
   *
   * Code Logic（这个函数做什么）:
   *   invoke mark_attention_items_unread。
   */
  markUnread: (itemIds: string[]): Promise<AttentionSnapshot> =>
    invokeDecoded(ATTENTION_MARK_UNREAD_COMMAND, { itemIds }, attentionSnapshotDecoder),

  /**
   * Business Logic（为什么需要这个函数）:
   *   Inbox 顶部「全部已读」一次性清掉本设备未读徽章。
   *
   * Code Logic（这个函数做什么）:
   *   invoke mark_all_attention_items_read（无参）。
   */
  markAllRead: (): Promise<AttentionSnapshot> =>
    invokeDecoded(ATTENTION_MARK_ALL_READ_COMMAND, undefined, attentionSnapshotDecoder),

  /**
   * Business Logic（为什么需要这个函数）:
   *   按分类清未读，避免用户只能全选或逐条点。
   *
   * Code Logic（这个函数做什么）:
   *   invoke mark_attention_category_read，category 为稳定字面量。
   */
  markCategoryRead: (category: AttentionCategory): Promise<AttentionSnapshot> =>
    invokeDecoded(
      ATTENTION_MARK_CATEGORY_READ_COMMAND,
      { category },
      attentionSnapshotDecoder,
    ),
};
