import type { AppConfig } from '../../lib/types';
import {
  buildConfigUpdate,
  cloudSyncFormToUpdate,
  cloudSyncConfigToForm,
  githubTrendingConfigToForm,
  isSettingsStateDirty,
  settingsStateFromConfig,
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
    screenshotHotkey: '<cmd>+<shift>+s',
    httpPort: 0,
    ...partial,
  };
}

const loaded = settingsStateFromConfig(configFixture());
assertDeepEqual(loaded, {
  deviceName: 'Hans-Mac',
  receiveDir: '/Users/hans/cc-partner-files',
  shortcuts: [
    {
      id: 'screenshot',
      labelKey: 'screenshot',
      value: '<cmd>+<shift>+s',
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
