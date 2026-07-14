/**
 * Settings 关于面板
 *
 * Business Logic（为什么需要这个组件）:
 *   用户在关于 tab 查看版本并检查/下载/安装更新；状态机与 API 由 controller 持有，
 *   本组件只渲染版本信息与更新块。
 *
 * Code Logic（这个组件做什么）:
 *   渲染关于 Card（version meta、检查更新、download/progress/install 状态机）；无 @/api 导入。
 */
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Card, Button, Pill } from '@/components/primitives';
import { SyncIcon, InfoIcon, DownloadIcon, XIcon } from '@/lib/icons';
import type {
  UpdateCheckResult,
  UpdateDownloadStatus,
  VersionInfo,
} from '@/lib/types';
import styles from './Settings.module.css';

/**
 * 安装按钮展示模式（与 settingsState.installButtonMode 返回值对齐）
 *
 * Business Logic（为什么需要这个类型）:
 *   About 面板需按 install / installing / retryInstall 切换按钮文案，类型与 controller 一致。
 *
 * Code Logic（这个类型做什么）:
 *   枚举三种安装按钮模式字面量。
 */
export type SettingsUpdateInstallMode = 'install' | 'installing' | 'retryInstall';

/**
 * 关于面板 props
 *
 * Business Logic（为什么需要这个接口）:
 *   Settings 壳层把 controller 的版本/更新状态透传给 pure panel。
 *
 * Code Logic（这个接口做什么）:
 *   声明 version/update/download 展示字段与 check/download/cancel/install/retry 回调。
 */
export interface SettingsAboutPanelProps {
  versionInfo: VersionInfo | null;
  versionLoadError: Error | null;
  updateResult: UpdateCheckResult | null;
  updateHint: string;
  updateCheckDisabled: boolean;
  updateDownloadDisabled: boolean;
  updateInstallRetry: boolean;
  updateInstallMode: SettingsUpdateInstallMode;
  updateIsInstalling: boolean;
  updateIsChecking: boolean;
  downloadStatus: UpdateDownloadStatus | null;
  formatSize: (bytes: number) => string;
  onCheckUpdate: () => void;
  onDownload: () => void;
  onCancelDownload: () => void;
  onInstall: () => void;
  onRetryVersionLoad: () => void;
  retryingVersion: boolean;
}

/**
 * 关于设置面板
 *
 * Business Logic（为什么需要这个组件）:
 *   关于 tab 独立承载版本展示与自动更新状态机 UI，需要 pure 视图配合 ownership 守卫。
 *
 * Code Logic（这个组件做什么）:
 *   useTranslation 置顶；原样渲染版本 meta 与 updateBlock 各分支。
 *
 * @param props 版本/更新受控状态与动作
 * @returns 关于 tab 内容
 */
