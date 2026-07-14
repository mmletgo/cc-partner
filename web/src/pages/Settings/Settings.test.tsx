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
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import type { AppConfig, CloudSyncConfig, GithubTrendingConfig, HealthConfig, VersionInfo } from '@/lib/types';
import type { OrchestratorAutomationConfig } from '@/api/orchestratorConfig';
import {
  findForbiddenDiagnosticsKeys,
  formatDiagnosticsForCopy,
} from '@/api/runtimeDiagnostics';

/**
 * Business Logic（为什么需要这个函数）:
 *   安全保存回归需要手动控制 mutation 响应时机。
 *
 * Code Logic（这个函数做什么）:
 *   返回 promise 与 resolve/reject 控制器。
 */
function deferred<T>(): {
  promise: Promise<T>;
  resolve: (value: T | PromiseLike<T>) => void;
  reject: (reason?: unknown) => void;
} {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

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
const updateConfig = vi.fn();
const updateCloudSyncConfig = vi.fn();
const updateGithubTrendingConfig = vi.fn();
const updateHealthConfig = vi.fn();
const updateAutomationConfig = vi.fn();
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
    update: (...args: unknown[]) => updateConfig(...args),
    chooseDir: vi.fn(),
    checkUpdate: vi.fn(),
    downloadUpdate: vi.fn(),
    getDownloadStatus: () => getDownloadStatus(),
    cancelDownload: vi.fn(),
    installUpdate: vi.fn(),
    permissions: vi.fn(),
    requestPermission: vi.fn(),
    updateCloudSyncConfig: (...args: unknown[]) => updateCloudSyncConfig(...args),
    triggerCloudSync: vi.fn(),
    testCloudSync: vi.fn(),
  },
}));

vi.mock('@/api/githubTrending', () => ({
  githubTrendingApi: {
    getConfig: () => getGithubTrendingConfig(),
    getDefaultConfig: () => getDefaultGithubTrendingConfig(),
    updateConfig: (...args: unknown[]) => updateGithubTrendingConfig(...args),
    testClaudeCli: vi.fn(),
    list: vi.fn(),
  },
}));

const {
  triggerLanSync,
  createBackup,
  inspectBackup,
  restoreBackup,
  listJobs,
  rollbackJob,
  pickExport,
  pickArchive,
} = vi.hoisted(() => {
  // 稳定空数组引用：避免 effect 每次 setState(new []) 触发无限重渲染
  const EMPTY_RECOVERY_JOBS: Array<{
    id: string;
    status: string;
    archivePath?: string | null;
    preRestoreBackupPath?: string | null;
    selectedDomainsJson: string;
    mode: string;
    errorSummary?: string | null;
    createdAt: string;
    updatedAt: string;
  }> = [];
  return {
  triggerLanSync: vi.fn(async () => ({
    accepted: true,
    succeeded_devices: 0,
    synced: 0,
    note: 'partial',
    devices: [
      {
        device_id: 'peer-1',
        device_name: 'Peer One',
        status: 'partial',
        domains: [
          {
            domain: 'prompt',
            outcome: { kind: 'succeeded', pulled: 1, pushed: 0, unchanged: 0 },
          },
          {
            domain: 'ssh_target',
            outcome: { kind: 'succeeded', pulled: 0, pushed: 0, unchanged: 1 },
          },
          {
            domain: 'scratchpad',
            outcome: { kind: 'unreachable', class: 'network' },
          },
        ],
      },
      {
        device_id: 'peer-2',
        device_name: 'Peer Two',
        status: 'unreachable',
        domains: [
          {
            domain: 'prompt',
            outcome: { kind: 'unreachable', class: 'timeout' },
          },
          {
            domain: 'ssh_target',
            outcome: { kind: 'unreachable', class: 'timeout' },
          },
          {
            domain: 'scratchpad',
            outcome: { kind: 'unreachable', class: 'timeout' },
          },
        ],
      },
    ],
  })),
  createBackup: vi.fn(async () => ({ path: '/tmp/export.zip', formatVersion: 1 })),
  inspectBackup: vi.fn(async () => ({
    formatVersion: 1,
    domainCounts: { prompts: 2, scratchpad: 1 },
    warnings: [],
    conflictsEstimate: 0,
  })),
  restoreBackup: vi.fn(async () => ({
    jobId: 'job-1',
    status: 'succeeded',
    appliedDomains: ['prompts'],
    preRestoreBackupPath: '/tmp/pre.zip',
    errorSummary: null,
  })),
  listJobs: vi.fn(async () => EMPTY_RECOVERY_JOBS),
  rollbackJob: vi.fn(async () => ({
    jobId: 'job-1',
    status: 'succeeded',
    appliedDomains: ['prompts'],
  })),
  pickExport: vi.fn(async () => '/tmp/export.zip'),
  pickArchive: vi.fn(async () => '/tmp/restore.zip'),
  };
});

