/**
 * settingsResources 单元测试
 *
 * Business Logic（为什么需要这个测试）:
 *   Settings 11 端点 allSettled 映射与分组 retry 是局部容错的正确性基础，
 *   必须保证 core 失败可识别、defaults 失败只禁 reset、retry 不碰其他组。
 *
 * Code Logic（这个测试做什么）:
 *   注入 fake SettingsResourceApi，覆盖全成功、core 失败、各组 current/defaults 失败、
 *   canResetFromPair、retry 单组调用次数与状态保留。
 */
import { describe, expect, test, vi } from 'vitest';
import { createDefaultHealthReminders } from '../../lib/healthReminders';
import type {
  AppConfig,
  CloudSyncConfig,
  GithubTrendingConfig,
  HealthConfig,
  VersionInfo,
} from '@/lib/types';
import type { OrchestratorAutomationConfig } from '@/api/orchestratorConfig';
import {
  SETTINGS_RESOURCE_ENDPOINT_ORDER,
  canResetFromPair,
  isResourceReady,
  loadSettingsResources,
  pairCurrentError,
  retrySettingsResource,
  type SettingsResourceApi,
} from './settingsResources';

/**
 * Business Logic（为什么需要这个函数）:
 *   测试夹具需要最小合法 AppConfig，避免每个断言重复无关字段。
 *
 * Code Logic（这个函数做什么）:
 *   返回带 partial 覆盖的 AppConfig。
 */