export function SettingsAboutPanel({
  versionInfo,
  versionLoadError,
  updateResult,
  updateHint,
  updateCheckDisabled,
  updateDownloadDisabled,
  updateInstallRetry,
  updateInstallMode,
  updateIsInstalling,
  updateIsChecking,
  downloadStatus,
  formatSize,
  onCheckUpdate,
  onDownload,
  onCancelDownload,
  onInstall,
  onRetryVersionLoad,
  retryingVersion,
}: SettingsAboutPanelProps): ReactElement {
  const { t } = useTranslation(['settings', 'common']);

  return (
    <Card variant="flat" padding="md">
      <Card.Header>
        <h2 className={styles.sectionTitle}>{t('settings:about.title')}</h2>
      </Card.Header>
      <Card.Body padding="md">
        {versionLoadError ? (
          <div className={styles.resourceError} role="alert">
            <span className={styles.updateError}>
              {t('settings:resource.versionLoadFailed', {
                error: versionLoadError.message,
              })}
            </span>
            <Button
              variant="secondary"
              size="sm"
              onClick={onRetryVersionLoad}
              disabled={retryingVersion}
            >
              {retryingVersion
                ? t('settings:resource.retrying')
                : t('settings:resource.retry')}
            </Button>
          </div>
        ) : null}
        <dl className={styles.metaList}>
          <div className={styles.metaRow}>
            <dt className={styles.metaKey}>{t('settings:about.versionLabel')}</dt>
            <dd className={styles.metaValue}>
              <Pill tone="accent">{`v${versionInfo?.version ?? '—'}`}</Pill>
            </dd>
          </div>
          <div className={styles.metaRow}>
            <dt className={styles.metaKey}>{t('settings:about.buildLabel')}</dt>
            <dd className={styles.metaValue}>{versionInfo?.buildDate ?? '—'}</dd>
          </div>
          <div className={styles.metaRow}>
            <dt className={styles.metaKey}>{t('settings:about.sourceLabel')}</dt>
            <dd className={styles.metaValue}>{t('settings:about.source')}</dd>
          </div>
        </dl>
        <div className={styles.aboutActions}>
          <Button
            variant="secondary"
            size="md"
            icon={<SyncIcon />}
            onClick={onCheckUpdate}
            disabled={updateCheckDisabled}
          >
            {updateIsChecking
              ? t('settings:about.checking')
              : t('settings:about.checkUpdate')}
          </Button>
          <span className={styles.aboutHint}>
            <InfoIcon size={14} />
            <span>{updateHint}</span>
          </span>
        </div>

        {/* 发现新版本时展示：版本说明 + 下载/进度/安装；installing 不伪造进度条 */}
        {updateResult?.hasUpdate ? (
          <div className={styles.updateBlock}>
            <div className={styles.metaRow}>
              <span className={styles.metaKey}>{t('settings:about.latestVersion')}</span>
              <Pill tone="accent">{`v${updateResult.version}`}</Pill>
            </div>
            {updateResult.body ? (
              <p className={styles.updateBody}>{updateResult.body}</p>
            ) : null}

            {downloadStatus?.status === 'downloading' ? (
              <div className={styles.progressRow}>
                <div className={styles.progressBar}>
                  <div
                    className={styles.progressFill}
                    style={{
                      width: `${Math.round(downloadStatus.progress * 100)}%`,
                    }}
                  />
                </div>
                <span className={styles.progressText}>
                  {Math.round(downloadStatus.progress * 100)}%
                </span>
                <Button
                  variant="ghost"
                  size="sm"
                  icon={<XIcon size={14} />}
                  onClick={onCancelDownload}
                >
                  {t('settings:about.cancel')}
                </Button>
              </div>
            ) : downloadStatus?.status === 'installing' || updateIsInstalling ? (
              <div className={styles.updateActions}>
                <Button
                  variant="primary"
                  size="sm"
                  icon={<DownloadIcon size={14} />}
                  disabled
                >
                  {t('settings:about.installing')}
                </Button>
                <span className={styles.aboutHint}>{t('settings:about.installing')}</span>
              </div>
            ) : downloadStatus?.status === 'completed' ? (
              <div className={styles.updateActions}>
                {updateInstallRetry ? (
                  <span className={styles.updateError}>
                    {downloadStatus.error || t('settings:about.installFailed')}
                  </span>
                ) : (
                  <span className={styles.aboutHint}>
                    {t('settings:about.downloadCompleted')}
                  </span>
                )}
                <Button
                  variant="primary"
                  size="sm"
                  icon={<DownloadIcon size={14} />}
                  onClick={onInstall}
                  disabled={updateIsInstalling}
                >
                  {updateInstallMode === 'installing'
                    ? t('settings:about.installing')
                    : updateInstallMode === 'retryInstall'
                      ? t('settings:about.retryInstall')
                      : t('settings:about.installAndRestart')}
                </Button>
              </div>
            ) : downloadStatus?.status === 'failed' ? (
              <div className={styles.updateActions}>
                <span className={styles.updateError}>
                  {downloadStatus.error || t('settings:about.downloadFailed')}
                </span>
                <Button
                  variant="secondary"
                  size="sm"
                  icon={<DownloadIcon size={14} />}
                  onClick={onDownload}
                  disabled={updateDownloadDisabled}
                >
                  {t('settings:about.retryDownload')}
                </Button>
              </div>
            ) : downloadStatus?.status === 'cancelled' ? (
              <div className={styles.updateActions}>
                <span className={styles.aboutHint}>
                  {t('settings:about.downloadCancelled')}
                </span>
                <Button
                  variant="secondary"
                  size="sm"
                  icon={<DownloadIcon size={14} />}
                  onClick={onDownload}
                  disabled={updateDownloadDisabled}
                >
                  {t('settings:about.redownload')}
                </Button>
              </div>
            ) : updateResult.downloadUrl ? (
              <div className={styles.updateActions}>
                <Button
                  variant="primary"
                  size="sm"
                  icon={<DownloadIcon size={14} />}
                  onClick={onDownload}
                  disabled={updateDownloadDisabled}
                >
                  {t('settings:about.downloadUpdate', {
                    size: formatSize(updateResult.size ?? 0),
                  })}
                </Button>
              </div>
            ) : (
              <span className={styles.aboutHint}>{t('settings:about.noAsset')}</span>
            )}
          </div>
        ) : null}
      </Card.Body>
    </Card>
  );
}
