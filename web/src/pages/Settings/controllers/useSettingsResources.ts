/**
 * Settings 资源加载/重试控制器
 *
 * Business Logic（为什么需要这个 hook）:
 *   设置页 11 端点按组 allSettled 加载；局部失败可重试且不得拖垮其它 tab 草稿。
 *   从巨型 controller 拆出后可独立锁定 load/retry/core loadError 合同。
 *
 * Code Logic（这个 hook 做什么）:
 *   持有 resourceResults/loading/loadError/retryingGroup/versionInfo；
 *   mount 时 loadSettingsResources；retry 仅更新一组；
 *   通过注入的 applyResourceResults/applyGroupResult 水合表单（Option A）。
 */
import { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { configApi } from '@/api/config';
import { healthApi } from '@/api/health';
import { listOrchestratorAgentAdapters } from '@/api/orchestrator';
import { orchestratorConfigApi } from '@/api/orchestratorConfig';
import { githubTrendingApi } from '@/api/githubTrending';
import type { OrchestratorAgentAdapterCatalogItem, VersionInfo } from '@/lib/types';
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
} from '../settingsResources';
import type { ApplyGroupOptions } from '../settingsControllerShared';

/**
 * Resources hook 依赖的水合接口（由 FormSaves 提供）。
 *
 * Business Logic（为什么需要这个接口）:
 *   资源加载与表单 state 解耦，但 load/retry 成功后必须写回表单；
 *   注入回调保留 dirty 保护语义且避免循环依赖。
 *
 * Code Logic（这个接口做什么）:
 *   applyAll 用于首次全量；applyGroup 用于单组重试（不含 resourceResults 合并）。
 */
export interface SettingsResourceHydrator {
  applyResourceResults: (results: SettingsResourceResults) => void;
  applyGroupResult: (
    group: SettingsResourceGroup,
    groupResult:
      | ResourceResult<import('@/lib/types').AppConfig>
      | ResourceResult<VersionInfo>
      | PairResourceResult<import('@/lib/types').CloudSyncConfig>
      | PairResourceResult<import('@/lib/types').GithubTrendingConfig>
      | PairResourceResult<import('@/lib/types').HealthConfig>
      | PairResourceResult<import('@/api/orchestratorConfig').OrchestratorAutomationConfig>,
    options?: ApplyGroupOptions,
  ) => void;
}

/**
 * useSettingsResources 入参。
 *
 * Business Logic（为什么需要这个类型）:
 *   composer 把 FormSaves 的水合回调注入 Resources。
 *
 * Code Logic（这个类型做什么）:
 *   只暴露 hydrator 字段。
 */
export interface UseSettingsResourcesParams {
  hydrator: SettingsResourceHydrator;
}

/**
 * useSettingsResources 返回值。
 *
 * Business Logic（为什么需要这个接口）:
 *   shell 需要 loading/loadError/retry；panel 需要 canReset* 与 *LoadError。
 *
 * Code Logic（这个接口做什么）:
 *   聚合资源态、派生标志、versionInfo 与 handleRetryResourceGroup。
 */
export interface UseSettingsResourcesResult {
  loading: boolean;
  loadError: string | null;
  resourceResults: SettingsResourceResults | null;
  retryingGroup: SettingsResourceGroup | null;
  handleRetryResourceGroup: (group: SettingsResourceGroup) => Promise<void>;
  canResetCoreDefaults: boolean;
  canResetCloudSyncDefaults: boolean;
  canResetGithubTrendingDefaults: boolean;
  canResetPromptOptimizerDefaults: boolean;
  canResetHealthDefaults: boolean;
  canResetAutomationDefaults: boolean;
  cloudSyncLoadError: Error | null;
  githubTrendingLoadError: Error | null;
  healthLoadError: Error | null;
  automationLoadError: Error | null;
  versionLoadError: Error | null;
  versionInfo: VersionInfo | null;
  /** owner adapter catalog（redacted；加载失败为空数组） */
  agentAdapters: OrchestratorAgentAdapterCatalogItem[];
}

/**
 * Settings 资源加载/重试 hook
 *
 * Business Logic（为什么需要这个函数）:
 *   用户进入设置页时需并行加载多组资源；单组失败只标该组，core 失败才写整页 loadError。
 *
 * Code Logic（这个函数做什么）:
 *   mount load + 单组 retry；合并 resourceResults；core/version 本地处理 loadError/versionInfo；
 *   表单写回委托 hydrator。
 *
 * @param params.hydrator FormSaves 提供的水合回调
 * @returns 资源态与 retry 动作
 */
