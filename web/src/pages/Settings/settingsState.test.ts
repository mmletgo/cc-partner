import { describe, test } from 'vitest';
import type { AppConfig, HealthConfig, UpdateDownloadStatus } from '../../lib/types';
import type { HealthForm } from './settingsState';
import {
  PENDING_HEALTH_FORM,
  buildConfigUpdate,
  cloudSyncConfigToForm,
  cloudSyncFormToUpdate,
  githubTrendingConfigToForm,
  healthConfigToForm,
  installButtonMode,
  isSettingsStateDirty,
  isUpdateCheckDisabled,
  isUpdateDownloadDisabled,
  parseExperimentalFeatureFromSearch,
  parseSettingsTabFromSearch,
  resolveExperimentalSettingsTab,
  mergeActivityStatsSlice,
  mergeHealthReminderSlice,
  resetHealthReminderDefaults,
  resolveSettingsTabId,
  settingsStateFromConfig,
  shouldPollUpdateStatus,
  shouldShowInstallRetry,
} from './settingsState';

/**
 * Business Logic（为什么需要）:
 *   Settings 页行为测试不依赖测试框架，便于直接用 tsx 在本目录验证关键状态逻辑。
 *
 * Code Logic（做什么）:
 *   比较 JSON 序列化结果，不一致时抛错让 node 进程以非零状态退出。
 */
function assertDeepEqual(actual: unknown, expected: unknown): void {
  const actualJson = JSON.stringify(actual);
  const expectedJson = JSON.stringify(expected);
  if (actualJson !== expectedJson) {
    throw new Error(`Expected ${expectedJson}, got ${actualJson}`);
  }
}

/**
 * Business Logic（为什么需要）:
 *   Settings 页需要用后端配置生成完整表单，不能在只改快捷键时丢失设备名和接收目录。
 *
 * Code Logic（做什么）:
 *   构造最小 AppConfig 测试夹具，避免每个断言重复无关字段。
 */
