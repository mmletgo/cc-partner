/**
 * Settings 同步面板（局域网同步 + 可验证导出/恢复）
 *
 * Business Logic（为什么需要这个组件）:
 *   用户在同步 tab 查看 LAN 同步真值、导出/恢复备份；GitHub 云端同步已迁入内测功能页。
 *
 * Code Logic（这个组件做什么）:
 *   渲染 LAN Card、备份导出/恢复 Card（含 Dialog）；
 *   可从 @/api/sync 导入类型与 pure helpers，禁止调用 backupApi/syncApi/invoke。
 */
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { Card, Button, Pill, Dialog } from '@/components/primitives';
import { CheckIcon, XIcon, SyncIcon, InfoIcon } from '@/lib/icons';
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
  getBackupRestoreDomains,
  isDeviceSucceeded,
  succeededCounts,
} from '@/api/sync';
import styles from './Settings.module.css';

/**
 * 同步面板 props
 *
 * Business Logic（为什么需要这个接口）:
 *   Settings 壳层透传 controller 的云端同步/LAN/备份表单与动作。
 *
 * Code Logic（这个接口做什么）:
 *   声明 LAN/备份结果、loading/error 与 sync/backup 回调。
 */
export interface SettingsSyncPanelProps {
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
  onLanSyncNow: () => void;
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
 * 同步设置面板
 *
 * Business Logic（为什么需要这个组件）:
 *   同步 tab 是独立业务组，需要 pure 视图配合 settingsResources 局部错误/重试。
 *
 * Code Logic（这个组件做什么）:
 *   useTranslation 置顶；渲染 LAN / 备份 Card 与 Dialog。
 *
 * @param props 受控同步表单与动作
 * @returns 同步 tab 内容
 */
export function SettingsSyncPanel({
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
  onLanSyncNow: handleLanSyncNow,
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
            {getBackupRestoreDomains(backupInspect.domainCounts).map((domain) => {
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
