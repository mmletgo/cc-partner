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
  gamePluginDir: string;
  screenshotHotkey: string;
  promptOptimizerHotkey: string;
  promptQuickInputHotkey: string;
  promptOptimizerFillLanguage: 'zh' | 'en';
  promptOptimizerProvider: 'claude' | 'grok';
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
    gamePluginDir: '/tmp/cc-partner-game-plugins',
    screenshotHotkey: 'CommandOrControl+Shift+S',
    promptOptimizerHotkey: 'Control',
    promptQuickInputHotkey: '<ctrl>+/',
    promptOptimizerFillLanguage: 'zh',
    promptOptimizerProvider: 'claude',
    httpPort: 62116,
    ...partial,
  };
}

/**
 * Business Logic（为什么需要这个类型）:
 *   E2E harness 的 get_mobile_access_info mock 必须对齐生产 DTO（entries + urls），
 *   避免卡片解析 entries 时落到空列表或旧字段。
 *
 * Code Logic（这个类型做什么）:
 *   描述 deviceName/port/urls/entries 结构；role 仅 wifi|wired。
 */
export type TestMobileAccessEntry = {
  id: string;
  url: string;
  host: string;
  role?: 'wifi' | 'wired' | null;
  isDefault: boolean;
};

/**
 * Business Logic（为什么需要这个类型）:
 *   桌面 AppShell / MobileAccessCard 消费的 access-info 响应形状。
 *
 * Code Logic（这个类型做什么）:
 *   urls 与 entries[].url 同序；entries 为权威列表。
 */
export type TestMobileAccessInfo = {
  deviceName: string;
  port: number;
  urls: string[];
  entries: TestMobileAccessEntry[];
};

/**
 * Business Logic（为什么需要这个函数）:
 *   多处 E2E 需要合法 get_mobile_access_info 响应，避免每个 spec 手写漂移字段。
 *
 * Code Logic（这个函数做什么）:
 *   默认单局域网入口（isDefault=true）；可用 partial 覆盖。
 */
