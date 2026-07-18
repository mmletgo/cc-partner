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
  PermissionActionResult,
  PermissionsStatus,
  PromptOptimizerFillLanguage,
} from '../types';
import {
  booleanDecoder,
  enumDecoder,
  numberDecoder,
  objectDecoder,
  stringDecoder,
  type Decoder,
} from '../runtimeSchema';

const fillLanguageDecoder: Decoder<PromptOptimizerFillLanguage> = enumDecoder(
  'PromptOptimizerFillLanguage',
  ['zh', 'en'] as const,
);

/**
 * Business Logic（为什么需要这个 decoder）:
 *   基础偏好与快捷键配置是 Settings 核心资源。
 *
 * Code Logic（这个 decoder 做什么）:
 *   严格解码 deviceId/deviceName/receiveDir/hotkeys/port/fillLanguage。
 */
export const appConfigDecoder: Decoder<AppConfig> = objectDecoder('AppConfig', {
  deviceId: stringDecoder,
  deviceName: stringDecoder,
  receiveDir: stringDecoder,
  screenshotHotkey: stringDecoder,
  promptOptimizerHotkey: stringDecoder,
  promptOptimizerFillLanguage: fillLanguageDecoder,
  httpPort: numberDecoder,
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
