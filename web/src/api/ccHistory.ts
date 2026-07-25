/**
 * Claude 历史 API - 通过 Tauri invoke 调用 Rust 后端 cc 历史采集命令
 *
 * Business Logic（为什么需要这个模块）:
 *   Claude Code 在本地 ~/.claude/projects 下留有用户输入 prompt 的会话历史。
 *   本模块封装读取/刷新/删除这些历史的 IPC 调用，供 CcHistory 页面拉取数据。
 *   跨设备同步复用 promptsApi.sync()（trigger_sync 已覆盖 cc 同步），不在此重复。
 *
 * Code Logic（这个模块做什么）:
 *   - listDevices: list_cc_history_devices → 历史所属设备（本机标记 + 离线设备）
 *   - listProjects: list_cc_projects → 指定设备内按 cwd 聚合的项目分组
 *   - listPrompts: list_cc_prompts → 指定设备、项目下（可选搜索词）的 prompt 列表
 *   - refresh: refresh_cc_history → 重新扫描本地 ~/.claude 采集入库
 *   - remove: delete_cc_prompt → 软删除单条
 */

import { invoke } from './client';
import type { CcHistoryDevice, CcProject, CcHistoryItem } from '@/lib/types';

export const ccHistoryApi = {
  /** 列出历史中出现过的设备；本机条目带 isSelf=true。 */
  listDevices: () => invoke<CcHistoryDevice[]>('list_cc_history_devices'),

  /** 列出指定设备采集到的 Claude 项目（按 cwd 分组） */
  listProjects: (deviceId: string) =>
    invoke<CcProject[]>('list_cc_projects', { deviceId }),

  /** 列出指定设备、指定项目下的 prompt（可选搜索关键词） */
  listPrompts: (projectPath: string, search: string | undefined, deviceId: string) =>
    invoke<CcHistoryItem[]>('list_cc_prompts', { projectPath, search, deviceId }),

  /** 立即刷新采集：扫描本地 ~/.claude 入库，返回本次新增条数 */
  refresh: () => invoke<{ ok: boolean; collected: number }>('refresh_cc_history'),

  /** 软删除单条 prompt */
  remove: (id: string) => invoke<{ ok: boolean; id: string }>('delete_cc_prompt', { id }),
};
