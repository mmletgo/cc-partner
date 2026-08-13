/**
 * Settings 次级表单（AI / Prompt 优化 / Health / Automation）控制器
 *
 * Business Logic（为什么需要这个 hook）:
 *   这些 tab 的 safe-save 与 general/sync 解耦后，FormSaves 可守 soft 行数上限。
 *
 * Code Logic（这个 hook 做什么）:
 *   持有 github/prompt/health/automation 草稿与 apply/save/reset；
 *   导出 applyFromResults/applyGroup* 供资源水合。
 */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { configApi } from '@/api/config';
import { healthApi } from '@/api/health';
import { orchestratorConfigApi } from '@/api/orchestratorConfig';
import { githubTrendingApi } from '@/api/githubTrending';
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
  automationConfigToForm,
  automationFormToPatch,
  isAutomationFormDirty,
  PENDING_AUTOMATION_SETTINGS_FORM,
} from '../automationSettingsState';
import type { AutomationSettingsForm } from '../automationSettingsState';
import {
  githubTrendingConfigToForm,
  healthConfigToForm,
  mergeActivityStatsSlice,
  mergeHealthReminderSlice,
  PENDING_GITHUB_TRENDING_FORM,
  PENDING_HEALTH_FORM,
  PENDING_PROMPT_OPTIMIZER_SETTINGS_FORM,
  promptOptimizerSettingsConfigToForm,
  promptOptimizerSettingsFormToUpdate,
} from '../settingsState';
import type {
  GithubTrendingForm,
  HealthForm,
  PromptOptimizerSettingsForm,
} from '../settingsState';
import type {
  ClaudeCliTestResult,
  GithubTrendingConfig,
  HealthConfig,
} from '@/lib/types';
import type { OrchestratorAutomationConfig } from '@/api/orchestratorConfig';
import type { ApplyGroupOptions } from '../settingsControllerShared';

/**
 * 次级表单 hook 返回值
 *
 * Business Logic（为什么需要这个接口）:
 *   FormSaves composer 需要透传 AI/Health/Automation 字段。
 *
 * Code Logic（这个接口做什么）:
 *   聚合四组表单 state/handlers 与水合入口。
 */
export interface UseSettingsSecondaryFormsResult {
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

  healthForm: HealthForm;
  healthConfig: HealthConfig | null;
  applyingHealth: boolean;
  healthError: string | null;
  patchHealthForm: (partial: Partial<HealthForm>) => void;
  handleResetHealthDefaults: () => void;
  handleApplyHealth: () => Promise<void>;
  applyingActivity: boolean;
  activityError: string | null;
  handleResetActivityDefaults: () => void;
  handleApplyActivity: () => Promise<void>;

  automationForm: AutomationSettingsForm;
  defaultAutomationForm: AutomationSettingsForm;
  automationDirty: boolean;
  savingAutomation: boolean;
  automationError: string | null;
  automationSaved: boolean;
  handleAutomationFormChange: (nextForm: AutomationSettingsForm) => void;
  handleResetAutomationDefaults: () => void;
  handleSaveAutomation: () => Promise<void>;

  applyFromResults: (results: SettingsResourceResults) => void;
  applyGithubGroup: (
    pair: PairResourceResult<GithubTrendingConfig>,
    options?: ApplyGroupOptions,
  ) => void;
  applyHealthGroup: (
    pair: PairResourceResult<HealthConfig>,
    options?: ApplyGroupOptions,
  ) => void;
  applyAutomationGroup: (
    pair: PairResourceResult<OrchestratorAutomationConfig>,
    options?: ApplyGroupOptions,
  ) => void;
  applyCorePromptOptimizer: (config: import('@/lib/types').AppConfig, rewriteForm: boolean) => void;
  applyDefaultsPromptOptimizer: (config: import('@/lib/types').AppConfig) => void;
}

/**
 * Settings 次级表单 hook
 *
 * Business Logic（为什么需要这个函数）:
 *   AI/Health/Automation 与 general 解耦，降低 FormSaves 体积。
 *
 * Code Logic（这个函数做什么）:
 *   持有四组表单与 safe-save handlers，并提供资源水合。
 *
 * @returns 次级表单状态与动作
 */
