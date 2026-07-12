/**
 * Desktop Attention API。
 *
 * Business Logic（为什么需要这个模块）:
 *   桌面端 Inbox 与侧栏 badge 需要通过 Tauri invoke 拉取本机聚合快照。
 *
 * Code Logic（这个模块做什么）:
 *   封装 `list_attention_items` invoke，返回类型化 AttentionSnapshot。
 */

import type { AttentionSnapshot } from '@/lib/types';
import { invoke } from './client';

/** 桌面 Attention 命令名（对齐 Rust #[tauri::command]）。 */
export const ATTENTION_DESKTOP_COMMAND = 'list_attention_items' as const;

/**
 * Business Logic（为什么需要这个对象）:
 *   桌面页面与 AttentionProvider 通过统一 API 入口加载快照，避免散落 invoke 字符串。
 *
 * Code Logic（这个对象做什么）:
 *   listSnapshot 调用 list_attention_items 并返回 AttentionSnapshot。
 */
export const attentionApi = {
  /**
   * Business Logic（为什么需要这个函数）:
   *   Provider 首次挂载与轮询都需要拉取完整 Attention 快照。
   *
   * Code Logic（这个函数做什么）:
   *   invoke list_attention_items，无参数，返回 camelCase AttentionSnapshot。
   */
  listSnapshot: (): Promise<AttentionSnapshot> =>
    invoke<AttentionSnapshot>(ATTENTION_DESKTOP_COMMAND),
};
