/**
 * Desktop Attention API。
 *
 * Business Logic（为什么需要这个模块）:
 *   桌面端 Inbox 与侧栏 badge 需要通过 Tauri invoke 拉取本机聚合快照；
 *   优先 v2（含 Agent 投影），旧后端回落 v1。
 *
 * Code Logic（这个模块做什么）:
 *   封装 `list_attention_items_v2` / `list_attention_items` invoke，返回 runtime decode 后的 AttentionSnapshot。
 */

import type { AttentionSnapshot } from '@/lib/types';
import { attentionSnapshotDecoder } from '@/lib/schemas/attention';
import { invokeDecoded } from './client';

/** 桌面 Attention v1 命令名（无 Agent 变体）。 */
export const ATTENTION_DESKTOP_COMMAND = 'list_attention_items' as const;

/** 桌面 Attention v2 命令名（含 Agent needsInput/failed）。 */
export const ATTENTION_DESKTOP_COMMAND_V2 = 'list_attention_items_v2' as const;

/**
 * Business Logic（为什么需要这个对象）:
 *   桌面页面与 AttentionProvider 通过统一 API 入口加载快照，避免散落 invoke 字符串。
 *
 * Code Logic（这个对象做什么）:
 *   listSnapshot 优先 v2，失败回落 v1。
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
};
