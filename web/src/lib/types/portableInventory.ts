/**
 * Portable inventory / local action / same-agent pull 域类型。
 *
 * Business Logic（为什么需要这个模块）:
 *   Agent Hub 四类资产管理 UI 必须消费后端冻结的严格 camelCase DTO；
 *   本机 inventory 是 actual 状态真源；MCP 仅 present/hash，无 secret 原文。
 *
 * Code Logic（这个模块做什么）:
 *   从 backend portable_inventory/portable_actions/replication/pull 模型原样编码 wire types；
 *   禁止 loose typing、禁止发明 optional default。
 */

import type { AgentTarget, DesiredPresence } from './agentHub';

/** Portable 四类资产。 */
export type PortableAssetKind = 'skill' | 'command' | 'plugin' | 'mcp';

/** 库存对账管理状态。 */
export type PortableInventoryManagementState =
  | 'unmanaged'
  | 'hubManaged'
  | 'drifted'
  | 'externalCollision'
  | 'unsupported';

/** 扫描到的资产来源命名空间。 */
export type PortableInventorySourceOrigin = 'standalone' | 'pluginComponent' | 'nativeConfig';

/** Target 扫描能力。 */
export type PortableInventoryScanCapability = 'supported' | 'readOnly' | 'blocked';

/** Target 写入/动作能力。 */
export type PortableInventoryMutationCapability = 'supported' | 'previewOnly' | 'blocked';

/** Scope kind（与 backend ScopeKind 对齐）。 */
export type PortableScopeKind = 'user' | 'project' | 'directory';

/** 本机 portable 资产动作类型。 */
export type PortableAssetActionKind =
  | 'adopt'
  | 'enable'
  | 'disable'
  | 'uninstall'
  | 'installToSourceTarget';

/** 单条动作结果状态。 */
export type PortableAssetActionItemState =
  | 'succeeded'
  | 'skipped'
  | 'failed'
  | 'blocked'
  | 'outcomeUnknown';

/** Canonical/ownership 影响。 */
export type PortableAssetCanonicalEffect =
  | 'none'
  | 'createOwnership'
  | 'updateDesired'
  | 'tombstoneComponents';

/** 文件/CLI 操作类别。 */
export type PortableAssetPlanOperation =
  | 'leave'
  | 'enable'
  | 'disable'
  | 'uninstall'
  | 'install'
  | 'adopt';

/** 备份策略。 */
export type PortableAssetBackupPolicy = 'none' | 'recoverableBeforeDelete';

/** 覆盖/冲突策略。 */
export type PortableAssetConflictPolicy = 'skipExisting' | 'replaceAfterPreview';

/** Pull 安装模式。 */
export type PortablePullInstallMode =
  | 'installToTarget'
  | 'importedCanonicalOnly'
  | 'skipExisting'
  | 'blocked';

/** Pull 逐项状态。 */
export type PortablePullItemState =
  | 'succeeded'
  | 'skipped'
  | 'failed'
  | 'blocked'
  | 'importedCanonicalOnly'
  | 'outcomeUnknown';

/** 单项动作能力摘要。 */
export interface PortableInventoryItemCapabilitiesDto {
  canEnable: boolean;
  canDisable: boolean;
  canUninstall: boolean;
  canAdopt: boolean;
  canInstallToSourceTarget: boolean;
  reasonCode: string | null;
  evidenceIds: string[];
}

/**
 * MCP 凭据观测事实（仅 present/hash）。
 *
 * Business Logic: 绝不携带 secret/token/value 原文。
 */
export interface PortableMcpCredentialFactDto {
  present: boolean;
  hash: string | null;
}

/** 单 target 探测与能力事实。 */
export interface PortableInventoryTargetDto {
  target: AgentTarget;
  installed: boolean;
  version: string | null;
  executable: string | null;
  configRoot: string;
  scanCapability: PortableInventoryScanCapability;
  mutationCapability: PortableInventoryMutationCapability;
  reasonCode: string | null;
  evidenceIds: string[];
}