function configFixture(partial: Partial<AppConfig> = {}): AppConfig {
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

describe('settingsState', () => {
  test('maps config <-> form <-> update across shortcuts, cloud, trending, prompt optimizer', () => {
    const loaded = settingsStateFromConfig(configFixture());
    assertDeepEqual(loaded, {
      deviceName: 'Hans-Mac',
      receiveDir: '/Users/hans/cc-partner-files',
      gamePluginDir: '/Users/hans/.cc-partner/plugins',
      shortcuts: [
        {
          id: 'screenshot',
          labelKey: 'shortcut.screenshot.label',
          helperKey: 'shortcut.screenshot.helper',
          value: '<cmd>+<shift>+s',
        },
        {
          id: 'promptOptimizer',
          labelKey: 'promptOptimizerSettings.hotkey.label',
          helperKey: 'promptOptimizerSettings.hotkey.helper',
          value: '<ctrl>',
        },
        {
          id: 'promptQuickInput',
          labelKey: 'promptQuickInputSettings.hotkey.label',
          helperKey: 'promptQuickInputSettings.hotkey.helper',
          value: '<ctrl>+/',
        },
      ],
    });

    const changedShortcut = {
      ...loaded,
      shortcuts: loaded.shortcuts.map((s) =>
        s.id === 'screenshot' ? { ...s, value: '<cmd>+<shift>+4' } : s,
      ),
    };
    assertDeepEqual(buildConfigUpdate(changedShortcut, loaded), {
      screenshotHotkey: '<cmd>+<shift>+4',
    });

    const defaults = settingsStateFromConfig(
      configFixture({
        deviceName: 'cc-partner',
        receiveDir: '/Users/hans/cc-partner-files',
        screenshotHotkey: '<cmd>+<shift>+s',
      }),
    );
    assertDeepEqual(defaults.deviceName, 'cc-partner');
    assertDeepEqual(defaults.receiveDir, '/Users/hans/cc-partner-files');
    assertDeepEqual(isSettingsStateDirty(defaults, changedShortcut), true);

    // 迁移正确性：改 promptOptimizer / promptQuickInput 的快捷键值时，常规 buildConfigUpdate
    // 必须把对应后端字段写入 patch——证明这两个快捷键已随常规「保存」持久化，而非只靠 AI tab。
    const changedPromptHotkeys: typeof loaded = {
      ...loaded,
      shortcuts: loaded.shortcuts.map((s) =>
        s.id === 'promptOptimizer'
          ? { ...s, value: '<cmd>+e' }
          : s.id === 'promptQuickInput'
            ? { ...s, value: '<cmd>+p' }
            : s,
      ),
    };
    assertDeepEqual(buildConfigUpdate(changedPromptHotkeys, loaded), {
      promptOptimizerHotkey: '<cmd>+e',
      promptQuickInputHotkey: '<cmd>+p',
    });

    const changedPluginDir = { ...loaded, gamePluginDir: '/tmp/more-games' };
    assertDeepEqual(buildConfigUpdate(changedPluginDir, loaded), {
      gamePluginDir: '/tmp/more-games',
    });

    // 同时改三个快捷键时 patch 应包含全部三个字段
    const changedAll: typeof loaded = {
      ...loaded,
      shortcuts: loaded.shortcuts.map((s) => {
        if (s.id === 'screenshot') return { ...s, value: '<cmd>+<shift>+4' };
        if (s.id === 'promptOptimizer') return { ...s, value: '<cmd>+e' };
        return { ...s, value: '<cmd>+p' };
      }),
    };
    assertDeepEqual(buildConfigUpdate(changedAll, loaded), {
      screenshotHotkey: '<cmd>+<shift>+4',
      promptOptimizerHotkey: '<cmd>+e',
      promptQuickInputHotkey: '<cmd>+p',
    });

    assertDeepEqual(
      cloudSyncConfigToForm({
        repoUrl: null,
        branch: null,
        enabled: false,
        auto: false,
        intervalSecs: 600,
      }),
      {
        repoUrl: '',
        branch: '',
        enabled: false,
        auto: false,
        intervalSecs: 600,
      },
    );

    assertDeepEqual(
      githubTrendingConfigToForm({
        aiEnabled: true,
        claudeCliPath: 'claude',
        claudeModel: 'sonnet',
        cacheTtlHours: 24,
      }),
      {
        aiEnabled: true,
        claudeCliPath: 'claude',
        claudeModel: 'sonnet',
        cacheTtlHours: 24,
      },
    );

    assertDeepEqual(
      cloudSyncFormToUpdate({
        repoUrl: '  ',
        branch: ' ',
        enabled: false,
        auto: false,
        intervalSecs: 600,
      }),
      {
        repoUrl: '',
        enabled: false,
        auto: false,
        intervalSecs: 600,
        branch: '',
      },
    );
  });

  test('healthConfigToForm normalizes water/fullscreen when health monitoring is enabled and returns fresh refs', () => {
    testHealthConfigToFormNull();
    testHealthConfigToFormConfig();
  });
});

// ===== healthConfigToForm: 健康表单映射 =====

/**
 * Business Logic（为什么需要）:
 *   健康 tab 的表单状态必须与已应用配置分离，且恢复默认时不能复用占位常量对象导致外部直接改到常量。
 *
 * Code Logic（做什么）:
 *   比较 actual/expected 深度相等(沿用 assertDeepEqual 语义),再断言两者非同一引用,不一致则抛错。
 */
function assertNotSameRef(actual: unknown, expected: unknown): void {
  if (actual === expected) {
    throw new Error('Expected distinct object references, got the same reference');
  }
}

/**
 * Business Logic（为什么需要）:
 *   健康 tab 加载配置前(null)需要占位默认值,且每次调用都返回新对象避免外部误改共享常量。
 *
 * Code Logic（做什么）:
 *   调用 healthConfigToForm(null),断言返回内容与 PENDING_HEALTH_FORM 深度相等且非同一引用。
 */
function testHealthConfigToFormNull(): void {
  const form = healthConfigToForm(null);
  assertDeepEqual(form, PENDING_HEALTH_FORM);
  assertNotSameRef(form, PENDING_HEALTH_FORM);
  assertDeepEqual(form.waterEnabled, true);
  assertDeepEqual(form.reminderFullscreen, true);
}

/**
 * Business Logic（为什么需要）:
 *   已有后端配置(含部分字段为 null,如 dndStart/dndEnd)需进入表单,且健康监测开启后
 *   喝水提醒与全屏遮罩不再允许单独关闭,表单层要归一为 true。
 *
 * Code Logic（做什么）:
 *   构造含 null dnd 且旧开关为 false 的 HealthConfig,断言返回对象非同一引用,
 *   其他字段保持原值,waterEnabled/reminderFullscreen 被归一为 true。
 */
function testHealthConfigToFormConfig(): void {
  const cfg: HealthConfig = {
    enabled: false,
    workWindowSeconds: 120,
    breakSeconds: 60,
    recordWindowTitle: false,
    retainDays: 7,
    notifyEnabled: false,
    dndStart: '22:00',
    dndEnd: null,
    waterEnabled: false,
    waterIntervalSeconds: 1800,
    reminderFullscreen: false,
    reminders: [],
  };
  const form = healthConfigToForm(cfg);
  if (form.reminders.length !== 3) {
    throw new Error(`expected seeded 3 reminders, got ${form.reminders.length}`);
  }
  assertDeepEqual(form.waterEnabled, true);
  assertDeepEqual(form.reminderFullscreen, true);
  assertDeepEqual(form.workWindowSeconds, 120);
  assertDeepEqual(form.waterIntervalSeconds, 1800);
  assertNotSameRef(form, cfg);
  assertNotSameRef(form.reminders, PENDING_HEALTH_FORM.reminders);
}


describe('settings tab deep link helpers', () => {
  test('resolves known tabs and falls back for unknown', () => {
    if (resolveSettingsTabId('dependencies') !== 'dependencies') {
      throw new Error('expected dependencies');
    }
    if (resolveSettingsTabId('nope', 'general') !== 'general') {
      throw new Error('expected fallback general');
    }
  });

  test('parses experimental feature from search and legacy tabs', () => {
    if (parseExperimentalFeatureFromSearch('?tab=experimental&feature=cloudSync') !== 'cloudSync') {
      throw new Error('expected cloudSync feature');
    }
    if (parseExperimentalFeatureFromSearch('?tab=battery') !== 'battery') {
      throw new Error('expected legacy battery feature');
    }
    if (parseExperimentalFeatureFromSearch('?tab=automation') !== 'automation') {
      throw new Error('expected legacy automation feature');
    }
    if (parseExperimentalFeatureFromSearch('?tab=sync') !== null) {
      throw new Error('expected null feature on sync tab');
    }
  });

  test('resolves nested experimental settings tab from enabled flags', () => {
    const allOff = {
      battery: false,
      game: false,
      browser: false,
      automation: false,
      cloudSync: false,
    };
    if (resolveExperimentalSettingsTab(allOff, 'battery') !== null) {
      throw new Error('expected null when all features off');
    }
    const cloudOnly = { ...allOff, cloudSync: true };
    if (resolveExperimentalSettingsTab(cloudOnly, null) !== 'cloudSync') {
      throw new Error('expected first enabled cloudSync');
    }
    if (resolveExperimentalSettingsTab(cloudOnly, 'battery') !== 'cloudSync') {
      throw new Error('expected fallback when requested feature is off');
    }
    const twoOn = { ...allOff, battery: true, cloudSync: true };
    if (resolveExperimentalSettingsTab(twoOn, 'cloudSync') !== 'cloudSync') {
      throw new Error('expected requested cloudSync when enabled');
    }
    if (resolveExperimentalSettingsTab(twoOn, null) !== 'battery') {
      throw new Error('expected first enabled battery');
    }
    const browserOnly = { ...allOff, browser: true };
    if (resolveExperimentalSettingsTab(browserOnly, 'browser') !== null) {
      throw new Error('expected no settings tab for browser-only opt-in');
    }
  });

  test('parses tab from search including while remounted query changes', () => {
    if (parseSettingsTabFromSearch('?tab=dependencies') !== 'dependencies') {
      throw new Error('expected dependencies from search');
    }
    if (parseSettingsTabFromSearch('?tab=automation') !== 'experimental') {
      throw new Error('expected legacy automation tab to resolve to experimental');
    }
    if (parseSettingsTabFromSearch('?tab=battery') !== 'experimental') {
      throw new Error('expected legacy battery tab to resolve to experimental');
    }
    if (parseSettingsTabFromSearch('?tab=experimental') !== 'experimental') {
      throw new Error('expected experimental from search');
    }
    if (parseSettingsTabFromSearch('?tab=fleet') !== 'general') {
      throw new Error('expected retired fleet tab to fall back to general');
    }
    if (parseSettingsTabFromSearch('?tab=activity') !== 'activity') {
      throw new Error('expected activity from search');
    }
    if (parseSettingsTabFromSearch('?tab=unknown') !== 'general') {
      throw new Error('expected unknown to fall back');
    }
  });

  test('reminder apply keeps last saved activity fields; activity apply keeps reminder fields', () => {
    const applied: HealthConfig = {
      enabled: true,
      workWindowSeconds: 45 * 60,
      breakSeconds: 5 * 60,
      recordWindowTitle: true,
      retainDays: 90,
      notifyEnabled: true,
      dndStart: null,
      dndEnd: null,
      waterEnabled: true,
      waterIntervalSeconds: 60 * 60,
      reminderFullscreen: true,
      reminders: PENDING_HEALTH_FORM.reminders,
    };
    const reminderDraft: HealthForm = {
      ...applied,
      workWindowSeconds: 20 * 60,
      recordWindowTitle: false,
      retainDays: 7,
    };
    const reminderPayload = mergeHealthReminderSlice(applied, reminderDraft);
    assertDeepEqual(reminderPayload.workWindowSeconds, 20 * 60);
    assertDeepEqual(reminderPayload.recordWindowTitle, true);
    assertDeepEqual(reminderPayload.retainDays, 90);

    const activityDraft: HealthForm = {
      ...applied,
      workWindowSeconds: 10 * 60,
      recordWindowTitle: false,
      retainDays: 14,
    };
    const activityPayload = mergeActivityStatsSlice(applied, activityDraft);
    assertDeepEqual(activityPayload.recordWindowTitle, false);
    assertDeepEqual(activityPayload.retainDays, 14);
    assertDeepEqual(activityPayload.workWindowSeconds, 45 * 60);
    assertDeepEqual(activityPayload.reminders.length, applied.reminders.length);
  });

  test('resetHealthReminderDefaults restores builtins and keeps custom templates', () => {
    const custom = {
      ...PENDING_HEALTH_FORM.reminders[0],
      id: 'custom-1',
      builtin: false,
      name: '伸展',
    };
    const draft: HealthForm = {
      ...PENDING_HEALTH_FORM,
      reminders: PENDING_HEALTH_FORM.reminders.map((item) =>
        item.id === 'water' ? { ...item, intervalSeconds: 1800 } : item,
      ).concat(custom),
    };
    const reset = resetHealthReminderDefaults(PENDING_HEALTH_FORM, draft, PENDING_HEALTH_FORM);
    assertDeepEqual(reset.reminders.find((item) => item.id === 'water')?.intervalSeconds, 3600);
    assertDeepEqual(reset.reminders.find((item) => item.id === 'custom-1')?.name, '伸展');
    assertDeepEqual(reset.reminders.length, 4);
  });
});


/**
 * Business Logic（为什么需要）:
 *   Updater UI 决策 helper 必须对 checking/installing/install-retry 边界做纯函数断言，避免 Settings 页回归。
 *
 * Code Logic（做什么）:
 *   构造最小 UpdateDownloadStatus 夹具，覆盖 disable / retry / poll / button mode。
 */
function updateStatusFixture(
  partial: Partial<UpdateDownloadStatus> & Pick<UpdateDownloadStatus, 'status'>,
): UpdateDownloadStatus {
  return {
    progress: 0,
    error: '',
    filePath: '',
    url: '',
    filename: '',
    size: 0,
    ...partial,
  };
}

describe('updater UI helpers', () => {
  test('disables check during local checking or backend checking/installing', () => {
    if (!isUpdateCheckDisabled({ checkingUpdate: true, downloadStatus: null })) {
      throw new Error('expected disabled when checkingUpdate');
    }
    if (
      !isUpdateCheckDisabled({
        checkingUpdate: false,
        downloadStatus: updateStatusFixture({ status: 'checking' }),
      })
    ) {
      throw new Error('expected disabled when status=checking');
    }
    if (
      !isUpdateCheckDisabled({
        checkingUpdate: false,
        downloadStatus: updateStatusFixture({ status: 'installing' }),
      })
    ) {
      throw new Error('expected disabled when status=installing');
    }
    if (
      isUpdateCheckDisabled({
        checkingUpdate: false,
        downloadStatus: updateStatusFixture({ status: 'completed' }),
      })
    ) {
      throw new Error('expected enabled when completed');
    }
  });

  test('disables download during checking/installing/downloading', () => {
    if (!isUpdateDownloadDisabled({ checkingUpdate: true, downloadStatus: null })) {
      throw new Error('expected disabled when checkingUpdate');
    }
    for (const status of ['checking', 'installing', 'downloading'] as const) {
      if (
        !isUpdateDownloadDisabled({
          checkingUpdate: false,
          downloadStatus: updateStatusFixture({ status }),
        })
      ) {
        throw new Error(`expected disabled when status=${status}`);
      }
    }
    if (
      isUpdateDownloadDisabled({
        checkingUpdate: false,
        downloadStatus: updateStatusFixture({ status: 'completed' }),
      })
    ) {
      throw new Error('expected enabled when completed');
    }
  });

  test('shouldShowInstallRetry only for completed with non-empty error', () => {
    if (
      !shouldShowInstallRetry(
        updateStatusFixture({ status: 'completed', error: 'signature failed' }),
      )
    ) {
      throw new Error('expected retry for completed+error');
    }
    if (shouldShowInstallRetry(updateStatusFixture({ status: 'completed', error: '' }))) {
      throw new Error('expected no retry for completed empty error');
    }
    if (shouldShowInstallRetry(updateStatusFixture({ status: 'completed', error: '   ' }))) {
      throw new Error('expected no retry for whitespace-only error');
    }
    if (
      shouldShowInstallRetry(
        updateStatusFixture({ status: 'failed', error: 'network down' }),
      )
    ) {
      throw new Error('expected no install retry for failed download');
    }
    if (shouldShowInstallRetry(null)) {
      throw new Error('expected no retry for null');
    }
  });

  test('shouldPollUpdateStatus only for checking/downloading/installing', () => {
    for (const status of ['checking', 'downloading', 'installing'] as const) {
      if (!shouldPollUpdateStatus(updateStatusFixture({ status }))) {
        throw new Error(`expected poll for ${status}`);
      }
    }
    for (const status of ['idle', 'completed', 'failed', 'cancelled'] as const) {
      if (shouldPollUpdateStatus(updateStatusFixture({ status }))) {
        throw new Error(`expected no poll for ${status}`);
      }
    }
    if (shouldPollUpdateStatus(null)) {
      throw new Error('expected no poll for null');
    }
  });

  test('installButtonMode covers install / installing / retryInstall', () => {
    if (
      installButtonMode({
        installing: true,
        downloadStatus: updateStatusFixture({ status: 'completed' }),
      }) !== 'installing'
    ) {
      throw new Error('expected installing when local installing');
    }
    if (
      installButtonMode({
        installing: false,
        downloadStatus: updateStatusFixture({ status: 'installing' }),
      }) !== 'installing'
    ) {
      throw new Error('expected installing when status=installing');
    }
    if (
      installButtonMode({
        installing: false,
        downloadStatus: updateStatusFixture({
          status: 'completed',
          error: 'install boom',
        }),
      }) !== 'retryInstall'
    ) {
      throw new Error('expected retryInstall for completed+error');
    }
    if (
      installButtonMode({
        installing: false,
        downloadStatus: updateStatusFixture({ status: 'completed', error: '' }),
      }) !== 'install'
    ) {
      throw new Error('expected install for completed empty error');
    }
  });
});
