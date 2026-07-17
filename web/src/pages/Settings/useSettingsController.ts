/**
 * Settings 页面控制器 hook（composer）
 *
 * Business Logic（为什么需要这个 hook）:
 *   Settings 同时持有 7 个 tab 的表单/资源加载/保存/重试状态；把编排从 JSX 中拆出，
 *   让 Settings.tsx 只做 tab/layout 组合，panel 保持纯 props 渲染。
 *
 * Code Logic（这个 hook 做什么）:
 *   组合 useSettingsResources / useSettingsFormSaves / useSettingsUpdatePermissions，
 *   并持有 URL tab 真源；返回 shell 与各 panel 所需字段，不渲染 tab JSX 树。
 *   公共 export 面（SETTINGS_TABS、helpers、UseSettingsControllerResult）保持从本文件可导入。
 */
import { useCallback, useMemo, useState } from 'react';
import type { KeyboardEvent } from 'react';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { useSearchParams } from 'react-router-dom';
import { backendApi } from '@/api/backend';
import { workbenchApi } from '@/api/workbench';
import {
  permissionOnboardedKey,
  permissionSkippedKey,
  type AppFlavor,
} from '@/hooks/usePermissions';
import { configApi } from '@/api/config';
import { pendingWrites } from '@/lib/pendingWrites';
import {
  installButtonMode,
  parseSettingsTabFromSearch,
  type SettingsTabId,
} from './settingsState';
import type {
  CloudSyncForm,
  GithubTrendingForm,
  HealthForm,
  PromptOptimizerSettingsForm,
  SettingsState,
} from './settingsState';
import type { AutomationSettingsForm } from './automationSettingsState';
import type {
  VersionInfo,
  UpdateCheckResult,
  UpdateDownloadStatus,
  PermissionType,
  CloudSyncConfig,
  CloudSyncResult,
  TestCloudSyncResult,
  ClaudeCliTestResult,
  GithubTrendingConfig,
  HealthConfig,
} from '@/lib/types';
import type {
  BackupInspectPreview,
  BackupRestoreResult,
  BackupRestoreDomain,
  RecoveryJobRow,
  RestoreMode,
  SyncRunResult,
} from '@/api/sync';
import type { SettingsResourceGroup, SettingsResourceResults } from './settingsResources';
import { formatShortcutForDisplay } from './shortcutRecorder';
import {
  SETTINGS_TABS,
  PROMPT_OPTIMIZER_SHORTCUT_ID,
  buildUpdateHint,
  formatTime,
  formatSize,
  type SettingsTab,
} from './settingsControllerShared';
import { useSettingsFormSaves } from './controllers/useSettingsFormSaves';
import { useSettingsResources } from './controllers/useSettingsResources';
import { useSettingsUpdatePermissions } from './controllers/useSettingsUpdatePermissions';

export type { SettingsTab };
export {
  SETTINGS_TABS,
  PROMPT_OPTIMIZER_SHORTCUT_ID,
  buildUpdateHint,
  formatTime,
  formatSize,
};

/**
 * Settings 控制器返回值：shell 与各 panel 所需字段。
 *
 * Business Logic（为什么需要这个接口）:
 *   Settings 壳层与 pure panel 需要稳定、可组合的 props 契约，避免再从页面散落读取内部 state。
 *
 * Code Logic（这个接口做什么）:
 *   聚合 loading/error/tab 壳层字段与 general/sync/dependencies/health/automation/ai/about 所需数据。
 */
export interface UseSettingsControllerResult {
  t: TFunction<['settings', 'common']>;
  loading: boolean;
  loadError: string | null;
  resourceResults: SettingsResourceResults | null;
  retryingGroup: SettingsResourceGroup | null;
  handleRetryResourceGroup: (group: SettingsResourceGroup) => Promise<void>;
  activeTab: SettingsTabId;
  setActiveTab: (tab: SettingsTabId) => void;
  handleTabKeyDown: (e: KeyboardEvent<HTMLButtonElement>, currentIndex: number) => void;
  tabs: typeof SETTINGS_TABS;

