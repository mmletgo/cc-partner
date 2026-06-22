/**
 * SSH API - 通过 Tauri invoke 调用 Rust 后端的 SSH 目标配置命令
 *
 * Business Logic（为什么需要这个模块）:
 *   SSH 页需要列出已配置的连接目标、新增/更新目标的用户名与端口、删除目标，
 *   以及查询本机操作系统用于按系统渲染配置指南。本模块封装 4 个后端命令。
 *
 * Code Logic（这个模块做什么）:
 *   - list: 列出所有未删除的 SSH 目标（invoke list_ssh_targets）
 *   - upsert: 新增/更新目标（host 主键，port 缺省 22），返回更新后的 dto
 *   - remove: 软删除目标（推进向量时钟，参与跨设备同步传播）
 *   - getOsInfo: 查询本机 OS（后端归一化为 mac/windows/ubuntu）
 */

import { invoke } from './client';
import type { SshTarget, OsInfo } from '@/lib/types';

export const sshApi = {
  /** 列出所有已配置的 SSH 目标（排除已删除） */
  list: () => invoke<SshTarget[]>('list_ssh_targets'),

  /** 新增/更新 SSH 目标（port 缺省 22），返回更新后的 dto */
  upsert: (
    host: string,
    username: string,
    port?: number,
    label?: string,
  ) =>
    invoke<SshTarget>('upsert_ssh_target', {
      host,
      username,
      port: port ?? 22,
      label: label ?? null,
    }),

  /** 软删除 SSH 目标 */
  remove: (host: string) =>
    invoke<{ ok: boolean }>('delete_ssh_target', { host }),

  /** 查询本机操作系统（归一化 mac/windows/ubuntu） */
  getOsInfo: () => invoke<OsInfo>('get_os_info'),
};
