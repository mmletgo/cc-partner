/**
 * Provider Manager 运行时 schema。
 *
 * Business Logic（为什么需要这个模块）:
 *   IPC 边界可能损坏或返回混合版本结构；写入页面状态前必须 fail-closed。
 *
 * Code Logic（这个模块做什么）:
 *   严格 decoder：AgentApp 枚举、CLI/GUI 状态、provider 列表与安装结果；
 *   Option<T> 字段（Rust）对应 nullableDecoder。
 */

import type {
  AgentApp,
  AppProviders,
  CcSwitchGuiStatus,
  CliStatus,
  InstallResult,
  ProviderEntry,
  ProviderManagerSummary,
} from '../types/providerManager';
import {
  arrayDecoder,
  booleanDecoder,
  enumDecoder,
  nullableDecoder,
  objectDecoder,
  stringDecoder,
  type Decoder,
} from '../runtimeSchema';

export const agentAppDecoder: Decoder<AgentApp> = enumDecoder('AgentApp', [
  'claude',
  'codex',
  'gemini',
  'opencode',
  'hermes',
  'openclaw',
] as const);

export const providerEntryDecoder: Decoder<ProviderEntry> = objectDecoder('ProviderEntry', {
  id: stringDecoder,
  name: stringDecoder,
  category: nullableDecoder(stringDecoder),
  isCurrent: booleanDecoder,
});

export const appProvidersDecoder: Decoder<AppProviders> = objectDecoder('AppProviders', {
  app: agentAppDecoder,
  providers: arrayDecoder(providerEntryDecoder),
  currentProviderId: nullableDecoder(stringDecoder),
});

export const cliStatusDecoder: Decoder<CliStatus> = objectDecoder('CliStatus', {
  available: booleanDecoder,
  path: nullableDecoder(stringDecoder),
  version: nullableDecoder(stringDecoder),
});

export const ccSwitchGuiStatusDecoder: Decoder<CcSwitchGuiStatus> = objectDecoder(
  'CcSwitchGuiStatus',
  {
    installed: booleanDecoder,
    version: nullableDecoder(stringDecoder),
    running: nullableDecoder(booleanDecoder),
    versionMismatch: nullableDecoder(booleanDecoder),
  },
);

export const providerManagerSummaryDecoder: Decoder<ProviderManagerSummary> = objectDecoder(
  'ProviderManagerSummary',
  {
    ccSwitchDbPresent: booleanDecoder,
    cli: cliStatusDecoder,
    gui: nullableDecoder(ccSwitchGuiStatusDecoder),
    apps: arrayDecoder(appProvidersDecoder),
  },
);

export const installResultDecoder: Decoder<InstallResult> = objectDecoder('InstallResult', {
  method: stringDecoder,
  ok: booleanDecoder,
  version: nullableDecoder(stringDecoder),
  path: nullableDecoder(stringDecoder),
  message: nullableDecoder(stringDecoder),
  url: nullableDecoder(stringDecoder),
});
