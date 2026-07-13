// @vitest-environment jsdom
/**
 * Settings 页面局部资源容错集成测试
 *
 * Business Logic（为什么需要这个测试）:
 *   core 成功时壳层必须可用；业务组失败不得 Promise.all 整页失败；defaults 失败禁用 reset。
 *
 * Code Logic（这个测试做什么）:
 *   mock 各 API 与 router/i18n，渲染 Settings，断言局部错误与重试入口。
 */
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import type { AppConfig, CloudSyncConfig, GithubTrendingConfig, HealthConfig, VersionInfo } from '@/lib/types';
import type { OrchestratorAutomationConfig } from '@/api/orchestratorConfig';

const appConfig = (partial: Partial<AppConfig> = {}): AppConfig => ({
  deviceId: 'device-1',
  deviceName: 'Hans-Mac',
  receiveDir: '/tmp/files',
  screenshotHotkey: '<cmd>+<shift>+s',
  promptOptimizerHotkey: '<ctrl>',
  promptOptimizerFillLanguage: 'zh',
  httpPort: 0,
  ...partial,
});

const cloudSync = (partial: Partial<CloudSyncConfig> = {}): CloudSyncConfig => ({
  repoUrl: 'git@github.com:u/r.git',
  enabled: true,
  auto: false,
  intervalSecs: 300,
  branch: 'main',
  ...partial,
});

const githubTrending = (partial: Partial<GithubTrendingConfig> = {}): GithubTrendingConfig => ({
  aiEnabled: true,
  claudeCliPath: 'claude',
  claudeModel: 'sonnet',
  cacheTtlHours: 24,
  ...partial,
});

const health = (partial: Partial<HealthConfig> = {}): HealthConfig => ({
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
});

const automation = (
  partial: Partial<OrchestratorAutomationConfig> = {},
): OrchestratorAutomationConfig => ({
  enabled: false,
  maxConcurrentTasks: 2,
  verificationCommands: ['npm test'],
  autoCommit: false,
  autoPushTaskBranch: false,
  autoMergeToMain: false,
  autoPushMain: false,
  ...partial,
});

const versionInfo: VersionInfo = { version: '1.0.0', buildDate: '2026-07-13' };

const getConfig = vi.fn(async () => appConfig());
const getDefaults = vi.fn(async () => appConfig({ deviceName: 'default' }));
const getVersion = vi.fn(async () => versionInfo);
const getCloudSyncConfig = vi.fn(async () => cloudSync());
const getDefaultCloudSyncConfig = vi.fn(async () => cloudSync({ repoUrl: null }));
const getGithubTrendingConfig = vi.fn(async () => githubTrending());
const getDefaultGithubTrendingConfig = vi.fn(async () => githubTrending());
const getHealthConfig = vi.fn(async () => health());
const getDefaultHealthConfig = vi.fn(async () => health());
const getAutomationConfig = vi.fn(async () => automation());
const getDefaultAutomationConfig = vi.fn(async () => automation());
const getDownloadStatus = vi.fn(async () => ({
  status: 'idle' as const,
  progress: 0,
  error: '',
  filePath: '',
  url: '',
  filename: '',
  size: 0,
}));

vi.mock('@/api/config', () => ({
  configApi: {
    get: () => getConfig(),
    getDefaults: () => getDefaults(),
    version: () => getVersion(),
    getCloudSyncConfig: () => getCloudSyncConfig(),
    getDefaultCloudSyncConfig: () => getDefaultCloudSyncConfig(),
    update: vi.fn(),
    chooseDir: vi.fn(),
    checkUpdate: vi.fn(),
    downloadUpdate: vi.fn(),
    getDownloadStatus: () => getDownloadStatus(),
    cancelDownload: vi.fn(),
    installUpdate: vi.fn(),
    permissions: vi.fn(),
    requestPermission: vi.fn(),
    updateCloudSyncConfig: vi.fn(),
    triggerCloudSync: vi.fn(),
    testCloudSync: vi.fn(),
  },
}));

vi.mock('@/api/githubTrending', () => ({
  githubTrendingApi: {
    getConfig: () => getGithubTrendingConfig(),
    getDefaultConfig: () => getDefaultGithubTrendingConfig(),
    updateConfig: vi.fn(),
    testClaudeCli: vi.fn(),
    list: vi.fn(),
  },
}));

vi.mock('@/api/health', () => ({
  healthApi: {
    getConfig: () => getHealthConfig(),
    getDefaultConfig: () => getDefaultHealthConfig(),
    updateConfig: vi.fn(),
  },
}));

vi.mock('@/api/orchestratorConfig', () => ({
  orchestratorConfigApi: {
    get: () => getAutomationConfig(),
    getDefaults: () => getDefaultAutomationConfig(),
    update: vi.fn(),
  },
}));

vi.mock('@/hooks/usePermissions', () => ({
  usePermissions: () => ({
    status: {
      screenCapture: true,
      accessibility: true,
      inputMonitoring: true,
      notification: true,
    },
    loading: false,
    refresh: vi.fn(),
  }),
}));

