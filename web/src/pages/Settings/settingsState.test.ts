import type { AppConfig, HealthConfig } from '../../lib/types';
import {
  buildConfigUpdate,
  cloudSyncFormToUpdate,
  cloudSyncConfigToForm,
  githubTrendingConfigToForm,
  healthConfigToForm,
  isSettingsStateDirty,
  PENDING_HEALTH_FORM,
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
  };
  const form = healthConfigToForm(cfg);
  assertDeepEqual(form, {
    ...cfg,
    waterEnabled: true,
    reminderFullscreen: true,
  });
  assertNotSameRef(form, cfg);
}

testHealthConfigToFormNull();
testHealthConfigToFormConfig();
