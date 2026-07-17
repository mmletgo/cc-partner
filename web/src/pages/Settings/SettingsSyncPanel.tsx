/**
 * Settings 同步面板（局域网同步 + 可验证导出/恢复 + 云端同步）
 *
 * Business Logic（为什么需要这个组件）:
 *   用户在同步 tab 查看 LAN 同步真值、导出/恢复备份，并编辑 GitHub 私有仓库同步配置；
 *   状态与 API 调用由 controller 持有，本组件只渲染。
 *
 * Code Logic（这个组件做什么）:
 *   渲染 LAN Card、备份导出/恢复 Card（含 Dialog）、云端同步 Card；
 *   可从 @/api/sync 导入类型与 pure helpers，禁止调用 backupApi/syncApi/invoke。
 */
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Card, Button, Input, Pill, Dialog } from '@/components/primitives';
import { CheckIcon, XIcon, SyncIcon, InfoIcon } from '@/lib/icons';
import type {
  CloudSyncConfig,
  CloudSyncResult,
  TestCloudSyncResult,
} from '@/lib/types';
import type {
  BackupInspectPreview,
  BackupRestoreDomain,
  BackupRestoreResult,
  DeviceSyncStatus,
  RecoveryJobRow,
  RestoreMode,
  SyncRunResult,
} from '@/api/sync';
import {
  BACKUP_RESTORE_DOMAINS,
  isDeviceSucceeded,
  succeededCounts,
} from '@/api/sync';
import type { CloudSyncForm } from './settingsState';
import styles from './Settings.module.css';

/**
 * 把 ISO 时间字符串格式化为 "HH:MM:SS" 本地时间
 *
 * Business Logic（为什么需要这个函数）:
 *   同步结果需要展示最近一次同步的本地时刻。
 *
 * Code Logic（这个函数做什么）:
 *   解析 ISO；非法则原样返回；否则输出 HH:MM:SS。
 *
 * @param iso ISO 时间字符串
 * @returns 形如 "12:34:56" 的本地时间
 */
