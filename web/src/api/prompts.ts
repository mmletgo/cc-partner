/**
 * Prompt API - 通过 Tauri invoke 调用 Rust 后端 Prompt CRUD 命令
 */

import { invoke } from './client';
import type { Prompt } from '@/lib/types';

export const promptsApi = {
  /** 列出全部 Prompt（不传搜索/标签） */
  list: () => invoke<Prompt[]>('list_prompts'),

  /** 按 ID 获取单条 Prompt */
  get: (id: string) => invoke<Prompt>('get_prompt', { id }),

  /** 新建 Prompt */
  create: (data: { title: string; content: string; tags?: string[] }) =>
    invoke<Prompt>('create_prompt', data),

  /** 更新 Prompt（展开 title?/content?/tags?） */
  update: (id: string, data: Partial<Prompt>) =>
    invoke<Prompt>('update_prompt', { id, ...data }),

  /** 软删除 Prompt */
  remove: (id: string) => invoke<void>('delete_prompt', { id }),

  /** 触发跨设备同步（后端 M4 实现，调用会 reject） */
  sync: () => invoke<{ synced: number }>('trigger_sync'),

  /** 列出所有标签 */
  listTags: () => invoke<string[]>('list_tags'),
};
