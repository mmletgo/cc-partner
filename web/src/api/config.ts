/**
 * 配置 / 版本 / 更新 / 权限 API 客户端（Tauri invoke 版本）
 *
 * Business Logic:
 *   前端设置页面、欢迎页等需要与后端交互：读写配置、选择目录、
 *   检查更新、下载安装更新、查询权限状态。本模块封装这些 invoke 调用。
 *
 * Code Logic:
 *   基于 invoke 封装各命令调用，返回类型化的 Promise。
 *   仅 deviceName/receiveDir/screenshotHotkey 可写（对齐 Rust update_config 签名）。
 *   恢复默认通过 get_default_config 读取后端环境默认值，避免前端硬编码主机名/用户目录。
 */

import { invoke } from './client';
import type {
  AppConfig,
  VersionInfo,
  UpdateCheckResult,
  UpdateDownloadStatus,
  PermissionsStatus,
  PermissionType,
  PermissionRequestResult,
  CloudSyncConfig,
  CloudSyncResult,
  TestCloudSyncResult,
} from '@/lib/types';

/** 可写的配置字段（对齐 Rust update_config 参数） */
export type ConfigUpdate = Pick<AppConfig, 'deviceName' | 'receiveDir' | 'screenshotHotkey'>;

/** 云端同步可更新字段（对齐 Rust update_cloud_sync_cmd 参数，全部可选部分更新） */
export interface CloudSyncConfigUpdate {
  repoUrl?: string | null;
  enabled?: boolean;
  auto?: boolean;
  intervalSecs?: number;
  branch?: string | null;
}

export const configApi = {
  /** 获取当前应用配置 */
  get: () => invoke<AppConfig>('get_config'),

  /** 获取应用偏好默认值（设备名/接收目录/截图快捷键） */
  getDefaults: () => invoke<AppConfig>('get_default_config'),

  /** 更新应用配置（仅 deviceName/receiveDir/screenshotHotkey 可写） */
  update: (data: Partial<AppConfig>) => invoke<AppConfig>('update_config', data),

  /** 打开原生目录选择对话框，返回选中的路径 */
  chooseDir: async (): Promise<{ path: string | null }> => {
    const p = await invoke<string | null>('choose_dir');
    return { path: p };
  },

  /** 获取版本号和构建日期 */
  version: () => invoke<VersionInfo>('get_version'),

  /** 触发 GitHub Releases 更新检查（M8 实现） */
  checkUpdate: () => invoke<UpdateCheckResult>('check_update'),

  /** 启动更新包下载（透传检查结果的 downloadUrl/filename）（M8 实现） */
  downloadUpdate: (url: string, filename: string) =>
    invoke<{ ok: boolean; error?: string }>('download_update', { url, filename }),

  /** 轮询下载进度状态（前端进度条）（M8 实现） */
  getDownloadStatus: () => invoke<UpdateDownloadStatus>('get_download_status'),

  /** 取消正在进行的下载（M8 实现） */
  cancelDownload: () => invoke<{ ok: boolean; error?: string }>('cancel_download'),

  /** 安装已下载的更新包并重启（进程随后退出）（M8 实现） */
  installUpdate: () => invoke<{ ok: boolean; error?: string }>('install_update'),

  /** 检查 macOS 权限状态（屏幕录制、输入监控）（M7 实现） */
  permissions: () => invoke<PermissionsStatus>('check_permissions'),

  /**
   * 触发权限请求（M7 实现）。
   * @param openSettings 是否打开系统设置面板兜底；缺省 true。启动主动引导时按类型差异化传：
   *   screenCapture 传 false（仅弹系统框）、inputMonitoring 传 true（它只能靠开面板引导）。
   */
  requestPermission: (type: PermissionType, openSettings?: boolean) =>
    invoke<PermissionRequestResult>('request_permission', { type, openSettings }),

  /** 获取 GitHub 私有仓库云端同步配置 */
  getCloudSyncConfig: () => invoke<CloudSyncConfig>('get_cloud_sync_config'),

  /** 获取 GitHub 私有仓库云端同步默认配置 */
  getDefaultCloudSyncConfig: () => invoke<CloudSyncConfig>('get_default_cloud_sync_config'),

  /** 更新云端同步配置（全部字段可选，部分更新） */
  updateCloudSyncConfig: (payload: CloudSyncConfigUpdate) =>
    invoke<CloudSyncConfig>(
      'update_cloud_sync_config',
      payload as unknown as Record<string, unknown>,
    ),

  /** 立即触发一次云端同步（pull + push） */
  triggerCloudSync: () => invoke<CloudSyncResult>('trigger_cloud_sync_cmd'),

  /** 测试云端同步连通性（git 可用性 + 仓库默认分支探测） */
  testCloudSync: () => invoke<TestCloudSyncResult>('test_cloud_sync'),
};