export function useSettingsResources(
  params: UseSettingsResourcesParams,
): UseSettingsResourcesResult {
  const { hydrator } = params;
  const { t } = useTranslation(['settings', 'common']);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [resourceResults, setResourceResults] = useState<SettingsResourceResults | null>(null);
  const [retryingGroup, setRetryingGroup] = useState<SettingsResourceGroup | null>(null);
  const [versionInfo, setVersionInfo] = useState<VersionInfo | null>(null);
  const [agentAdapters, setAgentAdapters] = useState<OrchestratorAgentAdapterCatalogItem[]>([]);

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

  const canResetCoreDefaults = resourceResults?.defaults.status === 'ready';
  const canResetCloudSyncDefaults = resourceResults?.cloudSync.defaults.status === 'ready';
  const canResetGithubTrendingDefaults =
    resourceResults?.githubTrending.defaults.status === 'ready';
  const canResetPromptOptimizerDefaults = canResetCoreDefaults;
  const canResetHealthDefaults = resourceResults?.health.defaults.status === 'ready';
  const canResetAutomationDefaults = resourceResults?.automation.defaults.status === 'ready';

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
   * 将 version 组结果写入 versionInfo
   *
   * Business Logic（为什么需要这个函数）:
   *   关于 tab 展示应用版本；version 失败清空而非用假值。
   *
   * Code Logic（这个函数做什么）:
   *   ready → set value；error → null。
   */
  const applyVersionResult = useCallback((result: ResourceResult<VersionInfo>) => {
    if (isResourceReady(result)) {
      setVersionInfo(result.value);
    } else {
      setVersionInfo(null);
    }
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   panel 局部失败时用户点重试，只请求该分组端点。
   *
   * Code Logic（这个函数做什么）:
   *   调用 retrySettingsResource，合并该组 resourceResults，再 hydrator.applyGroup；
   *   core 组同步 loadError；version 组同步 versionInfo。
   */
  const handleRetryResourceGroup = useCallback(
    async (group: SettingsResourceGroup) => {
      setRetryingGroup(group);
      try {
        const prev = resourceResults;
        const groupResult = await retrySettingsResource(settingsResourceApi, group);
        setResourceResults((current) => {
          if (!current) return current;
          return { ...current, [group]: groupResult } as SettingsResourceResults;
        });

        const prevStatus =
          group === 'core' || group === 'defaults' || group === 'version'
            ? prev?.[group]?.status
            : group === 'cloudSync' ||
                group === 'githubTrending' ||
                group === 'health' ||
                group === 'automation'
              ? prev?.[group]?.current.status
              : undefined;
        const allowRewriteForm = prevStatus === 'error' || prevStatus === undefined;

        if (group === 'core') {
          const result = groupResult as ResourceResult<import('@/lib/types').AppConfig>;
          if (isResourceReady(result)) {
            setLoadError(null);
          } else {
            setLoadError(result.error.message || t('error.loadConfigFailed'));
          }
        }
        if (group === 'version') {
          applyVersionResult(groupResult as ResourceResult<VersionInfo>);
        }

        hydrator.applyGroupResult(group, groupResult, { allowRewriteForm });
      } finally {
        setRetryingGroup(null);
      }
    },
    [applyVersionResult, hydrator, resourceResults, settingsResourceApi, t],
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
     *   loadSettingsResources + setResourceResults + hydrator.applyResourceResults；
     *   core 失败写 loadError；version 写入 versionInfo。
     */
    async function loadConfig() {
      try {
        const results = await loadSettingsResources(settingsResourceApi);
        if (cancelled) return;
        setResourceResults(results);
        if (results.core.status === 'error') {
          setLoadError(results.core.error.message || t('error.loadConfigFailed'));
        } else {
          setLoadError(null);
        }
        applyVersionResult(results.version);
        hydrator.applyResourceResults(results);
      } catch (err) {
        if (cancelled) return;
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

  // A3：并行拉取 redacted adapter catalog（失败不阻断 automation tab）。
  useEffect(() => {
    let cancelled = false;
    void listOrchestratorAgentAdapters()
      .then((catalog) => {
        if (!cancelled) {
          setAgentAdapters(catalog.adapters ?? []);
        }
      })
      .catch(() => {
        if (!cancelled) {
          setAgentAdapters([]);
        }
      });
    return () => {
      cancelled = true;
    };
  }, []);

  return {
    loading,
    loadError,
    resourceResults,
    retryingGroup,
    handleRetryResourceGroup,
    canResetCoreDefaults: Boolean(canResetCoreDefaults),
    canResetCloudSyncDefaults: Boolean(canResetCloudSyncDefaults),
    canResetGithubTrendingDefaults: Boolean(canResetGithubTrendingDefaults),
    canResetPromptOptimizerDefaults: Boolean(canResetPromptOptimizerDefaults),
    canResetHealthDefaults: Boolean(canResetHealthDefaults),
    canResetAutomationDefaults: Boolean(canResetAutomationDefaults),
    cloudSyncLoadError,
    githubTrendingLoadError,
    healthLoadError,
    automationLoadError,
    versionLoadError,
    versionInfo,
    agentAdapters,
  };
}