function formatIsoTime(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const pad = (n: number) => n.toString().padStart(2, '0');
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

/**
 * 同步面板 props
 *
 * Business Logic（为什么需要这个接口）:
 *   Settings 壳层透传 controller 的云端同步/LAN/备份表单与动作。
 *
 * Code Logic（这个接口做什么）:
 *   声明 form/applied config/结果/loading/error 与 patch/reset/test/apply/sync/backup 回调。
 */
export interface SettingsSyncPanelProps {
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
  /** 局域网同步结果 */
  lanSyncResult: SyncRunResult | null;
  lanSyncing: boolean;
  lanSyncError: string | null;
  backupExporting: boolean;
  backupExportPath: string | null;
  backupExportError: string | null;
  backupRestoring: boolean;
  backupInspect: BackupInspectPreview | null;
  backupArchivePath: string | null;
  backupSelectedDomains: BackupRestoreDomain[];
  backupMode: RestoreMode;
  backupRestoreDialogOpen: boolean;
  backupRestoreResult: BackupRestoreResult | null;
  backupRestoreError: string | null;
  backupJobs: RecoveryJobRow[];
  backupJobsLoading: boolean;
  backupJobsError: string | null;
  backupRollbackJobId: string | null;
  backupRollbackDialogOpen: boolean;
  backupRollingBack: boolean;
  onPatch: (partial: Partial<CloudSyncForm>) => void;
  onResetDefaults: () => void;
  onTest: () => void;
  onApply: () => void;
  onSyncNow: () => void;
  onLanSyncNow: () => void;
  onRetryLoad: () => void;
  onBackupExport: () => void;
  onBackupPickRestore: () => void;
  onBackupToggleDomain: (domain: BackupRestoreDomain) => void;
  onBackupSetMode: (mode: RestoreMode) => void;
  onBackupOpenRestoreDialog: () => void;
  onBackupRestoreConfirm: () => void;
  onCloseRestoreDialog: () => void;
  onRefreshRecoveryJobs: () => void;
  onOpenRollback: (jobId: string) => void;
  onConfirmRollback: () => void;
  onCloseRollbackDialog: () => void;
}

/**
 * 设备状态 → Pill tone。
 *
 * Business Logic: partial/unreachable 不得使用 success 色。
 * Code Logic: succeeded→success；其余 warn/danger/neutral。
 */
function deviceStatusTone(
  status: DeviceSyncStatus,
): 'success' | 'warn' | 'danger' | 'neutral' {
  if (isDeviceSucceeded(status)) return 'success';
  if (status === 'partial' || status === 'resource_limit') return 'warn';
  if (status === 'unreachable' || status === 'protocol_error') return 'danger';
  return 'neutral';
}

/**
 * 是否允许对该 recovery job 显示回滚按钮。
 *
 * Business Logic（为什么需要这个函数）:
 *   仅 succeeded/failed 且有 pre-restore 路径的任务可回滚。
 *
 * Code Logic（这个函数做什么）:
 *   检查 status 与 preRestoreBackupPath。
 */
function canRollbackJob(job: RecoveryJobRow): boolean {
  if (!job.preRestoreBackupPath) return false;
  return job.status === 'succeeded' || job.status === 'failed';
}

/**
 * 云端同步设置面板
 *
 * Business Logic（为什么需要这个组件）:
 *   同步 tab 是独立业务组，需要 pure 视图配合 settingsResources 局部错误/重试。
 *
 * Code Logic（这个组件做什么）:
 *   useTranslation 置顶；渲染 LAN / 备份 / 云端同步 Card 与 Dialog。
 *
 * @param props 受控同步表单与动作
 * @returns 同步 tab 内容
 */
export function SettingsSyncPanel({
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
  lanSyncResult,
  lanSyncing,
  lanSyncError,
  backupExporting,
  backupExportPath,
  backupExportError,
  backupRestoring,
  backupInspect,
  backupArchivePath,
  backupSelectedDomains,
  backupMode,
  backupRestoreDialogOpen,
  backupRestoreResult,
  backupRestoreError,
  backupJobs,
  backupJobsLoading,
  backupJobsError,
  backupRollbackJobId,
  backupRollbackDialogOpen,
  backupRollingBack,
  onPatch: patchCloudSyncForm,
  onResetDefaults: handleResetCloudSyncDefaults,
  onTest: handleTestCloudSync,
  onApply: handleApplyCloudSync,
  onSyncNow: handleSyncNow,
  onLanSyncNow: handleLanSyncNow,
  onRetryLoad,
  onBackupExport,
  onBackupPickRestore,
  onBackupToggleDomain,
  onBackupSetMode,
  onBackupOpenRestoreDialog,
  onBackupRestoreConfirm,
  onCloseRestoreDialog,
  onRefreshRecoveryJobs,
  onOpenRollback,
  onConfirmRollback,
  onCloseRollbackDialog,
}: SettingsSyncPanelProps): ReactElement {
  const { t } = useTranslation(['settings', 'common']);
  const retryingGroupCloudSync = retrying;
  const restoreBusy = backupRestoring;
  const rollbackBusy = backupRollingBack;

  return (
    <>
{/* Card: 局域网同步（per-device/domain 真值） */}
<Card variant="flat" padding="md">
  <Card.Header>
    <h2 className={styles.sectionTitle}>{t('settings:lanSync.title')}</h2>
  </Card.Header>
  <Card.Body padding="md">
    <p className={styles.helper}>{t('settings:lanSync.subtitle')}</p>
    <div className={styles.aboutActions}>
      <Button
        variant="primary"
        size="md"
        icon={<SyncIcon />}
        onClick={handleLanSyncNow}
        disabled={lanSyncing}
        data-testid="lan-sync-now"
      >
        {lanSyncing ? t('settings:lanSync.syncing') : t('settings:lanSync.syncNow')}
      </Button>
    </div>
    {lanSyncResult ? (
      <div data-testid="lan-sync-result">
        <div className={styles.metaRow}>
          <span className={styles.metaKey}>{t('settings:lanSync.lastRun')}</span>
          <span className={styles.metaValue}>
            {lanSyncResult.devices.length === 0
              ? t('settings:lanSync.noDevices')
              : t('settings:lanSync.summary', {
                  succeeded: lanSyncResult.succeeded_devices,
                  total: lanSyncResult.devices.length,
                })}
          </span>
        </div>
        {lanSyncResult.devices.map((device) => (
          <div
            key={device.device_id}
            className={styles.metaRow}
            data-testid={`lan-sync-device-${device.device_id}`}
            data-status={device.status}
          >
            <span className={styles.metaKey}>
              {device.device_name || device.device_id}
            </span>
            <span className={styles.metaValue}>
              <Pill
                tone={deviceStatusTone(device.status)}
                dot
                data-testid={`lan-sync-device-status-${device.device_id}`}
              >
                {t(`settings:lanSync.deviceStatus.${device.status}`)}
              </Pill>
              <ul className={styles.helper}>
                {device.domains.map((domain) => {
                  const counts = succeededCounts(domain.outcome);
                  return (
                    <li
                      key={`${device.device_id}-${domain.domain}`}
                      data-testid={`lan-sync-domain-${device.device_id}-${domain.domain}`}
                      data-kind={domain.outcome.kind}
                    >
                      {t(`settings:lanSync.domain.${domain.domain}`, {
                        defaultValue: domain.domain,
                      })}
                      {': '}
                      {t(`settings:lanSync.deviceStatus.${
                        domain.outcome.kind === 'succeeded'
                          ? 'succeeded'
                          : domain.outcome.kind === 'partial'
                            ? 'partial'
                            : domain.outcome.kind === 'unreachable'
                              ? 'unreachable'
                              : domain.outcome.kind === 'protocol_error'
                                ? 'protocol_error'
                                : 'resource_limit'
                      }`)}
                      {counts
                        ? ` — ${t('settings:lanSync.counts', {
                            pulled: counts.pulled,
                            pushed: counts.pushed,
                            unchanged: counts.unchanged,
                          })}`
                        : null}
                    </li>
                  );
                })}
              </ul>
            </span>
          </div>
        ))}
      </div>
    ) : null}
    {lanSyncError ? (
      <span className={styles.updateError} data-testid="lan-sync-error">
        {lanSyncError}
      </span>
    ) : null}
  </Card.Body>
</Card>

{/* Card: 可验证导出 / 恢复 */}
<Card variant="flat" padding="md">
  <Card.Header>
    <h2 className={styles.sectionTitle}>{t('settings:backup.title')}</h2>
  </Card.Header>
  <Card.Body padding="md">
    <p className={styles.helper}>{t('settings:backup.subtitle')}</p>
    <div className={styles.aboutActions}>
      <Button
        variant="primary"
        size="md"
        onClick={onBackupExport}
        disabled={backupExporting || restoreBusy}
        data-testid="backup-export"
      >
        {backupExporting ? t('settings:backup.exporting') : t('settings:backup.export')}
      </Button>
      <Button
        variant="secondary"
        size="md"
        onClick={onBackupPickRestore}
        disabled={backupExporting || restoreBusy}
        data-testid="backup-restore-pick"
      >
        {restoreBusy && !backupInspect
          ? t('settings:backup.inspecting')
          : t('settings:backup.restore')}
      </Button>
    </div>

    {backupExportPath ? (
      <span className={styles.aboutHint} data-testid="backup-export-success">
        <InfoIcon size={14} />
        <span>{t('settings:backup.exportSuccess', { path: backupExportPath })}</span>
      </span>
    ) : null}
    {backupExportError ? (
      <span className={styles.updateError} data-testid="backup-export-error">
        {backupExportError}
      </span>
    ) : null}

    {backupInspect && backupArchivePath ? (
      <div data-testid="backup-inspect-preview">
        <h3 className={styles.sectionTitle}>{t('settings:backup.previewTitle')}</h3>
        <div className={styles.metaRow}>
          <span className={styles.metaKey}>
            {t('settings:backup.formatVersion', { version: backupInspect.formatVersion })}
          </span>
          <span className={styles.metaValue}>
            {t('settings:backup.conflictsEstimate', {
              count: backupInspect.conflictsEstimate,
            })}
          </span>
        </div>
        <div className={styles.metaRow}>
          <span className={styles.metaKey}>{t('settings:backup.domainCounts')}</span>
          <span className={styles.metaValue}>
            {Object.entries(backupInspect.domainCounts)
              .map(([domain, count]) => `${domain}: ${count}`)
              .join(' · ') || '—'}
          </span>
        </div>
        {backupInspect.warnings.length > 0 ? (
          <div className={styles.metaRow}>
            <span className={styles.metaKey}>{t('settings:backup.warnings')}</span>
            <span className={`${styles.metaValue} ${styles.dangerText}`}>
              {backupInspect.warnings.join('; ')}
            </span>
          </div>
        ) : null}

        <div className={styles.field}>
          <span className={styles.label}>{t('settings:backup.selectDomains')}</span>
          <div className={styles.toggleList}>
            {BACKUP_RESTORE_DOMAINS.map((domain) => {
              const checked = backupSelectedDomains.includes(domain);
              return (
                <button
                  key={domain}
                  type="button"
                  className={styles.toggleRow}
                  role="checkbox"
                  aria-checked={checked}
                  data-testid={`backup-domain-${domain}`}
                  onClick={() => onBackupToggleDomain(domain)}
                  disabled={restoreBusy}
                >
                  <div className={styles.toggleText}>
                    <span className={styles.toggleLabel}>
                      {t(`settings:backup.domain.${domain}`)}
                    </span>
                  </div>
                  <span className={styles.toggleState}>
                    {checked ? (
                      <Pill tone="success" dot>
                        <CheckIcon size={12} />
                      </Pill>
                    ) : (
                      <Pill tone="neutral" dot>
                        <XIcon size={12} />
                      </Pill>
                    )}
                  </span>
                </button>
              );
            })}
          </div>
        </div>

        <div className={styles.field}>
          <span className={styles.label}>{t('settings:backup.mode')}</span>
          <div className={styles.aboutActions}>
            <Button
              variant={backupMode === 'merge' ? 'primary' : 'secondary'}
              size="sm"
              data-testid="backup-mode-merge"
              onClick={() => onBackupSetMode('merge')}
              disabled={restoreBusy}
            >
              {t('settings:backup.modeMerge')}
            </Button>
            <Button
              variant={backupMode === 'replaceDomain' ? 'primary' : 'secondary'}
              size="sm"
              data-testid="backup-mode-replace"
              onClick={() => onBackupSetMode('replaceDomain')}
              disabled={restoreBusy}
            >
              {t('settings:backup.modeReplaceDomain')}
            </Button>
          </div>
          <p className={styles.helper}>
            {backupMode === 'merge'
              ? t('settings:backup.modeMergeHelper')
              : t('settings:backup.modeReplaceHelper')}
          </p>
        </div>

        <div className={styles.aboutActions}>
          <Button
            variant="primary"
            size="md"
            data-testid="backup-restore-confirm"
            onClick={onBackupOpenRestoreDialog}
            disabled={restoreBusy || backupSelectedDomains.length === 0}
          >
            {t('settings:backup.confirmRestore')}
          </Button>
        </div>
      </div>
    ) : null}

    {backupRestoreResult ? (
      <div className={styles.metaRow} data-testid="backup-restore-result">
        <span className={styles.metaKey}>
          {t('settings:backup.restoreSuccess', { status: backupRestoreResult.status })}
        </span>
        <span className={styles.metaValue}>
          {t('settings:backup.appliedDomains', {
            domains: backupRestoreResult.appliedDomains.join(', ') || '—',
          })}
          {backupRestoreResult.preRestoreBackupPath
            ? ` · ${t('settings:backup.preRestorePath', {
                path: backupRestoreResult.preRestoreBackupPath,
              })}`
            : null}
        </span>
      </div>
    ) : null}
    {backupRestoreError ? (
      <span className={styles.updateError} data-testid="backup-restore-error">
        {backupRestoreError}
      </span>
    ) : null}

    <div data-testid="backup-jobs-list">
      <div className={styles.metaRow}>
        <span className={styles.metaKey}>{t('settings:backup.jobsTitle')}</span>
        <span className={styles.metaValue}>
          <Button
            variant="ghost"
            size="sm"
            onClick={onRefreshRecoveryJobs}
            disabled={backupJobsLoading || restoreBusy || rollbackBusy}
            data-testid="backup-jobs-refresh"
          >
            {backupJobsLoading
              ? t('settings:resource.retrying')
              : t('settings:backup.refreshJobs')}
          </Button>
        </span>
      </div>
      {backupJobsError ? (
        <span className={styles.updateError} data-testid="backup-jobs-error">
          {backupJobsError}
        </span>
      ) : null}
      {backupJobs.length === 0 && !backupJobsLoading ? (
        <p className={styles.helper} data-testid="backup-jobs-empty">
          {t('settings:backup.jobsEmpty')}
        </p>
      ) : (
        backupJobs.map((job) => (
          <div
            key={job.id}
            className={styles.metaRow}
            data-testid={`backup-job-${job.id}`}
          >
            <span className={styles.metaKey}>
              {t('settings:backup.jobStatus', { status: job.status })}
              {' · '}
              {t('settings:backup.jobMode', { mode: job.mode })}
            </span>
            <span className={styles.metaValue}>
              {canRollbackJob(job) ? (
                <Button
                  variant="secondary"
                  size="sm"
                  data-testid={`backup-rollback-${job.id}`}
                  onClick={() => onOpenRollback(job.id)}
                  disabled={rollbackBusy || restoreBusy}
                >
                  {t('settings:backup.rollback')}
                </Button>
              ) : null}
            </span>
          </div>
        ))
      )}
    </div>
  </Card.Body>
</Card>

{/* Card: 云端同步（GitHub 私有仓库，独立操作块，不混入底部统一 Save） */}
<Card variant="flat" padding="md">
  <Card.Header>
    <h2 className={styles.sectionTitle}>{t('settings:cloudSync.title')}</h2>
  </Card.Header>
  <Card.Body padding="md">
    <p className={styles.helper}>{t('settings:cloudSync.subtitle')}</p>

    {/* 仓库地址 */}
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

    {/* 分支 */}
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

    {/* 同步间隔 */}
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

    {/* 启用 / 自动定时 Toggle，复用同步与存储 Card 的视觉风格 */}
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

    {/* 当前已应用配置快照（与表单待编辑值区分） */}
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

    {/* 操作按钮组 */}
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

    {/* 测试结果 */}
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

    {/* 上次同步结果 */}
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

    {/* 分组加载失败：局部重试，不重置其他 tab */}
    {cloudSyncLoadError ? (
      <div className={styles.resourceError} role="alert">
        <span className={styles.updateError}>
          {t('settings:resource.loadFailed', { error: cloudSyncLoadError.message })}
        </span>
        <Button
          variant="secondary"
          size="sm"
          onClick={() => void onRetryLoad()}
          disabled={retryingGroupCloudSync}
        >
          {retryingGroupCloudSync
            ? t('settings:resource.retrying')
            : t('settings:resource.retry')}
        </Button>
      </div>
    ) : null}

    {/* 应用配置 / 同步失败错误提示 */}
    {cloudSyncError ? (
      <span className={styles.updateError}>{cloudSyncError}</span>
    ) : null}
  </Card.Body>
</Card>

{/* 恢复确认 Dialog */}
<Dialog
  open={backupRestoreDialogOpen}
  titleId="backup-restore-dialog-title"
  onClose={onCloseRestoreDialog}
  closeOnEscape={!restoreBusy}
  closeOnBackdrop={!restoreBusy}
  className={styles.dialogWithCard}
>
  <Card variant="elevated" padding="md">
    <Card.Header>
      <h3 id="backup-restore-dialog-title" className={styles.sectionTitle}>
        {t('settings:backup.confirmRestore')}
      </h3>
    </Card.Header>
    <Card.Body padding="md">
      <p className={styles.helper}>{t('settings:backup.confirmRestoreText')}</p>
      <div className={styles.aboutActions}>
        <Button
          variant="ghost"
          size="md"
          disabled={restoreBusy}
          onClick={onCloseRestoreDialog}
        >
          {t('common:action.cancel')}
        </Button>
        <Button
          variant="primary"
          size="md"
          disabled={restoreBusy}
          data-testid="backup-restore-dialog-confirm"
          onClick={onBackupRestoreConfirm}
        >
          {restoreBusy ? t('settings:backup.restoring') : t('settings:backup.confirmRestore')}
        </Button>
      </div>
    </Card.Body>
  </Card>
</Dialog>

{/* 回滚确认 Dialog */}
<Dialog
  open={backupRollbackDialogOpen}
  titleId="backup-rollback-dialog-title"
  onClose={onCloseRollbackDialog}
  closeOnEscape={!rollbackBusy}
  closeOnBackdrop={!rollbackBusy}
  className={styles.dialogWithCard}
>
  <Card variant="elevated" padding="md">
    <Card.Header>
      <h3 id="backup-rollback-dialog-title" className={styles.sectionTitle}>
        {t('settings:backup.confirmRollback')}
      </h3>
    </Card.Header>
    <Card.Body padding="md">
      <p className={styles.helper}>
        {t('settings:backup.confirmRollbackText')}
        {backupRollbackJobId ? ` (${backupRollbackJobId})` : null}
      </p>
      <div className={styles.aboutActions}>
        <Button
          variant="ghost"
          size="md"
          disabled={rollbackBusy}
          onClick={onCloseRollbackDialog}
        >
          {t('common:action.cancel')}
        </Button>
        <Button
          variant="danger"
          size="md"
          disabled={rollbackBusy}
          data-testid="backup-rollback-dialog-confirm"
          onClick={onConfirmRollback}
        >
          {rollbackBusy ? t('settings:backup.rollingBack') : t('settings:backup.rollback')}
        </Button>
      </div>
    </Card.Body>
  </Card>
</Dialog>
    </>
  );
}

SettingsSyncPanel.displayName = 'SettingsSyncPanel';
