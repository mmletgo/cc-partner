/**
 * Multi-CLI Agent Hub 域类型。
 *
 * Business Logic（为什么需要这个模块）:
 *   Agent Hub 统一管理 Claude / Codex / OpenCode 的指令与资产投影；
 *   前端页面、Attention deep link 与 IPC schema 必须共享稳定 DTO。
 *
 * Code Logic（这个模块做什么）:
 *   导出 target/status/asset/block/preview 等 camelCase 契约类型。
 */

/**
 * CLI 目标运行时。
 *
 * Business Logic: 投影与适配必须区分三端路径与能力。
 * Code Logic: wire token 为 claude / codex / opencode。
 */
export type AgentTarget = 'claude' | 'codex' | 'opencode';

/**
 * CLI probe 支持级别。
 *
 * Business Logic: UI 需展示 full/partial/scanOnly/unsupported 与后端 supported 等价语义。
 * Code Logic: 字面量联合，额外允许未知前向兼容字符串。
 */
export type AgentHubSupportLevel =
  | 'full'
  | 'partial'
  | 'scanOnly'
  | 'unsupported'
  | 'supported'
  | string;

/**
 * 目标绑定期望存在性。
 *
 * Business Logic: 决定是否应投影到对应 CLI。
 * Code Logic: present | absent。
 */
export type DesiredPresence = 'present' | 'absent';

/**
 * 指令块共享策略。
 *
 * Business Logic: shared/adapted/targetOnly 决定跨 CLI 正文分配。
 * Code Logic: camelCase wire token。
 */
export type InstructionBlockMode = 'shared' | 'adapted' | 'targetOnly';

/**
 * Materialization 状态。
 *
 * Business Logic: UI 需解释同步、漂移、阻塞与失败。
 * Code Logic: 已知 token 联合 + 前向兼容 string。
 */
export type MaterializationStatus =
  | 'synced'
  | 'pending'
  | 'blocked'
  | 'drifted'
  | 'drift'
  | 'detached'
  | 'failed'
  | 'writing'
  | 'conflict'
  | 'unsupported'
  | 'activationRequired'
  | 'externalCollision'
  | string;

/**
 * 单 CLI 探测结果。
 *
 * Business Logic: 页面顶部展示可执行文件、版本与支持级别。
 * Code Logic: camelCase probe DTO。
 */
export interface AgentHubProbe {
  target: AgentTarget;
  executable?: string | null;
  version?: string | null;
  support: string;
  configRoot?: string | null;
}

/**
 * Agent Hub 运行时状态。
 *
 * Business Logic: 首屏加载与升级门闸依赖 enabled / writeCompatible / 冲突计数。
 * Code Logic: camelCase status DTO。
 */
export interface AgentHubStatus {
  enabled: boolean;
  backgroundEnabled: boolean;
  agentHubApiVersion: number;
  ownerInstanceId?: string | null;
  writeCompatible: boolean;
  probes: AgentHubProbe[];
  conflictCount: number;
  blockedMaterializationCount: number;
}

/**
 * 资产在某一 target 上的单元格。
 *
 * Business Logic: 列表行按 Claude/Codex/OpenCode 三列展示投影状态。
 * Code Logic: desired presence + enabled + optional materialization。
 */
export interface AgentHubTargetCell {
  target: AgentTarget;
  desiredPresence: DesiredPresence;
  desiredEnabled: boolean;
  materializationStatus?: MaterializationStatus | null;
  lastError?: string | null;
}

/**
 * 资产列表摘要。
 *
 * Business Logic: Hub 列表以 instruction 为主展示逻辑资产。
 * Code Logic: 含 targets 三元单元格与冲突标记。
 */
export interface AgentHubAssetSummary {
  assetId: string;
  scopeId: string;
  kind: string;
  displayName: string;
  logicalKey: string;
  originNamespace: string;
  policy: string;
  currentRevisionId?: string | null;
  targets: AgentHubTargetCell[];
  hasConflict?: boolean;
}

/**
 * 指令块 DTO。
 *
 * Business Logic: InstructionBlocksDrawer 编辑 shared/adapted/targetOnly 与变体。
 * Code Logic: commonMarkdown + optional variants map。
 */
export interface InstructionBlockDto {
  id: string;
  mode: InstructionBlockMode;
  commonMarkdown: string;
  variants?: Partial<Record<AgentTarget, string>> | null;
  headingPath?: string[] | null;
  sourceTarget?: AgentTarget | null;
  needsAdaptation?: boolean;
}

/**
 * 资产冲突摘要。
 *
 * Business Logic: 冲突抽屉按 conflictId 展示并可 resolve。
 * Code Logic: 最小字段集 + 可选 target/detail。
 */
export interface AgentHubConflictDto {
  id: string;
  target?: AgentTarget | null;
  detailJson?: string;
  createdAt: string;
}

/**
 * 资产详情（含 blocks）。
 *
 * Business Logic: 选中资产后加载块结构与冲突列表。
 * Code Logic: 扩展 summary + blocks/content/conflicts。
 */
export interface AgentHubAssetDetail extends AgentHubAssetSummary {
  blocks?: InstructionBlockDto[];
  contentMarkdown?: string | null;
  conflicts?: AgentHubConflictDto[];
}

/**
 * 项目启用预览（宽松对象）。
 *
 * Business Logic: Dialog 展示 checkout / plannedActions / 不 commit 承诺。
 * Code Logic: 索引签名 + 常见字段可选。
 */
export interface AgentHubProjectPreview {
  projectId?: string;
  hubProjectId?: string | null;
  path?: string;
  optedIn?: boolean;
  checkouts?: unknown[];
  plannedActions?: unknown[];
  warnings?: string[];
  noCommitNotice?: string;
  gitRemoteFingerprint?: string | null;
  [key: string]: unknown;
}

/**
 * 项目启用结果。
 *
 * Business Logic: enable 成功后刷新列表。
 * Code Logic: 宽松 camelCase 对象。
 */
export interface AgentHubProjectStatus {
  projectId?: string;
  hubProjectId?: string;
  optedIn?: boolean;
  warnings?: string[];
  [key: string]: unknown;
}

/**
 * 列表 + 状态组合快照（schema 测试入口）。
 *
 * Business Logic: 首屏并行加载 status/assets 可视为 snapshot。
 * Code Logic: status + assets。
 */
export interface AgentHubSnapshot {
  status: AgentHubStatus;
  assets: AgentHubAssetSummary[];
}
