/**
 * Settings 云端同步 / 局域网同步 / 备份恢复 控制器
 *
 * Business Logic（为什么需要这个 hook）:
 *   同步 tab 的 cloud/LAN/backup 状态与动作体量大，从 form-saves 再拆以守 soft 行数上限。
 *
 * Code Logic（这个 hook 做什么）:
 *   持有 cloudSync 表单与 backup/LAN 状态；safe-save 应用 cloud 配置；
 *   导出 applyFromResults/applyGroup 供全量/重试水合。
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { configApi } from '@/api/config';
import {
  backupApi,
  pickBackupArchivePath,
  pickBackupExportPath,
  BACKUP_RESTORE_DOMAINS,
  type BackupInspectPreview,
  type BackupRestoreResult,
  type BackupRestoreDomain,
  type RecoveryJobRow,
  type RestoreMode,
  type SyncRunResult,
  syncApi,
} from '@/api/sync';
import {
  createSaveAttempt,
  resolveSaveFailure,
  resolveSaveSuccess,
} from '@/lib/asyncState/saveAttempt';
import {
  isResourceReady,
  type PairResourceResult,
  type SettingsResourceResults,
} from '../settingsResources';
import {
  cloudSyncConfigToForm,
  cloudSyncFormToUpdate,
  PENDING_CLOUD_SYNC_FORM,
} from '../settingsState';
import type { CloudSyncForm } from '../settingsState';
import type { CloudSyncConfig, CloudSyncResult, TestCloudSyncResult } from '@/lib/types';
import type { ApplyGroupOptions } from '../settingsControllerShared';

/**
 * Sync/Backup hook 返回值。
 *
 * Business Logic（为什么需要这个接口）:
 *   FormSaves composer 需要透传同步 tab 字段与水合入口。
 *
 * Code Logic（这个接口做什么）:
 *   聚合 cloud/LAN/backup 状态、handlers 与 applyFromResults/applyGroup。
 */
export interface UseSettingsSyncBackupSavesResult {
  cloudSyncForm: CloudSyncForm;
  cloudSync: CloudSyncConfig | null;
  syncResult: CloudSyncResult | null;
  testResult: TestCloudSyncResult | null;
  cloudSyncError: string | null;
  testing: boolean;
  applying: boolean;
  syncing: boolean;
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
  patchCloudSyncForm: (partial: Partial<CloudSyncForm>) => void;
  handleResetCloudSyncDefaults: () => void;
  handleTestCloudSync: () => Promise<void>;
  handleApplyCloudSync: () => Promise<void>;
  handleSyncNow: () => Promise<void>;
  handleLanSyncNow: () => Promise<void>;
  handleBackupExport: () => Promise<void>;
  handleBackupPickRestore: () => Promise<void>;
  handleBackupToggleDomain: (domain: BackupRestoreDomain) => void;
  handleBackupSetMode: (mode: RestoreMode) => void;
  handleBackupOpenRestoreDialog: () => void;
  handleBackupRestoreConfirm: () => Promise<void>;
  handleCloseRestoreDialog: () => void;
  handleRefreshRecoveryJobs: () => Promise<void>;
  handleOpenRollback: (jobId: string) => void;
  handleConfirmRollback: () => Promise<void>;
  handleCloseRollbackDialog: () => void;
  applyFromResults: (results: SettingsResourceResults) => void;
  applyGroup: (
    pair: PairResourceResult<CloudSyncConfig>,
    options?: ApplyGroupOptions,
  ) => void;
}

/**
 * 云端同步 / LAN / 备份 hook
 *
 * Business Logic（为什么需要这个函数）:
 *   同步 tab 独立于 general/AI 表单，拆出后 FormSaves 可守 soft 上限。
 *
 * Code Logic（这个函数做什么）:
 *   持有 cloud/LAN/backup 全部 state 与 handlers，并提供资源水合入口。
 *
 * @returns 同步 tab 状态与动作
 */
