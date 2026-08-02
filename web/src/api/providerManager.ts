/**
 * Provider Manager API — Tauri invoke 封装。
 *
 * Business Logic（为什么需要这个模块）:
 *   桌面端「Provider Manager」页与「设置 → 依赖环境」的 cc-switch 依赖卡片通过统一入口
 *   读写 provider 列表/状态、切换当前 provider、安装 cc-switch CLI，避免散落 invoke 字符串。
 *
 * Code Logic（这个模块做什么）:
 *   使用 invokeDecoded + providerManager schema 做 fail-closed 边界校验；
 *   命令名为 snake_case 对齐 Rust #[tauri::command]。
 */

import {
  appProvidersDecoder,
  installResultDecoder,
  providerManagerSummaryDecoder,
} from '@/lib/schemas/providerManager';
import { arrayDecoder } from '@/lib/runtimeSchema';
import type { AgentApp, AppProviders, InstallResult, ProviderManagerSummary } from '@/lib/types/providerManager';
import { invokeDecoded } from './client';

/** Tauri 命令名（snake_case，与后端 #[tauri::command] 对齐）。 */
export const PROVIDER_MANAGER_COMMANDS = {
  status: 'provider_manager_status',
  list: 'provider_manager_list',
  switch: 'provider_manager_switch',
  installCli: 'provider_manager_install_cli',
} as const;

/** 列表 decoder（包一层具名 contract，便于错误定位）。 */
const appProvidersListDecoder = {
  name: 'AppProviders[]',
  decode: (value: unknown, path = '$') => arrayDecoder(appProvidersDecoder).decode(value, path),
};

/**
 * Business Logic: 页面与 controller 统一入口。
 * Code Logic: 各方法 invokeDecoded 对应命令。
 */
export const providerManagerApi = {
  /**
   * Business Logic: 首屏展示 DB 是否存在、CLI 检测/版本、GUI 检测与各 app provider 列表。
   * Code Logic: provider_manager_status → ProviderManagerSummary。
   */
  status: (): Promise<ProviderManagerSummary> =>
    invokeDecoded(PROVIDER_MANAGER_COMMANDS.status, undefined, providerManagerSummaryDecoder),

  /**
   * Business Logic: 仅取各 app provider 列表（隐藏 0 provider 的 app，排除 claude-desktop）。
   * Code Logic: provider_manager_list → AppProviders[]。
   */
  list: (): Promise<AppProviders[]> =>
    invokeDecoded(PROVIDER_MANAGER_COMMANDS.list, undefined, appProvidersListDecoder),

  /**
   * Business Logic: 切换某 agent 的当前 provider（委托 cc-switch CLI 执行真实写盘）。
   * Code Logic: provider_manager_switch({ app, providerId }) → 更新后的 AppProviders。
   */
  switch: (app: AgentApp, providerId: string): Promise<AppProviders> =>
    invokeDecoded(PROVIDER_MANAGER_COMMANDS.switch, { app, providerId }, appProvidersDecoder),

  /**
   * Business Logic: 安装 cc-switch CLI（显式用户动作；macOS brew / 其余人工指引）。
   * Code Logic: provider_manager_install_cli → InstallResult。
   */
  installCli: (): Promise<InstallResult> =>
    invokeDecoded(PROVIDER_MANAGER_COMMANDS.installCli, undefined, installResultDecoder),
};
