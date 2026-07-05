/**
 * Orchestrator 自动化配置 API
 *
 * Business Logic（为什么需要这个模块）:
 *   Settings 自动化 tab 需要读取、恢复默认并保存设备级 Orchestrator 自动化配置；
 *   配置读写已由 Rust Phase 1 命令提供，前端必须通过独立 API 模块调用，避免混入任务看板 API。
 *
 * Code Logic（这个模块做什么）:
 *   基于统一 invoke wrapper 调用 get/update/default 三个 Tauri 命令，并暴露前端表单需要的 DTO 类型。
 */
import { invoke } from './client';

/** Orchestrator 设备级自动化配置 DTO。 */
export interface OrchestratorAutomationConfig {
  enabled: boolean;
  maxConcurrentTasks: number;
  verificationCommands: string[];
  autoCommit: boolean;
  autoPushTaskBranch: boolean;
  autoMergeToMain: boolean;
  autoPushMain: boolean;
}

/** Orchestrator 自动化配置更新 patch；verificationCommands 由 textarea 多行文本提交给后端归一化。 */
export interface OrchestratorAutomationConfigPatch {
  enabled?: boolean;
  maxConcurrentTasks?: number;
  verificationCommands?: string;
  autoCommit?: boolean;
  autoPushTaskBranch?: boolean;
  autoMergeToMain?: boolean;
  autoPushMain?: boolean;
}

/**
 * 读取当前 Orchestrator 自动化配置
 *
 * Business Logic（为什么需要这个函数）:
 *   Settings 自动化 tab 打开时需要展示当前设备已经保存的全局自动化策略。
 *
 * Code Logic（这个函数做什么）:
 *   调用 Rust `get_orchestrator_config` 命令并返回类型化 DTO。
 */
function getOrchestratorConfig(): Promise<OrchestratorAutomationConfig> {
  return invoke<OrchestratorAutomationConfig>('get_orchestrator_config');
}

/**
 * 读取 Orchestrator 自动化默认配置
 *
 * Business Logic（为什么需要这个函数）:
 *   用户点击「恢复默认」时必须使用后端权威默认值，而不是前端硬编码默认策略。
 *
 * Code Logic（这个函数做什么）:
 *   调用 Rust `get_default_orchestrator_config` 命令并返回类型化 DTO。
 */
function getDefaultOrchestratorConfig(): Promise<OrchestratorAutomationConfig> {
  return invoke<OrchestratorAutomationConfig>('get_default_orchestrator_config');
}

/**
 * 更新 Orchestrator 自动化配置
 *
 * Business Logic（为什么需要这个函数）:
 *   用户在 Settings 自动化 tab 点击应用配置后，需要把表单 patch 持久化到当前设备全局配置。
 *
 * Code Logic（这个函数做什么）:
 *   调用 Rust `update_orchestrator_config` 命令，按后端签名把 patch 包在 `{ patch }` 参数中。
 */
function updateOrchestratorConfig(
  patch: OrchestratorAutomationConfigPatch,
): Promise<OrchestratorAutomationConfig> {
  return invoke<OrchestratorAutomationConfig>('update_orchestrator_config', { patch });
}

export const orchestratorConfigApi = {
  get: getOrchestratorConfig,
  getDefaults: getDefaultOrchestratorConfig,
  update: updateOrchestratorConfig,
};