vi.mock('@/api/sync', async () => {
  const actual = await vi.importActual<typeof import('@/api/sync')>('@/api/sync');
  return {
    ...actual,
    syncApi: {
      trigger: () => triggerLanSync(),
    },
    backupApi: {
      create: ((...args: Parameters<typeof createBackup>) =>
        createBackup(...args)) as typeof createBackup,
      inspect: ((...args: Parameters<typeof inspectBackup>) =>
        inspectBackup(...args)) as typeof inspectBackup,
      restore: ((...args: Parameters<typeof restoreBackup>) =>
        restoreBackup(...args)) as typeof restoreBackup,
      listJobs: ((...args: Parameters<typeof listJobs>) =>
        listJobs(...args)) as typeof listJobs,
      listBackups: vi.fn(async () => []),
      rollback: ((...args: Parameters<typeof rollbackJob>) =>
        rollbackJob(...args)) as typeof rollbackJob,
    },
    pickBackupExportPath: () => pickExport(),
    pickBackupArchivePath: () => pickArchive(),
  };
});

vi.mock('@/api/health', () => ({
  healthApi: {
    getConfig: () => getHealthConfig(),
    getDefaultConfig: () => getDefaultHealthConfig(),
    updateConfig: (...args: unknown[]) => updateHealthConfig(...args),
  },
}));

vi.mock('@/api/orchestratorConfig', () => ({
  orchestratorConfigApi: {
    get: () => getAutomationConfig(),
    getDefaults: () => getDefaultAutomationConfig(),
    update: (...args: unknown[]) => updateAutomationConfig(...args),
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
    refreshing: false,
    error: null,
    requesting: new Set(),
    request: vi.fn(async () => undefined),
    refresh: vi.fn(async () => undefined),
    allRequiredGranted: true,
    allGranted: true,
  }),
}));

vi.mock('@/lib/notification', () => ({
  requestNotificationPermission: vi.fn(),
}));

vi.mock('@/lib/permissionEntries', () => ({
  mapPermissions: () => [],
}));

const {
  runtimeDiagnosticsFixture,
  getRuntimeDiagnostics,
  openBackendLogDir,
} = vi.hoisted(() => {
  const runtimeDiagnosticsFixture = {
    ownerInstanceId: 'owner-test',
    generation: 3,
    startedAt: '2026-07-14T00:00:00Z',
    configFingerprint: 'fp-test',
    cloudSyncPhase: 'idle',
    terminalSessionCount: 1,
    bridgeCount: 0,
    bridges: [] as Array<{ phase: string; attempt: number; lastErrorClass?: string | null }>,
    orchestrator: {
      latestTickAt: null as string | null,
      latestErrorClass: null as string | null,
    },
  };
  return {
    runtimeDiagnosticsFixture,
    getRuntimeDiagnostics: vi.fn(async () => runtimeDiagnosticsFixture),
    openBackendLogDir: vi.fn(async () => undefined),
  };
});

vi.mock('@/api/runtimeDiagnostics', async () => {
  const actual = await vi.importActual<typeof import('@/api/runtimeDiagnostics')>(
    '@/api/runtimeDiagnostics',
  );
  return {
    ...actual,
    runtimeDiagnosticsApi: {
      get: () => getRuntimeDiagnostics(),
      openLogDir: () => openBackendLogDir(),
    },
  };
});