export function useSettingsSyncBackupSaves(): UseSettingsSyncBackupSavesResult {
  const { t } = useTranslation(['settings', 'common']);
  const [cloudSyncForm, setCloudSyncForm] = useState<CloudSyncForm>({ ...PENDING_CLOUD_SYNC_FORM });
  const [defaultCloudSyncForm, setDefaultCloudSyncForm] = useState<CloudSyncForm>({
    ...PENDING_CLOUD_SYNC_FORM,
  });
  const [cloudSync, setCloudSync] = useState<CloudSyncConfig | null>(null);
  const [syncResult, setSyncResult] = useState<CloudSyncResult | null>(null);
  const [lanSyncResult, setLanSyncResult] = useState<SyncRunResult | null>(null);
  const [lanSyncing, setLanSyncing] = useState(false);
  const [lanSyncError, setLanSyncError] = useState<string | null>(null);
  const [backupExporting, setBackupExporting] = useState(false);
  const [backupExportPath, setBackupExportPath] = useState<string | null>(null);
  const [backupExportError, setBackupExportError] = useState<string | null>(null);
  const [backupRestoring, setBackupRestoring] = useState(false);
  const [backupInspect, setBackupInspect] = useState<BackupInspectPreview | null>(null);
  const [backupArchivePath, setBackupArchivePath] = useState<string | null>(null);
  const [backupSelectedDomains, setBackupSelectedDomains] = useState<BackupRestoreDomain[]>([
    ...BACKUP_RESTORE_DOMAINS,
  ]);
  const [backupMode, setBackupMode] = useState<RestoreMode>('merge');
  const [backupRestoreDialogOpen, setBackupRestoreDialogOpen] = useState(false);
  const [backupRestoreResult, setBackupRestoreResult] = useState<BackupRestoreResult | null>(null);
  const [backupRestoreError, setBackupRestoreError] = useState<string | null>(null);
  const [backupJobs, setBackupJobs] = useState<RecoveryJobRow[]>([]);
  const [backupJobsLoading, setBackupJobsLoading] = useState(false);
  const [backupJobsError, setBackupJobsError] = useState<string | null>(null);
  const [backupRollbackJobId, setBackupRollbackJobId] = useState<string | null>(null);
  const [backupRollbackDialogOpen, setBackupRollbackDialogOpen] = useState(false);
  const [backupRollingBack, setBackupRollingBack] = useState(false);
  const [testResult, setTestResult] = useState<TestCloudSyncResult | null>(null);
  const [cloudSyncError, setCloudSyncError] = useState<string | null>(null);
  const [testing, setTesting] = useState(false);
  const [applying, setApplying] = useState(false);
  const [syncing, setSyncing] = useState(false);

  const cloudSyncEditVersionRef = useRef(0);
  const cloudSyncRequestSeqRef = useRef(0);
  const cloudSyncFormRef = useRef(cloudSyncForm);
  const cloudSyncRef = useRef(cloudSync);
  useEffect(() => {
    cloudSyncFormRef.current = cloudSyncForm;
    cloudSyncRef.current = cloudSync;
  });

  /**
   * 全量资源结果中 cloudSync 组水合
   *
   * Business Logic（为什么需要这个函数）:
   *   首次加载把 ready 的 cloud current/defaults 写入表单。
   *
   * Code Logic（这个函数做什么）:
   *   仅 ready 时 set cloudSync/form/defaults。
   */
  const applyFromResults = useCallback((results: SettingsResourceResults) => {
    if (isResourceReady(results.cloudSync.current)) {
      const cloudSyncConfig = results.cloudSync.current.value;
      setCloudSync(cloudSyncConfig);
      setCloudSyncForm(cloudSyncConfigToForm(cloudSyncConfig));
      setCloudSyncError(null);
    }
    if (isResourceReady(results.cloudSync.defaults)) {
      setDefaultCloudSyncForm(cloudSyncConfigToForm(results.cloudSync.defaults.value));
    }
  }, []);

  /**
   * 单组 cloudSync 重试水合（dirty 保护）
   *
   * Business Logic（为什么需要这个函数）:
   *   重试成功不得覆盖未保存 cloud 草稿。
   *
   * Code Logic（这个函数做什么）:
   *   比较 form 与 server baseline；allowRewriteForm 或非 dirty 时写 form。
   */
  const applyGroup = useCallback(
    (pair: PairResourceResult<CloudSyncConfig>, options?: ApplyGroupOptions) => {
      const allowRewriteForm = options?.allowRewriteForm === true;
      if (isResourceReady(pair.current)) {
        setCloudSync(pair.current.value);
        const serverForm = cloudSyncConfigToForm(pair.current.value);
        const dirty =
          cloudSync !== null &&
          JSON.stringify(cloudSyncForm) !== JSON.stringify(cloudSyncConfigToForm(cloudSync));
        if (allowRewriteForm || !dirty) {
          setCloudSyncForm(serverForm);
        }
        setCloudSyncError(null);
      }
      if (isResourceReady(pair.defaults)) {
        setDefaultCloudSyncForm(cloudSyncConfigToForm(pair.defaults.value));
      }
    },
    [cloudSync, cloudSyncForm],
  );

  /**
   * 更新云端同步表单的某个字段（浅合并）
   */
  const patchCloudSyncForm = useCallback((partial: Partial<CloudSyncForm>) => {
    setCloudSyncForm((prev) => {
      const next = { ...prev, ...partial };
      cloudSyncFormRef.current = next;
      return next;
    });
    cloudSyncEditVersionRef.current += 1;
    setCloudSyncError(null);
  }, []);

  /**
   * 云端同步「恢复默认」
   */
  const handleResetCloudSyncDefaults = useCallback(() => {
    cloudSyncFormRef.current = defaultCloudSyncForm;
    cloudSyncEditVersionRef.current += 1;
    setCloudSyncForm(defaultCloudSyncForm);
    setCloudSyncError(null);
  }, [defaultCloudSyncForm]);

  /**
   * 云端同步「测试连接」
   */
  const handleTestCloudSync = async () => {
    setTesting(true);
    setCloudSyncError(null);
    setTestResult(null);
    try {
      const result = await configApi.testCloudSync();
      setTestResult(result);
    } catch (err) {
      setTestResult({
        ok: false,
        gitVersion: null,
        defaultBranch: null,
        error: err instanceof Error ? err.message : t('cloudSync.testFailed', { error: '' }).trim(),
      });
    } finally {
      setTesting(false);
    }
  };

  /**
   * 云端同步「应用配置」（safe-save）
   */
  const handleApplyCloudSync = async () => {
    const snapshot: CloudSyncForm = { ...cloudSyncFormRef.current };
    const attempt = createSaveAttempt(
      ++cloudSyncRequestSeqRef.current,
      snapshot,
      cloudSyncEditVersionRef.current,
    );
    setApplying(true);
    setCloudSyncError(null);
    try {
      const updated = await configApi.updateCloudSyncConfig(
        cloudSyncFormToUpdate(attempt.submittedSnapshot),
      );
      const serverForm = cloudSyncConfigToForm(updated);
      const baselineForm = cloudSyncRef.current
        ? cloudSyncConfigToForm(cloudSyncRef.current)
        : attempt.submittedSnapshot;
      const resolution = resolveSaveSuccess({
        attempt,
        currentRequestSeq: cloudSyncRequestSeqRef.current,
        currentDraft: cloudSyncFormRef.current,
        currentEditVersion: cloudSyncEditVersionRef.current,
        serverValue: serverForm,
        currentBaseline: baselineForm,
      });
      if (!resolution.applied) return;
      cloudSyncRef.current = updated;
      cloudSyncFormRef.current = resolution.draft;
      setCloudSync(updated);
      setCloudSyncForm(resolution.draft);
    } catch (err) {
      const baselineForm = cloudSyncRef.current
        ? cloudSyncConfigToForm(cloudSyncRef.current)
        : attempt.submittedSnapshot;
      const failure = resolveSaveFailure({
        attempt,
        currentRequestSeq: cloudSyncRequestSeqRef.current,
        currentDraft: cloudSyncFormRef.current,
        currentBaseline: baselineForm,
      });
      if (!failure.applied) return;
      setCloudSyncError(err instanceof Error ? err.message : t('settings:cloudSync.applyFailed'));
    } finally {
      if (attempt.requestSeq === cloudSyncRequestSeqRef.current) {
        setApplying(false);
      }
    }
  };

  /**
   * 云端同步「立即同步」
   */
  const handleSyncNow = async () => {
    setSyncing(true);
    setCloudSyncError(null);
    try {
      const result = await configApi.triggerCloudSync();
      setSyncResult(result);
    } catch (err) {
      setSyncResult({
        ok: false,
        pulled: 0,
        pushed: 0,
        note: err instanceof Error ? err.message : t('cloudSync.syncFailed', { time: '', note: '' }),
        syncedAt: new Date().toISOString(),
      });
    } finally {
      setSyncing(false);
    }
  };

  /**
   * 局域网同步「立即同步」
   */
  const handleLanSyncNow = async () => {
    setLanSyncing(true);
    setLanSyncError(null);
    try {
      const result = await syncApi.trigger();
      setLanSyncResult(result);
    } catch (err) {
      setLanSyncResult(null);
      setLanSyncError(err instanceof Error ? err.message : t('settings:lanSync.failed'));
    } finally {
      setLanSyncing(false);
    }
  };

  /**
   * 刷新恢复任务列表
   */
  const handleRefreshRecoveryJobs = useCallback(async () => {
    setBackupJobsLoading(true);
    setBackupJobsError(null);
    try {
      const jobs = await backupApi.listJobs(50);
      setBackupJobs(jobs);
    } catch (err) {
      setBackupJobsError(
        err instanceof Error ? err.message : t('settings:backup.restoreFailed'),
      );
    } finally {
      setBackupJobsLoading(false);
    }
  }, [t]);

  /* eslint-disable react-hooks/set-state-in-effect -- 合法 fetch-in-effect */
  useEffect(() => {
    void handleRefreshRecoveryJobs();
  }, [handleRefreshRecoveryJobs]);
  /* eslint-enable react-hooks/set-state-in-effect */

  /**
   * 导出可验证备份包
   */
  const handleBackupExport = async () => {
    setBackupExportError(null);
    setBackupExportPath(null);
    const destPath = await pickBackupExportPath();
    if (!destPath) return;
    setBackupExporting(true);
    try {
      const result = await backupApi.create(destPath);
      setBackupExportPath(result.path);
      await handleRefreshRecoveryJobs();
    } catch (err) {
      setBackupExportError(
        err instanceof Error ? err.message : t('settings:backup.exportFailed'),
      );
    } finally {
      setBackupExporting(false);
    }
  };

  /**
   * 选择备份文件并 inspect 预览
   */
  const handleBackupPickRestore = async () => {
    setBackupRestoreError(null);
    setBackupRestoreResult(null);
    setBackupInspect(null);
    setBackupArchivePath(null);
    const archivePath = await pickBackupArchivePath();
    if (!archivePath) return;
    setBackupRestoring(true);
    try {
      const preview = await backupApi.inspect(archivePath);
      setBackupArchivePath(archivePath);
      setBackupInspect(preview);
      setBackupSelectedDomains([...BACKUP_RESTORE_DOMAINS]);
      setBackupMode('merge');
      await handleRefreshRecoveryJobs();
    } catch (err) {
      setBackupRestoreError(
        err instanceof Error ? err.message : t('settings:backup.restoreFailed'),
      );
    } finally {
      setBackupRestoring(false);
    }
  };

  /**
   * 切换恢复领域勾选
   */
  const handleBackupToggleDomain = useCallback((domain: BackupRestoreDomain) => {
    setBackupSelectedDomains((prev) =>
      prev.includes(domain) ? prev.filter((d) => d !== domain) : [...prev, domain],
    );
  }, []);

  /**
   * 设置恢复模式
   */
  const handleBackupSetMode = useCallback((mode: RestoreMode) => {
    setBackupMode(mode);
  }, []);

  /**
   * 打开恢复确认 Dialog
   */
  const handleBackupOpenRestoreDialog = useCallback(() => {
    if (!backupArchivePath || !backupInspect) return;
    if (backupSelectedDomains.length === 0) {
      setBackupRestoreError(t('settings:backup.noDomainsSelected'));
      return;
    }
    setBackupRestoreError(null);
    setBackupRestoreDialogOpen(true);
  }, [backupArchivePath, backupInspect, backupSelectedDomains.length, t]);

  /**
   * 确认执行 restore
   */
  const handleBackupRestoreConfirm = async () => {
    if (!backupArchivePath || backupSelectedDomains.length === 0) {
      setBackupRestoreError(t('settings:backup.noDomainsSelected'));
      return;
    }
    setBackupRestoring(true);
    setBackupRestoreError(null);
    try {
      const result = await backupApi.restore(
        backupArchivePath,
        backupMode,
        [...backupSelectedDomains],
      );
      setBackupRestoreResult(result);
      setBackupRestoreDialogOpen(false);
      await handleRefreshRecoveryJobs();
    } catch (err) {
      setBackupRestoreError(
        err instanceof Error ? err.message : t('settings:backup.restoreFailed'),
      );
    } finally {
      setBackupRestoring(false);
    }
  };

  /**
   * 关闭恢复确认 Dialog
   */
  const handleCloseRestoreDialog = useCallback(() => {
    if (backupRestoring) return;
    setBackupRestoreDialogOpen(false);
  }, [backupRestoring]);

  /**
   * 打开回滚确认
   */
  const handleOpenRollback = useCallback((jobId: string) => {
    setBackupRollbackJobId(jobId);
    setBackupRollbackDialogOpen(true);
  }, []);

  /**
   * 关闭回滚确认 Dialog
   */
  const handleCloseRollbackDialog = useCallback(() => {
    if (backupRollingBack) return;
    setBackupRollbackDialogOpen(false);
    setBackupRollbackJobId(null);
  }, [backupRollingBack]);

  /**
   * 确认回滚
   */
  const handleConfirmRollback = async () => {
    if (!backupRollbackJobId) return;
    setBackupRollingBack(true);
    setBackupRestoreError(null);
    try {
      const result = await backupApi.rollback(backupRollbackJobId);
      setBackupRestoreResult(result);
      setBackupRollbackDialogOpen(false);
      setBackupRollbackJobId(null);
      await handleRefreshRecoveryJobs();
    } catch (err) {
      setBackupRestoreError(
        err instanceof Error ? err.message : t('settings:backup.restoreFailed'),
      );
    } finally {
      setBackupRollingBack(false);
    }
  };

  return {
    cloudSyncForm,
    cloudSync,
    syncResult,
    testResult,
    cloudSyncError,
    testing,
    applying,
    syncing,
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
    patchCloudSyncForm,
    handleResetCloudSyncDefaults,
    handleTestCloudSync,
    handleApplyCloudSync,
    handleSyncNow,
    handleLanSyncNow,
    handleBackupExport,
    handleBackupPickRestore,
    handleBackupToggleDomain,
    handleBackupSetMode,
    handleBackupOpenRestoreDialog,
    handleBackupRestoreConfirm,
    handleCloseRestoreDialog,
    handleRefreshRecoveryJobs,
    handleOpenRollback,
    handleConfirmRollback,
    handleCloseRollbackDialog,
    applyFromResults,
    applyGroup,
  };
}