vi.mock('@/lib/notification', () => ({
  requestNotificationPermission: vi.fn(),
}));

vi.mock('@/lib/permissionEntries', () => ({
  mapPermissions: () => [],
}));

vi.mock('@/components/domain', () => ({
  LanFirewallDependencyCard: () => <div data-testid="lan-card" />,
  PermissionCard: () => <div data-testid="perm-card" />,
  WorkbenchDependencyCard: () => <div data-testid="wb-card" />,
}));

vi.mock('react-i18next', () => ({
  useTranslation: () => {
    const t = (key: string, opts?: Record<string, unknown>) => {
      if (opts && 'error' in (opts ?? {})) return `${key}:${String(opts.error)}`;
      if (opts && 'time' in (opts ?? {})) return `${key}:${String(opts.time)}`;
      return key;
    };
    // 支持 const { t } = useTranslation(...) 与 const [t] = useTranslation(...)
    const result = Object.assign([t, {}, false], {
      t,
      i18n: { language: 'zh' },
      ready: true,
    });
    return result;
  },
}));

const searchParamsState = { value: new URLSearchParams() };
const setSearchParamsMock = vi.fn((update: unknown) => {
  if (typeof update === 'function') {
    const next = (update as (prev: URLSearchParams) => URLSearchParams)(searchParamsState.value);
    searchParamsState.value = next;
  } else if (update instanceof URLSearchParams) {
    searchParamsState.value = update;
  }
});

vi.mock('react-router-dom', async () => {
  const actual = await vi.importActual<typeof import('react-router-dom')>('react-router-dom');
  return {
    ...actual,
    useSearchParams: () => [searchParamsState.value, setSearchParamsMock],
  };
});

import { Settings } from './Settings';

/**
 * Business Logic（为什么需要这个函数）:
 *   Settings 测试需要稳定的挂载/卸载边界。
 *
 * Code Logic（这个函数做什么）:
 *   直接 render Settings。
 */
function renderSettings(): ReturnType<typeof render> {
  return render(<Settings />);
}

beforeEach(() => {
  vi.clearAllMocks();
  searchParamsState.value = new URLSearchParams();
  getConfig.mockResolvedValue(appConfig());
  getDefaults.mockResolvedValue(appConfig({ deviceName: 'default' }));
  getVersion.mockResolvedValue(versionInfo);
  getCloudSyncConfig.mockResolvedValue(cloudSync());
  getDefaultCloudSyncConfig.mockResolvedValue(cloudSync({ repoUrl: null }));
  getGithubTrendingConfig.mockResolvedValue(githubTrending());
  getDefaultGithubTrendingConfig.mockResolvedValue(githubTrending());
  getHealthConfig.mockResolvedValue(health());
  getDefaultHealthConfig.mockResolvedValue(health());
  getAutomationConfig.mockResolvedValue(automation());
  getDefaultAutomationConfig.mockResolvedValue(automation());
});

afterEach(() => {
  cleanup();
});

describe('Settings partial resource loading', () => {
  test('core success keeps shell when optional group fails', async () => {
    getCloudSyncConfig.mockRejectedValue(new Error('cloud down'));
    renderSettings();

    await waitFor(() => {
      expect(screen.getByRole('tablist')).toBeTruthy();
    });
    // 壳层 tab 可用
    expect(screen.getByRole('tab', { name: 'settings:tabs.general' })).toBeTruthy();
    expect(screen.queryByText(/settings:loadFailed/)).toBeNull();
  });

  test('core failure shows page error with retry', async () => {
    getConfig.mockRejectedValue(new Error('core down'));
    renderSettings();

    await waitFor(() => {
      expect(screen.getByText(/settings:loadFailed:core down/)).toBeTruthy();
    });
    expect(screen.getByRole('button', { name: 'settings:resource.retry' })).toBeTruthy();
    expect(screen.queryByRole('tablist')).toBeNull();
  });

  test('defaults failure disables general reset but keeps shell', async () => {
    getDefaults.mockRejectedValue(new Error('defaults down'));
    renderSettings();

    await waitFor(() => {
      expect(screen.getByRole('tablist')).toBeTruthy();
    });
    // 等待加载结束（reset 出现在 general footer）
    const reset = await screen.findByRole('button', { name: 'settings:action.resetDefault' });
    await waitFor(() => {
      expect((reset as HTMLButtonElement).disabled).toBe(true);
      expect(reset.getAttribute('title')).toBe('settings:resource.defaultsUnavailable');
    });
  });

  test('version failure does not block shell', async () => {
    getVersion.mockRejectedValue(new Error('version down'));
    searchParamsState.value = new URLSearchParams('tab=about');
    renderSettings();

    await waitFor(() => {
      expect(screen.getByRole('tablist')).toBeTruthy();
    });
    await waitFor(() => {
      expect(
        screen.getByText(/settings:resource.versionLoadFailed:version down/),
      ).toBeTruthy();
    });
  });
});