/** 单条库存项（观测事实 + 对账摘要）。 */
export interface PortableInventoryItemDto {
  inventoryItemId: string;
  target: AgentTarget;
  kind: PortableAssetKind;
  nativeId: string;
  displayName: string;
  description: string | null;
  version: string | null;
  scopeId: string;
  scopeKind: PortableScopeKind;
  projectId: string | null;
  projectOptedIn: boolean;
  sourcePath: string | null;
  sourceOrigin: PortableInventorySourceOrigin;
  parentPluginInventoryItemId: string | null;
  actualEnabled: boolean | null;
  contentHash: string | null;
  treeHash: string | null;
  canonicalAssetId: string | null;
  canonicalRevisionId: string | null;
  managementState: PortableInventoryManagementState;
  desiredPresence: DesiredPresence | null;
  desiredEnabled: boolean | null;
  materializationStatus: string | null;
  capabilities: PortableInventoryItemCapabilitiesDto;
  warnings: string[];
  /** 仅 MCP 可能出现；非 MCP 常省略。 */
  mcpCredential?: PortableMcpCredentialFactDto | null;
}

/** 完整库存快照。 */
export interface PortableInventorySnapshotDto {
  inventorySnapshotHash: string;
  refreshedAt: string;
  stale: boolean;
  targets: PortableInventoryTargetDto[];
  items: PortableInventoryItemDto[];
}

/** Inventory 扫描过滤条件；全空表示完整扫描。 */
export interface PortableInventoryQuery {
  target?: AgentTarget;
  kind?: PortableAssetKind;
  scopeKind?: PortableScopeKind;
  /** 本机 Workbench 项目 id；后端必须先解析为唯一 Hub project id。 */
  localProjectId?: string;
}

/** Preview 本机动作请求。 */
export interface PreviewPortableAssetActionRequest {
  inventorySnapshotHash: string;
  /** 必须与生成 inventorySnapshotHash 的扫描条件一致。 */
  inventoryQuery?: PortableInventoryQuery;
  inventoryItemIds: string[];
  action: PortableAssetActionKind;
  keepData: boolean;
  conflictPolicy: PortableAssetConflictPolicy;
  expectedCanonicalRevisionId: string | null;
  /** 用户级设备上下文：null/缺省=本机；非空=peer（T7 前端 fail-closed）。 */
  deviceId?: string | null;
  /** 项目级身份：本机 workbench id 或 remote:…（T7 远端 fail-closed）。 */
  projectRef?: string | null;
}

/** 单条 preview 变更。 */
export interface PortableAssetActionChangeDto {
  inventoryItemId: string;
  target: AgentTarget;
  kind: PortableAssetKind;
  path: string | null;
  operation: PortableAssetPlanOperation;
  expectedSourceHash: string | null;
  expectedTreeHash: string | null;
  expectedCanonicalRevisionId: string | null;
  backupPolicy: PortableAssetBackupPolicy;
  createsOwnership: boolean;
  canonicalEffect: PortableAssetCanonicalEffect;
  blockingReasons: string[];
  warnings: string[];
}

/** 短期 preview plan。 */
export interface PortableAssetActionPlanDto {
  planToken: string;
  expiresAt: string;
  inventorySnapshotHash: string;
  action: PortableAssetActionKind;
  keepData: boolean;
  conflictPolicy: PortableAssetConflictPolicy;
  changes: PortableAssetActionChangeDto[];
  blockingReasons: string[];
}

/** Apply 本机动作请求。 */
export interface ApplyPortableAssetActionRequest {
  planToken: string;
  clientRequestId: string;
  /** 用户级设备上下文：null/缺省=本机；非空=peer（T7 前端 fail-closed）。 */
  deviceId?: string | null;
  /** 项目级身份：本机 workbench id 或 remote:…（T7 远端 fail-closed）。 */
  projectRef?: string | null;
}

/** 单条 apply 结果。 */
export interface PortableAssetActionItemResultDto {
  inventoryItemId: string;
  state: PortableAssetActionItemState;
  errorCode: string | null;
  message: string | null;
}

/** Apply 聚合结果。 */
export interface PortableAssetActionResultDto {
  planToken: string;
  clientRequestId: string;
  items: PortableAssetActionItemResultDto[];
}

/** 远端 inventory 单项（metadata only，无 path/secret）。 */
export interface RemotePortableInventoryItemDto {
  inventoryItemId: string;
  target: AgentTarget;
  kind: PortableAssetKind;
  nativeId: string;
  displayName: string;
  description: string | null;
  version: string | null;
  scopeId: string;
  projectId: string | null;
  projectOptedIn: boolean;
  sourceOrigin: PortableInventorySourceOrigin;
  actualEnabled: boolean | null;
  contentHash: string | null;
  treeHash: string | null;
  warnings: string[];
  mcpCredential?: PortableMcpCredentialFactDto | null;
}

