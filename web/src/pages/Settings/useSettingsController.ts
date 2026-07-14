/**
 * Settings 页面控制器 hook
 *
 * Business Logic（为什么需要这个 hook）:
 *   Settings 同时持有 7 个 tab 的表单/资源加载/保存/重试状态；把编排从 JSX 中拆出，
 *   让 Settings.tsx 只做 tab/layout 组合，panel 保持纯 props 渲染。
 *
 * Code Logic（这个 hook 做什么）:
 *   集中管理 settingsResources 加载/重试、各 tab 表单 dirty/save/reset、权限与更新状态；
 *   返回 shell 与各 panel 所需字段，不渲染 tab JSX 树。
 */
import { useCallback, useEffect, useMemo, useState } from 'react';
import type { ChangeEvent, KeyboardEvent } from 'react';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { useSearchParams } from 'react-router-dom';
import { configApi } from '@/api/config';
import { healthApi } from '@/api/health';
import { orchestratorConfigApi } from '@/api/orchestratorConfig';
import { githubTrendingApi } from '@/api/githubTrending';
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
import { usePermissions } from '@/hooks/usePermissions';
import { useVisibilityPolling } from '@/hooks/useVisibilityPolling';
import {
  createSettingsResourceApi,
  isResourceReady,
  loadSettingsResources,
  pairCurrentError,
  retrySettingsResource,
  type PairResourceResult,
  type ResourceResult,
  type SettingsResourceGroup,
  type SettingsResourceResults,
} from './settingsResources';
import {
  formatShortcutForDisplay,
  resolveShortcutRecording,
} from './shortcutRecorder';
import {
  automationConfigToForm,
  automationFormToPatch,
  isAutomationFormDirty,
  PENDING_AUTOMATION_SETTINGS_FORM,
} from './automationSettingsState';
import type { AutomationSettingsForm } from './automationSettingsState';
import {
  buildConfigUpdate,
  cloudSyncConfigToForm,
  cloudSyncFormToUpdate,
  createPendingSettingsState,
  githubTrendingConfigToForm,
  healthConfigToForm,
  installButtonMode,
  isSettingsStateDirty,
  isUpdateCheckDisabled,
  isUpdateDownloadDisabled,
  PENDING_CLOUD_SYNC_FORM,
  PENDING_GITHUB_TRENDING_FORM,
  PENDING_HEALTH_FORM,
  PENDING_PROMPT_OPTIMIZER_SETTINGS_FORM,
  promptOptimizerSettingsConfigToForm,
  promptOptimizerSettingsFormToUpdate,
  settingsStateFromConfig,
  parseSettingsTabFromSearch,
  shouldPollUpdateStatus,
  shouldShowInstallRetry,
  type SettingsTabId,
} from './settingsState';
import type {
  CloudSyncForm,
  GithubTrendingForm,
  HealthForm,
  PromptOptimizerSettingsForm,
  SettingsState,
} from './settingsState';
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

/** Settings 页内子 tab 定义 */
export interface SettingsTab {
  id: SettingsTabId;
  labelKey: SettingsTabId;
}

/** Settings 页内子 tab 顺序：按用户查看任务组织，而不是按底层配置来源组织 */
export const SETTINGS_TABS: SettingsTab[] = [
  { id: 'general', labelKey: 'general' },
  { id: 'dependencies', labelKey: 'dependencies' },
  { id: 'health', labelKey: 'health' },
  { id: 'sync', labelKey: 'sync' },
  { id: 'ai', labelKey: 'ai' },
  { id: 'automation', labelKey: 'automation' },
  { id: 'about', labelKey: 'about' },
];

/** Workbench Prompt 优化快捷键录制控件 id */
export const PROMPT_OPTIMIZER_SHORTCUT_ID = 'promptOptimizer';

/**
 * 计算更新检查结果的提示文本
 *
 * Business Logic（为什么需要这个函数）:
 *   关于 tab 需要根据检查进度与结果展示统一提示，避免 JSX 内嵌多分支文案。
 *
 * Code Logic（这个函数做什么）:
 *   优先 checking；无结果为 upToDate；有 error 显示 error；hasUpdate 插值版本；否则 upToDate。
 *
 * @param updateResult 更新检查结果
 * @param checkingUpdate 是否正在检查
 * @param t i18next 翻译函数（settings ns）
 * @returns 当前应展示的提示文本
 */
export function buildUpdateHint(
  updateResult: UpdateCheckResult | null,
  checkingUpdate: boolean,
  t: TFunction<'settings'>,
): string {
  if (checkingUpdate) return t('about.checkingHint');
  if (!updateResult) return t('about.upToDate');
  if (updateResult.error) return updateResult.error;
  if (updateResult.hasUpdate) return t('about.newVersionFound', { version: updateResult.version });
  return t('about.upToDate');
}

/**
 * 把 Date 格式化为 "HH:MM:SS" 字符串
 *
 * Business Logic（为什么需要这个函数）:
 *   常规 tab 保存成功后需在页脚展示本地保存时间。
 *
 * Code Logic（这个函数做什么）:
 *   从 Date 取本地时分秒并零填充。
 *
 * @param d Date 实例
 * @returns 时间字符串
 */
