/**
 * Settings 权限与自动更新控制器
 *
 * Business Logic（为什么需要这个 hook）:
 *   设置页依赖/关于 tab 需要 macOS 权限请求与自动更新 check/download/install 编排，
 *   与表单草稿/资源加载解耦后可独立测并缩小主 composer 体积。
 *
 * Code Logic（这个 hook 做什么）:
 *   接线 usePermissions + 更新检查/下载/取消/安装；通过 useVisibilityPolling 轮询下载状态；
 *   派生 updateHint / 按钮禁用与安装重试文案相关标记。
 */
import { useCallback, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { configApi } from '@/api/config';
import { usePermissions } from '@/hooks/usePermissions';
import { useVisibilityPolling } from '@/hooks/useVisibilityPolling';
import type { PermissionType, UpdateCheckResult, UpdateDownloadStatus } from '@/lib/types';
import type { PermissionEntryAction } from '@/lib/permissionEntries';
import {
  installButtonMode,
  isUpdateCheckDisabled,
  isUpdateDownloadDisabled,
  shouldPollUpdateStatus,
  shouldShowInstallRetry,
} from '../settingsState';
import { buildUpdateHint } from '../settingsControllerShared';

/**
 * useSettingsUpdatePermissions 返回值：权限与更新动作契约。
 *
 * Business Logic（为什么需要这个接口）:
 *   composer 与 About/Dependencies panel 需要稳定的权限/更新字段，避免散落读取内部 state。
 *
 * Code Logic（这个接口做什么）:
 *   聚合 perm* 字段、update 派生标记与 check/download/cancel/install handlers。
 */
export interface UseSettingsUpdatePermissionsResult {
  permStatus: import('@/lib/types').PermissionsStatus | null;
  permLoading: boolean;
  permRefreshing: boolean;
  permError: string | null;
  permRequesting: ReadonlySet<PermissionType>;
  permissionBuildHelpVisible: boolean;
  refreshPermissions: () => void | Promise<void>;
  handleRequestAccess: (type: PermissionType, action?: PermissionEntryAction) => void;
  updateResult: UpdateCheckResult | null;
  updateHint: string;
  updateCheckDisabled: boolean;
  updateDownloadDisabled: boolean;
  updateInstallRetry: boolean;
  updateInstallMode: ReturnType<typeof installButtonMode>;
  updateIsInstalling: boolean;
  updateIsChecking: boolean;
  downloadStatus: UpdateDownloadStatus | null;
  handleCheckUpdate: () => Promise<void>;
  handleDownload: () => Promise<void>;
  handleCancelDownload: () => Promise<void>;
  handleInstall: () => Promise<void>;
}

/**
 * Settings 权限与自动更新 hook
 *
 * Business Logic（为什么需要这个函数）:
 *   依赖/关于 tab 需要与表单保存无关的权限轮询与更新生命周期，拆出后可单独 characterization。
 *
 * Code Logic（这个函数做什么）:
 *   持有 updateResult/downloadStatus/checking/installing；接线 permissions 与下载轮询；返回 panel 字段。
 *
 * @returns 权限状态与更新动作
 */
export function useSettingsUpdatePermissions(): UseSettingsUpdatePermissionsResult {
  const { t } = useTranslation(['settings', 'common']);
  const [updateResult, setUpdateResult] = useState<UpdateCheckResult | null>(null);
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  const [downloadStatus, setDownloadStatus] = useState<UpdateDownloadStatus | null>(null);
  const [installing, setInstalling] = useState(false);
  const [permissionBuildHelpVisible, setPermissionBuildHelpVisible] = useState(false);

  const {
    status: permStatus,
    loading: permLoading,
    refreshing: permRefreshing,
    error: permError,
    requesting: permRequesting,
    request: requestPermissionItem,
    openSettings: openPermissionSettings,
    refresh: refreshPermissions,
  } = usePermissions();

  /**
   * Business Logic（为什么需要这个函数）:
   *   下载进行中需要轮询进度，但后台标签页不得空转，且不得与旧 setInterval 重叠。
   *
   * Code Logic（这个函数做什么）:
   *   拉取 getDownloadStatus 并写回 downloadStatus；失败静默等下一轮。
   */
  const pollDownloadStatus = useCallback(async () => {
    try {
      const status = await configApi.getDownloadStatus();
      setDownloadStatus(status);
    } catch {
      // 轮询失败静默，下一轮重试
    }
  }, []);

  // checking/downloading/installing 时启用可见性感知 800ms 轮询；终态停止
  useVisibilityPolling(pollDownloadStatus, {
    intervalMs: 800,
    enabled: shouldPollUpdateStatus(downloadStatus),
    runImmediately: true,
  });

  /**
   * Business Logic（为什么需要这个函数）:
   *   设置页权限卡需要逐项请求授权，且错误由 usePermissions 统一投影。
   *
   * Code Logic（这个函数做什么）:
   *   调用 requestPermissionItem(type)，吞掉 rejection（error 已由 hook 写入）。
   *
   * @param type 权限类型 screenCapture / accessibility / inputMonitoring / notification
   */
  const handleRequestAccess = useCallback(
    (type: PermissionType, action: PermissionEntryAction = 'request') => {
      setPermissionBuildHelpVisible(action === 'buildHelp');
      if (action === 'request') {
        void requestPermissionItem(type).catch(() => undefined);
      } else if (action === 'openSettings') {
        void openPermissionSettings(type).catch(() => undefined);
      }
    },
    [openPermissionSettings, requestPermissionItem],
  );

  const updateHint = useMemo(
    () => buildUpdateHint(updateResult, checkingUpdate, t),
    [updateResult, checkingUpdate, t],
  );
  const updateCheckDisabled = useMemo(
    () => isUpdateCheckDisabled({ checkingUpdate, downloadStatus }),
    [checkingUpdate, downloadStatus],
  );
  const updateDownloadDisabled = useMemo(
    () => isUpdateDownloadDisabled({ checkingUpdate, downloadStatus }),
    [checkingUpdate, downloadStatus],
  );
  const updateInstallRetry = useMemo(
    () => shouldShowInstallRetry(downloadStatus),
    [downloadStatus],
  );
  const updateInstallMode = useMemo(
    () => installButtonMode({ installing, downloadStatus }),
    [installing, downloadStatus],
  );
  const updateIsInstalling = installing || downloadStatus?.status === 'installing';
  const updateIsChecking = checkingUpdate || downloadStatus?.status === 'checking';

  /**
   * 检查更新按钮：调用后端 updater/check 接口
   *
   * Business Logic（为什么需要这个函数）:
   *   关于 tab 需要显式检查更新并展示 hasUpdate/error。
   *
   * Code Logic（这个函数做什么）:
   *   清旧结果后调 checkUpdate；失败写入 error 形态的 UpdateCheckResult。
   */
  const handleCheckUpdate = async () => {
    setCheckingUpdate(true);
    setUpdateResult(null);
    setDownloadStatus(null);
    try {
      const result = await configApi.checkUpdate();
      setUpdateResult(result);
    } catch (err) {
      setUpdateResult({
        hasUpdate: false,
        error: err instanceof Error ? err.message : t('error.checkFailed'),
      });
    } finally {
      setCheckingUpdate(false);
    }
  };

  /**
   * 启动更新下载：透传检查结果的 downloadUrl/filename，立即进入 downloading 状态
   *
   * Business Logic（为什么需要这个函数）:
   *   用户确认下载后应立刻看到进度条，不能等后端首帧状态。
   *
   * Code Logic（这个函数做什么）:
   *   乐观 set downloading → downloadUpdate；失败写 failed 状态。
   */
  const handleDownload = async () => {
    if (!updateResult?.downloadUrl || !updateResult?.filename) return;
    setDownloadStatus({
      status: 'downloading',
      progress: 0,
      error: '',
      filePath: '',
      url: updateResult.downloadUrl,
      filename: updateResult.filename,
      size: updateResult.size ?? 0,
    });
    try {
      await configApi.downloadUpdate(updateResult.downloadUrl, updateResult.filename);
    } catch (err) {
      setDownloadStatus({
        status: 'failed',
        progress: 0,
        error: err instanceof Error ? err.message : t('error.startDownloadFailed'),
        filePath: '',
        url: '',
        filename: '',
        size: 0,
      });
    }
  };

  /**
   * 取消正在进行的下载
   *
   * Business Logic（为什么需要这个函数）:
   *   用户可中止大包下载，避免占带宽。
   *
   * Code Logic（这个函数做什么）:
   *   cancelDownload 后把本地 status 标 cancelled；失败静默。
   */
  const handleCancelDownload = async () => {
    try {
      await configApi.cancelDownload();
      setDownloadStatus((prev) => (prev ? { ...prev, status: 'cancelled' } : prev));
    } catch {
      // 取消失败静默
    }
  };

  /**
   * 安装已下载的更新包并重启（进程随后退出）。
   *
   * Business Logic（为什么需要这个函数）:
   *   下载完成后需安装；失败时展示 completed+error 重试态。
   *
   * Code Logic（这个函数做什么）:
   *   乐观 installing → installUpdate；失败刷新 getDownloadStatus。
   */
  const handleInstall = async () => {
    setInstalling(true);
    setDownloadStatus((prev) =>
      prev
        ? { ...prev, status: 'installing', error: '' }
        : {
            status: 'installing',
            progress: 0,
            error: '',
            filePath: '',
            url: '',
            filename: '',
            size: 0,
          },
    );
    try {
      await configApi.installUpdate();
    } catch {
      try {
        const status = await configApi.getDownloadStatus();
        setDownloadStatus(status);
      } catch {
        // 刷新失败时保留 installing 前状态由下一轮轮询/用户重试覆盖
      }
    } finally {
      setInstalling(false);
    }
  };

  return {
    permStatus,
    permLoading,
    permRefreshing,
    permError,
    permRequesting,
    permissionBuildHelpVisible,
    refreshPermissions,
    handleRequestAccess,
    updateResult,
    updateHint,
    updateCheckDisabled,
    updateDownloadDisabled,
    updateInstallRetry,
    updateInstallMode,
    updateIsInstalling,
    updateIsChecking,
    downloadStatus,
    handleCheckUpdate,
    handleDownload,
    handleCancelDownload,
    handleInstall,
  };
}