  // general
  state: SettingsState;
  isDirty: boolean;
  savedAt: Date | null;
  saving: boolean;
  /** 常规 tab 保存失败文案；不得写入 loadError，否则整页会卸掉脏表单 */
  saveError: string | null;
  choosingDir: boolean;
  canResetCoreDefaults: boolean;
  recordingShortcutId: string | null;
  handleDeviceNameChange: (e: import('react').ChangeEvent<HTMLInputElement>) => void;
  handleReceiveDirChange: (e: import('react').ChangeEvent<HTMLInputElement>) => void;
  handleChooseDir: () => Promise<void>;
  handleShortcutFocus: (id: string) => void;
  handleShortcutBlur: (id: string) => void;
  handleShortcutKeyDown: (e: KeyboardEvent<HTMLInputElement>, id: string) => void;
  handleResetDefaults: () => void;
  handleSave: () => Promise<void>;
  agentLedgerClearDialogOpen: boolean;
  agentLedgerClearing: boolean;
  agentLedgerClearMessage: string | null;
  agentLedgerClearError: string | null;
  openAgentLedgerClearDialog: () => void;
  closeAgentLedgerClearDialog: () => void;
  confirmClearAgentLedger: () => Promise<void>;
  onboardingResetDialogOpen: boolean;
  onboardingResetting: boolean;
  onboardingResetError: string | null;
  openOnboardingResetDialog: () => void;
  closeOnboardingResetDialog: () => void;
  confirmOnboardingReset: () => Promise<void>;

  // dependencies / permissions
  permStatus: import('@/lib/types').PermissionsStatus | null;
  permLoading: boolean;
  permRefreshing: boolean;
  permError: string | null;
  permRequesting: ReadonlySet<PermissionType>;
  refreshPermissions: () => void | Promise<void>;
  handleRequestAccess: (type: PermissionType) => void;

  // health
  healthForm: HealthForm;
  healthConfig: HealthConfig | null;
  applyingHealth: boolean;
  healthError: string | null;
  healthLoadError: Error | null;
  canResetHealthDefaults: boolean;
  patchHealthForm: (partial: Partial<HealthForm>) => void;
  handleResetHealthDefaults: () => void;
  handleApplyHealth: () => Promise<void>;

  // cloud sync
  cloudSyncForm: CloudSyncForm;
  cloudSync: CloudSyncConfig | null;
  syncResult: CloudSyncResult | null;
  testResult: TestCloudSyncResult | null;
  cloudSyncError: string | null;
  testing: boolean;
  applying: boolean;
  syncing: boolean;
  cloudSyncLoadError: Error | null;
  canResetCloudSyncDefaults: boolean;
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

  // ai / github trending
  githubTrendingForm: GithubTrendingForm;
  githubTrendingConfig: GithubTrendingConfig | null;
  claudeCliTest: ClaudeCliTestResult | null;
  githubTrendingError: string | null;
  testingClaudeCli: boolean;
  applyingGithubTrending: boolean;
  githubTrendingLoadError: Error | null;
  canResetGithubTrendingDefaults: boolean;
  patchGithubTrendingForm: (partial: Partial<GithubTrendingForm>) => void;
  handleResetGithubTrendingDefaults: () => void;
  handleApplyGithubTrending: () => Promise<void>;
  handleTestClaudeCli: () => Promise<void>;

  // prompt optimizer
  promptOptimizerForm: PromptOptimizerSettingsForm;
  promptOptimizerConfig: PromptOptimizerSettingsForm | null;
  applyingPromptOptimizer: boolean;
  promptOptimizerSettingsError: string | null;
  canResetPromptOptimizerDefaults: boolean;
  patchPromptOptimizerForm: (partial: Partial<PromptOptimizerSettingsForm>) => void;
  handleResetPromptOptimizerSettingsDefaults: () => void;
  handleApplyPromptOptimizerSettings: () => Promise<void>;
  promptOptimizerShortcutId: typeof PROMPT_OPTIMIZER_SHORTCUT_ID;
  formatShortcutForDisplay: typeof formatShortcutForDisplay;