export function useSettingsSecondaryForms(): UseSettingsSecondaryFormsResult {
  const { t } = useTranslation(['settings', 'common']);

  const [githubTrendingForm, setGithubTrendingForm] = useState<GithubTrendingForm>({
    ...PENDING_GITHUB_TRENDING_FORM,
  });
  const [defaultGithubTrendingForm, setDefaultGithubTrendingForm] = useState<GithubTrendingForm>({
    ...PENDING_GITHUB_TRENDING_FORM,
  });
  const [githubTrendingConfig, setGithubTrendingConfig] = useState<GithubTrendingConfig | null>(
    null,
  );
  const [claudeCliTest, setClaudeCliTest] = useState<ClaudeCliTestResult | null>(null);
  const [githubTrendingError, setGithubTrendingError] = useState<string | null>(null);
  const [testingClaudeCli, setTestingClaudeCli] = useState(false);
  const [applyingGithubTrending, setApplyingGithubTrending] = useState(false);

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

  const [healthForm, setHealthForm] = useState<HealthForm>({ ...PENDING_HEALTH_FORM });
  const [defaultHealthForm, setDefaultHealthForm] = useState<HealthForm>({ ...PENDING_HEALTH_FORM });
  const [healthConfig, setHealthConfig] = useState<HealthConfig | null>(null);
  const [applyingHealth, setApplyingHealth] = useState(false);
  const [healthError, setHealthError] = useState<string | null>(null);
  const [applyingActivity, setApplyingActivity] = useState(false);
  const [activityError, setActivityError] = useState<string | null>(null);

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

  const githubTrendingEditVersionRef = useRef(0);
  const githubTrendingRequestSeqRef = useRef(0);
  const promptOptimizerEditVersionRef = useRef(0);
  const promptOptimizerRequestSeqRef = useRef(0);
  const healthEditVersionRef = useRef(0);
  const healthRequestSeqRef = useRef(0);
  const activityEditVersionRef = useRef(0);
  const activityRequestSeqRef = useRef(0);
  const automationEditVersionRef = useRef(0);
  const automationRequestSeqRef = useRef(0);

  const githubTrendingFormRef = useRef(githubTrendingForm);
  const githubTrendingConfigRef = useRef(githubTrendingConfig);
  const promptOptimizerFormRef = useRef(promptOptimizerForm);
  const promptOptimizerConfigRef = useRef(promptOptimizerConfig);
  const healthFormRef = useRef(healthForm);
  const healthConfigRef = useRef(healthConfig);
  const automationFormRef = useRef(automationForm);
  const initialAutomationFormRef = useRef(initialAutomationForm);
  useEffect(() => {
    githubTrendingFormRef.current = githubTrendingForm;
    githubTrendingConfigRef.current = githubTrendingConfig;
    promptOptimizerFormRef.current = promptOptimizerForm;
    promptOptimizerConfigRef.current = promptOptimizerConfig;
    healthFormRef.current = healthForm;
    healthConfigRef.current = healthConfig;
    automationFormRef.current = automationForm;
    initialAutomationFormRef.current = initialAutomationForm;
  });

  const automationDirty = useMemo(
    () => isAutomationFormDirty(automationForm, initialAutomationForm),
    [automationForm, initialAutomationForm],
  );

  /**
   * 全量资源水合次级表单
   */
  const applyFromResults = useCallback((results: SettingsResourceResults) => {
    if (isResourceReady(results.core)) {
      const config = results.core.value;
      setPromptOptimizerConfig(promptOptimizerSettingsConfigToForm(config));
      setPromptOptimizerForm(promptOptimizerSettingsConfigToForm(config));
    }
    if (isResourceReady(results.defaults)) {
      setDefaultPromptOptimizerForm(
        promptOptimizerSettingsConfigToForm(results.defaults.value),
      );
    }
    if (isResourceReady(results.githubTrending.current)) {
      const loaded = results.githubTrending.current.value;
      setGithubTrendingConfig(loaded);
      setGithubTrendingForm(githubTrendingConfigToForm(loaded));
      setGithubTrendingError(null);
    }
    if (isResourceReady(results.githubTrending.defaults)) {
      setDefaultGithubTrendingForm(
        githubTrendingConfigToForm(results.githubTrending.defaults.value),
      );
    }
    if (isResourceReady(results.health.current)) {
      const loaded = results.health.current.value;
      setHealthConfig(loaded);
      setHealthForm(healthConfigToForm(loaded));
      setHealthError(null);
      setActivityError(null);
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
  }, []);

  /**
   * core 重试时同步 prompt optimizer（按 rewrite 决定是否写 form）
   */
  const applyCorePromptOptimizer = useCallback(
    (config: import('@/lib/types').AppConfig, rewriteForm: boolean) => {
      if (rewriteForm) {
        setPromptOptimizerForm(promptOptimizerSettingsConfigToForm(config));
      }
      setPromptOptimizerConfig(promptOptimizerSettingsConfigToForm(config));
    },
    [],
  );

  /**
   * defaults 重试时同步 prompt optimizer 默认快照
   */
  const applyDefaultsPromptOptimizer = useCallback((config: import('@/lib/types').AppConfig) => {
    setDefaultPromptOptimizerForm(promptOptimizerSettingsConfigToForm(config));
  }, []);

  /**
   * github 组重试水合
   */
  const applyGithubGroup = useCallback(
    (pair: PairResourceResult<GithubTrendingConfig>, options?: ApplyGroupOptions) => {
      const allowRewriteForm = options?.allowRewriteForm === true;
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
    },
    [githubTrendingConfig, githubTrendingForm],
  );

  /**
   * health 组重试水合
   */
  const applyHealthGroup = useCallback(
    (pair: PairResourceResult<HealthConfig>, options?: ApplyGroupOptions) => {
      const allowRewriteForm = options?.allowRewriteForm === true;
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
        setActivityError(null);
      }
      if (isResourceReady(pair.defaults)) {
        setDefaultHealthForm(healthConfigToForm(pair.defaults.value));
      }
    },
    [healthConfig, healthForm],
  );

  /**
   * automation 组重试水合
   */
  const applyAutomationGroup = useCallback(
    (pair: PairResourceResult<OrchestratorAutomationConfig>, options?: ApplyGroupOptions) => {
      const allowRewriteForm = options?.allowRewriteForm === true;
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
    },
    [automationForm, initialAutomationForm],
  );

  const patchGithubTrendingForm = useCallback((partial: Partial<GithubTrendingForm>) => {
    setGithubTrendingForm((prev) => {
      const next = { ...prev, ...partial };
      githubTrendingFormRef.current = next;
      return next;
    });
    githubTrendingEditVersionRef.current += 1;
    setGithubTrendingError(null);
  }, []);

  const handleResetGithubTrendingDefaults = useCallback(() => {
    githubTrendingFormRef.current = defaultGithubTrendingForm;
    githubTrendingEditVersionRef.current += 1;
    setGithubTrendingForm(defaultGithubTrendingForm);
    setGithubTrendingError(null);
  }, [defaultGithubTrendingForm]);

  const handleApplyGithubTrending = async () => {
    const snapshot: GithubTrendingForm = { ...githubTrendingFormRef.current };
    const attempt = createSaveAttempt(
      ++githubTrendingRequestSeqRef.current,
      snapshot,
      githubTrendingEditVersionRef.current,
    );
    setApplyingGithubTrending(true);
    setGithubTrendingError(null);
    try {
      const updated = await githubTrendingApi.updateConfig({
        aiEnabled: attempt.submittedSnapshot.aiEnabled,
        claudeCliPath: attempt.submittedSnapshot.claudeCliPath.trim() || 'claude',
        claudeModel: attempt.submittedSnapshot.claudeModel.trim() || 'sonnet',
        cacheTtlHours: attempt.submittedSnapshot.cacheTtlHours,
      });
      const serverForm = githubTrendingConfigToForm(updated);
      const baselineForm = githubTrendingConfigRef.current
        ? githubTrendingConfigToForm(githubTrendingConfigRef.current)
        : attempt.submittedSnapshot;
      const resolution = resolveSaveSuccess({
        attempt,
        currentRequestSeq: githubTrendingRequestSeqRef.current,
        currentDraft: githubTrendingFormRef.current,
        currentEditVersion: githubTrendingEditVersionRef.current,
        serverValue: serverForm,
        currentBaseline: baselineForm,
      });
      if (!resolution.applied) return;
      githubTrendingConfigRef.current = updated;
      githubTrendingFormRef.current = resolution.draft;
      setGithubTrendingConfig(updated);
      setGithubTrendingForm(resolution.draft);
    } catch (err) {
      const baselineForm = githubTrendingConfigRef.current
        ? githubTrendingConfigToForm(githubTrendingConfigRef.current)
        : attempt.submittedSnapshot;
      const failure = resolveSaveFailure({
        attempt,
        currentRequestSeq: githubTrendingRequestSeqRef.current,
        currentDraft: githubTrendingFormRef.current,
        currentBaseline: baselineForm,
      });
      if (!failure.applied) return;
      setGithubTrendingError(
        err instanceof Error ? err.message : t('settings:githubTrending.applyFailed'),
      );
    } finally {
      if (attempt.requestSeq === githubTrendingRequestSeqRef.current) {
        setApplyingGithubTrending(false);
      }
    }
  };

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
        error:
          err instanceof Error ? err.message : t('githubTrending.testFailed', { error: '' }).trim(),
      });
    } finally {
      setTestingClaudeCli(false);
    }
  };

  const patchPromptOptimizerForm = useCallback((partial: Partial<PromptOptimizerSettingsForm>) => {
    setPromptOptimizerForm((prev) => {
      const next = { ...prev, ...partial };
      promptOptimizerFormRef.current = next;
      return next;
    });
    promptOptimizerEditVersionRef.current += 1;
    setPromptOptimizerSettingsError(null);
  }, []);

  const handleResetPromptOptimizerSettingsDefaults = useCallback(() => {
    promptOptimizerFormRef.current = defaultPromptOptimizerForm;
    promptOptimizerEditVersionRef.current += 1;
    setPromptOptimizerForm(defaultPromptOptimizerForm);
    setPromptOptimizerSettingsError(null);
  }, [defaultPromptOptimizerForm]);

  const handleApplyPromptOptimizerSettings = async () => {
    const snapshot: PromptOptimizerSettingsForm = { ...promptOptimizerFormRef.current };
    const attempt = createSaveAttempt(
      ++promptOptimizerRequestSeqRef.current,
      snapshot,
      promptOptimizerEditVersionRef.current,
    );
    setApplyingPromptOptimizer(true);
    setPromptOptimizerSettingsError(null);
    try {
      const updated = await configApi.update(
        promptOptimizerSettingsFormToUpdate(attempt.submittedSnapshot),
      );
      const serverForm = promptOptimizerSettingsConfigToForm(updated);
      const baselineForm = promptOptimizerConfigRef.current ?? attempt.submittedSnapshot;
      const resolution = resolveSaveSuccess({
        attempt,
        currentRequestSeq: promptOptimizerRequestSeqRef.current,
        currentDraft: promptOptimizerFormRef.current,
        currentEditVersion: promptOptimizerEditVersionRef.current,
        serverValue: serverForm,
        currentBaseline: baselineForm,
      });
      if (!resolution.applied) return;
      promptOptimizerConfigRef.current = resolution.baseline;
      promptOptimizerFormRef.current = resolution.draft;
      setPromptOptimizerConfig(resolution.baseline);
      setPromptOptimizerForm(resolution.draft);
    } catch (err) {
      const failure = resolveSaveFailure({
        attempt,
        currentRequestSeq: promptOptimizerRequestSeqRef.current,
        currentDraft: promptOptimizerFormRef.current,
        currentBaseline: promptOptimizerConfigRef.current ?? attempt.submittedSnapshot,
      });
      if (!failure.applied) return;
      setPromptOptimizerSettingsError(
        err instanceof Error ? err.message : t('settings:promptOptimizerSettings.applyFailed'),
      );
    } finally {
      if (attempt.requestSeq === promptOptimizerRequestSeqRef.current) {
        setApplyingPromptOptimizer(false);
      }
    }
  };

  const patchHealthForm = useCallback((partial: Partial<HealthForm>) => {
    setHealthForm((prev) => {
      const next = { ...prev, ...partial };
      healthFormRef.current = next;
      return next;
    });
    healthEditVersionRef.current += 1;
    activityEditVersionRef.current += 1;
    setHealthError(null);
    setActivityError(null);
  }, []);

  const handleResetHealthDefaults = useCallback(() => {
    const next = mergeHealthReminderSlice(healthFormRef.current, defaultHealthForm);
    healthFormRef.current = next;
    healthEditVersionRef.current += 1;
    setHealthForm(next);
    setHealthError(null);
  }, [defaultHealthForm]);

  const handleResetActivityDefaults = useCallback(() => {
    const next = mergeActivityStatsSlice(healthFormRef.current, defaultHealthForm);
    healthFormRef.current = next;
    activityEditVersionRef.current += 1;
    setHealthForm(next);
    setActivityError(null);
  }, [defaultHealthForm]);

  const handleApplyHealth = async () => {
    const snapshot = mergeHealthReminderSlice(
      healthConfigRef.current,
      healthFormRef.current,
    );
    const attempt = createSaveAttempt(
      ++healthRequestSeqRef.current,
      snapshot,
      healthEditVersionRef.current,
    );
    setApplyingHealth(true);
    setHealthError(null);
    try {
      const updated = await healthApi.updateConfig(attempt.submittedSnapshot);
      const serverForm = healthConfigToForm(updated);
      const baselineForm = healthConfigRef.current
        ? healthConfigToForm(healthConfigRef.current)
        : attempt.submittedSnapshot;
      const resolution = resolveSaveSuccess({
        attempt,
        currentRequestSeq: healthRequestSeqRef.current,
        currentDraft: healthFormRef.current,
        currentEditVersion: healthEditVersionRef.current,
        serverValue: serverForm,
        currentBaseline: baselineForm,
      });
      if (!resolution.applied) return;
      healthConfigRef.current = updated;
      healthFormRef.current = resolution.draft;
      setHealthConfig(updated);
      setHealthForm(resolution.draft);
    } catch (err) {
      const baselineForm = healthConfigRef.current
        ? healthConfigToForm(healthConfigRef.current)
        : attempt.submittedSnapshot;
      const failure = resolveSaveFailure({
        attempt,
        currentRequestSeq: healthRequestSeqRef.current,
        currentDraft: healthFormRef.current,
        currentBaseline: baselineForm,
      });
      if (!failure.applied) return;
      setHealthError(err instanceof Error ? err.message : t('settings:health.applyFailed'));
    } finally {
      if (attempt.requestSeq === healthRequestSeqRef.current) {
        setApplyingHealth(false);
      }
    }
  };

  const handleApplyActivity = async () => {
    const snapshot = mergeActivityStatsSlice(
      healthConfigRef.current,
      healthFormRef.current,
    );
    const attempt = createSaveAttempt(
      ++activityRequestSeqRef.current,
      snapshot,
      activityEditVersionRef.current,
    );
    setApplyingActivity(true);
    setActivityError(null);
    try {
      const updated = await healthApi.updateConfig(attempt.submittedSnapshot);
      const serverForm = healthConfigToForm(updated);
      const baselineForm = healthConfigRef.current
        ? healthConfigToForm(healthConfigRef.current)
        : attempt.submittedSnapshot;
      const resolution = resolveSaveSuccess({
        attempt,
        currentRequestSeq: activityRequestSeqRef.current,
        currentDraft: healthFormRef.current,
        currentEditVersion: activityEditVersionRef.current,
        serverValue: serverForm,
        currentBaseline: baselineForm,
      });
      if (!resolution.applied) return;
      healthConfigRef.current = updated;
      healthFormRef.current = resolution.draft;
      setHealthConfig(updated);
      setHealthForm(resolution.draft);
    } catch (err) {
      const baselineForm = healthConfigRef.current
        ? healthConfigToForm(healthConfigRef.current)
        : attempt.submittedSnapshot;
      const failure = resolveSaveFailure({
        attempt,
        currentRequestSeq: activityRequestSeqRef.current,
        currentDraft: healthFormRef.current,
        currentBaseline: baselineForm,
      });
      if (!failure.applied) return;
      setActivityError(err instanceof Error ? err.message : t('settings:activity.applyFailed'));
    } finally {
      if (attempt.requestSeq === activityRequestSeqRef.current) {
        setApplyingActivity(false);
      }
    }
  };

  const handleAutomationFormChange = useCallback((nextForm: AutomationSettingsForm) => {
    automationFormRef.current = nextForm;
    automationEditVersionRef.current += 1;
    setAutomationForm(nextForm);
    setAutomationError(null);
    setAutomationSaved(false);
  }, []);

  const handleResetAutomationDefaults = useCallback(() => {
    automationFormRef.current = defaultAutomationForm;
    automationEditVersionRef.current += 1;
    setAutomationForm(defaultAutomationForm);
    setAutomationError(null);
    setAutomationSaved(false);
  }, [defaultAutomationForm]);

  const handleSaveAutomation = async () => {
    const snapshot: AutomationSettingsForm = { ...automationFormRef.current };
    const attempt = createSaveAttempt(
      ++automationRequestSeqRef.current,
      snapshot,
      automationEditVersionRef.current,
    );
    setSavingAutomation(true);
    setAutomationError(null);
    setAutomationSaved(false);
    try {
      const updated = await orchestratorConfigApi.update(
        automationFormToPatch(attempt.submittedSnapshot),
      );
      const serverForm = automationConfigToForm(updated);
      const resolution = resolveSaveSuccess({
        attempt,
        currentRequestSeq: automationRequestSeqRef.current,
        currentDraft: automationFormRef.current,
        currentEditVersion: automationEditVersionRef.current,
        serverValue: serverForm,
        currentBaseline: initialAutomationFormRef.current,
      });
      if (!resolution.applied) return;
      initialAutomationFormRef.current = resolution.baseline;
      automationFormRef.current = resolution.draft;
      setInitialAutomationForm(resolution.baseline);
      setAutomationForm(resolution.draft);
      setAutomationSaved(!resolution.dirty);
    } catch (err) {
      const failure = resolveSaveFailure({
        attempt,
        currentRequestSeq: automationRequestSeqRef.current,
        currentDraft: automationFormRef.current,
        currentBaseline: initialAutomationFormRef.current,
      });
      if (!failure.applied) return;
      setAutomationError(err instanceof Error ? err.message : t('settings:automation.applyFailed'));
    } finally {
      if (attempt.requestSeq === automationRequestSeqRef.current) {
        setSavingAutomation(false);
      }
    }
  };

  return {
    githubTrendingForm,
    githubTrendingConfig,
    claudeCliTest,
    githubTrendingError,
    testingClaudeCli,
    applyingGithubTrending,
    patchGithubTrendingForm,
    handleResetGithubTrendingDefaults,
    handleApplyGithubTrending,
    handleTestClaudeCli,
    promptOptimizerForm,
    promptOptimizerConfig,
    applyingPromptOptimizer,
    promptOptimizerSettingsError,
    patchPromptOptimizerForm,
    handleResetPromptOptimizerSettingsDefaults,
    handleApplyPromptOptimizerSettings,
    healthForm,
    healthConfig,
    applyingHealth,
    healthError,
    patchHealthForm,
    handleResetHealthDefaults,
    handleApplyHealth,
    applyingActivity,
    activityError,
    handleResetActivityDefaults,
    handleApplyActivity,
    automationForm,
    defaultAutomationForm,
    automationDirty,
    savingAutomation,
    automationError,
    automationSaved,
    handleAutomationFormChange,
    handleResetAutomationDefaults,
    handleSaveAutomation,
    applyFromResults,
    applyGithubGroup,
    applyHealthGroup,
    applyAutomationGroup,
    applyCorePromptOptimizer,
    applyDefaultsPromptOptimizer,
  };
}
