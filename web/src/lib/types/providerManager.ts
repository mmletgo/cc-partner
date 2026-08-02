/**
 * Provider Manager 类型（对齐 Rust `provider_manager::models` 的 camelCase DTO）。
 *
 * Business Logic（为什么需要这个模块）:
 *   cc-partner 联动 cc-switch：列出每个 agent 在 cc-switch 已配置的 provider 并切换当前
 *   provider（不编辑详情）。DTO 只含展示所需字段，绝不携带 `settings_config`（API key）。
 *
 * Code Logic（这个模块做什么）:
 *   定义 AgentApp 联合类型、CLI/GUI 检测状态、provider 列表与安装结果。
 */

/** cc-switch-cli 支持的 agent（`--app` 目标集合；`claude-desktop` 不在内）。 */
export type AgentApp = 'claude' | 'codex' | 'gemini' | 'opencode' | 'hermes' | 'openclaw';

/** cc-switch CLI 检测结果（按行为判定，非按名字）。 */
export interface CliStatus {
  available: boolean;
  /** 解析到的绝对路径。 */
  path: string | null;
  /** `cc-switch --version` 输出。 */
  version: string | null;
}

/** cc-switch GUI 检测结果（best-effort，只读，从不启动/修改 GUI）。 */
export interface CcSwitchGuiStatus {
  installed: boolean;
  version: string | null;
  /** v1 不检测运行态（避免每次轮询都跑 ps）；null 表示未知。 */
  running: boolean | null;
  /** CLI 与 GUI 主版本不一致时为 true，用于提示"对齐版本"。 */
  versionMismatch: boolean | null;
}

/** 单个 provider 摘要（不含 settings_config/secret）。 */
export interface ProviderEntry {
  id: string;
  name: string;
  category: string | null;
  isCurrent: boolean;
}

/** 某 agent 下全部 provider 及其当前 provider id。 */
export interface AppProviders {
  app: AgentApp;
  providers: ProviderEntry[];
  currentProviderId: string | null;
}

/** Provider Manager 整体状态快照。 */
export interface ProviderManagerSummary {
  ccSwitchDbPresent: boolean;
  cli: CliStatus;
  /** 非 macOS 平台 v1 不检测 GUI，返回 null（前端按未知处理）。 */
  gui: CcSwitchGuiStatus | null;
  apps: AppProviders[];
}

/** 安装 cc-switch CLI 的结果（method: brew|manual）。 */
export interface InstallResult {
  method: string;
  ok: boolean;
  version: string | null;
  path: string | null;
  message: string | null;
  url: string | null;
}
