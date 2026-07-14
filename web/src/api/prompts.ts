/**
 * Prompt API - 通过 Tauri invoke 调用 Rust 后端 Prompt CRUD 命令
 *
 * Business Logic（为什么需要这个模块）:
 *   Prompt 权威源在 Rust/SQLite，前端经 Tauri invoke 完成 CRUD、同步与版本历史。
 *
 * Code Logic（这个模块做什么）:
 *   封装 list/get/create/update/remove/sync/listTags 与 listVersions/restoreVersion 命令。
 */

import { invoke } from './client';
import type { ContentVersion, Prompt } from '@/lib/types';

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

  /**
   * 列出指定 Prompt 的版本历史（含 conflict 副本）。
   *
   * Business Logic（为什么需要这个方法）:
   *   用户需要查看同步冲突与历史版本，以便复制冲突内容或恢复为新版本。
   *
   * Code Logic（这个方法做什么）:
   *   调用 list_prompt_versions，返回 ContentVersion 摘要数组。
   */
  listVersions: (id: string) =>
    invoke<ContentVersion[]>('list_prompt_versions', { id }),

  /**
   * 将指定历史版本恢复为新的 active 版本（后端推进 vector clock）。
   *
   * Business Logic（为什么需要这个方法）:
   *   用户选中历史/冲突版本后应能恢复，且不得原地覆盖历史。
   *
   * Code Logic（这个方法做什么）:
   *   调用 restore_prompt_version，返回更新后的 Prompt DTO。
   */
  restoreVersion: (id: string, versionId: string) =>
    invoke<Prompt>('restore_prompt_version', { id, versionId }),
};