function appConfig(partial: Partial<AppConfig> = {}): AppConfig {
  return {
    deviceId: 'device-1',
    deviceName: 'Hans-Mac',
    receiveDir: '/Users/hans/cc-partner-files',
    gamePluginDir: '/Users/hans/.cc-partner/plugins',
    screenshotHotkey: '<cmd>+<shift>+s',
    promptOptimizerHotkey: '<ctrl>',
    promptOptimizerFillLanguage: 'zh',
    promptOptimizerProvider: 'claude',
    promptQuickInputHotkey: '<ctrl>+/',
    httpPort: 0,
    experimentalFeatures: {
      battery: false,
      game: false,
      browser: false,
      automation: false,
      cloudSync: false,
    },
    ...partial,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   云端同步夹具。
 *
 * Code Logic（这个函数做什么）:
 *   返回 CloudSyncConfig 默认值 + partial。
 */
function cloudSync(partial: Partial<CloudSyncConfig> = {}): CloudSyncConfig {
  return {
    repoUrl: 'git@github.com:user/repo.git',
    enabled: true,
    auto: false,
    intervalSecs: 300,
    branch: 'main',
    ...partial,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   GitHub Trending 夹具。
 *
 * Code Logic（这个函数做什么）:
 *   返回 GithubTrendingConfig 默认值 + partial。
 */
function githubTrending(partial: Partial<GithubTrendingConfig> = {}): GithubTrendingConfig {
  return {
    aiEnabled: true,
    claudeCliPath: 'claude',
    claudeModel: 'sonnet',
    cacheTtlHours: 24,
    ...partial,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   健康配置夹具。
 *
 * Code Logic（这个函数做什么）:
 *   返回 HealthConfig 默认值 + partial。
 */
function health(partial: Partial<HealthConfig> = {}): HealthConfig {
  return {
    enabled: true,
    workWindowSeconds: 1800,
    breakSeconds: 300,
    recordWindowTitle: true,
    retainDays: 14,
    notifyEnabled: true,
    dndStart: null,
    dndEnd: null,
    waterEnabled: true,
    waterIntervalSeconds: 3600,
    reminderFullscreen: true,
    ...partial,
    reminders: partial.reminders ?? createDefaultHealthReminders(),
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   自动化配置夹具。
 *
 * Code Logic（这个函数做什么）:
 *   返回 OrchestratorAutomationConfig 默认值 + partial。
 */
function automation(
  partial: Partial<OrchestratorAutomationConfig> = {},
): OrchestratorAutomationConfig {
  return {
    enabled: false,
    maxConcurrentTasks: 2,
    verificationCommands: ['npm test'],
    autoCommit: false,
    autoPushTaskBranch: false,
    autoMergeToMain: false,
    autoPushMain: false,
    notifyHumanReview: true,
    notifyBlocked: true,
    notifyRemoteOutboxFailed: true,
    notifyTaskDone: false,
    ...partial,
  };
}

const versionInfo: VersionInfo = { version: '1.2.3', buildDate: '2026-07-13' };

/**
 * Business Logic（为什么需要这个函数）:
 *   构造全成功 fake API，可按 key 覆盖拒绝或替换实现。
 *
 * Code Logic（这个函数做什么）:
 *   每个端点 vi.fn 默认 resolve 夹具；overrides 可替换。
 */
function createApi(
  overrides: Partial<SettingsResourceApi> = {},
): SettingsResourceApi {
  // 显式构造完整对象，避免 Partial spread 把各端点推断成交叉联合类型
  const api: SettingsResourceApi = {
    getConfig: vi.fn(async () => appConfig({ deviceName: 'current-device' })),
    getDefaults: vi.fn(async () => appConfig({ deviceName: 'default-device' })),
    getVersion: vi.fn(async () => versionInfo),
    getCloudSyncConfig: vi.fn(async () => cloudSync({ repoUrl: 'current-repo' })),
    getDefaultCloudSyncConfig: vi.fn(async () => cloudSync({ repoUrl: 'default-repo' })),
    getGithubTrendingConfig: vi.fn(async () => githubTrending({ claudeModel: 'current-model' })),
    getDefaultGithubTrendingConfig: vi.fn(async () =>
      githubTrending({ claudeModel: 'default-model' }),
    ),
    getHealthConfig: vi.fn(async () => health({ workWindowSeconds: 1000 })),
    getDefaultHealthConfig: vi.fn(async () => health({ workWindowSeconds: 2000 })),
    getAutomationConfig: vi.fn(async () => automation({ maxConcurrentTasks: 3 })),
    getDefaultAutomationConfig: vi.fn(async () => automation({ maxConcurrentTasks: 1 })),
  };
  if (overrides.getConfig) api.getConfig = overrides.getConfig;
  if (overrides.getDefaults) api.getDefaults = overrides.getDefaults;
  if (overrides.getVersion) api.getVersion = overrides.getVersion;
  if (overrides.getCloudSyncConfig) api.getCloudSyncConfig = overrides.getCloudSyncConfig;
  if (overrides.getDefaultCloudSyncConfig) {
    api.getDefaultCloudSyncConfig = overrides.getDefaultCloudSyncConfig;
  }
  if (overrides.getGithubTrendingConfig) {
    api.getGithubTrendingConfig = overrides.getGithubTrendingConfig;
  }
  if (overrides.getDefaultGithubTrendingConfig) {
    api.getDefaultGithubTrendingConfig = overrides.getDefaultGithubTrendingConfig;
  }
  if (overrides.getHealthConfig) api.getHealthConfig = overrides.getHealthConfig;
  if (overrides.getDefaultHealthConfig) {
    api.getDefaultHealthConfig = overrides.getDefaultHealthConfig;
  }
  if (overrides.getAutomationConfig) api.getAutomationConfig = overrides.getAutomationConfig;
  if (overrides.getDefaultAutomationConfig) {
    api.getDefaultAutomationConfig = overrides.getDefaultAutomationConfig;
  }
  return api;
}

describe('settingsResources', () => {
  test('endpoint order documents 11 stable indices', () => {
    expect(SETTINGS_RESOURCE_ENDPOINT_ORDER).toHaveLength(11);
    expect(SETTINGS_RESOURCE_ENDPOINT_ORDER[0]).toBe('core.current');
    expect(SETTINGS_RESOURCE_ENDPOINT_ORDER[10]).toBe('automation.defaults');
  });

  test('loadSettingsResources maps all 11 success values by stable index', async () => {
    const api = createApi();
    const results = await loadSettingsResources(api);

    expect(results.core).toEqual({
      status: 'ready',
      value: expect.objectContaining({ deviceName: 'current-device' }),
    });
    expect(results.defaults).toEqual({
      status: 'ready',
      value: expect.objectContaining({ deviceName: 'default-device' }),
    });
    expect(results.version).toEqual({ status: 'ready', value: versionInfo });
    expect(results.cloudSync.current).toEqual({
      status: 'ready',
      value: expect.objectContaining({ repoUrl: 'current-repo' }),
    });
    expect(results.cloudSync.defaults).toEqual({
      status: 'ready',
      value: expect.objectContaining({ repoUrl: 'default-repo' }),
    });
    expect(results.githubTrending.current).toEqual({
      status: 'ready',
      value: expect.objectContaining({ claudeModel: 'current-model' }),
    });
    expect(results.githubTrending.defaults).toEqual({
      status: 'ready',
      value: expect.objectContaining({ claudeModel: 'default-model' }),
    });
    expect(results.health.current).toEqual({
      status: 'ready',
      value: expect.objectContaining({ workWindowSeconds: 1000 }),
    });
    expect(results.health.defaults).toEqual({
      status: 'ready',
      value: expect.objectContaining({ workWindowSeconds: 2000 }),
    });
    expect(results.automation.current).toEqual({
      status: 'ready',
      value: expect.objectContaining({ maxConcurrentTasks: 3 }),
    });
    expect(results.automation.defaults).toEqual({
      status: 'ready',
      value: expect.objectContaining({ maxConcurrentTasks: 1 }),
    });

    // 全部端点各调用一次
    expect(api.getConfig).toHaveBeenCalledTimes(1);
    expect(api.getDefaults).toHaveBeenCalledTimes(1);
    expect(api.getVersion).toHaveBeenCalledTimes(1);
    expect(api.getCloudSyncConfig).toHaveBeenCalledTimes(1);
    expect(api.getDefaultCloudSyncConfig).toHaveBeenCalledTimes(1);
    expect(api.getGithubTrendingConfig).toHaveBeenCalledTimes(1);
    expect(api.getDefaultGithubTrendingConfig).toHaveBeenCalledTimes(1);
    expect(api.getHealthConfig).toHaveBeenCalledTimes(1);
    expect(api.getDefaultHealthConfig).toHaveBeenCalledTimes(1);
    expect(api.getAutomationConfig).toHaveBeenCalledTimes(1);
    expect(api.getDefaultAutomationConfig).toHaveBeenCalledTimes(1);
  });

  test('core current failure does not reject and leaves other groups ready', async () => {
    const api = createApi({
      getConfig: vi.fn(async () => {
        throw new Error('core boom');
      }),
    });
    const results = await loadSettingsResources(api);

    expect(results.core).toEqual({ status: 'error', error: expect.objectContaining({ message: 'core boom' }) });
    expect(isResourceReady(results.defaults)).toBe(true);
    expect(isResourceReady(results.version)).toBe(true);
    expect(isResourceReady(results.cloudSync.current)).toBe(true);
  });

  test('defaults failure is isolated and disables core reset', async () => {
    const api = createApi({
      getDefaults: vi.fn(async () => {
        throw new Error('defaults boom');
      }),
    });
    const results = await loadSettingsResources(api);

    expect(isResourceReady(results.core)).toBe(true);
    expect(results.defaults).toEqual({
      status: 'error',
      error: expect.objectContaining({ message: 'defaults boom' }),
    });
    // defaults 失败 → 不能用 defaults 顶替 current
    expect(results.core.status === 'ready' ? results.core.value.deviceName : null).toBe(
      'current-device',
    );
  });

  test('cloudSync current failure isolates pair and pairCurrentError is set', async () => {
    const api = createApi({
      getCloudSyncConfig: vi.fn(async () => {
        throw new Error('cloudSync current boom');
      }),
    });
    const results = await loadSettingsResources(api);
    const pair = results.cloudSync;
    expect(pair.current.status).toBe('error');
    expect(pairCurrentError(pair)?.message).toBe('cloudSync current boom');
    expect(isResourceReady(pair.defaults)).toBe(true);
    expect(pair.current.status).not.toBe('ready');
    expect(isResourceReady(results.core)).toBe(true);
    expect(api.getDefaultCloudSyncConfig).toHaveBeenCalledTimes(1);
  });

  test('githubTrending current failure isolates pair and pairCurrentError is set', async () => {
    const api = createApi({
      getGithubTrendingConfig: vi.fn(async () => {
        throw new Error('githubTrending current boom');
      }),
    });
    const results = await loadSettingsResources(api);
    const pair = results.githubTrending;
    expect(pair.current.status).toBe('error');
    expect(pairCurrentError(pair)?.message).toBe('githubTrending current boom');
    expect(isResourceReady(pair.defaults)).toBe(true);
    expect(isResourceReady(results.core)).toBe(true);
    expect(api.getDefaultGithubTrendingConfig).toHaveBeenCalledTimes(1);
  });

  test('health current failure isolates pair and pairCurrentError is set', async () => {
    const api = createApi({
      getHealthConfig: vi.fn(async () => {
        throw new Error('health current boom');
      }),
    });
    const results = await loadSettingsResources(api);
    const pair = results.health;
    expect(pair.current.status).toBe('error');
    expect(pairCurrentError(pair)?.message).toBe('health current boom');
    expect(isResourceReady(pair.defaults)).toBe(true);
    expect(isResourceReady(results.core)).toBe(true);
    expect(api.getDefaultHealthConfig).toHaveBeenCalledTimes(1);
  });

  test('automation current failure isolates pair and pairCurrentError is set', async () => {
    const api = createApi({
      getAutomationConfig: vi.fn(async () => {
        throw new Error('automation current boom');
      }),
    });
    const results = await loadSettingsResources(api);
    const pair = results.automation;
    expect(pair.current.status).toBe('error');
    expect(pairCurrentError(pair)?.message).toBe('automation current boom');
    expect(isResourceReady(pair.defaults)).toBe(true);
    expect(isResourceReady(results.core)).toBe(true);
    expect(api.getDefaultAutomationConfig).toHaveBeenCalledTimes(1);
  });

  test('cloudSync defaults failure keeps current ready and disables reset', async () => {
    const api = createApi({
      getDefaultCloudSyncConfig: vi.fn(async () => {
        throw new Error('cloudSync defaults boom');
      }),
    });
    const results = await loadSettingsResources(api);
    const pair = results.cloudSync;
    expect(isResourceReady(pair.current)).toBe(true);
    expect(pair.defaults.status).toBe('error');
    expect(canResetFromPair(pair)).toBe(false);
    expect(pairCurrentError(pair)).toBeNull();
  });

  test('githubTrending defaults failure keeps current ready and disables reset', async () => {
    const api = createApi({
      getDefaultGithubTrendingConfig: vi.fn(async () => {
        throw new Error('githubTrending defaults boom');
      }),
    });
    const results = await loadSettingsResources(api);
    const pair = results.githubTrending;
    expect(isResourceReady(pair.current)).toBe(true);
    expect(pair.defaults.status).toBe('error');
    expect(canResetFromPair(pair)).toBe(false);
    expect(pairCurrentError(pair)).toBeNull();
  });

  test('health defaults failure keeps current ready and disables reset', async () => {
    const api = createApi({
      getDefaultHealthConfig: vi.fn(async () => {
        throw new Error('health defaults boom');
      }),
    });
    const results = await loadSettingsResources(api);
    const pair = results.health;
    expect(isResourceReady(pair.current)).toBe(true);
    expect(pair.defaults.status).toBe('error');
    expect(canResetFromPair(pair)).toBe(false);
    expect(pairCurrentError(pair)).toBeNull();
  });

  test('automation defaults failure keeps current ready and disables reset', async () => {
    const api = createApi({
      getDefaultAutomationConfig: vi.fn(async () => {
        throw new Error('automation defaults boom');
      }),
    });
    const results = await loadSettingsResources(api);
    const pair = results.automation;
    expect(isResourceReady(pair.current)).toBe(true);
    expect(pair.defaults.status).toBe('error');
    expect(canResetFromPair(pair)).toBe(false);
    expect(pairCurrentError(pair)).toBeNull();
  });

  test('canResetFromPair is true only when defaults ready', () => {
    expect(
      canResetFromPair({
        current: { status: 'ready', value: cloudSync() },
        defaults: { status: 'ready', value: cloudSync() },
      }),
    ).toBe(true);
    expect(
      canResetFromPair({
        current: { status: 'ready', value: cloudSync() },
        defaults: { status: 'error', error: new Error('x') },
      }),
    ).toBe(false);
  });

  test('retry one group invokes only its endpoints and preserves other state', async () => {
    const api = createApi({
      getCloudSyncConfig: vi
        .fn()
        .mockRejectedValueOnce(new Error('cloud first fail'))
        .mockResolvedValueOnce(cloudSync({ repoUrl: 'retried-repo' })),
      getDefaultCloudSyncConfig: vi
        .fn()
        .mockRejectedValueOnce(new Error('cloud defaults first fail'))
        .mockResolvedValueOnce(cloudSync({ repoUrl: 'retried-default-repo' })),
    });

    const initial = await loadSettingsResources(api);
    expect(pairCurrentError(initial.cloudSync)?.message).toBe('cloud first fail');
    expect(canResetFromPair(initial.cloudSync)).toBe(false);

    // 记录其他组成功快照
    const coreSnapshot = initial.core;
    const healthSnapshot = initial.health;

    // 清零调用计数以便断言 retry 只打 cloudSync 两个端点
    vi.mocked(api.getConfig).mockClear();
    vi.mocked(api.getDefaults).mockClear();
    vi.mocked(api.getVersion).mockClear();
    vi.mocked(api.getGithubTrendingConfig).mockClear();
    vi.mocked(api.getDefaultGithubTrendingConfig).mockClear();
    vi.mocked(api.getHealthConfig).mockClear();
    vi.mocked(api.getDefaultHealthConfig).mockClear();
    vi.mocked(api.getAutomationConfig).mockClear();
    vi.mocked(api.getDefaultAutomationConfig).mockClear();
    vi.mocked(api.getCloudSyncConfig).mockClear();
    vi.mocked(api.getDefaultCloudSyncConfig).mockClear();

    const retried = await retrySettingsResource(api, 'cloudSync');
    expect(retried).toEqual({
      current: {
        status: 'ready',
        value: expect.objectContaining({ repoUrl: 'retried-repo' }),
      },
      defaults: {
        status: 'ready',
        value: expect.objectContaining({ repoUrl: 'retried-default-repo' }),
      },
    });

    expect(api.getCloudSyncConfig).toHaveBeenCalledTimes(1);
    expect(api.getDefaultCloudSyncConfig).toHaveBeenCalledTimes(1);
    expect(api.getConfig).not.toHaveBeenCalled();
    expect(api.getDefaults).not.toHaveBeenCalled();
    expect(api.getVersion).not.toHaveBeenCalled();
    expect(api.getGithubTrendingConfig).not.toHaveBeenCalled();
    expect(api.getHealthConfig).not.toHaveBeenCalled();
    expect(api.getAutomationConfig).not.toHaveBeenCalled();

    // 其他组状态由调用方保留；本函数不改写 initial
    expect(coreSnapshot).toEqual(initial.core);
    expect(healthSnapshot).toEqual(initial.health);
  });

  test('retry core only calls getConfig', async () => {
    const api = createApi({
      getConfig: vi
        .fn()
        .mockRejectedValueOnce(new Error('core fail'))
        .mockResolvedValueOnce(appConfig({ deviceName: 'after-retry' })),
    });
    await loadSettingsResources(api);
    vi.mocked(api.getConfig).mockClear();
    vi.mocked(api.getDefaults).mockClear();
    vi.mocked(api.getVersion).mockClear();

    const retried = await retrySettingsResource(api, 'core');
    expect(retried).toEqual({
      status: 'ready',
      value: expect.objectContaining({ deviceName: 'after-retry' }),
    });
    expect(api.getConfig).toHaveBeenCalledTimes(1);
    expect(api.getDefaults).not.toHaveBeenCalled();
    expect(api.getVersion).not.toHaveBeenCalled();
  });

  test('version failure is isolated', async () => {
    const api = createApi({
      getVersion: vi.fn(async () => {
        throw new Error('version boom');
      }),
    });
    const results = await loadSettingsResources(api);
    expect(results.version).toEqual({
      status: 'error',
      error: expect.objectContaining({ message: 'version boom' }),
    });
    expect(isResourceReady(results.core)).toBe(true);
  });
});