export function formatTime(d: Date): string {
  const pad = (n: number) => n.toString().padStart(2, '0');
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

/**
 * 把字节数格式化为人类可读的大小字符串（B/KB/MB/GB）
 *
 * Business Logic（为什么需要这个函数）:
 *   关于 tab 下载按钮需展示更新包体积。
 *
 * Code Logic（这个函数做什么）:
 *   按 1024 进制递降单位，保留合适小数位。
 *
 * @param bytes 字节数
 * @returns 形如 "12.3 MB" 的字符串
 */
export function formatSize(bytes: number): string {
  if (!bytes) return '';
  const units = ['B', 'KB', 'MB', 'GB'];
  let value = bytes;
  let unitIndex = 0;
  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024;
    unitIndex += 1;
  }
  return `${value.toFixed(value >= 100 || unitIndex === 0 ? 0 : 1)} ${units[unitIndex]}`;
}

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
  handleDeviceNameChange: (e: ChangeEvent<HTMLInputElement>) => void;
  handleReceiveDirChange: (e: ChangeEvent<HTMLInputElement>) => void;
  handleChooseDir: () => Promise<void>;
  handleShortcutFocus: (id: string) => void;
  handleShortcutBlur: (id: string) => void;
  handleShortcutKeyDown: (e: KeyboardEvent<HTMLInputElement>, id: string) => void;
  handleResetDefaults: () => void;
  handleSave: () => Promise<void>;

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
  /** 局域网同步最近一次结果（per-device/domain） */
  lanSyncResult: SyncRunResult | null;
  /** 局域网同步进行中 */
  lanSyncing: boolean;
  /** 局域网同步错误文案 */
  lanSyncError: string | null;
  /** 备份导出进行中 */
  backupExporting: boolean;
  /** 最近一次导出成功路径 */
  backupExportPath: string | null;
  /** 导出错误文案 */
  backupExportError: string | null;
  /** 恢复/inspect 进行中 */
  backupRestoring: boolean;
  /** inspect 预览 */
  backupInspect: BackupInspectPreview | null;
  /** 待恢复归档路径 */
  backupArchivePath: string | null;
  /** 勾选的恢复领域 */
  backupSelectedDomains: BackupRestoreDomain[];
  /** 恢复模式 */
  backupMode: RestoreMode;
  /** 恢复确认 Dialog 是否打开 */
  backupRestoreDialogOpen: boolean;
  /** 最近一次恢复结果 */
  backupRestoreResult: BackupRestoreResult | null;
  /** 恢复/inspect 错误文案 */
  backupRestoreError: string | null;
  /** 恢复任务列表 */
  backupJobs: RecoveryJobRow[];
  /** 任务列表加载中 */
  backupJobsLoading: boolean;
  /** 任务列表错误 */
  backupJobsError: string | null;
  /** 待回滚 job id */
  backupRollbackJobId: string | null;
  /** 回滚确认 Dialog */
  backupRollbackDialogOpen: boolean;
  /** 回滚进行中 */
  backupRollingBack: boolean;
  patchCloudSyncForm: (partial: Partial<CloudSyncForm>) => void;
  handleResetCloudSyncDefaults: () => void;
  handleTestCloudSync: () => Promise<void>;
  handleApplyCloudSync: () => Promise<void>;
  handleSyncNow: () => Promise<void>;
  /** 触发局域网 trigger_sync */
  handleLanSyncNow: () => Promise<void>;
  /** 导出可验证备份 */
  handleBackupExport: () => Promise<void>;
  /** 选择备份并 inspect 预览 */
  handleBackupPickRestore: () => Promise<void>;
  /** 切换恢复领域勾选 */
  handleBackupToggleDomain: (domain: BackupRestoreDomain) => void;
  /** 设置恢复模式 */
  handleBackupSetMode: (mode: RestoreMode) => void;
  /** 打开恢复确认 Dialog */
  handleBackupOpenRestoreDialog: () => void;
  /** 确认执行 restore */
  handleBackupRestoreConfirm: () => Promise<void>;
  /** 关闭恢复确认 Dialog */
  handleCloseRestoreDialog: () => void;
  /** 刷新恢复任务列表 */
  handleRefreshRecoveryJobs: () => Promise<void>;
  /** 打开回滚确认 */
  handleOpenRollback: (jobId: string) => void;
  /** 确认回滚 */
  handleConfirmRollback: () => Promise<void>;
  /** 关闭回滚确认 */
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
 *   持有全部 Settings 状态与 handler（含 settingsResources 加载/局部重试与 dirty 保护），
 *   返回供 Settings.tsx 组合的数据对象；不渲染 tabpanel JSX。
 *
 * @returns Settings shell/panel 所需的状态与动作
 */
