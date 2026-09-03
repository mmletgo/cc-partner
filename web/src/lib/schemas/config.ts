/**
 * Settings config / permissions 运行时 schema。
 *
 * Business Logic（为什么需要这个模块）:
 *   设置页与 Welcome 权限流在写入 state 前必须拒绝残缺 config/permissions。
 *
 * Code Logic（这个模块做什么）:
 *   解码 AppConfig 与 PermissionsStatus（通知字段对后端响应显式 default）。
 */

import type {
  AppConfig,
  ExperimentalFeaturesConfig,
  PermissionActionResult,
  PermissionsStatus,
  PromptOptimizerFillLanguage,
  PromptOptimizerProvider,
  RelayConfig,
} from '../types';
import {
  arrayDecoder,
  booleanDecoder,
  enumDecoder,
  numberDecoder,
  objectDecoder,
  optionalDecoder,
  stringDecoder,
  type Decoder,
} from '../runtimeSchema';

const fillLanguageDecoder: Decoder<PromptOptimizerFillLanguage> = enumDecoder(
  'PromptOptimizerFillLanguage',
  ['zh', 'en'] as const,
);

const promptOptimizerProviderDecoder: Decoder<PromptOptimizerProvider> = enumDecoder(
  'PromptOptimizerProvider',
  ['claude', 'grok'] as const,
);

/**
 * Business Logic（为什么需要这个 decoder）:
 *   旧后端缺 experimentalFeatures 时必须 fail-closed 全关，不能把残缺对象当成已开启。
 *
 * Code Logic（这个 decoder 做什么）:
 *   五开关各自 default false。
 */
export const experimentalFeaturesDecoder: Decoder<ExperimentalFeaturesConfig> = objectDecoder(
  'ExperimentalFeaturesConfig',
  {
    battery: booleanDecoder,
    game: booleanDecoder,
    browser: booleanDecoder,
    automation: booleanDecoder,
    cloudSync: booleanDecoder,
  },
  {
    defaults: {
      battery: false,
      game: false,
      browser: false,
      automation: false,
      cloudSync: false,
    },
  },
);

const DEFAULT_EXPERIMENTAL_FEATURES: ExperimentalFeaturesConfig = {
  battery: false,
  game: false,
  browser: false,
  automation: false,
  cloudSync: false,
};

/**
 * Business Logic（为什么需要这个 decoder）:
 *   旧后端不带 relay 段时必须回落默认值，不能把整个 config 判成残缺而拒绝加载；
 *   新后端带的 relay 段必须严格解码，避免半截数组进入跳板管理 UI。
 *
 * Code Logic（这个 decoder做什么）:
 *   解码 enabled/viaDeviceIds/ignoredTargetIds；三个字段均缺省（默认允许中转、空列表）。
 */
export const relayConfigDecoder: Decoder<RelayConfig> = objectDecoder(
  'RelayConfig',
  {
    enabled: booleanDecoder,
    viaDeviceIds: arrayDecoder(stringDecoder),
    ignoredTargetIds: arrayDecoder(stringDecoder),
  },
  {
    defaults: {
      enabled: true,
      viaDeviceIds: [],
      ignoredTargetIds: [],
    },
  },
);

/**
 * Business Logic（为什么需要这个 decoder）:
 *   基础偏好与快捷键配置是 Settings 核心资源。
 *
 * Code Logic（这个 decoder 做什么）:
 *   严格解码 deviceId/deviceName/receiveDir/hotkeys/port/fillLanguage；
 *   experimentalFeatures 缺省全关；relay 为可选字段（旧后端缺省 undefined）。
 */
export const appConfigDecoder: Decoder<AppConfig> = objectDecoder('AppConfig', {
  deviceId: stringDecoder,
  deviceName: stringDecoder,
  receiveDir: stringDecoder,
  gamePluginDir: stringDecoder,
  screenshotHotkey: stringDecoder,
  promptOptimizerHotkey: stringDecoder,
  promptOptimizerFillLanguage: fillLanguageDecoder,
  promptOptimizerProvider: promptOptimizerProviderDecoder,
  promptQuickInputHotkey: stringDecoder,
  httpPort: numberDecoder,
  experimentalFeatures: experimentalFeaturesDecoder,
  relay: optionalDecoder(relayConfigDecoder),
}, {
  defaults: {
    promptOptimizerProvider: 'claude',
    experimentalFeatures: DEFAULT_EXPERIMENTAL_FEATURES,
  },
});

const grantedDecoder = objectDecoder('PermissionGranted', {
  granted: booleanDecoder,
});

const inputMonitoringStateDecoder = enumDecoder('InputMonitoringState', [
  'granted',
  'denied',
  'notDetermined',
  'unavailable',
] as const);

const inputMonitoringDecoder = objectDecoder('InputMonitoringPermissionState', {
  granted: booleanDecoder,
  state: inputMonitoringStateDecoder,
});

/** 解码显式权限操作结果，拒绝旧的 prompt/settings 混合返回形态。 */
export const permissionActionResultDecoder: Decoder<PermissionActionResult> = objectDecoder(
  'PermissionActionResult',
  {
    permission: stringDecoder,
    operation: enumDecoder('PermissionOperation', ['request', 'openSettings', 'noop'] as const),
    before: stringDecoder,
    after: stringDecoder,
  },
);

/**
 * Business Logic（为什么需要这个 decoder）:
 *   权限引导依赖三项 TCC + notification 合并结构。
 *
 * Code Logic（这个 decoder 做什么）:
 *   解码 screenCapture/inputMonitoring/accessibility；
 *   notification 由后端权威返回；缺省时 default `{ granted: false }`（fail-closed，避免假绿）。
 */
export const permissionsStatusDecoder: Decoder<PermissionsStatus> = objectDecoder<PermissionsStatus>(
  'PermissionsStatus',
  {
    screenCapture: grantedDecoder,
    inputMonitoring: inputMonitoringDecoder,
    accessibility: grantedDecoder,
    notification: grantedDecoder,
  },
  {
    defaults: {
      notification: { granted: false },
    },
  },
);
