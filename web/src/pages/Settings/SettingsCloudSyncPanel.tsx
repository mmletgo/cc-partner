/**
 * Settings 云端同步面板（GitHub 私有仓库）
 *
 * Business Logic（为什么需要这个组件）:
 *   云端同步是内测功能，设置入口从同步 tab 迁到内测功能页；LAN/备份仍留在同步 tab。
 *
 * Code Logic（这个组件做什么）:
 *   渲染仓库/分支/间隔、启用与自动定时开关、测试/应用/立即同步；禁止 transport 调用。
 */
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Card, Button, Input, Pill } from '@/components/primitives';
import { CheckIcon, XIcon, SyncIcon, InfoIcon } from '@/lib/icons';
import type {
  CloudSyncConfig,
  CloudSyncResult,
  TestCloudSyncResult,
} from '@/lib/types';
import type { CloudSyncForm } from './settingsState';
import styles from './Settings.module.css';

/**
 * Business Logic（为什么需要这个函数）:
 *   同步结果需要展示最近一次同步的本地时刻。
 *
 * Code Logic（这个函数做什么）:
 *   解析 ISO；非法则原样返回；否则输出 HH:MM:SS。
 */
function formatIsoTime(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const pad = (n: number) => n.toString().padStart(2, '0');
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

/**
 * 云端同步面板 props
 *
 * Business Logic（为什么需要这个接口）:
 *   内测功能页透传 controller 的云端同步表单与动作，panel 保持纯渲染。
 *
 * Code Logic（这个接口做什么）:
 *   声明 form/applied config/结果/loading/error 与 patch/reset/test/apply/sync 回调。
 */
export interface SettingsCloudSyncPanelProps {
  form: CloudSyncForm;
  cloudSync: CloudSyncConfig | null;
  syncResult: CloudSyncResult | null;
  testResult: TestCloudSyncResult | null;
  cloudSyncError: string | null;
  testing: boolean;
  applying: boolean;
  syncing: boolean;
  loadError: Error | null;
  retrying: boolean;
  canResetDefaults: boolean;
  onPatch: (partial: Partial<CloudSyncForm>) => void;
  onResetDefaults: () => void;
  onTest: () => void;
  onApply: () => void;
  onSyncNow: () => void;
  onRetryLoad: () => void;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   开启内测功能「云端同步」后，用户仍需编辑仓库与手动同步。
 *
 * Code Logic（这个组件做什么）:
 *   原同步 tab 云端 Card 的纯视图；id 保持 settings-cloud-* 以便既有测试定位。
 */
export function SettingsCloudSyncPanel({
  form: cloudSyncForm,
  cloudSync,
  syncResult,
  testResult,
  cloudSyncError,
  testing,
  applying,
  syncing,
  loadError: cloudSyncLoadError,
  retrying,
  canResetDefaults: canResetCloudSyncDefaults,
  onPatch: patchCloudSyncForm,
  onResetDefaults: handleResetCloudSyncDefaults,
  onTest: handleTestCloudSync,
  onApply: handleApplyCloudSync,
  onSyncNow: handleSyncNow,
  onRetryLoad,
}: SettingsCloudSyncPanelProps): ReactElement {
  const { t } = useTranslation(['settings', 'common']);

  return (
    <Card variant="flat" padding="md">
      <Card.Header>
        <h2 className={styles.sectionTitle}>{t('settings:cloudSync.title')}</h2>
      </Card.Header>
      <Card.Body padding="md">
        <p className={styles.helper}>{t('settings:cloudSync.subtitle')}</p>

        <div className={styles.field}>
          <label className={styles.label} htmlFor="settings-cloud-repo-url">
            {t('settings:cloudSync.repoUrl.label')}
          </label>
          <Input
            id="settings-cloud-repo-url"
            type="text"
            value={cloudSyncForm.repoUrl}
            onChange={(e) => patchCloudSyncForm({ repoUrl: e.target.value })}
            mono
          />
          <p className={styles.helper}>{t('settings:cloudSync.repoUrl.helper')}</p>
        </div>

        <div className={styles.field}>
          <label className={styles.label} htmlFor="settings-cloud-branch">
            {t('settings:cloudSync.branch.label')}
          </label>
          <Input
            id="settings-cloud-branch"
            type="text"
            value={cloudSyncForm.branch}
            onChange={(e) => patchCloudSyncForm({ branch: e.target.value })}
            mono
          />
          <p className={styles.helper}>{t('settings:cloudSync.branch.helper')}</p>
        </div>

        <div className={styles.field}>
          <label className={styles.label} htmlFor="settings-cloud-interval">
            {t('settings:cloudSync.interval.label')}
          </label>
          <Input
            id="settings-cloud-interval"
            type="number"
            value={cloudSyncForm.intervalSecs}
            onChange={(e) =>
              patchCloudSyncForm({ intervalSecs: Number(e.target.value) || 0 })
            }
            mono
          />
          <p className={styles.helper}>{t('settings:cloudSync.interval.helper')}</p>
        </div>

        <div className={styles.toggleList}>
          <button
            type="button"
            className={styles.toggleRow}
            onClick={() => patchCloudSyncForm({ enabled: !cloudSyncForm.enabled })}
            role="switch"
            aria-checked={cloudSyncForm.enabled}
            aria-label={t('settings:cloudSync.enabled.label')}
          >
            <div className={styles.toggleText}>
              <span className={styles.toggleLabel}>
                {t('settings:cloudSync.enabled.label')}
              </span>
              <span className={styles.toggleHelper}>
                {t('settings:cloudSync.enabled.helper')}
              </span>
            </div>
            <span className={styles.toggleState}>
              {cloudSyncForm.enabled ? (
                <Pill tone="success" dot>
                  <CheckIcon size={12} />
                  {t('settings:sync.enabled')}
                </Pill>
              ) : (
                <Pill tone="neutral" dot>
                  <XIcon size={12} />
                  {t('settings:sync.disabled')}
                </Pill>
              )}
            </span>
          </button>

          <button
            type="button"
            className={styles.toggleRow}
            onClick={() => patchCloudSyncForm({ auto: !cloudSyncForm.auto })}
            role="switch"
            aria-checked={cloudSyncForm.auto}
            aria-label={t('settings:cloudSync.auto.label')}
          >
            <div className={styles.toggleText}>
              <span className={styles.toggleLabel}>
                {t('settings:cloudSync.auto.label')}
              </span>
              <span className={styles.toggleHelper}>
                {t('settings:cloudSync.auto.helper')}
              </span>
            </div>
            <span className={styles.toggleState}>
              {cloudSyncForm.auto ? (
                <Pill tone="success" dot>
                  <CheckIcon size={12} />
                  {t('settings:sync.enabled')}
                </Pill>
              ) : (
                <Pill tone="neutral" dot>
                  <XIcon size={12} />
                  {t('settings:sync.disabled')}
                </Pill>
              )}
            </span>
          </button>
        </div>

        {cloudSync ? (
          <div className={styles.metaRow}>
            <span className={styles.metaKey}>{t('settings:cloudSync.appliedConfig')}</span>
            <span className={styles.metaValue}>
              {cloudSync.enabled ? t('settings:sync.enabled') : t('settings:sync.disabled')}
              {' · '}
              {cloudSync.repoUrl || '—'}
              {cloudSync.branch ? ` · ${cloudSync.branch}` : ''}
            </span>
          </div>
        ) : null}

        <div className={styles.aboutActions}>
          <Button
            variant="secondary"
            size="md"
            icon={<SyncIcon />}
            onClick={handleTestCloudSync}
            disabled={testing}
          >
            {testing ? t('settings:cloudSync.testing') : t('settings:cloudSync.testConnection')}
          </Button>
          <Button
            variant="ghost"
            size="md"
            onClick={handleResetCloudSyncDefaults}
            disabled={!canResetCloudSyncDefaults}
            title={
              canResetCloudSyncDefaults
                ? undefined
                : t('settings:resource.defaultsUnavailable')
            }
          >
            {t('settings:action.resetDefault')}
          </Button>
          <Button
            variant="secondary"
            size="md"
            onClick={handleApplyCloudSync}
            disabled={applying}
          >
            {applying ? t('settings:cloudSync.applying') : t('settings:cloudSync.apply')}
          </Button>
          <Button
            variant="primary"
            size="md"
            icon={<SyncIcon />}
            onClick={handleSyncNow}
            disabled={syncing}
          >
            {syncing ? t('settings:cloudSync.syncing') : t('settings:cloudSync.syncNow')}
          </Button>
        </div>

        {testResult ? (
          <span className={`${styles.aboutHint} ${testResult.ok ? '' : styles.dangerText}`}>
            <InfoIcon size={14} />
            <span>
              {testResult.ok
                ? t('settings:cloudSync.testOk', {
                    gitVersion: testResult.gitVersion ?? '—',
                    branch: testResult.defaultBranch ?? '—',
                  })
                : t('settings:cloudSync.testFailed', {
                    error: testResult.error ?? '',
                  })}
            </span>
          </span>
        ) : null}

        {syncResult ? (
          <div className={styles.metaRow}>
            <span className={styles.metaKey}>{t('settings:cloudSync.lastSync')}</span>
            <span className={`${styles.metaValue} ${syncResult.ok ? '' : styles.dangerText}`}>
              {syncResult.ok
                ? t('settings:cloudSync.syncSuccess', {
                    time: formatIsoTime(syncResult.syncedAt),
                    pulled: syncResult.pulled,
                    pushed: syncResult.pushed,
                  })
                : t('settings:cloudSync.syncFailed', {
                    time: formatIsoTime(syncResult.syncedAt),
                    note: syncResult.note,
                  })}
            </span>
          </div>
        ) : null}

        {cloudSyncLoadError ? (
          <div className={styles.resourceError} role="alert">
            <span className={styles.updateError}>
              {t('settings:resource.loadFailed', { error: cloudSyncLoadError.message })}
            </span>
            <Button
              variant="secondary"
              size="sm"
              onClick={() => void onRetryLoad()}
              disabled={retrying}
            >
              {retrying ? t('settings:resource.retrying') : t('settings:resource.retry')}
            </Button>
          </div>
        ) : null}

        {cloudSyncError ? (
          <span className={styles.updateError}>{cloudSyncError}</span>
        ) : null}
      </Card.Body>
    </Card>
  );
}
