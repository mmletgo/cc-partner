/**
 * 配置 / 版本 / 更新 / 权限 API 客户端（Tauri invoke 版本）
 *
 * Business Logic:
 *   前端设置页面、欢迎页等需要与后端交互：读写配置、选择目录、
 *   检查更新、下载安装更新、查询权限状态。本模块封装这些 invoke 调用。
 *
 * Code Logic:
 *   基于 invoke 封装各命令调用，返回类型化的 Promise。
 *   基础偏好与 Workbench Prompt 优化偏好可写（对齐 Rust update_config 签名）。
 *   恢复默认通过 get_default_config 读取后端环境默认值，避免前端硬编码主机名/用户目录。
 */

import { invoke, invokeDecoded } from './client';
import {
  appConfigDecoder,
  permissionActionResultDecoder,
  permissionsStatusDecoder,
} from '@/lib/schemas/config';
import type {
  AppConfig,
  VersionInfo,
  UpdateCheckResult,
  UpdateDownloadStatus,
  PermissionType,
  PermissionActionResult,
  CloudSyncConfig,
  CloudSyncResult,
  TestCloudSyncResult,
} from '@/lib/types';

/** 可写的配置字段（对齐 Rust update_config 参数） */
export type ConfigUpdate = Pick<
  AppConfig,
  | 'deviceName'
  | 'receiveDir'
  | 'gamePluginDir'
  | 'screenshotHotkey'
  | 'promptOptimizerHotkey'
  | 'promptOptimizerFillLanguage'
  | 'promptOptimizerProvider'
  | 'promptQuickInputHotkey'
  | 'experimentalFeatures'
  | 'relay'
>;

/** 云端同步可更新字段（对齐 Rust update_cloud_sync_cmd 参数，全部可选部分更新） */
export interface CloudSyncConfigUpdate {
  repoUrl?: string | null;
  enabled?: boolean;
  auto?: boolean;
  intervalSecs?: number;
  branch?: string | null;
}

export const configApi = {
  /**
   * Business Logic（为什么需要这个函数）:
   *   设置页核心资源需要当前配置。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded get_config → AppConfig。
   */
  get: () => invokeDecoded('get_config', undefined, appConfigDecoder),

  /**
   * Business Logic（为什么需要这个函数）:
   *   「恢复默认」需要后端环境默认值。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded get_default_config → AppConfig。
   */
  getDefaults: () => invokeDecoded('get_default_config', undefined, appConfigDecoder),

  /**
   * Business Logic（为什么需要这个函数）:
   *   保存基础偏好与 Prompt 优化偏好。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded update_config → AppConfig。
   */
  update: (data: Partial<AppConfig>) =>
    invokeDecoded('update_config', data as Record<string, unknown>, appConfigDecoder),

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

  /**
   * 轮询更新状态（checking / downloading / installing 等）。
   * 进度条仅对 downloading 有意义；installing 不伪造进度。
   */
  getDownloadStatus: () => invoke<UpdateDownloadStatus>('get_download_status'),

  /** 取消正在进行的下载（M8 实现） */
  cancelDownload: () => invoke<{ ok: boolean; error?: string }>('cancel_download'),

  /**
   * 安装已下载的更新包并重启（进程随后退出）。
   * 失败时状态回到 completed 且 error 非空，前端可展示重试安装。
   */
  installUpdate: () => invoke<{ ok: boolean; error?: string }>('install_update'),

  /**
   * Business Logic（为什么需要这个函数）:
   *   Welcome/Settings 权限流需要三项 TCC 状态；notification 由 decoder 显式 default。
   *
   * Code Logic（这个函数做什么）:
   *   invokeDecoded check_permissions → PermissionsStatus。
   */
  permissions: () =>
    invokeDecoded('check_permissions', undefined, permissionsStatusDecoder),

  /**
   * Business Logic（为什么需要这个函数）:
   *   Welcome / OnboardingGuard 按 dev|release 隔离引导 localStorage。
   *
   * Code Logic（这个函数做什么）:
   *   invoke get_app_identity → { bundleId, flavor }。
   */
  appIdentity: () =>
    invoke<{ bundleId: string | null; flavor: 'dev' | 'release' }>('get_app_identity'),

  /** 显式请求单项权限；该命令不会打开系统设置或重启应用。 */
  requestPermission: (type: PermissionType): Promise<PermissionActionResult> =>
    invokeDecoded('request_permission', { type }, permissionActionResultDecoder),

  /** 显式打开单项权限的系统设置；该命令不会 Request 或重启应用。 */
  openPermissionSettings: (type: PermissionType): Promise<PermissionActionResult> =>
    invokeDecoded('open_permission_settings', { type }, permissionActionResultDecoder),

  /**
   * Business Logic（为什么需要这个函数）:
   *   系统设置打开 sticky TCC 开关后当前进程检测常仍为未授权；仅 Welcome
   *   「重新打开应用」按钮可调用本方法，在新进程反映已授权。
   *   **禁止** request / visibility / focus / recheck 自动调用（避免闪白屏）。
   *
   * Code Logic（这个函数做什么）:
   *   invoke relaunch_for_permissions；macOS 经 LaunchServices open .app 后退出。
   *   成功路径进程退出，Promise 可能永不 resolve。
   */
  relaunchForPermissions: () => invoke<void>('relaunch_for_permissions'),

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
