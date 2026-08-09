/**
 * Settings 表单草稿与 safe-save 控制器（composer）
 *
 * Business Logic（为什么需要这个 hook）:
 *   General/CloudSync/GitHub·AI/PromptOptimizer/Health/Automation 与备份/LAN 同步
 *   需要共享 dirty 保护与 saveAttempt 合同；对外仍是单一 FormSaves 面。
 *
 * Code Logic（这个 hook 做什么）:
 *   持有 general 表单；组合 SyncBackup + SecondaryForms；
 *   统一 applyResourceResults/applyGroupResult 水合入口（不写 resourceResults/loadError/versionInfo）。
 */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ChangeEvent, KeyboardEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { configApi } from '@/api/config';
import {
  createSaveAttempt,
  resolveSaveFailure,
  resolveSaveSuccess,
} from '@/lib/asyncState/saveAttempt';
import {
  isResourceReady,
  type PairResourceResult,
  type ResourceResult,
  type SettingsResourceGroup,
  type SettingsResourceResults,
} from '../settingsResources';
import { resolveShortcutRecording } from '../shortcutRecorder';
import {
  buildConfigUpdate,
  createPendingSettingsState,
  isSettingsStateDirty,
  settingsStateFromConfig,
} from '../settingsState';
import type {
  CloudSyncForm,
  GithubTrendingForm,
  HealthForm,
  PromptOptimizerSettingsForm,
  SettingsState,
} from '../settingsState';
import type { AutomationSettingsForm } from '../automationSettingsState';
import type {
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
import {
  PROMPT_OPTIMIZER_SHORTCUT_ID,
  PROMPT_QUICK_INPUT_SHORTCUT_ID,
  type ApplyGroupOptions,
} from '../settingsControllerShared';
import { useSettingsSyncBackupSaves } from './useSettingsSyncBackupSaves';
import { useSettingsSecondaryForms } from './useSettingsSecondaryForms';

export type { ApplyGroupOptions };

/**
 * useSettingsFormSaves 返回值。
 *
 * Business Logic（为什么需要这个接口）:
 *   composer 与 Resources hook 需要表单字段、保存动作与水合回调的稳定契约。
 *
 * Code Logic（这个接口做什么）:
 *   聚合 general/sync/ai/health/automation 字段与 applyResourceResults/applyGroupResult。
 */
export interface UseSettingsFormSavesResult {
  state: SettingsState;
  isDirty: boolean;
  savedAt: Date | null;
  saving: boolean;
  saveError: string | null;
  choosingDir: boolean;
  recordingShortcutId: string | null;
  handleDeviceNameChange: (e: ChangeEvent<HTMLInputElement>) => void;
  handleReceiveDirChange: (e: ChangeEvent<HTMLInputElement>) => void;
  handleChooseDir: () => Promise<void>;
  handleShortcutFocus: (id: string) => void;
  handleShortcutBlur: (id: string) => void;
  handleShortcutKeyDown: (e: KeyboardEvent<HTMLInputElement>, id: string) => void;
  handleResetDefaults: () => void;
  handleSave: () => Promise<void>;

  healthForm: HealthForm;
  healthConfig: HealthConfig | null;
  applyingHealth: boolean;
  healthError: string | null;
  patchHealthForm: (partial: Partial<HealthForm>) => void;
  handleResetHealthDefaults: () => void;
  handleApplyHealth: () => Promise<void>;

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

  githubTrendingForm: GithubTrendingForm;
  githubTrendingConfig: GithubTrendingConfig | null;
  claudeCliTest: ClaudeCliTestResult | null;
  githubTrendingError: string | null;
  testingClaudeCli: boolean;
  applyingGithubTrending: boolean;
  patchGithubTrendingForm: (partial: Partial<GithubTrendingForm>) => void;
  handleResetGithubTrendingDefaults: () => void;
  handleApplyGithubTrending: () => Promise<void>;
  handleTestClaudeCli: () => Promise<void>;

  promptOptimizerForm: PromptOptimizerSettingsForm;
  promptOptimizerConfig: PromptOptimizerSettingsForm | null;
  applyingPromptOptimizer: boolean;
  promptOptimizerSettingsError: string | null;
  patchPromptOptimizerForm: (partial: Partial<PromptOptimizerSettingsForm>) => void;
  handleResetPromptOptimizerSettingsDefaults: () => void;
  handleApplyPromptOptimizerSettings: () => Promise<void>;

  automationForm: AutomationSettingsForm;
  defaultAutomationForm: AutomationSettingsForm;
  automationDirty: boolean;
  savingAutomation: boolean;
  automationError: string | null;
  automationSaved: boolean;
  handleAutomationFormChange: (nextForm: AutomationSettingsForm) => void;
  handleResetAutomationDefaults: () => void;
  handleSaveAutomation: () => Promise<void>;

  applyResourceResults: (results: SettingsResourceResults) => void;
  applyGroupResult: (
    group: SettingsResourceGroup,
    groupResult:
      | ResourceResult<import('@/lib/types').AppConfig>
      | ResourceResult<import('@/lib/types').VersionInfo>
      | PairResourceResult<import('@/lib/types').CloudSyncConfig>
      | PairResourceResult<import('@/lib/types').GithubTrendingConfig>
      | PairResourceResult<import('@/lib/types').HealthConfig>
      | PairResourceResult<import('@/api/orchestratorConfig').OrchestratorAutomationConfig>,
    options?: ApplyGroupOptions,
  ) => void;
}

/**
 * Settings 表单草稿与 safe-save hook
 *
 * Business Logic（为什么需要这个函数）:
 *   多 tab 草稿与保存合同必须集中对外，且资源重试不得覆盖未保存编辑。
 *
 * Code Logic（这个函数做什么）:
 *   general + syncBackup + secondaryForms；统一 apply* 供 Resources 调用。
 *
 * @returns 表单状态、保存动作与水合回调
 */
export function useSettingsFormSaves(): UseSettingsFormSavesResult {
  const { t } = useTranslation(['settings', 'common']);
  const [state, setState] = useState<SettingsState>(createPendingSettingsState);
  const [initialState, setInitialState] = useState<SettingsState>(createPendingSettingsState);
  const [defaultState, setDefaultState] = useState<SettingsState>(createPendingSettingsState);
  const [savedAt, setSavedAt] = useState<Date | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [choosingDir, setChoosingDir] = useState(false);
  const [recordingShortcutId, setRecordingShortcutId] = useState<string | null>(null);

  const generalEditVersionRef = useRef(0);
  const generalRequestSeqRef = useRef(0);
  const stateRef = useRef(state);
  const initialStateRef = useRef(initialState);
  useEffect(() => {
    stateRef.current = state;
    initialStateRef.current = initialState;
  });

  const syncBackup = useSettingsSyncBackupSaves();
  const secondary = useSettingsSecondaryForms();

  const isDirty = useMemo(() => isSettingsStateDirty(state, initialState), [state, initialState]);

  /**
   * 通用字段更新：merge 浅层部分字段
   */
  const patchState = useCallback((partial: Partial<SettingsState>) => {
    setState((prev) => {
      const next = { ...prev, ...partial };
      stateRef.current = next;
      return next;
    });
    generalEditVersionRef.current += 1;
    setSaveError(null);
  }, []);

  const handleDeviceNameChange = (e: ChangeEvent<HTMLInputElement>) => {
    patchState({ deviceName: e.target.value });
  };

  const handleReceiveDirChange = (e: ChangeEvent<HTMLInputElement>) => {
    patchState({ receiveDir: e.target.value });
  };

  /**
   * 处理快捷键输入
   */
  const handleShortcutChange = useCallback((id: string, value: string) => {
    setState((prev) => {
      const next = {
        ...prev,
        shortcuts: prev.shortcuts.map((s) => (s.id === id ? { ...s, value } : s)),
      };
      stateRef.current = next;
      return next;
    });
    generalEditVersionRef.current += 1;
    setSaveError(null);
  }, []);

  const handleShortcutFocus = useCallback((id: string) => {
    setRecordingShortcutId(id);
  }, []);

  const handleShortcutBlur = useCallback((id: string) => {
    setRecordingShortcutId((prev) => (prev === id ? null : prev));
  }, []);

  /**
   * 录制快捷键按键
   */
  const handleShortcutKeyDown = useCallback(
    (e: KeyboardEvent<HTMLInputElement>, id: string) => {
      e.preventDefault();
      e.stopPropagation();
      const result = resolveShortcutRecording(e, {
        allowModifierOnly: id === PROMPT_OPTIMIZER_SHORTCUT_ID,
        allowBareKey: id === PROMPT_QUICK_INPUT_SHORTCUT_ID,
      });
      if (result.type === 'pending') return;
      if (result.type === 'cancel') {
        setRecordingShortcutId(null);
        e.currentTarget.blur();
        return;
      }
      // 三个快捷键（screenshot / promptOptimizer / promptQuickInput）都直接写入 state.shortcuts，
      // 随常规「保存」经 buildConfigUpdate 持久化到 screenshotHotkey / promptOptimizerHotkey /
      // promptQuickInputHotkey；不再分派到 AI tab 的 secondary 表单。
      handleShortcutChange(id, result.value);
      setRecordingShortcutId(null);
      e.currentTarget.blur();
    },
    [handleShortcutChange],
  );

  const handleResetDefaults = () => {
    stateRef.current = defaultState;
    generalEditVersionRef.current += 1;
    setState(defaultState);
    setSaveError(null);
  };

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
   * 保存常规偏好（safe-save）
   */
  const handleSave = async () => {
    const snapshot: SettingsState = {
      ...stateRef.current,
      shortcuts: stateRef.current.shortcuts.map((s) => ({ ...s })),
    };
    const attempt = createSaveAttempt(
      ++generalRequestSeqRef.current,
      snapshot,
      generalEditVersionRef.current,
    );
    setSaving(true);
    setSaveError(null);
    try {
      const updatedConfig = await configApi.update(
        buildConfigUpdate(attempt.submittedSnapshot, initialStateRef.current),
      );
      const serverState = settingsStateFromConfig(updatedConfig);
      const resolution = resolveSaveSuccess({
        attempt,
        currentRequestSeq: generalRequestSeqRef.current,
        currentDraft: stateRef.current,
        currentEditVersion: generalEditVersionRef.current,
        serverValue: serverState,
        currentBaseline: initialStateRef.current,
      });
      if (!resolution.applied) return;
      initialStateRef.current = resolution.baseline;
      stateRef.current = resolution.draft;
      setInitialState(resolution.baseline);
      setState(resolution.draft);
      setSavedAt(new Date());
      setSaveError(null);
    } catch (err) {
      const failure = resolveSaveFailure({
        attempt,
        currentRequestSeq: generalRequestSeqRef.current,
        currentDraft: stateRef.current,
        currentBaseline: initialStateRef.current,
      });
      if (!failure.applied) return;
      setSaveError(err instanceof Error ? err.message : t('error.saveFailed'));
    } finally {
      if (attempt.requestSeq === generalRequestSeqRef.current) {
        setSaving(false);
      }
    }
  };

  /**
   * 全量资源 ready 组水合到表单
   */
  const applyResourceResults = useCallback(
    (results: SettingsResourceResults) => {
      if (isResourceReady(results.core)) {
        const loaded = settingsStateFromConfig(results.core.value);
        setState(loaded);
        setInitialState(loaded);
      }
      if (isResourceReady(results.defaults)) {
        setDefaultState(settingsStateFromConfig(results.defaults.value));
      }
      syncBackup.applyFromResults(results);
      secondary.applyFromResults(results);
    },
    [secondary.applyFromResults, syncBackup.applyFromResults],
  );

  /**
   * 单组资源结果水合到表单（dirty 保护）
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
      options?: ApplyGroupOptions,
    ) => {
      const allowRewriteForm = options?.allowRewriteForm === true;
      if (group === 'core') {
        const result = groupResult as ResourceResult<import('@/lib/types').AppConfig>;
        if (isResourceReady(result)) {
          const config = result.value;
          // shortcuts（含三个 hotkey）由 settingsStateFromConfig 从 config 重新生成；
          // promptOptimizer 的 fillLanguage 仍在 secondary 表单，单独水合。
          const loaded = settingsStateFromConfig(config);
          const rewrite = allowRewriteForm || !isSettingsStateDirty(state, initialState);
          if (rewrite) {
            setState(loaded);
          }
          setInitialState(loaded);
          secondary.applyCorePromptOptimizer(config, rewrite);
        }
        return;
      }
      if (group === 'defaults') {
        const result = groupResult as ResourceResult<import('@/lib/types').AppConfig>;
        if (isResourceReady(result)) {
          setDefaultState(settingsStateFromConfig(result.value));
          secondary.applyDefaultsPromptOptimizer(result.value);
        }
        return;
      }
      if (group === 'version') return;
      if (group === 'cloudSync') {
        syncBackup.applyGroup(
          groupResult as PairResourceResult<CloudSyncConfig>,
          options,
        );
        return;
      }
      if (group === 'githubTrending') {
        secondary.applyGithubGroup(
          groupResult as PairResourceResult<GithubTrendingConfig>,
          options,
        );
        return;
      }
      if (group === 'health') {
        secondary.applyHealthGroup(groupResult as PairResourceResult<HealthConfig>, options);
        return;
      }
      if (group === 'automation') {
        secondary.applyAutomationGroup(
          groupResult as PairResourceResult<
            import('@/api/orchestratorConfig').OrchestratorAutomationConfig
          >,
          options,
        );
      }
    },
    [
      initialState,
      state,
      secondary.applyAutomationGroup,
      secondary.applyCorePromptOptimizer,
      secondary.applyDefaultsPromptOptimizer,
      secondary.applyGithubGroup,
      secondary.applyHealthGroup,
      syncBackup.applyGroup,
    ],
  );

  return {
    state,
    isDirty,
    savedAt,
    saving,
    saveError,
    choosingDir,
    recordingShortcutId,
    handleDeviceNameChange,
    handleReceiveDirChange,
    handleChooseDir,
    handleShortcutFocus,
    handleShortcutBlur,
    handleShortcutKeyDown,
    handleResetDefaults,
    handleSave,

    healthForm: secondary.healthForm,
    healthConfig: secondary.healthConfig,
    applyingHealth: secondary.applyingHealth,
    healthError: secondary.healthError,
    patchHealthForm: secondary.patchHealthForm,
    handleResetHealthDefaults: secondary.handleResetHealthDefaults,
    handleApplyHealth: secondary.handleApplyHealth,

    cloudSyncForm: syncBackup.cloudSyncForm,
    cloudSync: syncBackup.cloudSync,
    syncResult: syncBackup.syncResult,
    testResult: syncBackup.testResult,
    cloudSyncError: syncBackup.cloudSyncError,
    testing: syncBackup.testing,
    applying: syncBackup.applying,
    syncing: syncBackup.syncing,
    lanSyncResult: syncBackup.lanSyncResult,
    lanSyncing: syncBackup.lanSyncing,
    lanSyncError: syncBackup.lanSyncError,
    backupExporting: syncBackup.backupExporting,
    backupExportPath: syncBackup.backupExportPath,
    backupExportError: syncBackup.backupExportError,
    backupRestoring: syncBackup.backupRestoring,
    backupInspect: syncBackup.backupInspect,
    backupArchivePath: syncBackup.backupArchivePath,
    backupSelectedDomains: syncBackup.backupSelectedDomains,
    backupMode: syncBackup.backupMode,
    backupRestoreDialogOpen: syncBackup.backupRestoreDialogOpen,
    backupRestoreResult: syncBackup.backupRestoreResult,
    backupRestoreError: syncBackup.backupRestoreError,
    backupJobs: syncBackup.backupJobs,
    backupJobsLoading: syncBackup.backupJobsLoading,
    backupJobsError: syncBackup.backupJobsError,
    backupRollbackJobId: syncBackup.backupRollbackJobId,
    backupRollbackDialogOpen: syncBackup.backupRollbackDialogOpen,
    backupRollingBack: syncBackup.backupRollingBack,
    patchCloudSyncForm: syncBackup.patchCloudSyncForm,
    handleResetCloudSyncDefaults: syncBackup.handleResetCloudSyncDefaults,
    handleTestCloudSync: syncBackup.handleTestCloudSync,
    handleApplyCloudSync: syncBackup.handleApplyCloudSync,
    handleSyncNow: syncBackup.handleSyncNow,
    handleLanSyncNow: syncBackup.handleLanSyncNow,
    handleBackupExport: syncBackup.handleBackupExport,
    handleBackupPickRestore: syncBackup.handleBackupPickRestore,
    handleBackupToggleDomain: syncBackup.handleBackupToggleDomain,
    handleBackupSetMode: syncBackup.handleBackupSetMode,
    handleBackupOpenRestoreDialog: syncBackup.handleBackupOpenRestoreDialog,
    handleBackupRestoreConfirm: syncBackup.handleBackupRestoreConfirm,
    handleCloseRestoreDialog: syncBackup.handleCloseRestoreDialog,
    handleRefreshRecoveryJobs: syncBackup.handleRefreshRecoveryJobs,
    handleOpenRollback: syncBackup.handleOpenRollback,
    handleConfirmRollback: syncBackup.handleConfirmRollback,
    handleCloseRollbackDialog: syncBackup.handleCloseRollbackDialog,

    githubTrendingForm: secondary.githubTrendingForm,
    githubTrendingConfig: secondary.githubTrendingConfig,
    claudeCliTest: secondary.claudeCliTest,
    githubTrendingError: secondary.githubTrendingError,
    testingClaudeCli: secondary.testingClaudeCli,
    applyingGithubTrending: secondary.applyingGithubTrending,
    patchGithubTrendingForm: secondary.patchGithubTrendingForm,
    handleResetGithubTrendingDefaults: secondary.handleResetGithubTrendingDefaults,
    handleApplyGithubTrending: secondary.handleApplyGithubTrending,
    handleTestClaudeCli: secondary.handleTestClaudeCli,

    promptOptimizerForm: secondary.promptOptimizerForm,
    promptOptimizerConfig: secondary.promptOptimizerConfig,
    applyingPromptOptimizer: secondary.applyingPromptOptimizer,
    promptOptimizerSettingsError: secondary.promptOptimizerSettingsError,
    patchPromptOptimizerForm: secondary.patchPromptOptimizerForm,
    handleResetPromptOptimizerSettingsDefaults:
      secondary.handleResetPromptOptimizerSettingsDefaults,
    handleApplyPromptOptimizerSettings: secondary.handleApplyPromptOptimizerSettings,

    automationForm: secondary.automationForm,
    defaultAutomationForm: secondary.defaultAutomationForm,
    automationDirty: secondary.automationDirty,
    savingAutomation: secondary.savingAutomation,
    automationError: secondary.automationError,
    automationSaved: secondary.automationSaved,
    handleAutomationFormChange: secondary.handleAutomationFormChange,
    handleResetAutomationDefaults: secondary.handleResetAutomationDefaults,
    handleSaveAutomation: secondary.handleSaveAutomation,

    applyResourceResults,
    applyGroupResult,
  };
}
