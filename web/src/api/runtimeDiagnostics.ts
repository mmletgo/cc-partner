/**
 * 运行时脱敏诊断 API
 *
 * Business Logic（为什么需要这个模块）:
 *   Settings 依赖环境页需要读取 sidecar owner 诊断，并打开日志目录；
 *   复制摘要只能包含 counts/phases/error codes。
 *
 * Code Logic（这个模块做什么）:
 *   封装 get_runtime_diagnostics / open_backend_log_dir invoke。
 */

import { invoke } from './client';

/**
 * 单 bridge 脱敏快照
 *
 * Business Logic（为什么需要这个接口）:
 *   诊断只展示相位与错误类别，禁止 URL/token/内容。
 *
 * Code Logic（这个接口做什么）:
 *   对齐后端 RemoteEventBridgeSnapshot camelCase。
 */
export interface RuntimeBridgeSnapshot {
  phase: string;
  attempt: number;
  lastErrorClass?: string | null;
}

/**
 * Orchestrator 轻量摘要
 *
 * Business Logic（为什么需要这个接口）:
 *   诊断条展示最近 tick 与错误类别。
 *
 * Code Logic（这个接口做什么）:
 *   对齐 OrchestratorRuntimeSummary。
 */
export interface RuntimeOrchestratorSummary {
  latestTickAt?: string | null;
  latestErrorClass?: string | null;
}

/**
 * 脱敏运行诊断
 *
 * Business Logic（为什么需要这个接口）:
 *   复制到剪贴板的 JSON 必须无 secret/content。
 *
 * Code Logic（这个接口做什么）:
 *   对齐 SanitizedRuntimeDiagnostics camelCase。
 */
export interface SanitizedRuntimeDiagnostics {
  ownerInstanceId: string;
  generation: number;
  startedAt: string;
  configFingerprint: string;
  cloudSyncPhase: string;
  terminalSessionCount: number;
  bridgeCount: number;
  bridges: RuntimeBridgeSnapshot[];
  orchestrator: RuntimeOrchestratorSummary;
}

/**
 * 禁止出现在复制诊断中的敏感键片段（测试与防御扫描共用）。
 *
 * Business Logic（为什么需要这个常量）:
 *   复制摘要不得含 token/content/prompt 等字段。
 *
 * Code Logic（这个常量做什么）:
 *   小写子串列表，供 includes 扫描。
 */
export const RUNTIME_DIAGNOSTICS_FORBIDDEN_KEYS = [
  'token',
  'content',
  'prompt',
  'password',
  'authorization',
  'controltoken',
  'baseurl',
] as const;

/**
 * 扫描诊断 JSON 是否含敏感键名。
 *
 * Business Logic（为什么需要这个函数）:
 *   单元测试与复制前防御需同一规则。
 *
 * Code Logic（这个函数做什么）:
 *   对 JSON 字符串小写扫描 forbidden 子串；返回命中列表。
 *
 * @param json 序列化诊断
 * @returns 命中的 forbidden 片段
 */
export function findForbiddenDiagnosticsKeys(json: string): string[] {
  const lower = json.toLowerCase();
  return RUNTIME_DIAGNOSTICS_FORBIDDEN_KEYS.filter((key) => lower.includes(key));
}

/**
 * 将诊断对象格式化为可复制 JSON。
 *
 * Business Logic（为什么需要这个函数）:
 *   复制按钮需要稳定 pretty JSON。
 *
 * Code Logic（这个函数做什么）:
 *   JSON.stringify pretty；失败回落 String(value)。
 *
 * @param diagnostics 诊断对象
 * @returns pretty JSON
 */
export function formatDiagnosticsForCopy(diagnostics: SanitizedRuntimeDiagnostics): string {
  try {
    return JSON.stringify(diagnostics, null, 2);
  } catch {
    return String(diagnostics);
  }
}

export const runtimeDiagnosticsApi = {
  /** 读取 sidecar 脱敏诊断。 */
  get: () => invoke<SanitizedRuntimeDiagnostics>('get_runtime_diagnostics'),
  /** 打开后端日志目录。 */
  openLogDir: () => invoke<void>('open_backend_log_dir'),
};