  // automation
  automationForm: AutomationSettingsForm;
  defaultAutomationForm: AutomationSettingsForm;
  automationDirty: boolean;
  savingAutomation: boolean;
  automationError: string | null;
  automationSaved: boolean;
  automationLoadError: Error | null;
  canResetAutomationDefaults: boolean;
  handleAutomationFormChange: (nextForm: AutomationSettingsForm) => void;
  handleResetAutomationDefaults: () => void;
  handleSaveAutomation: () => Promise<void>;
  /** owner adapter catalog（Settings 自动化 tab 只读展示） */
  agentAdapters: import('@/lib/types').OrchestratorAgentAdapterCatalogItem[];

  // about / update
  versionInfo: VersionInfo | null;
  versionLoadError: Error | null;
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
  formatSize: typeof formatSize;
  formatTime: typeof formatTime;
}

/**
 * Settings 页面控制器
 *
 * Business Logic（为什么需要这个函数）:
 *   设置页需要统一编排多 tab 资源加载、表单 dirty/save/reset 与权限/更新动作，
 *   才能让 panel 只做展示、shell 只做布局。
 *
 * Code Logic（这个函数做什么）:
 *   组合 resources/formSaves/updatePermissions 三个域 hook + URL tab；
 *   返回供 Settings.tsx 组合的数据对象；不渲染 tabpanel JSX。
 *
 * @returns Settings shell/panel 所需的状态与动作
 */