export function makeMobileAccessInfo(
  partial: Partial<TestMobileAccessInfo> = {},
): TestMobileAccessInfo {
  const host = '192.168.1.10';
  const port = partial.port ?? 62116;
  const url = `http://${host}:${port}/mobile`;
  const defaultEntry: TestMobileAccessEntry = {
    id: host,
    url,
    host,
    role: 'wifi',
    isDefault: true,
  };
  const entries = partial.entries ?? [defaultEntry];
  return {
    deviceName: partial.deviceName ?? 'Test Device',
    port,
    urls: partial.urls ?? entries.map((entry) => entry.url),
    entries,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Welcome/Settings 权限卡需要稳定的 TCC 状态。
 *
 * Code Logic（这个函数做什么）:
 *   返回四项 granted 可配置的 PermissionsStatus；输入监控同时携带权威四态。
 */
export function makePermissionsStatus(granted = true): {
  screenCapture: { granted: boolean };
  accessibility: { granted: boolean };
  inputMonitoring: { granted: boolean; state: 'granted' | 'notDetermined' };
  notification: { granted: boolean };
} {
  return {
    screenCapture: { granted },
    accessibility: { granted },
    inputMonitoring: { granted, state: granted ? 'granted' : 'notDetermined' },
    notification: { granted },
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
    notifyHumanReview: true,
    notifyBlocked: true,
    notifyRemoteOutboxFailed: true,
    notifyTaskDone: false,
  },
  version: { version: '0.0.0-test', buildDate: '2026-07-14' },
} as const;

/**
 * Business Logic（为什么需要这个函数）:
 *   默认 E2E 应从已确认 LAN disclosure 基线启动，仅专用旅程模拟未确认。
 *
 * Code Logic（这个函数做什么）:
 *   返回 required=false 的 LanDisclosureStatus DTO。
 */
export function makeAcknowledgedLanDisclosureStatus(): {
  required: boolean;
  version: number;
  localAddresses: string[];
  preferredPort: number;
  mdnsPort: number;
  alreadyRunning: boolean;
  actualHttpPort: number | null;
} {
  return {
    required: false,
    version: 1,
    localAddresses: ['192.168.1.10'],
    preferredPort: 62116,
    mdnsPort: 5353,
    alreadyRunning: false,
    actualHttpPort: 62116,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   首启未确认旅程需要 required=true 的 status。
 *
 * Code Logic（这个函数做什么）:
 *   返回 required=true 的 DTO，可选 alreadyRunning。
 */
export function makeRequiredLanDisclosureStatus(
  options: { alreadyRunning?: boolean; actualHttpPort?: number | null } = {},
): {
  required: boolean;
  version: number;
  localAddresses: string[];
  preferredPort: number;
  mdnsPort: number;
  alreadyRunning: boolean;
  actualHttpPort: number | null;
} {
  return {
    required: true,
    version: 1,
    localAddresses: ['192.168.1.10'],
    preferredPort: 62116,
    mdnsPort: 5353,
    alreadyRunning: options.alreadyRunning ?? false,
    actualHttpPort: options.actualHttpPort ?? null,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   在 goto 前写入 onboarded/语言/主题，避免 Welcome 拦截与语言抖动。
 *
 * Code Logic（这个函数做什么）:
 *   page.addInitScript 设置 localStorage；语言默认 zh，可选 en 覆盖供英文 L1 旅程。
 *
 * @param page Playwright Page
 * @param options.permissionOnboarded 是否写 cp-permission-onboarded（默认 true）
 * @param options.lang 写入 cp-lang 的语言（默认 'zh'）
 */
export async function installAppLocalStorage(
  page: Page,
  options: { permissionOnboarded?: boolean; lang?: 'zh' | 'en' } = {},
): Promise<void> {
  const permissionOnboarded = options.permissionOnboarded ?? true;
  const lang = options.lang ?? 'zh';
  await page.addInitScript(
    ({ onboarded, language }) => {
      if (onboarded) {
        window.localStorage.setItem('cp-permission-onboarded', '1');
      } else {
        window.localStorage.removeItem('cp-permission-onboarded');
      }
      window.localStorage.setItem('cp-lang', language);
      window.localStorage.setItem('cp-theme', 'light');
    },
    { onboarded: permissionOnboarded, language: lang },
  );
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
  harness.command('list_workbench_window_occupancy', { kind: 'resolve', value: [] });
  harness.command('claim_workbench_window_project', {
    kind: 'resolve',
    value: { action: 'claimed', label: 'main', projectId: 'proj-placeholder' },
  });
  harness.command('open_workbench_window', {
    kind: 'resolve',
    value: { action: 'created', label: 'workbench-1', projectId: 'proj-placeholder' },
  });
  harness.command('focus_workbench_window', { kind: 'resolve', value: null });
  harness.command('close_workbench_window', { kind: 'resolve', value: null });
  harness.command('apply_workbench_window_deeplink', { kind: 'resolve', value: null });
  harness.command('get_workbench_launch_summary', {
    kind: 'resolve',
    value: {
      projects: { kind: 'ready', value: [] },
      sessions: { kind: 'ready', value: [] },
      tasks: { kind: 'ready', value: [] },
      transfers: { kind: 'ready', value: [] },
      devices: { kind: 'ready', value: [] },
      generatedAt: '2026-07-14T00:00:00.000Z',
    },
  });
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
    value: makeMobileAccessInfo(),
  });
  // 默认已确认 LAN disclosure，避免无关 E2E 被 gate 拦截
  harness.command('get_lan_disclosure_status', {
    kind: 'resolve',
    value: makeAcknowledgedLanDisclosureStatus(),
  });
  harness.command('acknowledge_lan_disclosure_and_start_backend', {
    kind: 'resolve',
    value: {
      actualHttpPort: 62116,
      localAddresses: ['192.168.1.10'],
      reusedExisting: false,
      version: 1,
    },
  });
  // App 级 OperationalNotificationCoordinator 冷启动 baseline + 偏好（无业务副作用）
  harness.command('get_operational_notification_snapshot', {
    kind: 'resolve',
    value: {
      asOfCursor: { ownerInstanceId: 'owner-shell', sequence: 0 },
      items: [],
      truncated: false,
    },
  });
  harness.command('get_orchestrator_config', {
    kind: 'resolve',
    value: SETTINGS_FIXTURES.automation,
  });
  harness.command('get_default_orchestrator_config', {
    kind: 'resolve',
    value: SETTINGS_FIXTURES.automation,
  });
  // Workbench mount：workspace layout restore / Agent runtime / LAN Fleet / ledger / experiments
  harness.command('get_workspace_layout', { kind: 'resolve', value: null });
  harness.command('save_workspace_layout', {
    kind: 'resolve',
    value: {
      schemaVersion: 1,
      id: 'layout-auto',
      revision: 1,
      slotKey: 'desktop:auto',
      kind: 'auto',
      name: null,
      projectId: 'proj-placeholder',
      activeWorktreeId: null,
      activeSessionId: null,
      workspaceView: 'terminal',
      inspectorTab: 'files',
      browserTargetUrl: null,
      createdAt: '2026-07-14T00:00:00.000Z',
      updatedAt: '2026-07-14T00:00:00.000Z',
    },
  });
  harness.command('list_named_workspace_layouts', { kind: 'resolve', value: [] });
  harness.command('delete_named_workspace_layout', { kind: 'resolve', value: null });
  harness.command('preflight_workspace_restore_cmd', {
    kind: 'resolve',
    value: {
      restoreId: 'restore-empty',
      layoutId: '',
      layoutRevision: 0,
      status: 'empty',
      resolvedProjectId: null,
      resolvedWorktreeId: null,
      resolvedSessionId: null,
      workspaceView: 'terminal',
      inspectorTab: 'files',
      browserTargetUrl: null,
      actions: [],
    },
  });
  harness.command('apply_workspace_restore_cmd', {
    kind: 'resolve',
    value: {
      restoreId: 'restore-empty',
      status: 'empty',
      restoredCount: 0,
      skippedCount: 0,
      actions: [],
    },
  });
  harness.command('get_agent_runtime_snapshot', {
    kind: 'resolve',
    value: {
      ownerInstanceId: 'owner-shell',
      asOfSequence: 0,
      projectId: null,
      sessions: [],
      truncated: false,
    },
  });
  harness.command('get_workbench_lan_fleet', {
    kind: 'resolve',
    value: {
      generatedAt: '2026-07-14T00:00:00.000Z',
      devices: [],
      truncated: false,
    },
  });
  harness.command('list_agent_ledger', {
    kind: 'resolve',
    value: { items: [], nextCursor: null },
  });
  harness.command('summarize_agent_ledger', {
    kind: 'resolve',
    value: {
      window: '7d',
      projectId: null,
      sessions: 0,
      completed: 0,
      failed: 0,
      cancelled: 0,
      disconnected: 0,
      durationMs: 0,
      inputTokens: null,
      outputTokens: null,
      costByCurrency: [],
      usageCoverage: 'unavailable',
    },
  });
  harness.command('clear_agent_ledger', { kind: 'resolve', value: 0 });
  harness.command('list_orchestrator_experiments', { kind: 'resolve', value: [] });

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
