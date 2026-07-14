/**
 * Desktop Attention API。
 *
 * Business Logic（为什么需要这个模块）:
 *   桌面端 Inbox 与侧栏 badge 需要通过 Tauri invoke 拉取本机聚合快照。
 *
 * Code Logic（这个模块做什么）:
 *   封装 `list_attention_items` invoke，返回 runtime decode 后的 AttentionSnapshot。
 */

import type { AttentionSnapshot } from '@/lib/types';
import { attentionSnapshotDecoder } from '@/lib/schemas/attention';
import { invokeDecoded } from './client';

/** 桌面 Attention 命令名（对齐 Rust #[tauri::command]）。 */
export const ATTENTION_DESKTOP_COMMAND = 'list_attention_items' as const;

/**
 * Business Logic（为什么需要这个对象）:
 *   桌面页面与 AttentionProvider 通过统一 API 入口加载快照，避免散落 invoke 字符串。
 *
 * Code Logic（这个对象做什么）:
 *   listSnapshot 调用 list_attention_items 并 decode AttentionSnapshot。
 */
export const attentionApi = {
  /**
   * Business Logic（为什么需要这个函数）:
   *   Provider 首次挂载与轮询都需要拉取完整 Attention 快照。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded list_attention_items，无参数，返回校验后的 AttentionSnapshot。
   */
  listSnapshot: (): Promise<AttentionSnapshot> =>
    invokeDecoded(ATTENTION_DESKTOP_COMMAND, undefined, attentionSnapshotDecoder),
};