vi.mock('@/components/domain', () => {
  // 轻量 stub：避免 importActual 拉起真实 RuntimeDiagnosticsCard 依赖链。
  // 诊断复制敏感字段合同由本文件下方纯函数断言 + 此 stub 的 data-testid 覆盖。
  const RuntimeDiagnosticsCardStub = () => (
    <div data-testid="runtime-diagnostics-card">
      <span data-testid="runtime-owner">owner-test</span>
      <button
        type="button"
        data-testid="runtime-copy-diagnostics"
        onClick={() => {
          const text = JSON.stringify(runtimeDiagnosticsFixture);
          void navigator.clipboard.writeText(text);
        }}
      >
        copy
      </button>
    </div>
  );
  return {
    LanFirewallDependencyCard: () => <div data-testid="lan-card" />,
    PermissionCard: () => <div data-testid="perm-card" />,
    WorkbenchDependencyCard: () => <div data-testid="wb-card" />,
    RuntimeDiagnosticsCard: RuntimeDiagnosticsCardStub,
  };
});

// 稳定 t 引用：真实 i18next 的 t 跨 render 稳定；不稳定 mock 会让依赖 [t] 的
// useCallback/useEffect（如 recovery jobs 刷新）每次 render 重建 → 无限 setState 循环，
// 最终让 act()/safe-save 测试卡死到 5s timeout。
const stableT = vi.hoisted(() => {
  const t = (key: string, opts?: Record<string, unknown>) => {
    if (opts && 'error' in (opts ?? {})) return `${key}:${String(opts.error)}`;
    if (opts && 'time' in (opts ?? {})) return `${key}:${String(opts.time)}`;
    return key;
  };
  // 支持 const { t } = useTranslation(...) 与 const [t] = useTranslation(...)
  return Object.assign([t, {}, false], {
    t,
    i18n: { language: 'zh' },
    ready: true,
  });
});

