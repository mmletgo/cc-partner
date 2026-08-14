// @vitest-environment jsdom
/**
 * useSettingsResources characterization 测试
 *
 * Business Logic（为什么需要这个测试文件）:
 *   资源加载拆 hook 后必须锁定：局部失败不拖垮其它组、retry 只打一组、core 错误→loadError。
 *
 * Code Logic（这个测试文件做什么）:
 *   mock loadSettingsResources/retrySettingsResource 与 hydrator；renderHook 断言合同。
 */
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { act, cleanup, renderHook, waitFor } from '@testing-library/react';
import type { SettingsResourceResults } from '../settingsResources';

const loadSettingsResourcesMock = vi.fn();
const retrySettingsResourceMock = vi.fn();
const createSettingsResourceApiMock = vi.fn(() => ({ api: true }));

vi.mock('../settingsResources', async () => {
  const actual = await vi.importActual<typeof import('../settingsResources')>('../settingsResources');
  return {
    ...actual,
    loadSettingsResources: (...args: unknown[]) => loadSettingsResourcesMock(...args),
    retrySettingsResource: (...args: unknown[]) => retrySettingsResourceMock(...args),
    createSettingsResourceApi: () => createSettingsResourceApiMock(),
  };
});

vi.mock('react-i18next', () => ({
  useTranslation: () => ({
    t: (key: string) => key,
  }),
}));

vi.mock('@/api/config', () => ({ configApi: {} }));
vi.mock('@/api/health', () => ({ healthApi: {} }));
vi.mock('@/api/githubTrending', () => ({ githubTrendingApi: {} }));
vi.mock('@/api/orchestratorConfig', () => ({ orchestratorConfigApi: {} }));

import { useSettingsResources } from './useSettingsResources';

function readyPair<T>(value: T) {
  return {
    current: { status: 'ready' as const, value },
    defaults: { status: 'ready' as const, value },
  };
}

function buildResults(overrides: Partial<SettingsResourceResults> = {}): SettingsResourceResults {
  const coreValue = {
    deviceId: 'd1',
    deviceName: 'Mac',
    receiveDir: '/tmp',
    gamePluginDir: '/tmp/plugins',
    screenshotHotkey: 'CommandOrControl+Shift+S',
    promptOptimizerHotkey: 'Control',
    promptOptimizerFillLanguage: 'zh' as const,
  };
  return {
    core: { status: 'ready', value: coreValue as never },
    defaults: { status: 'ready', value: coreValue as never },
    version: { status: 'ready', value: { version: '1.0.0', tauriVersion: '2' } as never },
    cloudSync: readyPair({
      enabled: false,
      repoUrl: '',
      branch: 'main',
      authMode: 'https' as const,
      token: '',
      sshKeyPath: '',
    }) as never,
    githubTrending: readyPair({
      aiEnabled: true,
      claudeCliPath: 'claude',
      claudeModel: 'sonnet',
      cacheTtlHours: 24,
    }) as never,
    health: readyPair({
      enabled: true,
      workWindowSeconds: 3600,
      breakSeconds: 300,
      waterIntervalMinutes: 60,
      retentionDays: 30,
      dndStart: '',
      dndEnd: '',
    }) as never,
    automation: readyPair({
      enabled: true,
      maxConcurrent: 1,
      verificationCommands: [],
      autoCommit: false,
      autoPush: false,
      autoMerge: false,
      autoPushMain: false,
      notifyHumanReview: true,
      notifyBlocked: true,
      notifyRemoteOutboxFailed: true,
      notifyTaskDone: false,
    }) as never,
    ...overrides,
  };
}

describe('useSettingsResources', () => {
  beforeEach(() => {
    loadSettingsResourcesMock.mockReset();
    retrySettingsResourceMock.mockReset();
  });

  afterEach(() => {
    cleanup();
  });

  test('局部失败保留其它组 ready，且 core 错误写入 loadError', async () => {
    const applyAll = vi.fn();
    const applyGroup = vi.fn();
    const results = buildResults({
      core: { status: 'error', error: new Error('core down') },
      cloudSync: {
        current: { status: 'error', error: new Error('cloud down') },
        defaults: { status: 'ready', value: { enabled: false } as never },
      },
    });
    loadSettingsResourcesMock.mockResolvedValue(results);

    const { result } = renderHook(() =>
      useSettingsResources({
        hydrator: { applyResourceResults: applyAll, applyGroupResult: applyGroup },
      }),
    );

    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.loadError).toBe('core down');
    expect(result.current.resourceResults?.cloudSync.current.status).toBe('error');
    expect(result.current.resourceResults?.version.status).toBe('ready');
    expect(result.current.cloudSyncLoadError?.message).toBe('cloud down');
    expect(applyAll).toHaveBeenCalledWith(results);
  });

  test('retry 只请求一组并允许失败组 rewrite form', async () => {
    const applyAll = vi.fn();
    const applyGroup = vi.fn();
    const initial = buildResults({
      health: {
        current: { status: 'error', error: new Error('health fail') },
        defaults: { status: 'error', error: new Error('health defaults fail') },
      },
    });
    loadSettingsResourcesMock.mockResolvedValue(initial);
    const healed = {
      current: {
        status: 'ready' as const,
        value: {
          enabled: true,
          workWindowSeconds: 3600,
          breakSeconds: 300,
          waterIntervalMinutes: 60,
          retentionDays: 30,
          dndStart: '',
          dndEnd: '',
        },
      },
      defaults: {
        status: 'ready' as const,
        value: {
          enabled: true,
          workWindowSeconds: 3600,
          breakSeconds: 300,
          waterIntervalMinutes: 60,
          retentionDays: 30,
          dndStart: '',
          dndEnd: '',
        },
      },
    };
    retrySettingsResourceMock.mockResolvedValue(healed);

    const { result } = renderHook(() =>
      useSettingsResources({
        hydrator: { applyResourceResults: applyAll, applyGroupResult: applyGroup },
      }),
    );
    await waitFor(() => expect(result.current.loading).toBe(false));

    await act(async () => {
      await result.current.handleRetryResourceGroup('health');
    });

    expect(retrySettingsResourceMock).toHaveBeenCalledTimes(1);
    expect(retrySettingsResourceMock.mock.calls[0][1]).toBe('health');
    expect(result.current.resourceResults?.health.current.status).toBe('ready');
    expect(result.current.resourceResults?.cloudSync.current.status).toBe('ready');
    expect(applyGroup).toHaveBeenCalledWith('health', healed, { allowRewriteForm: true });
  });
});