export function useSettingsController(): UseSettingsControllerResult {
  const { t } = useTranslation(['settings', 'common']);
  const [searchParams, setSearchParams] = useSearchParams();
  const activeTab = parseSettingsTabFromSearch(
    searchParams.toString() ? `?${searchParams.toString()}` : '',
  );

  const form = useSettingsFormSaves();
  const hydrator = useMemo(
    () => ({
      applyResourceResults: form.applyResourceResults,
      applyGroupResult: form.applyGroupResult,
    }),
    [form.applyResourceResults, form.applyGroupResult],
  );
  const resources = useSettingsResources({ hydrator });
  const updatePermissions = useSettingsUpdatePermissions();

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户点 Settings tab 与 Attention 深链共用同一 URL 真源，才能在已挂载时响应 search 变化。
   *
   * Code Logic（这个函数做什么）:
   *   将 tab 写入 searchParams（general 省略 tab）；replace 避免历史堆叠。
   */
  const setActiveTab = useCallback(
    (tab: SettingsTabId) => {
      setSearchParams(
        (prev) => {
          const next = new URLSearchParams(prev);
          if (tab === 'general') {
            next.delete('tab');
          } else {
            next.set('tab', tab);
          }
          return next;
        },
        { replace: true },
      );
    },
    [setSearchParams],
  );

  /**
   * 页内 tab 键盘切换：支持左右方向键 / Home / End，保持 tablist 可访问性
   *
   * @param e 当前 tab button 的键盘事件
   * @param currentIndex 当前 tab 在 SETTINGS_TABS 中的索引
   */
  const handleTabKeyDown = useCallback(
    (e: KeyboardEvent<HTMLButtonElement>, currentIndex: number) => {
      let nextIndex: number | null = null;
      if (e.key === 'ArrowRight') {
        nextIndex = (currentIndex + 1) % SETTINGS_TABS.length;
      } else if (e.key === 'ArrowLeft') {
        nextIndex = (currentIndex - 1 + SETTINGS_TABS.length) % SETTINGS_TABS.length;
      } else if (e.key === 'Home') {
        nextIndex = 0;
      } else if (e.key === 'End') {
        nextIndex = SETTINGS_TABS.length - 1;
      }

      if (nextIndex === null) return;
      e.preventDefault();
      const nextTab = SETTINGS_TABS[nextIndex];
      setActiveTab(nextTab.id);
      window.requestAnimationFrame(() => {
        document.getElementById(`settings-tab-${nextTab.id}`)?.focus();
      });
    },
    [setActiveTab],
  );

  const [agentLedgerClearDialogOpen, setAgentLedgerClearDialogOpen] = useState(false);
  const [agentLedgerClearing, setAgentLedgerClearing] = useState(false);
  const [agentLedgerClearMessage, setAgentLedgerClearMessage] = useState<string | null>(null);
  const [agentLedgerClearError, setAgentLedgerClearError] = useState<string | null>(null);

  /**
   * Business Logic（为什么需要这个函数）:
   *   打开清除确认 Dialog，避免误删。
   *
   * Code Logic（这个函数做什么）:
   *   open=true 并清空上次结果。
   */
  const openAgentLedgerClearDialog = useCallback(() => {
    setAgentLedgerClearDialogOpen(true);
    setAgentLedgerClearError(null);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   取消清除。
   *
   * Code Logic（这个函数做什么）:
   *   open=false。
   */
  const closeAgentLedgerClearDialog = useCallback(() => {
    setAgentLedgerClearDialogOpen(false);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户确认后清除本机 Agent metadata 历史。
   *
   * Code Logic（这个函数做什么）:
   *   workbenchApi.agentLedger.clear → 成功提示 deleted count。
   */
  const confirmClearAgentLedger = useCallback(async () => {
    setAgentLedgerClearing(true);
    setAgentLedgerClearError(null);
    try {
      const deleted = await workbenchApi.agentLedger.clear();
      setAgentLedgerClearMessage(
        t('settings:agentLedger.clearSuccess', { count: deleted }),
      );
      setAgentLedgerClearDialogOpen(false);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setAgentLedgerClearError(message || t('settings:agentLedger.clearFailed'));
    } finally {
      setAgentLedgerClearing(false);
    }
  }, [t]);

  const [onboardingResetDialogOpen, setOnboardingResetDialogOpen] = useState(false);
  const [onboardingResetting, setOnboardingResetting] = useState(false);
  const [onboardingResetError, setOnboardingResetError] = useState<string | null>(null);

  /**
   * Business Logic（为什么需要这个函数）:
   *   打开「重置首次启动引导」确认 Dialog，避免误操作退出。
   *
   * Code Logic（这个函数做什么）:
   *   open=true 并清空上次错误。
   */
  const openOnboardingResetDialog = useCallback(() => {
    setOnboardingResetDialogOpen(true);
    setOnboardingResetError(null);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   取消重置。
   *
   * Code Logic（这个函数做什么）:
   *   open=false（busy 时由 Dialog 禁用关闭）。
   */
  const closeOnboardingResetDialog = useCallback(() => {
    if (onboardingResetting) return;
    setOnboardingResetDialogOpen(false);
  }, [onboardingResetting]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户确认后清 LAN 披露 + 权限 onboarding 标记，停止 backend 并退出 GUI。
   *
   * Code Logic（这个函数做什么）:
   *   resetOnboardingGates（只清当前 flavor 的 LAN bootstrap 文件）→
   *   清当前/兼容的 permission onboarded+skipped keys →
   *   best-effort pendingWrites.flushAll → exitGui；失败不 exit。
   */
  const confirmOnboardingReset = useCallback(async () => {
    setOnboardingResetting(true);
    setOnboardingResetError(null);
    try {
      await backendApi.resetOnboardingGates();
      try {
        let flavor: AppFlavor = 'release';
        try {
          const identity = await configApi.appIdentity();
          if (identity.flavor === 'dev' || identity.flavor === 'release') {
            flavor = identity.flavor;
          }
        } catch {
          // 旧后端：清 release key
        }
        localStorage.removeItem(permissionOnboardedKey(flavor));
        localStorage.removeItem(permissionSkippedKey(flavor));
        // 兼容清旧无后缀 key / 另一 flavor 残留
        localStorage.removeItem(permissionOnboardedKey('release'));
        localStorage.removeItem(permissionSkippedKey('release'));
        localStorage.removeItem(permissionOnboardedKey('dev'));
        localStorage.removeItem(permissionSkippedKey('dev'));
      } catch {
        // WebView storage 异常不阻断退出：后端 bootstrap 已重置
      }
      try {
        await pendingWrites.flushAll();
      } catch {
        // flush 失败不阻断退出（与规格 best-effort 一致）
      }
      await backendApi.exitGui();
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setOnboardingResetError(message || t('settings:onboardingReset.failed'));
      setOnboardingResetting(false);
    }
  }, [t]);

  return {
    t,
    loading: resources.loading,
    loadError: resources.loadError,
    resourceResults: resources.resourceResults,
    retryingGroup: resources.retryingGroup,
    handleRetryResourceGroup: resources.handleRetryResourceGroup,
    activeTab,
    setActiveTab,
    handleTabKeyDown,
    tabs: SETTINGS_TABS,

    state: form.state,
    isDirty: form.isDirty,
    savedAt: form.savedAt,
    saving: form.saving,
    saveError: form.saveError,
    choosingDir: form.choosingDir,
    canResetCoreDefaults: resources.canResetCoreDefaults,
    recordingShortcutId: form.recordingShortcutId,
    handleDeviceNameChange: form.handleDeviceNameChange,
    handleReceiveDirChange: form.handleReceiveDirChange,
    handleChooseDir: form.handleChooseDir,
    handleShortcutFocus: form.handleShortcutFocus,
    handleShortcutBlur: form.handleShortcutBlur,
    handleShortcutKeyDown: form.handleShortcutKeyDown,
    handleResetDefaults: form.handleResetDefaults,
    handleSave: form.handleSave,
    agentLedgerClearDialogOpen,
    agentLedgerClearing,
    agentLedgerClearMessage,
    agentLedgerClearError,
    openAgentLedgerClearDialog,
    closeAgentLedgerClearDialog,
    confirmClearAgentLedger,
    onboardingResetDialogOpen,
    onboardingResetting,
    onboardingResetError,
    openOnboardingResetDialog,
    closeOnboardingResetDialog,
    confirmOnboardingReset,

    permStatus: updatePermissions.permStatus,
    permLoading: updatePermissions.permLoading,
    permRefreshing: updatePermissions.permRefreshing,
    permError: updatePermissions.permError,
    permRequesting: updatePermissions.permRequesting,
    refreshPermissions: updatePermissions.refreshPermissions,
    handleRequestAccess: updatePermissions.handleRequestAccess,

    healthForm: form.healthForm,
    healthConfig: form.healthConfig,
    applyingHealth: form.applyingHealth,
    healthError: form.healthError,
    healthLoadError: resources.healthLoadError,
    canResetHealthDefaults: resources.canResetHealthDefaults,
    patchHealthForm: form.patchHealthForm,
    handleResetHealthDefaults: form.handleResetHealthDefaults,
    handleApplyHealth: form.handleApplyHealth,

    cloudSyncForm: form.cloudSyncForm,
    cloudSync: form.cloudSync,
    syncResult: form.syncResult,
    testResult: form.testResult,
    cloudSyncError: form.cloudSyncError,
    testing: form.testing,
    applying: form.applying,
    syncing: form.syncing,
    cloudSyncLoadError: resources.cloudSyncLoadError,
    canResetCloudSyncDefaults: resources.canResetCloudSyncDefaults,
    patchCloudSyncForm: form.patchCloudSyncForm,
    handleResetCloudSyncDefaults: form.handleResetCloudSyncDefaults,
    handleTestCloudSync: form.handleTestCloudSync,
    handleApplyCloudSync: form.handleApplyCloudSync,
    handleSyncNow: form.handleSyncNow,
    lanSyncResult: form.lanSyncResult,
    lanSyncing: form.lanSyncing,
    lanSyncError: form.lanSyncError,
    handleLanSyncNow: form.handleLanSyncNow,
    backupExporting: form.backupExporting,
    backupExportPath: form.backupExportPath,
    backupExportError: form.backupExportError,
    backupRestoring: form.backupRestoring,
    backupInspect: form.backupInspect,
    backupArchivePath: form.backupArchivePath,
    backupSelectedDomains: form.backupSelectedDomains,
    backupMode: form.backupMode,
    backupRestoreDialogOpen: form.backupRestoreDialogOpen,
    backupRestoreResult: form.backupRestoreResult,
    backupRestoreError: form.backupRestoreError,
    backupJobs: form.backupJobs,
    backupJobsLoading: form.backupJobsLoading,
    backupJobsError: form.backupJobsError,
    backupRollbackJobId: form.backupRollbackJobId,
    backupRollbackDialogOpen: form.backupRollbackDialogOpen,
    backupRollingBack: form.backupRollingBack,
    handleBackupExport: form.handleBackupExport,
    handleBackupPickRestore: form.handleBackupPickRestore,
    handleBackupToggleDomain: form.handleBackupToggleDomain,
    handleBackupSetMode: form.handleBackupSetMode,
    handleBackupOpenRestoreDialog: form.handleBackupOpenRestoreDialog,
    handleBackupRestoreConfirm: form.handleBackupRestoreConfirm,
    handleCloseRestoreDialog: form.handleCloseRestoreDialog,
    handleRefreshRecoveryJobs: form.handleRefreshRecoveryJobs,
    handleOpenRollback: form.handleOpenRollback,
    handleConfirmRollback: form.handleConfirmRollback,
    handleCloseRollbackDialog: form.handleCloseRollbackDialog,

    githubTrendingForm: form.githubTrendingForm,
    githubTrendingConfig: form.githubTrendingConfig,
    claudeCliTest: form.claudeCliTest,
    githubTrendingError: form.githubTrendingError,
    testingClaudeCli: form.testingClaudeCli,
    applyingGithubTrending: form.applyingGithubTrending,
    githubTrendingLoadError: resources.githubTrendingLoadError,
    canResetGithubTrendingDefaults: resources.canResetGithubTrendingDefaults,
    patchGithubTrendingForm: form.patchGithubTrendingForm,
    handleResetGithubTrendingDefaults: form.handleResetGithubTrendingDefaults,
    handleApplyGithubTrending: form.handleApplyGithubTrending,
    handleTestClaudeCli: form.handleTestClaudeCli,

    promptOptimizerForm: form.promptOptimizerForm,
    promptOptimizerConfig: form.promptOptimizerConfig,
    applyingPromptOptimizer: form.applyingPromptOptimizer,
    promptOptimizerSettingsError: form.promptOptimizerSettingsError,
    canResetPromptOptimizerDefaults: resources.canResetPromptOptimizerDefaults,
    patchPromptOptimizerForm: form.patchPromptOptimizerForm,
    handleResetPromptOptimizerSettingsDefaults: form.handleResetPromptOptimizerSettingsDefaults,
    handleApplyPromptOptimizerSettings: form.handleApplyPromptOptimizerSettings,
    promptOptimizerShortcutId: form.promptOptimizerShortcutId,
    formatShortcutForDisplay: form.formatShortcutForDisplay,

    automationForm: form.automationForm,
    defaultAutomationForm: form.defaultAutomationForm,
    automationDirty: form.automationDirty,
    savingAutomation: form.savingAutomation,
    automationError: form.automationError,
    automationSaved: form.automationSaved,
    automationLoadError: resources.automationLoadError,
    agentAdapters: resources.agentAdapters,
    canResetAutomationDefaults: resources.canResetAutomationDefaults,
    handleAutomationFormChange: form.handleAutomationFormChange,
    handleResetAutomationDefaults: form.handleResetAutomationDefaults,
    handleSaveAutomation: form.handleSaveAutomation,

    versionInfo: resources.versionInfo,
    versionLoadError: resources.versionLoadError,
    updateResult: updatePermissions.updateResult,
    updateHint: updatePermissions.updateHint,
    updateCheckDisabled: updatePermissions.updateCheckDisabled,
    updateDownloadDisabled: updatePermissions.updateDownloadDisabled,
    updateInstallRetry: updatePermissions.updateInstallRetry,
    updateInstallMode: updatePermissions.updateInstallMode,
    updateIsInstalling: updatePermissions.updateIsInstalling,
    updateIsChecking: updatePermissions.updateIsChecking,
    downloadStatus: updatePermissions.downloadStatus,
    handleCheckUpdate: updatePermissions.handleCheckUpdate,
    handleDownload: updatePermissions.handleDownload,
    handleCancelDownload: updatePermissions.handleCancelDownload,
    handleInstall: updatePermissions.handleInstall,
    formatSize,
    formatTime,
  };
}