export function useSettingsController(): UseSettingsControllerResult {
  const { t } = useTranslation(['settings', 'common']);
  const [state, setState] = useState<SettingsState>(createPendingSettingsState);
  // 最近一次"已保存/已加载"的配置快照，用于检测是否处于未保存状态
  const [initialState, setInitialState] = useState<SettingsState>(createPendingSettingsState);
  const [defaultState, setDefaultState] = useState<SettingsState>(createPendingSettingsState);
  const [savedAt, setSavedAt] = useState<Date | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  /** 仅常规 tab 保存失败；与 core 加载失败的 loadError 分离 */
  const [saveError, setSaveError] = useState<string | null>(null);
  const [versionInfo, setVersionInfo] = useState<VersionInfo | null>(null);
  const [updateResult, setUpdateResult] = useState<UpdateCheckResult | null>(null);
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  const [downloadStatus, setDownloadStatus] = useState<UpdateDownloadStatus | null>(null);
  const [installing, setInstalling] = useState(false);
  const [saving, setSaving] = useState(false);
  const [choosingDir, setChoosingDir] = useState(false);
  // 深链激活：activeTab 完全由 ?tab= 派生，挂载后 search 变化自动生效；用户点 tab 时写回 URL。
  const [searchParams, setSearchParams] = useSearchParams();
  const activeTab = parseSettingsTabFromSearch(
    searchParams.toString() ? `?${searchParams.toString()}` : '',
  );
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
  const [recordingShortcutId, setRecordingShortcutId] = useState<string | null>(null);

  // 云端同步（GitHub 私有仓库）独立操作块：表单值 / 已应用配置 / 上次同步结果 / 测试结果 / 各动作 loading
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
  const [backupRestoreResult, setBackupRestoreResult] = useState<BackupRestoreResult | null>(
    null,
  );
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

  // Claude CLI / AI 能力配置：GitHub 解说开关与 GitHub/Prompt 优化共用的 CLI 路径、模型配置。
  const [githubTrendingForm, setGithubTrendingForm] = useState<GithubTrendingForm>({
    ...PENDING_GITHUB_TRENDING_FORM,
  });
  const [defaultGithubTrendingForm, setDefaultGithubTrendingForm] = useState<GithubTrendingForm>({
    ...PENDING_GITHUB_TRENDING_FORM,
  });
  const [githubTrendingConfig, setGithubTrendingConfig] = useState<GithubTrendingConfig | null>(null);
  const [claudeCliTest, setClaudeCliTest] = useState<ClaudeCliTestResult | null>(null);
  const [githubTrendingError, setGithubTrendingError] = useState<string | null>(null);
  const [testingClaudeCli, setTestingClaudeCli] = useState(false);
  const [applyingGithubTrending, setApplyingGithubTrending] = useState(false);

  // Workbench Prompt 优化小组件偏好：快捷键与自动填入语言独立保存到 AppConfig。
  const [promptOptimizerForm, setPromptOptimizerForm] = useState<PromptOptimizerSettingsForm>({
    ...PENDING_PROMPT_OPTIMIZER_SETTINGS_FORM,
  });
  const [defaultPromptOptimizerForm, setDefaultPromptOptimizerForm] =
    useState<PromptOptimizerSettingsForm>({
      ...PENDING_PROMPT_OPTIMIZER_SETTINGS_FORM,
    });
  const [promptOptimizerConfig, setPromptOptimizerConfig] =
    useState<PromptOptimizerSettingsForm | null>(null);
  const [applyingPromptOptimizer, setApplyingPromptOptimizer] = useState(false);
  const [promptOptimizerSettingsError, setPromptOptimizerSettingsError] = useState<string | null>(
    null,
  );

  // 健康提醒配置：独立表单编辑 + 恢复默认 + 应用配置（与同步/AI 同模式）。
  const [healthForm, setHealthForm] = useState<HealthForm>({ ...PENDING_HEALTH_FORM });
  const [defaultHealthForm, setDefaultHealthForm] = useState<HealthForm>({ ...PENDING_HEALTH_FORM });
  const [healthConfig, setHealthConfig] = useState<HealthConfig | null>(null);
  const [applyingHealth, setApplyingHealth] = useState(false);
  const [healthError, setHealthError] = useState<string | null>(null);

  // Orchestrator 自动化配置：Settings 独立 tab，Phase 2 只做前端表单读写，不接运行时调度。
  const [automationForm, setAutomationForm] = useState<AutomationSettingsForm>({
    ...PENDING_AUTOMATION_SETTINGS_FORM,
  });
  const [initialAutomationForm, setInitialAutomationForm] = useState<AutomationSettingsForm>({
    ...PENDING_AUTOMATION_SETTINGS_FORM,
  });
  const [defaultAutomationForm, setDefaultAutomationForm] = useState<AutomationSettingsForm>({
    ...PENDING_AUTOMATION_SETTINGS_FORM,
  });
  const [savingAutomation, setSavingAutomation] = useState(false);
  const [automationError, setAutomationError] = useState<string | null>(null);
  const [automationSaved, setAutomationSaved] = useState(false);

  // 分组资源结果：局部失败可重试，不重置其他 tab 草稿
  const [resourceResults, setResourceResults] = useState<SettingsResourceResults | null>(null);
  const [retryingGroup, setRetryingGroup] = useState<SettingsResourceGroup | null>(null);

  // macOS 权限状态（设置页手动授权入口，持续轮询以反映用户在系统设置的变更）
  const {
    status: permStatus,
    loading: permLoading,
    refreshing: permRefreshing,
    error: permError,
    requesting: permRequesting,
    request: requestPermissionItem,
    refresh: refreshPermissions,
  } = usePermissions();

  /**
   * Business Logic（为什么需要这个常量）:
   *   Settings 加载/重试依赖稳定的 11 端点 API 面，避免每次 render 重建。
   *
   * Code Logic（这个常量做什么）:
   *   绑定生产 config/health/github/orchestrator API。
   */
  const settingsResourceApi = useMemo(
    () =>
      createSettingsResourceApi({
        configApi,
        githubTrendingApi,
        healthApi,
        orchestratorConfigApi,
      }),
    [],
  );

  /** core 默认配置是否可用（决定常规 tab「恢复默认」） */
  const canResetCoreDefaults = resourceResults?.defaults.status === 'ready';
  /** 云端同步默认配置是否可用 */
  const canResetCloudSyncDefaults = resourceResults?.cloudSync.defaults.status === 'ready';
  /** GitHub Trending 默认配置是否可用 */
  const canResetGithubTrendingDefaults =
    resourceResults?.githubTrending.defaults.status === 'ready';
  /** Prompt 优化默认（来自 core defaults）是否可用 */
  const canResetPromptOptimizerDefaults = canResetCoreDefaults;
  /** 健康提醒默认配置是否可用 */
  const canResetHealthDefaults = resourceResults?.health.defaults.status === 'ready';
  /** 自动化默认配置是否可用 */
  const canResetAutomationDefaults = resourceResults?.automation.defaults.status === 'ready';

  /** 业务 tab current 加载错误（仅 current 失败阻断 panel） */
  const cloudSyncLoadError = resourceResults ? pairCurrentError(resourceResults.cloudSync) : null;
  const githubTrendingLoadError = resourceResults
    ? pairCurrentError(resourceResults.githubTrending)
    : null;
  const healthLoadError = resourceResults ? pairCurrentError(resourceResults.health) : null;
  const automationLoadError = resourceResults ? pairCurrentError(resourceResults.automation) : null;
  const versionLoadError =
    resourceResults && resourceResults.version.status === 'error'
      ? resourceResults.version.error
      : null;

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
   * 页内 tab 键盘切换：支持左右方向键 / Home / End，保持 tablist 可访问性
   *
   * @param e 当前 tab button 的键盘事件
   * @param currentIndex 当前 tab 在 SETTINGS_TABS 中的索引
   */
  const handleTabKeyDown = useCallback((e: KeyboardEvent<HTMLButtonElement>, currentIndex: number) => {
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
  }, [setActiveTab]);

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
    (type: PermissionType) => {
      void requestPermissionItem(type).catch(() => undefined);
    },
    [requestPermissionItem],
  );

  // 计算是否处于"未保存"状态：当前 state 与最近一次已保存/已加载的快照是否一致
  const isDirty = useMemo(() => {
    return isSettingsStateDirty(state, initialState);
  }, [state, initialState]);

  // 自动化 tab 是否存在未应用修改，比较当前表单与最近一次加载/保存快照。
  const automationDirty = useMemo(() => {
    return isAutomationFormDirty(automationForm, initialAutomationForm);
  }, [automationForm, initialAutomationForm]);

  // 渲染更新检查结果的提示文本
  const updateHint = useMemo(
    () => buildUpdateHint(updateResult, checkingUpdate, t),
    [updateResult, checkingUpdate, t],
  );

  // 检查/下载按钮禁用：checking 或 installing 期间不可并发触发
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
  const updateIsInstalling =
    installing || downloadStatus?.status === 'installing';
  const updateIsChecking =
    checkingUpdate || downloadStatus?.status === 'checking';

  /**
   * 通用字段更新：merge 浅层部分字段
   *
   * @param partial 待合并的字段
   */
  const patchState = useCallback((partial: Partial<SettingsState>) => {
    setState((prev) => ({ ...prev, ...partial }));
    // 用户继续编辑时清掉上一次保存错误，避免陈旧 alert 误导
    setSaveError(null);
  }, []);

  /**
   * 处理 deviceName 输入
   *
   * @param e change 事件
   */
  const handleDeviceNameChange = (e: ChangeEvent<HTMLInputElement>) => {
    patchState({ deviceName: e.target.value });
  };

  /**
   * 处理 receiveDir 输入
   *
   * @param e change 事件
   */
  const handleReceiveDirChange = (e: ChangeEvent<HTMLInputElement>) => {
    patchState({ receiveDir: e.target.value });
  };

  /**
   * 处理快捷键输入
   *
   * @param id 快捷键 id
   * @param value 新的按键字符串
   */
  const handleShortcutChange = useCallback((id: string, value: string) => {
    setState((prev) => ({
      ...prev,
      shortcuts: prev.shortcuts.map((s) => (s.id === id ? { ...s, value } : s)),
    }));
  }, []);

  /**
   * 更新 Workbench Prompt 优化快捷键
   *
   * Business Logic（为什么需要）:
   *   Prompt 优化快捷键是页面内快捷动作，不使用系统全局注册，因此允许 Control 单键。
   *
   * Code Logic（做什么）:
   *   只改 Prompt 优化表单中的 hotkey 字段，等待用户点击“应用配置”再持久化。
   */
  const handlePromptOptimizerShortcutChange = useCallback((value: string) => {
    setPromptOptimizerForm((prev) => ({ ...prev, hotkey: value }));
  }, []);

  /**
   * 激活快捷键录制态
   *
   * Business Logic（为什么需要）:
   *   用户点进快捷键输入框后应直接按键录制，不需要手动输入格式化字符串。
   *
   * Code Logic（做什么）:
   *   记录当前正在录制的快捷键 id，渲染层据此切换提示文案与激活样式。
   *
   * @param id 快捷键 id
   */
  const handleShortcutFocus = useCallback((id: string) => {
    setRecordingShortcutId(id);
  }, []);

  /**
   * 快捷键输入失焦时退出录制态
   *
   * Business Logic（为什么需要）:
   *   用户离开输入框时应停止捕获按键，避免后续键盘操作继续改写快捷键。
   *
   * Code Logic（做什么）:
   *   仅当失焦字段仍是当前录制字段时清空 recordingShortcutId。
   *
   * @param id 快捷键 id
   */
  const handleShortcutBlur = useCallback((id: string) => {
    setRecordingShortcutId((prev) => (prev === id ? null : prev));
  }, []);

  /**
   * 录制快捷键按键：阻止文本输入并按结果更新字段
   *
   * Business Logic（为什么需要）:
   *   快捷键设置应由用户按下组合键自动生成，Esc 可取消，Delete/Backspace 可清空。
   *
   * Code Logic（做什么）:
   *   阻止 input 默认输入，把 React 键盘事件交给 shortcutRecorder 解析；
   *   record/clear 更新 state，cancel 只退出录制态，pending 保持等待。
   *
   * @param e 键盘事件
   * @param id 快捷键 id
   */
  const handleShortcutKeyDown = useCallback(
    (e: KeyboardEvent<HTMLInputElement>, id: string) => {
      e.preventDefault();
      e.stopPropagation();

      const result = resolveShortcutRecording(e, {
        allowModifierOnly: id === PROMPT_OPTIMIZER_SHORTCUT_ID,
      });
      if (result.type === 'pending') return;
      if (result.type === 'cancel') {
        setRecordingShortcutId(null);
        e.currentTarget.blur();
        return;
      }

      if (id === PROMPT_OPTIMIZER_SHORTCUT_ID) {
        handlePromptOptimizerShortcutChange(result.value);
      } else {
        handleShortcutChange(id, result.value);
      }
      setRecordingShortcutId(null);
      e.currentTarget.blur();
    },
    [handlePromptOptimizerShortcutChange, handleShortcutChange],
  );

  /**
   * 恢复默认：重置 state 到后端提供的环境默认值
   *
   * Business Logic（为什么需要）:
   *   用户保存自定义快捷键后仍应能随时恢复系统默认值，同时不能把基础设置重置为空。
   *
   * Code Logic（做什么）:
   *   使用加载阶段从后端取得的默认配置快照更新表单；是否需要保存仍由 isDirty 重新计算。
   */
  const handleResetDefaults = () => {
    setState(defaultState);
  };

  /**
   * 打开原生目录选择对话框，将返回路径写入 receiveDir
   */
  const handleChooseDir = async () => {
    setChoosingDir(true);
    try {
      const result = await configApi.chooseDir();
      if (result.path) {
        patchState({ receiveDir: result.path });
      }
    } catch {
      // 目录选择取消或失败时静默处理
    } finally {
      setChoosingDir(false);
    }
  };

  /**
   * 保存按钮：把当前 state 发送到后端持久化
   *
   * Business Logic（为什么需要这个函数）:
   *   用户应用常规偏好；失败时必须保留脏表单与本地修改，不能整页卸载成 loadError。
   *
   * Code Logic（这个函数做什么）:
   *   update_config 成功写 initialState/savedAt 并清 saveError；
   *   失败只 setSaveError，不碰 loadError / state / initialState。
   */
  const handleSave = async () => {
    setSaving(true);
    setSaveError(null);
    try {
      const updatedConfig = await configApi.update(buildConfigUpdate(state, initialState));
      const savedState = settingsStateFromConfig(updatedConfig);
      setState(savedState);
      // 保存成功后，把已保存快照更新为当前 state，使 isDirty 归零
      setInitialState(savedState);
      setSavedAt(new Date());
      setSaveError(null);
    } catch (err) {
      // 局部错误：保留脏草稿，暴露 localized 文案供 panel 展示
      setSaveError(err instanceof Error ? err.message : t('error.saveFailed'));
    } finally {
      setSaving(false);
    }
  };

  /**
   * 检查更新按钮：调用后端 updater/check 接口
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
   * Business Logic（为什么需要这个函数）:
   *   allSettled 结果需按 ready 写入对应 state；失败不得用 defaults 顶替 current。
   *
   * Code Logic（这个函数做什么）:
   *   仅对 status=ready 的分组写表单/快照；defaults 独立写入默认快照。
   */
  const applyResourceResults = useCallback((results: SettingsResourceResults) => {
    if (isResourceReady(results.core)) {
      const config = results.core.value;
      const loaded = settingsStateFromConfig(config);
      setState(loaded);
      setInitialState(loaded);
      setPromptOptimizerConfig(promptOptimizerSettingsConfigToForm(config));
      setPromptOptimizerForm(promptOptimizerSettingsConfigToForm(config));
    }

    if (isResourceReady(results.defaults)) {
      const defaultConfig = results.defaults.value;
      setDefaultState(settingsStateFromConfig(defaultConfig));
      setDefaultPromptOptimizerForm(promptOptimizerSettingsConfigToForm(defaultConfig));
    }

    if (isResourceReady(results.version)) {
      setVersionInfo(results.version.value);
    } else {
      setVersionInfo(null);
    }

    if (isResourceReady(results.cloudSync.current)) {
      const cloudSyncConfig = results.cloudSync.current.value;
      setCloudSync(cloudSyncConfig);
      setCloudSyncForm(cloudSyncConfigToForm(cloudSyncConfig));
      setCloudSyncError(null);
    }
    if (isResourceReady(results.cloudSync.defaults)) {
      setDefaultCloudSyncForm(cloudSyncConfigToForm(results.cloudSync.defaults.value));
    }

    if (isResourceReady(results.githubTrending.current)) {
      const githubTrendingLoaded = results.githubTrending.current.value;
      setGithubTrendingConfig(githubTrendingLoaded);
      setGithubTrendingForm(githubTrendingConfigToForm(githubTrendingLoaded));
      setGithubTrendingError(null);
    }
    if (isResourceReady(results.githubTrending.defaults)) {
      setDefaultGithubTrendingForm(
        githubTrendingConfigToForm(results.githubTrending.defaults.value),
      );
    }

    if (isResourceReady(results.health.current)) {
      const healthLoaded = results.health.current.value;
      setHealthConfig(healthLoaded);
      setHealthForm(healthConfigToForm(healthLoaded));
      setHealthError(null);
    }
    if (isResourceReady(results.health.defaults)) {
      setDefaultHealthForm(healthConfigToForm(results.health.defaults.value));
    }

    if (isResourceReady(results.automation.current)) {
      const loadedAutomationForm = automationConfigToForm(results.automation.current.value);
      setAutomationForm(loadedAutomationForm);
      setInitialAutomationForm(loadedAutomationForm);
      setAutomationError(null);
    }
    if (isResourceReady(results.automation.defaults)) {
      setDefaultAutomationForm(automationConfigToForm(results.automation.defaults.value));
    }

    // core 失败 → 整页错误；成功则清除 page-level loadError（保留 save 错误另议）
    if (results.core.status === 'error') {
      setLoadError(results.core.error.message || t('error.loadConfigFailed'));
    } else {
      setLoadError(null);
    }
  }, [t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   单组 retry 成功后只应用该组，避免覆盖其他 tab 未保存草稿。
   *
   * Code Logic（这个函数做什么）:
   *   合并 resourceResults 中对应 group，再按 group 写 state。
   */
  /**
   * Business Logic（为什么需要这个函数）:
   *   单组 retry 成功后只应用该组，且不得覆盖用户未保存草稿。
   *
   * Code Logic（这个函数做什么）:
   *   合并 resourceResults 中对应 group；仅当该组先前为 error 或 form 未 dirty 时写 form；
   *   dirty 时只更新服务端权威 config 元数据与 error 标记。
   */
  const applyGroupResult = useCallback(
    (
      group: SettingsResourceGroup,
      groupResult:
        | ResourceResult<import('@/lib/types').AppConfig>
        | ResourceResult<import('@/lib/types').VersionInfo>
        | PairResourceResult<import('@/lib/types').CloudSyncConfig>
        | PairResourceResult<import('@/lib/types').GithubTrendingConfig>
        | PairResourceResult<import('@/lib/types').HealthConfig>
        | PairResourceResult<import('@/api/orchestratorConfig').OrchestratorAutomationConfig>,
      options?: { allowRewriteForm?: boolean },
    ) => {
      const allowRewriteForm = options?.allowRewriteForm === true;

      setResourceResults((prev) => {
        if (!prev) return prev;
        return { ...prev, [group]: groupResult } as SettingsResourceResults;
      });

      if (group === 'core') {
        const result = groupResult as ResourceResult<import('@/lib/types').AppConfig>;
        if (isResourceReady(result)) {
          const config = result.value;
          const loaded = settingsStateFromConfig(config);
          const rewrite =
            allowRewriteForm || !isSettingsStateDirty(state, initialState);
          if (rewrite) {
            setState(loaded);
            setPromptOptimizerForm(promptOptimizerSettingsConfigToForm(config));
          }
          setInitialState(loaded);
          setPromptOptimizerConfig(promptOptimizerSettingsConfigToForm(config));
          setLoadError(null);
        } else {
          setLoadError(result.error.message || t('error.loadConfigFailed'));
        }
        return;
      }

      if (group === 'defaults') {
        const result = groupResult as ResourceResult<import('@/lib/types').AppConfig>;
        if (isResourceReady(result)) {
          const defaultConfig = result.value;
          setDefaultState(settingsStateFromConfig(defaultConfig));
          setDefaultPromptOptimizerForm(promptOptimizerSettingsConfigToForm(defaultConfig));
        }
        return;
      }

      if (group === 'version') {
        const result = groupResult as ResourceResult<import('@/lib/types').VersionInfo>;
        if (isResourceReady(result)) {
          setVersionInfo(result.value);
        } else {
          setVersionInfo(null);
        }
        return;
      }

      if (group === 'cloudSync') {
        const pair = groupResult as PairResourceResult<import('@/lib/types').CloudSyncConfig>;
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
        return;
      }

      if (group === 'githubTrending') {
        const pair = groupResult as PairResourceResult<import('@/lib/types').GithubTrendingConfig>;
        if (isResourceReady(pair.current)) {
          setGithubTrendingConfig(pair.current.value);
          const serverForm = githubTrendingConfigToForm(pair.current.value);
          const dirty =
            githubTrendingConfig !== null &&
            JSON.stringify(githubTrendingForm) !==
              JSON.stringify(githubTrendingConfigToForm(githubTrendingConfig));
          if (allowRewriteForm || !dirty) {
            setGithubTrendingForm(serverForm);
          }
          setGithubTrendingError(null);
        }
        if (isResourceReady(pair.defaults)) {
          setDefaultGithubTrendingForm(githubTrendingConfigToForm(pair.defaults.value));
        }
        return;
      }

      if (group === 'health') {
        const pair = groupResult as PairResourceResult<import('@/lib/types').HealthConfig>;
        if (isResourceReady(pair.current)) {
          setHealthConfig(pair.current.value);
          const serverForm = healthConfigToForm(pair.current.value);
          const dirty =
            healthConfig !== null &&
            JSON.stringify(healthForm) !== JSON.stringify(healthConfigToForm(healthConfig));
          if (allowRewriteForm || !dirty) {
            setHealthForm(serverForm);
          }
          setHealthError(null);
        }
        if (isResourceReady(pair.defaults)) {
          setDefaultHealthForm(healthConfigToForm(pair.defaults.value));
        }
        return;
      }

      if (group === 'automation') {
        const pair = groupResult as PairResourceResult<
          import('@/api/orchestratorConfig').OrchestratorAutomationConfig
        >;
        if (isResourceReady(pair.current)) {
          const loadedAutomationForm = automationConfigToForm(pair.current.value);
          const dirty = isAutomationFormDirty(automationForm, initialAutomationForm);
          if (allowRewriteForm || !dirty) {
            setAutomationForm(loadedAutomationForm);
          }
          setInitialAutomationForm(loadedAutomationForm);
          setAutomationError(null);
        }
        if (isResourceReady(pair.defaults)) {
          setDefaultAutomationForm(automationConfigToForm(pair.defaults.value));
        }
      }
    },
    [
      t,
      state,
      initialState,
      cloudSync,
      cloudSyncForm,
      githubTrendingConfig,
      githubTrendingForm,
      healthConfig,
      healthForm,
      automationForm,
      initialAutomationForm,
    ],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   panel 局部失败时用户点重试，只请求该分组端点。
   *
   * Code Logic（这个函数做什么）:
   *   调用 retrySettingsResource，成功/失败都写回该组结果；不触碰其他组。
   */
  const handleRetryResourceGroup = useCallback(
    async (group: SettingsResourceGroup) => {
      setRetryingGroup(group);
      try {
        const prev = resourceResults;
        const groupResult = await retrySettingsResource(settingsResourceApi, group);
        // 失败组重试成功允许写 form；已 ready 组重试则遵守 dirty 保护
        const prevStatus =
          group === 'core' || group === 'defaults' || group === 'version'
            ? prev?.[group]?.status
            : group === 'cloudSync' ||
                group === 'githubTrending' ||
                group === 'health' ||
                group === 'automation'
              ? prev?.[group]?.current.status
              : undefined;
        applyGroupResult(group, groupResult, {
          allowRewriteForm: prevStatus === 'error' || prevStatus === undefined,
        });
      } finally {
        setRetryingGroup(null);
      }
    },
    [applyGroupResult, settingsResourceApi, resourceResults],
  );

  // 组件挂载时分组加载配置（allSettled，非整页 Promise.all）
  useEffect(() => {
    let cancelled = false;

    /**
     * 加载 Settings 页面所需的全部分组资源
     *
     * Business Logic（为什么需要这个函数）:
     *   用户进入设置页时需要并行获得当前配置、默认值和版本信息；任一非 core 失败不得拖垮整页。
     *
     * Code Logic（这个函数做什么）:
     *   loadSettingsResources + applyResourceResults；core 失败写 loadError，其余分组局部错误保留在 resourceResults。
     */
    async function loadConfig() {
      try {
        const results = await loadSettingsResources(settingsResourceApi);
        if (cancelled) return;
        setResourceResults(results);
        applyResourceResults(results);
      } catch (err) {
        if (cancelled) return;
        // allSettled 本身不应抛；兜底仍显示整页错误
        setLoadError(err instanceof Error ? err.message : t('error.loadConfigFailed'));
      } finally {
        if (!cancelled) {
          setLoading(false);
        }
      }
    }

    void loadConfig();
    return () => {
      cancelled = true;
    };
    // 仅在挂载时执行一次
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

/**
   * 启动更新下载：透传检查结果的 downloadUrl/filename，立即进入 downloading 状态
   */
  const handleDownload = async () => {
    if (!updateResult?.downloadUrl || !updateResult?.filename) return;
    // 乐观进入 downloading，让进度条立即显示
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
   */
  const handleCancelDownload = async () => {
    try {
      await configApi.cancelDownload();
      setDownloadStatus((prev) =>
        prev ? { ...prev, status: 'cancelled' } : prev,
      );
    } catch {
      // 取消失败静默
    }
  };

  /**
   * 安装已下载的更新包并重启（进程随后退出）。
   * 失败时刷新 getDownloadStatus，以展示 completed + error 的重试安装态。
   */
  const handleInstall = async () => {
    setInstalling(true);
    // 乐观进入 installing，禁用检查/下载并显示安装中文案；不伪造进度条
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

  /**
   * 更新云端同步表单的某个字段（浅合并）
   *
   * @param partial 待合并的字段
   */
  const patchCloudSyncForm = useCallback((partial: Partial<CloudSyncForm>) => {
    setCloudSyncForm((prev) => ({ ...prev, ...partial }));
  }, []);

  /**
   * 云端同步「恢复默认」：把表单重置为后端默认配置
   *
   * Business Logic（为什么需要）:
   *   同步 tab 也需要一键恢复默认，且默认值必须与 Rust 配置默认值保持一致。
   *
   * Code Logic（做什么）:
   *   使用加载时保存的默认表单快照覆盖当前表单；是否落盘仍由用户点击“应用配置”决定。
   */
  const handleResetCloudSyncDefaults = useCallback(() => {
    setCloudSyncForm(defaultCloudSyncForm);
    setCloudSyncError(null);
  }, [defaultCloudSyncForm]);

  /**
   * 云端同步「测试连接」：探测 git 可用性与仓库默认分支
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
   * 云端同步「应用配置」：把当前表单值提交到后端，并用返回值刷新已应用配置
   */
  const handleApplyCloudSync = async () => {
    setApplying(true);
    setCloudSyncError(null);
    try {
      const updated = await configApi.updateCloudSyncConfig(cloudSyncFormToUpdate(cloudSyncForm));
      setCloudSync(updated);
      setCloudSyncForm(cloudSyncConfigToForm(updated));
    } catch (err) {
      setCloudSyncError(err instanceof Error ? err.message : t('settings:cloudSync.applyFailed'));
    } finally {
      setApplying(false);
    }
  };

  /**
   * 云端同步「立即同步」：触发一次 pull + push，展示结果
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
   * 局域网同步「立即同步」：trigger_sync 返回 per-device/domain 真值。
   *
   * Business Logic: Settings 同步 tab 展示设备/领域状态；partial/unreachable 不计成功。
   * Code Logic: syncApi.trigger() → setLanSyncResult；失败写 lanSyncError。
   */
  const handleLanSyncNow = async () => {
    setLanSyncing(true);
    setLanSyncError(null);
    try {
      const result = await syncApi.trigger();
      setLanSyncResult(result);
    } catch (err) {
      setLanSyncResult(null);
      setLanSyncError(
        err instanceof Error ? err.message : t('settings:lanSync.failed'),
      );
    } finally {
      setLanSyncing(false);
    }
  };

  /**
   * 刷新恢复任务列表。
   *
   * Business Logic（为什么需要这个函数）:
   *   导出/恢复/回滚后需要展示最新 recovery jobs，供用户回滚。
   *
   * Code Logic（这个函数做什么）:
   *   backupApi.listJobs() → setBackupJobs；失败写 backupJobsError。
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

  /* eslint-disable react-hooks/set-state-in-effect -- 合法 fetch-in-effect，setState 在 await 后异步执行 */
  useEffect(() => {
    void handleRefreshRecoveryJobs();
  }, [handleRefreshRecoveryJobs]);
  /* eslint-enable react-hooks/set-state-in-effect */

  /**
   * 导出可验证备份包。
   *
   * Business Logic（为什么需要这个函数）:
   *   用户在同步 tab 一键导出 zip 备份。
   *
   * Code Logic（这个函数做什么）:
   *   pickBackupExportPath → backupApi.create；写 success path 或 error。
   */
  const handleBackupExport = async () => {
    setBackupExportError(null);
    setBackupExportPath(null);
    const destPath = await pickBackupExportPath();
    // 用户取消对话框：静默返回，不记错误
    if (!destPath) {
      return;
    }
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
   * 选择备份文件并 inspect 预览。
   *
   * Business Logic（为什么需要这个函数）:
   *   恢复前必须预览领域计数/警告，默认全选可恢复领域。
   *
   * Code Logic（这个函数做什么）:
   *   pickBackupArchivePath → backupApi.inspect；重置 domains/mode。
   */
  const handleBackupPickRestore = async () => {
    setBackupRestoreError(null);
    setBackupRestoreResult(null);
    setBackupInspect(null);
    setBackupArchivePath(null);
    const archivePath = await pickBackupArchivePath();
    // 用户取消对话框：静默返回，不记错误
    if (!archivePath) {
      return;
    }
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
   * 切换恢复领域勾选。
   *
   * Business Logic（为什么需要这个函数）:
   *   用户可只恢复部分领域。
   *
   * Code Logic（这个函数做什么）:
   *   若已选则移除，否则追加。
   */
  const handleBackupToggleDomain = useCallback((domain: BackupRestoreDomain) => {
    setBackupSelectedDomains((prev) =>
      prev.includes(domain) ? prev.filter((d) => d !== domain) : [...prev, domain],
    );
  }, []);

  /**
   * 设置恢复模式 merge / replaceDomain。
   *
   * Business Logic（为什么需要这个函数）:
   *   合并与替换领域语义不同，需显式选择。
   *
   * Code Logic（这个函数做什么）:
   *   setBackupMode(mode)。
   */
  const handleBackupSetMode = useCallback((mode: RestoreMode) => {
    setBackupMode(mode);
  }, []);

  /**
   * 打开恢复确认 Dialog（需已 inspect 且至少勾选一个领域）。
   *
   * Business Logic（为什么需要这个函数）:
   *   写入前二次确认。
   *
   * Code Logic（这个函数做什么）:
   *   校验 domains；通过则 open dialog。
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
   * 确认执行 restore_backup。
   *
   * Business Logic（为什么需要这个函数）:
   *   用户确认后写入本地数据并刷新 jobs。
   *
   * Code Logic（这个函数做什么）:
   *   backupApi.restore；busy 期间 dialog 不关闭；成功关闭 dialog。
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
   * 关闭恢复确认 Dialog。
   *
   * Business Logic（为什么需要这个函数）:
   *   busy 时禁止关闭，避免半程打断。
   *
   * Code Logic（这个函数做什么）:
   *   backupRestoring 时 early return，否则 set open false。
   */
  const handleCloseRestoreDialog = useCallback(() => {
    if (backupRestoring) return;
    setBackupRestoreDialogOpen(false);
  }, [backupRestoring]);

  /**
   * 打开指定 job 的回滚确认。
   *
   * Business Logic（为什么需要这个函数）:
   *   仅有 pre-restore 的任务可回滚，需二次确认。
   *
   * Code Logic（这个函数做什么）:
   *   setRollbackJobId + open dialog。
   */
  const handleOpenRollback = useCallback((jobId: string) => {
    setBackupRollbackJobId(jobId);
    setBackupRollbackDialogOpen(true);
  }, []);

  /**
   * 关闭回滚确认 Dialog。
   *
   * Business Logic（为什么需要这个函数）:
   *   busy 时禁止关闭。
   *
   * Code Logic（这个函数做什么）:
   *   rollingBack early return，否则清空 jobId 并关闭。
   */
  const handleCloseRollbackDialog = useCallback(() => {
    if (backupRollingBack) return;
    setBackupRollbackDialogOpen(false);
    setBackupRollbackJobId(null);
  }, [backupRollingBack]);

  /**
   * 确认回滚 recovery job。
   *
   * Business Logic（为什么需要这个函数）:
   *   用 pre-restore 备份恢复到 restore 前状态。
   *
   * Code Logic（这个函数做什么）:
   *   backupApi.rollback(jobId)；刷新 jobs 并展示结果。
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

  /**
   * 更新 Claude CLI / AI 表单字段
   */
  const patchGithubTrendingForm = useCallback((partial: Partial<GithubTrendingForm>) => {
    setGithubTrendingForm((prev) => ({ ...prev, ...partial }));
  }, []);

  /**
   * Claude CLI / AI「恢复默认」：把 AI 表单重置为后端默认配置
   *
   * Business Logic（为什么需要）:
   *   AI tab 用户可能改过 CLI 路径、模型或缓存时间，需要随时回到应用内置默认。
   *
   * Code Logic（做什么）:
   *   使用加载时保存的默认表单快照覆盖当前表单；持久化仍由“应用配置”按钮完成。
   */
  const handleResetGithubTrendingDefaults = useCallback(() => {
    setGithubTrendingForm(defaultGithubTrendingForm);
    setGithubTrendingError(null);
  }, [defaultGithubTrendingForm]);

  /**
   * Claude CLI / AI「应用配置」：保存 GitHub 解说开关、Claude CLI 路径、模型与缓存设置
   */
  const handleApplyGithubTrending = async () => {
    setApplyingGithubTrending(true);
    setGithubTrendingError(null);
    try {
      const updated = await githubTrendingApi.updateConfig({
        aiEnabled: githubTrendingForm.aiEnabled,
        claudeCliPath: githubTrendingForm.claudeCliPath.trim() || 'claude',
        claudeModel: githubTrendingForm.claudeModel.trim() || 'sonnet',
        cacheTtlHours: githubTrendingForm.cacheTtlHours,
      });
      setGithubTrendingConfig(updated);
      setGithubTrendingForm(githubTrendingConfigToForm(updated));
    } catch (err) {
      setGithubTrendingError(err instanceof Error ? err.message : t('settings:githubTrending.applyFailed'));
    } finally {
      setApplyingGithubTrending(false);
    }
  };

  /**
   * GitHub Trending「测试 Claude CLI」：只跑 --version，不触发 AI 生成
   */
  const handleTestClaudeCli = async () => {
    setTestingClaudeCli(true);
    setGithubTrendingError(null);
    setClaudeCliTest(null);
    try {
      const result = await githubTrendingApi.testClaudeCli(githubTrendingForm.claudeCliPath);
      setClaudeCliTest(result);
    } catch (err) {
      setClaudeCliTest({
        ok: false,
        version: null,
        error: err instanceof Error ? err.message : t('githubTrending.testFailed', { error: '' }).trim(),
      });
    } finally {
      setTestingClaudeCli(false);
    }
  };

  /**
   * 更新 Workbench Prompt 优化设置表单字段
   */
  const patchPromptOptimizerForm = useCallback((partial: Partial<PromptOptimizerSettingsForm>) => {
    setPromptOptimizerForm((prev) => ({ ...prev, ...partial }));
  }, []);

  /**
   * Workbench Prompt 优化「恢复默认」：把快捷键和填入语言重置为后端默认配置
   *
   * Business Logic（为什么需要这个函数）:
   *   用户可能改过 Prompt 优化触发键或填入语言，需要能回到默认 Control + 中文。
   *
   * Code Logic（这个函数做什么）:
   *   用加载时保存的默认表单快照覆盖当前表单；持久化仍由「应用配置」完成。
   */
  const handleResetPromptOptimizerSettingsDefaults = useCallback(() => {
    setPromptOptimizerForm(defaultPromptOptimizerForm);
    setPromptOptimizerSettingsError(null);
  }, [defaultPromptOptimizerForm]);

  /**
   * Workbench Prompt 优化「应用配置」：保存页面内快捷键与自动填入语言
   */
  const handleApplyPromptOptimizerSettings = async () => {
    setApplyingPromptOptimizer(true);
    setPromptOptimizerSettingsError(null);
    try {
      const updated = await configApi.update(
        promptOptimizerSettingsFormToUpdate(promptOptimizerForm),
      );
      const savedForm = promptOptimizerSettingsConfigToForm(updated);
      setPromptOptimizerConfig(savedForm);
      setPromptOptimizerForm(savedForm);
    } catch (err) {
      setPromptOptimizerSettingsError(
        err instanceof Error ? err.message : t('settings:promptOptimizerSettings.applyFailed'),
      );
    } finally {
      setApplyingPromptOptimizer(false);
    }
  };

  /**
   * 更新健康提醒表单字段（浅合并，只改本地，不落盘）
   *
   * @param partial 待合并的字段
   */
  const patchHealthForm = useCallback((partial: Partial<HealthForm>) => {
    setHealthForm((prev) => ({ ...prev, ...partial }));
  }, []);

  /**
   * 健康提醒「恢复默认」：把表单重置为后端默认配置
   *
   * Business Logic（为什么需要这个函数）:
   *   健康 tab 用户改过工作窗口/提醒等，需随时回到应用内置默认。
   *
   * Code Logic（这个函数做什么）:
   *   用加载时保存的默认表单快照覆盖当前表单；持久化仍由「应用配置」完成。
   */
  const handleResetHealthDefaults = useCallback(() => {
    setHealthForm(defaultHealthForm);
    setHealthError(null);
  }, [defaultHealthForm]);

  /**
   * 健康提醒「应用配置」：整体提交表单到后端并用返回值刷新已应用快照
   *
   * Business Logic（为什么需要这个函数）:
   *   健康配置需整体覆盖式回写（后端 update_health_config 不做部分合并），
   *   提交后用后端返回值刷新已应用快照与表单，保证 UI 与后端一致。
   */
  const handleApplyHealth = async () => {
    setApplyingHealth(true);
    setHealthError(null);
    try {
      const updated = await healthApi.updateConfig(healthForm);
      setHealthConfig(updated);
      setHealthForm(healthConfigToForm(updated));
    } catch (err) {
      setHealthError(err instanceof Error ? err.message : t('settings:health.applyFailed'));
    } finally {
      setApplyingHealth(false);
    }
  };

  /**
   * 更新 Orchestrator 自动化表单
   *
   * Business Logic（为什么需要这个函数）:
   *   自动化 tab 是受控表单，用户改动任一字段后应清掉上次保存成功/失败提示，避免旧状态误导当前编辑。
   *
   * Code Logic（这个函数做什么）:
   *   接收完整 nextForm 写入状态，同时重置 saved/error 标记；是否 dirty 由 automationDirty 派生计算。
   */
  const handleAutomationFormChange = useCallback((nextForm: AutomationSettingsForm) => {
    setAutomationForm(nextForm);
    setAutomationError(null);
    setAutomationSaved(false);
  }, []);

  /**
   * Orchestrator 自动化「恢复默认」：把表单重置为后端默认配置
   *
   * Business Logic（为什么需要这个函数）:
   *   用户应能把自动化策略恢复到应用默认值，但恢复默认不应立刻落盘，需由用户再次点击应用配置确认。
   *
   * Code Logic（这个函数做什么）:
   *   使用加载阶段保存的 defaultAutomationForm 覆盖当前表单，并清理保存状态提示。
   */
  const handleResetAutomationDefaults = useCallback(() => {
    setAutomationForm(defaultAutomationForm);
    setAutomationError(null);
    setAutomationSaved(false);
  }, [defaultAutomationForm]);

  /**
   * Orchestrator 自动化「应用配置」：提交自动化表单到后端并刷新基准快照
   *
   * Business Logic（为什么需要这个函数）:
   *   自动化配置由 Phase 1 后端命令持久化；保存成功后 UI 必须以返回值为准，展示后端归一化后的验证命令。
   *
   * Code Logic（这个函数做什么）:
   *   把表单转成 update patch 调用 update_orchestrator_config；成功后用返回 DTO 重新生成 form/initial，
   *   失败则展示错误并保留用户当前输入。
   */
  const handleSaveAutomation = async () => {
    setSavingAutomation(true);
    setAutomationError(null);
    setAutomationSaved(false);
    try {
      const updated = await orchestratorConfigApi.update(automationFormToPatch(automationForm));
      const savedForm = automationConfigToForm(updated);
      setAutomationForm(savedForm);
      setInitialAutomationForm(savedForm);
      setAutomationSaved(true);
    } catch (err) {
      setAutomationError(err instanceof Error ? err.message : t('settings:automation.applyFailed'));
    } finally {
      setSavingAutomation(false);
    }
  };


  return {
    t,
    loading,
    loadError,
    resourceResults,
    retryingGroup,
    handleRetryResourceGroup,
    activeTab,
    setActiveTab,
    handleTabKeyDown,
    tabs: SETTINGS_TABS,

    state,
    isDirty,
    savedAt,
    saving,
    saveError,
    choosingDir,
    canResetCoreDefaults,
    recordingShortcutId,
    handleDeviceNameChange,
    handleReceiveDirChange,
    handleChooseDir,
    handleShortcutFocus,
    handleShortcutBlur,
    handleShortcutKeyDown,
    handleResetDefaults,
    handleSave,

    permStatus,
    permLoading,
    permRefreshing,
    permError,
    permRequesting,
    refreshPermissions,
    handleRequestAccess,

    healthForm,
    healthConfig,
    applyingHealth,
    healthError,
    healthLoadError,
    canResetHealthDefaults,
    patchHealthForm,
    handleResetHealthDefaults,
    handleApplyHealth,

    cloudSyncForm,
    cloudSync,
    syncResult,
    testResult,
    cloudSyncError,
    testing,
    applying,
    syncing,
    cloudSyncLoadError,
    canResetCloudSyncDefaults,
    patchCloudSyncForm,
    handleResetCloudSyncDefaults,
    handleTestCloudSync,
    handleApplyCloudSync,
    handleSyncNow,
    lanSyncResult,
    lanSyncing,
    lanSyncError,
    handleLanSyncNow,
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

    githubTrendingForm,
    githubTrendingConfig,
    claudeCliTest,
    githubTrendingError,
    testingClaudeCli,
    applyingGithubTrending,
    githubTrendingLoadError,
    canResetGithubTrendingDefaults,
    patchGithubTrendingForm,
    handleResetGithubTrendingDefaults,
    handleApplyGithubTrending,
    handleTestClaudeCli,

    promptOptimizerForm,
    promptOptimizerConfig,
    applyingPromptOptimizer,
    promptOptimizerSettingsError,
    canResetPromptOptimizerDefaults,
    patchPromptOptimizerForm,
    handleResetPromptOptimizerSettingsDefaults,
    handleApplyPromptOptimizerSettings,
    promptOptimizerShortcutId: PROMPT_OPTIMIZER_SHORTCUT_ID,
    formatShortcutForDisplay,

    automationForm,
    defaultAutomationForm,
    automationDirty,
    savingAutomation,
    automationError,
    automationSaved,
    automationLoadError,
    canResetAutomationDefaults,
    handleAutomationFormChange,
    handleResetAutomationDefaults,
    handleSaveAutomation,

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
    handleCheckUpdate,
    handleDownload,
    handleCancelDownload,
    handleInstall,
    formatSize,
    formatTime,
  };
}
