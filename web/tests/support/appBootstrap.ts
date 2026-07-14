/**
 * L1 E2E 共享 AppShell bootstrap（backendHarness）。
 *
 * Business Logic（为什么需要这个模块）:
 *   Transfer/Scratchpad/Prompts/Permissions/Settings 等桌面旅程都挂在同一 AppShell，
 *   需要相同的 onboarded/权限/attention/workbench 基线 mock，避免每个 spec 重复注册。
 *
 * Code Logic（这个模块做什么）:
 *   提供合法 DTO 工厂与 registerAppShellCommands；写入 localStorage 语言/主题/onboarded。
 */

import type { Page } from '@playwright/test';
import type { BackendHarness } from './backendHarness';

/** 最小合法 AppConfig（满足 runtime schema）。 */
export type TestAppConfig = {
  deviceId: string;
  deviceName: string;
  receiveDir: string;
  screenshotHotkey: string;
  promptOptimizerHotkey: string;
  promptOptimizerFillLanguage: 'zh' | 'en';
  httpPort: number;
};

/**
 * Business Logic（为什么需要这个函数）:
 *   Settings/AppShell 多处需要合法 config DTO。
 *
 * Code Logic（这个函数做什么）:
 *   返回默认 AppConfig，可用 partial 覆盖。
 */