/** 远端 portable inventory（metadata only）。 */
export interface RemotePortableInventoryDto {
  sourceDeviceId: string;
  sourceTarget: AgentTarget;
  inventorySnapshotHash: string;
  refreshedAt: string;
  stale: boolean;
  items: RemotePortableInventoryItemDto[];
}

/** 列出远端 inventory 请求。 */
export interface ListRemotePortableInventoryRequest {
  sourceDeviceId: string;
  sourceTarget: AgentTarget;
  /** owning peer 的本地 Workbench 项目 id；缺省为 user scope。 */
  sourceLocalProjectId?: string;
  /** 本机保存的 remote shortcut id；后端解析为 owning peer local project id。 */
  sourceProjectRef?: string;
}

/** Pull preview 请求。 */
export interface PreviewPortablePullRequest {
  sourceDeviceId: string;
  sourceTarget: AgentTarget;
  destinationTarget: AgentTarget;
  sourceLocalProjectId?: string;
  sourceProjectRef?: string;
  destinationLocalProjectId?: string;
  remoteInventorySnapshotHash: string;
  inventoryItemIds: string[];
  conflictPolicy: PortableAssetConflictPolicy;
}

/** 单条 pull 变更预览。 */
export interface PortablePullChangeDto {
  inventoryItemId: string;
  kind: PortableAssetKind;
  nativeId: string;
  displayName: string;
  installMode: PortablePullInstallMode;
  conflict: boolean;
  legacyLossy: boolean;
  credentialBearing: boolean;
  blockingReasons: string[];
  warnings: string[];
}

/** Pull plan（短期）。 */
export interface PortablePullPlanDto {
  planToken: string;
  expiresAt: string;
  sourceDeviceId: string;
  sourceTarget: AgentTarget;
  destinationTarget: AgentTarget;
  sourceLocalProjectId?: string;
  sourceProjectRef?: string;
  destinationLocalProjectId?: string;
  remoteInventorySnapshotHash: string;
  localInventorySnapshotHash: string;
  conflictPolicy: PortableAssetConflictPolicy;
  selectionManifestHash: string;
  credentialBearingCount: number;
  hasCredentialBearingAssets: boolean;
  changes: PortablePullChangeDto[];
  blockingReasons: string[];
}

/** Apply pull 请求。 */
export interface ApplyPortablePullRequest {
  planToken: string;
  clientRequestId: string;
}

/** 单条 pull 结果。 */
export interface PortablePullItemResultDto {
  inventoryItemId: string;
  state: PortablePullItemState;
  installMode: PortablePullInstallMode | null;
  errorCode: string | null;
  message: string | null;
}

/** Pull 聚合结果。 */
export interface PortablePullResultDto {
  planToken: string;
  clientRequestId: string;
  sourceDeviceId: string;
  sourceTarget: AgentTarget;
  destinationTarget: AgentTarget;
  partial: boolean;
  items: PortablePullItemResultDto[];
}

/** inspect / mutation 可选设备与项目上下文。 */
export interface PortableInventoryRequestContext {
  deviceId?: string | null;
  projectRef?: string | null;
  target?: AgentTarget;
  kind?: PortableAssetKind;
  scopeKind?: PortableScopeKind;
  /** 本机 Workbench 项目 id；仅 project scope 使用。 */
  localProjectId?: string;
}

/** 本机 portable 资产 API 形状（controller 消费）。 */
export interface PortableAssetApi {
  inspect(context?: PortableInventoryRequestContext): Promise<PortableInventorySnapshotDto>;
  previewAction(request: PreviewPortableAssetActionRequest): Promise<PortableAssetActionPlanDto>;
  applyAction(request: ApplyPortableAssetActionRequest): Promise<PortableAssetActionResultDto>;
  getAction(
    clientRequestId: string,
    context?: PortableInventoryRequestContext,
  ): Promise<PortableAssetActionResultDto>;
}

/** 同类远端 Pull API 形状。 */
export interface PortablePullApi {
  listRemote(request: ListRemotePortableInventoryRequest): Promise<RemotePortableInventoryDto>;
  preview(request: PreviewPortablePullRequest): Promise<PortablePullPlanDto>;
  apply(request: ApplyPortablePullRequest): Promise<PortablePullResultDto>;
  get(clientRequestId: string): Promise<PortablePullResultDto>;
}
