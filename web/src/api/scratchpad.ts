/**
 * Scratchpad API - 通过 Tauri invoke 调用 Rust 后端速记本命令
 *
 * Business Logic（为什么需要这个模块）:
 *   速记本内容权威源已经迁移到 Rust/SQLite，前端页面不能再直接读写 localStorage。
 *   本模块集中封装读取、自动保存和局域网同步命令。
 *
 * Code Logic（这个模块做什么）:
 *   调用 get_scratchpad / update_scratchpad / sync_scratchpad 三个 invoke 命令，
 *   返回类型与 Rust ScratchpadDto / SyncResult 对齐。
 */

import { invoke } from './client';
import type { LanSyncResult, Scratchpad } from '@/lib/types';

export const scratchpadApi = {
  /** 获取速记本单例内容 */
  get: () => invoke<Scratchpad>('get_scratchpad'),

  /** 更新速记本文本内容 */
  update: (content: string) => invoke<Scratchpad>('update_scratchpad', { content }),

  /** 触发局域网同步（后端会复用全局 trigger_sync） */
  syncLan: () => invoke<LanSyncResult>('sync_scratchpad'),
};