export function makeAppConfig(partial: Partial<TestAppConfig> = {}): TestAppConfig {
  return {
    deviceId: 'self-1',
    deviceName: 'Test Device',
    receiveDir: '/tmp/cc-partner-files',
    screenshotHotkey: 'CommandOrControl+Shift+S',
    promptOptimizerHotkey: 'Control',
    promptOptimizerFillLanguage: 'zh',
    httpPort: 62116,
    ...partial,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Welcome/Settings 权限卡需要稳定的 TCC 状态。
 *
 * Code Logic（这个函数做什么）:
 *   返回三项 granted 可配置的 PermissionsStatus（notification 由 decoder default）。
 */
export function makePermissionsStatus(granted = true): {
  screenCapture: { granted: boolean };
  accessibility: { granted: boolean };
  inputMonitoring: { granted: boolean };
} {
  return {
    screenCapture: { granted },
    accessibility: { granted },
    inputMonitoring: { granted },
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   AttentionProvider 挂载时会 list_attention_items。
 *
 * Code Logic（这个函数做什么）:
 *   返回空快照。
 */
export function makeEmptyAttentionSnapshot(): {
  generatedAt: string;
  counts: { total: number; decision: number; blocked: number; environment: number };
  items: unknown[];
} {
  return {
    generatedAt: '2026-07-14T00:00:00.000Z',
    counts: { total: 0, decision: 0, blocked: 0, environment: 0 },
    items: [],
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   WorkbenchDependencyProvider 挂载时检查 tmux 依赖。
 *
 * Code Logic（这个函数做什么）:
 *   返回 ready 状态。
 */
export function makeWorkbenchDependencyReady(): Record<string, unknown> {
  return {
    status: 'ready',
    available: true,
    version: '3.0',
    backend: 'native',
    path: '/usr/bin/tmux',
    installable: false,
    installCommandPreview: [],
    error: null,
    output: [],
    statusChangedAt: '2026-07-14T00:00:00.000Z',
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   局域网防火墙依赖卡在 Settings dependencies 需要完整 DTO。
 *
 * Code Logic（这个函数做什么）:
 *   返回对齐 LanFirewallDependencyStatus 的最小成功态（含 guidance.commands）。
 */
export function makeLanFirewallStatus(): Record<string, unknown> {
  return {
    platform: 'macos',
    platformLabel: 'macOS',
    lanIp: '192.168.1.10',
    httpPort: 62116,
    mdnsPort: 5353,
    appPath: '/Applications/cc-partner.app/Contents/MacOS/cc-partner',
    checks: [
      { id: 'httpListener', ok: true, detail: 'TCP 62116' },
      { id: 'lanIp', ok: true, detail: '192.168.1.10' },
      { id: 'tcpFirewall', ok: true, detail: 'TCP 62116' },
      { id: 'mdnsFirewall', ok: true, detail: 'UDP 5353' },
    ],
    guidance: {
      summaryKey: 'settings:lanFirewall.guidance.macos.summary',
      steps: [],
      commands: [
        {
          labelKey: 'settings:lanFirewall.guidance.macos.allowAppCommand',
          command:
            'sudo /usr/libexec/ApplicationFirewall/socketfilterfw --add "/Applications/cc-partner.app/Contents/MacOS/cc-partner"',
        },
      ],
    },
  };
}

/** Settings 业务组默认 DTO。 */
export const SETTINGS_FIXTURES = {
  cloudSync: {
    repoUrl: 'git@github.com:user/repo.git',
    enabled: true,
    auto: false,
    intervalSecs: 300,
    branch: 'main',
  },
  githubTrending: {
    aiEnabled: true,
    claudeCliPath: 'claude',
    claudeModel: 'sonnet',
    cacheTtlHours: 24,
  },
  health: {
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
  },
  automation: {
    enabled: false,
    maxConcurrentTasks: 2,
    verificationCommands: ['npm test'],
    autoCommit: false,
    autoPushTaskBranch: false,
    autoMergeToMain: false,
    autoPushMain: false,
  },
  version: { version: '0.0.0-test', buildDate: '2026-07-14' },
} as const;

/**
 * Business Logic（为什么需要这个函数）:
 *   在 goto 前写入 onboarded/语言/主题，避免 Welcome 拦截与语言抖动。
 *
 * Code Logic（这个函数做什么）:
 *   page.addInitScript 设置 localStorage。
 *
 * @param page Playwright Page
 * @param options.permissionOnboarded 是否写 cp-permission-onboarded（默认 true）
 */
export async function installAppLocalStorage(
  page: Page,
  options: { permissionOnboarded?: boolean } = {},
): Promise<void> {
  const permissionOnboarded = options.permissionOnboarded ?? true;
  await page.addInitScript((onboarded) => {
    if (onboarded) {
      window.localStorage.setItem('cp-permission-onboarded', '1');
    } else {
      window.localStorage.removeItem('cp-permission-onboarded');
    }
    window.localStorage.setItem('cp-lang', 'zh');
    window.localStorage.setItem('cp-theme', 'light');
  }, permissionOnboarded);
}

export type RegisterAppShellOptions = {
  /** check_permissions 返回值；默认全 granted */
  permissions?: ReturnType<typeof makePermissionsStatus>;
  /** get_config / get_default_config；默认 makeAppConfig() */
  config?: TestAppConfig;
  /**
   * 是否注册通知 plugin 命令为 granted。
   * 默认 true；Permissions 旅程可改为 reject。
   */
  notificationGranted?: boolean;
};

/**
 * Business Logic（为什么需要这个函数）:
 *   任意 AppShell 路由都可能调用 attention/projects/dependency/config/version/notification。
 *
 * Code Logic（这个函数做什么）:
 *   以 sticky resolve 注册常用命令；不覆盖页面专属命令（transfer/prompts 等）。
 *
 * @param harness backendHarness 控制器
 * @param options 可选覆盖
 */
export function registerAppShellCommands(
  harness: BackendHarness,
  options: RegisterAppShellOptions = {},
): void {
  const permissions = options.permissions ?? makePermissionsStatus(true);
  const config = options.config ?? makeAppConfig();
  const notificationGranted = options.notificationGranted ?? true;

  harness.command('check_permissions', { kind: 'resolve', value: permissions });
  harness.command('request_permission', { kind: 'resolve', value: { requested: true } });
  harness.command('get_version', {
    kind: 'resolve',
    value: SETTINGS_FIXTURES.version,
  });
  harness.command('get_config', { kind: 'resolve', value: config });
  harness.command('get_default_config', { kind: 'resolve', value: config });
  harness.command('list_attention_items', {
    kind: 'resolve',
    value: makeEmptyAttentionSnapshot(),
  });
  harness.command('list_workbench_projects', { kind: 'resolve', value: [] });
  harness.command('list_workbench_sessions', { kind: 'resolve', value: [] });
  harness.command('check_workbench_dependency', {
    kind: 'resolve',
    value: makeWorkbenchDependencyReady(),
  });
  harness.command('get_workbench_dependency_install_status', {
    kind: 'resolve',
    value: makeWorkbenchDependencyReady(),
  });
  harness.command('list_github_trending_repos', {
    kind: 'resolve',
    value: { repos: [], cached: true, generatedAt: null },
  });
  harness.command('check_lan_firewall_dependency', {
    kind: 'resolve',
    value: makeLanFirewallStatus(),
  });
  harness.command('get_mobile_access_info', {
    kind: 'resolve',
    value: {
      url: 'http://192.168.1.10:62116/mobile',
      lanAddresses: ['192.168.1.10'],
      httpPort: 62116,
    },
  });

  if (notificationGranted) {
    harness.command('plugin:notification|is_permission_granted', {
      kind: 'resolve',
      value: true,
    });
    harness.command('plugin:notification|request_permission', {
      kind: 'resolve',
      value: 'granted',
    });
  } else {
    harness.command('plugin:notification|is_permission_granted', {
      kind: 'reject',
      error: new Error('notification probe failed'),
    });
    harness.command('plugin:notification|request_permission', {
      kind: 'reject',
      error: new Error('notification request failed'),
    });
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Settings 页 11 端点并行加载需要全套成功/部分失败夹具。
 *
 * Code Logic（这个函数做什么）:
 *   注册 core/defaults/version 与 4 个 current+defaults 业务组；failGroup 指定一组 reject。
 *
 * @param harness backendHarness
 * @param options.failGroup 非 core 失败组（默认 githubTrending.current）
 * @param options.config 核心配置
 */
export function registerSettingsResourceCommands(
  harness: BackendHarness,
  options: {
    failGroup?:
      | 'none'
      | 'cloudSync'
      | 'githubTrending'
      | 'health'
      | 'automation'
      | 'version';
    config?: TestAppConfig;
  } = {},
): void {
  const config = options.config ?? makeAppConfig();
  const failGroup = options.failGroup ?? 'none';

  harness.command('get_config', { kind: 'resolve', value: config });
  harness.command('get_default_config', { kind: 'resolve', value: config });

  const cloudOk = failGroup !== 'cloudSync';
  const githubOk = failGroup !== 'githubTrending';
  const healthOk = failGroup !== 'health';
  const automationOk = failGroup !== 'automation';
  const versionOk = failGroup !== 'version';

  if (versionOk) {
    harness.command('get_version', {
      kind: 'resolve',
      value: SETTINGS_FIXTURES.version,
    });
  } else {
    harness.command('get_version', {
      kind: 'reject',
      error: new Error('version loader failed'),
    });
  }

  if (cloudOk) {
    harness.command('get_cloud_sync_config', {
      kind: 'resolve',
      value: SETTINGS_FIXTURES.cloudSync,
    });
    harness.command('get_default_cloud_sync_config', {
      kind: 'resolve',
      value: SETTINGS_FIXTURES.cloudSync,
    });
  } else {
    harness.command('get_cloud_sync_config', {
      kind: 'reject',
      error: new Error('cloudSync loader failed'),
    });
    harness.command('get_default_cloud_sync_config', {
      kind: 'resolve',
      value: SETTINGS_FIXTURES.cloudSync,
    });
  }

  if (githubOk) {
    harness.command('get_github_trending_config', {
      kind: 'resolve',
      value: SETTINGS_FIXTURES.githubTrending,
    });
    harness.command('get_default_github_trending_config', {
      kind: 'resolve',
      value: SETTINGS_FIXTURES.githubTrending,
    });
  } else {
    harness.command('get_github_trending_config', {
      kind: 'reject',
      error: new Error('githubTrending loader failed'),
    });
    harness.command('get_default_github_trending_config', {
      kind: 'resolve',
      value: SETTINGS_FIXTURES.githubTrending,
    });
  }

  if (healthOk) {
    harness.command('get_health_config', {
      kind: 'resolve',
      value: SETTINGS_FIXTURES.health,
    });
    harness.command('get_default_health_config', {
      kind: 'resolve',
      value: SETTINGS_FIXTURES.health,
    });
  } else {
    harness.command('get_health_config', {
      kind: 'reject',
      error: new Error('health loader failed'),
    });
    harness.command('get_default_health_config', {
      kind: 'resolve',
      value: SETTINGS_FIXTURES.health,
    });
  }

  if (automationOk) {
    harness.command('get_orchestrator_config', {
      kind: 'resolve',
      value: SETTINGS_FIXTURES.automation,
    });
    harness.command('get_default_orchestrator_config', {
      kind: 'resolve',
      value: SETTINGS_FIXTURES.automation,
    });
  } else {
    harness.command('get_orchestrator_config', {
      kind: 'reject',
      error: new Error('automation loader failed'),
    });
    harness.command('get_default_orchestrator_config', {
      kind: 'resolve',
      value: SETTINGS_FIXTURES.automation,
    });
  }
}