vi.mock('react-i18next', () => ({
  useTranslation: () => stableT,
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
  updateConfig.mockReset();
  updateCloudSyncConfig.mockReset();
  updateGithubTrendingConfig.mockReset();
  updateHealthConfig.mockReset();
  updateAutomationConfig.mockReset();
  getRuntimeDiagnostics.mockResolvedValue(runtimeDiagnosticsFixture);
  openBackendLogDir.mockResolvedValue(undefined);
  Object.assign(navigator, {
    clipboard: {
      writeText: vi.fn(async () => undefined),
    },
  });
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

  test('dependencies tab shows runtime diagnostics and copy has no sensitive fields', async () => {
    searchParamsState.value = new URLSearchParams('tab=dependencies');
    renderSettings();

    await waitFor(() => {
      expect(screen.getByTestId('runtime-diagnostics-card')).toBeTruthy();
    });
    await waitFor(() => {
      expect(screen.getByTestId('runtime-owner').textContent).toBe('owner-test');
    });

    const copyText = formatDiagnosticsForCopy(runtimeDiagnosticsFixture);
    expect(findForbiddenDiagnosticsKeys(copyText)).toEqual([]);

    fireEvent.click(screen.getByTestId('runtime-copy-diagnostics'));
    await waitFor(() => {
      expect(navigator.clipboard.writeText).toHaveBeenCalled();
    });
    const written = (navigator.clipboard.writeText as ReturnType<typeof vi.fn>).mock
      .calls[0]?.[0] as string;
    expect(findForbiddenDiagnosticsKeys(written)).toEqual([]);
    expect(written).toContain('ownerInstanceId');
    expect(written).not.toMatch(/token|content|prompt|password/i);
  });


  test('partial and unreachable never display as success on sync tab', async () => {
    searchParamsState.value = new URLSearchParams('tab=sync');
    renderSettings();

    await waitFor(() => {
      expect(screen.getByTestId('lan-sync-now')).toBeTruthy();
    });

    fireEvent.click(screen.getByTestId('lan-sync-now'));

    await waitFor(() => {
      expect(screen.getByTestId('lan-sync-result')).toBeTruthy();
    });

    const partialDevice = screen.getByTestId('lan-sync-device-peer-1');
    const unreachableDevice = screen.getByTestId('lan-sync-device-peer-2');
    expect(partialDevice.getAttribute('data-status')).toBe('partial');
    expect(unreachableDevice.getAttribute('data-status')).toBe('unreachable');

    // status pill text must not be the success label for partial/unreachable
    const partialStatus = screen.getByTestId('lan-sync-device-status-peer-1');
    const unreachableStatus = screen.getByTestId('lan-sync-device-status-peer-2');
    expect(partialStatus.textContent).not.toMatch(/deviceStatus\.succeeded/);
    expect(unreachableStatus.textContent).not.toMatch(/deviceStatus\.succeeded/);
    expect(partialStatus.textContent).toMatch(/partial/);
    expect(unreachableStatus.textContent).toMatch(/unreachable/);

    const scratch = screen.getByTestId('lan-sync-domain-peer-1-scratchpad');
    expect(scratch.getAttribute('data-kind')).toBe('unreachable');
  });

  test('backup export calls create with picked path and shows success', async () => {
    searchParamsState.value = new URLSearchParams('tab=sync');
    renderSettings();

    await waitFor(() => {
      expect(screen.getByTestId('backup-export')).toBeTruthy();
    });

    fireEvent.click(screen.getByTestId('backup-export'));

    await waitFor(() => {
      expect(pickExport).toHaveBeenCalled();
      expect(createBackup).toHaveBeenCalledWith('/tmp/export.zip');
    });
    await waitFor(() => {
      expect(screen.getByTestId('backup-export-success')).toBeTruthy();
    });
  });

  test('backup restore pick inspects archive and shows domain checkboxes', async () => {
    searchParamsState.value = new URLSearchParams('tab=sync');
    renderSettings();

    await waitFor(() => {
      expect(screen.getByTestId('backup-restore-pick')).toBeTruthy();
    });

    fireEvent.click(screen.getByTestId('backup-restore-pick'));

    await waitFor(() => {
      expect(pickArchive).toHaveBeenCalled();
      expect(inspectBackup).toHaveBeenCalledWith('/tmp/restore.zip');
    });
    await waitFor(() => {
      expect(screen.getByTestId('backup-inspect-preview')).toBeTruthy();
      expect(screen.getByTestId('backup-domain-prompts')).toBeTruthy();
      expect(screen.getByTestId('backup-restore-confirm')).toBeTruthy();
    });
  });
});

describe('Settings safe save preserves concurrent edits', () => {
  test('keeps edits typed while general settings save is pending', async () => {
    const save = deferred<AppConfig>();
    updateConfig.mockReturnValue(save.promise);
    renderSettings();

    const deviceName = (await screen.findByLabelText(
      'settings:basic.deviceName',
    )) as HTMLInputElement;
    fireEvent.change(deviceName, { target: { value: 'A' } });
    fireEvent.click(screen.getByRole('button', { name: 'settings:action.apply' }));

    await waitFor(() => {
      expect(updateConfig).toHaveBeenCalledTimes(1);
    });

    fireEvent.change(deviceName, { target: { value: 'AB' } });
    expect(deviceName.value).toBe('AB');

    await act(async () => {
      save.resolve(appConfig({ deviceName: 'A' }));
      await save.promise;
    });

    await waitFor(() => {
      expect(updateConfig).toHaveBeenCalledTimes(1);
    });
    expect(deviceName.value).toBe('AB');
    expect(screen.getByText('settings:status.dirtyHint')).toBeTruthy();
  });

  test('general save failure keeps draft and surfaces scoped saveError', async () => {
    updateConfig.mockRejectedValue(new Error('save offline'));
    renderSettings();

    const deviceName = (await screen.findByLabelText(
      'settings:basic.deviceName',
    )) as HTMLInputElement;
    fireEvent.change(deviceName, { target: { value: 'Draft-Name' } });
    fireEvent.click(screen.getByRole('button', { name: 'settings:action.apply' }));

    await waitFor(() => {
      expect(screen.getByRole('alert').textContent).toContain('save offline');
    });
    expect(deviceName.value).toBe('Draft-Name');
    expect(screen.getByText('settings:status.dirtyHint')).toBeTruthy();
    expect(screen.queryByText(/settings:loadFailed/)).toBeNull();
  });

  test('keeps edits typed while cloud sync save is pending', async () => {
    const save = deferred<CloudSyncConfig>();
    updateCloudSyncConfig.mockReturnValue(save.promise);
    searchParamsState.value = new URLSearchParams('tab=sync');
    renderSettings();

    const repoUrl = (await screen.findByLabelText(
      'settings:cloudSync.repoUrl.label',
    )) as HTMLInputElement;
    fireEvent.change(repoUrl, { target: { value: 'git@github.com:u/new.git' } });
    fireEvent.click(screen.getByRole('button', { name: 'settings:cloudSync.apply' }));

    await waitFor(() => {
      expect(updateCloudSyncConfig).toHaveBeenCalledTimes(1);
    });

    fireEvent.change(repoUrl, { target: { value: 'git@github.com:u/new-edit.git' } });

    await act(async () => {
      save.resolve(cloudSync({ repoUrl: 'git@github.com:u/new.git' }));
      await save.promise;
    });

    expect(repoUrl.value).toBe('git@github.com:u/new-edit.git');
  });

  test('keeps edits typed while github AI save is pending', async () => {
    const save = deferred<GithubTrendingConfig>();
    updateGithubTrendingConfig.mockReturnValue(save.promise);
    searchParamsState.value = new URLSearchParams('tab=ai');
    renderSettings();

    const model = (await screen.findByLabelText(
      'settings:githubTrending.claudeModel.label',
    )) as HTMLInputElement;
    fireEvent.change(model, { target: { value: 'opus' } });
    fireEvent.click(screen.getByRole('button', { name: 'settings:githubTrending.apply' }));

    await waitFor(() => {
      expect(updateGithubTrendingConfig).toHaveBeenCalledTimes(1);
    });

    fireEvent.change(model, { target: { value: 'opus-edit' } });

    await act(async () => {
      save.resolve(githubTrending({ claudeModel: 'opus' }));
      await save.promise;
    });

    expect(model.value).toBe('opus-edit');
  });

  test('keeps fill language selection while prompt optimizer save is pending', async () => {
    const save = deferred<AppConfig>();
    updateConfig.mockReturnValue(save.promise);
    searchParamsState.value = new URLSearchParams('tab=ai');
    renderSettings();

    const enOption = await screen.findByRole('radio', {
      name: 'settings:promptOptimizerSettings.fillLanguage.en',
    });
    fireEvent.click(enOption);
    fireEvent.click(
      screen.getByRole('button', { name: 'settings:promptOptimizerSettings.apply' }),
    );

    await waitFor(() => {
      expect(updateConfig).toHaveBeenCalledTimes(1);
    });

    const zhOption = screen.getByRole('radio', {
      name: 'settings:promptOptimizerSettings.fillLanguage.zh',
    });
    fireEvent.click(zhOption);

    await act(async () => {
      save.resolve(appConfig({ promptOptimizerFillLanguage: 'en' }));
      await save.promise;
    });

    // 保存期间改回 zh，成功响应不得回填 en
    expect(zhOption.getAttribute('aria-checked')).toBe('true');
  });

  test('keeps edits typed while health save is pending', async () => {
    const save = deferred<HealthConfig>();
    updateHealthConfig.mockReturnValue(save.promise);
    searchParamsState.value = new URLSearchParams('tab=health');
    renderSettings();

    await waitFor(() => {
      expect(screen.getByRole('button', { name: 'settings:action.apply' })).toBeTruthy();
    });

    // Health NumberRow 无 htmlFor；按 number spinbutton 顺序取工作窗口分钟输入
    const numberInputs = screen.getAllByRole('spinbutton') as HTMLInputElement[];
    const workWindow = numberInputs[0];
    fireEvent.change(workWindow, { target: { value: '20' } });
    fireEvent.click(screen.getByRole('button', { name: 'settings:action.apply' }));

    await waitFor(() => {
      expect(updateHealthConfig).toHaveBeenCalledTimes(1);
    });

    fireEvent.change(workWindow, { target: { value: '25' } });

    await act(async () => {
      save.resolve(health({ workWindowSeconds: 20 * 60 }));
      await save.promise;
    });

    expect(workWindow.value).toBe('25');
  });

  test('keeps edits typed while automation save is pending', async () => {
    const save = deferred<OrchestratorAutomationConfig>();
    updateAutomationConfig.mockReturnValue(save.promise);
    searchParamsState.value = new URLSearchParams('tab=automation');
    renderSettings();

    const commands = (await screen.findByLabelText(
      'settings:automation.verificationCommands',
    )) as HTMLTextAreaElement;
    fireEvent.change(commands, { target: { value: 'npm test\nnpm run lint' } });
    fireEvent.click(screen.getByRole('button', { name: 'settings:action.apply' }));

    await waitFor(() => {
      expect(updateAutomationConfig).toHaveBeenCalledTimes(1);
    });

    fireEvent.change(commands, { target: { value: 'npm test\nnpm run lint\nnpm run build' } });

    await act(async () => {
      save.resolve(automation({ verificationCommands: ['npm test', 'npm run lint'] }));
      await save.promise;
    });

    expect(commands.value).toBe('npm test\nnpm run lint\nnpm run build');
  });
});
