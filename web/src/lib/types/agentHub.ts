/**
 * Multi-CLI Agent Hub 域类型。
 *
 * Business Logic（为什么需要这个模块）:
 *   Agent Hub 统一管理 catalog Hub target 的指令与资产投影；
 *   前端页面、Attention deep link 与 IPC schema 必须共享稳定 DTO。
 *
 * Code Logic（这个模块做什么）:
 *   导出 target/status/asset/block/preview 等 camelCase 契约类型。
 */

/**
 * CLI 目标运行时。
 *
 * Business Logic: 投影与适配必须区分各端路径与能力。
 * Code Logic: wire token 与 `allHubTargets()` 对齐。
 */
export type AgentTarget = 'claude' | 'codex' | 'opencode' | 'grok' | 'gemini' | 'cursor' | 'pi';

/**
 * 用户级指令 V2 设置阶段。
 *
 * Business Logic: 未管理任何目标时必须呈现可执行的首次设置，而不是 legacy partial。
 * Code Logic: 与 Spec 9.6/10.2 wire token 对齐。
 */
export type UserInstructionSetupState = 'unconfigured' | 'readyToReview' | 'configured';

/** 用户级指令 V2 健康阶段。 */
export type UserInstructionHealthState = 'healthy' | 'actionRequired' | 'blocked';

/** 用户级指令来源角色。 */
export type UserInstructionSourceRole = 'native' | 'override' | 'fallback' | 'shadowed';

/** 用户级指令来源所有权。 */
export type UserInstructionSourceOwnership = 'external' | 'hubManaged' | 'unknown';

/** Hub 对单个 Agent 的管理模式。 */
export type UserInstructionManagementMode = 'unmanaged' | 'managedActive' | 'managedPaused';

/** 用户级指令投影状态。 */
export type UserInstructionProjectionState =
  | 'none'
  | 'pending'
  | 'inSync'
  | 'drift'
  | 'detached'
  | 'conflict'
  | 'collision'
  | 'activationRequired'
  | 'failed'
  | 'blocked';

/**
 * 后端根据 capability/ownership/state 计算的安全动作。
 *
 * Business Logic: view 不得从零散布尔字段猜测危险操作是否可用。
 * Code Logic: 当前 V2 动作闭集；新增动作需同步 schema 与动作矩阵。
 */
export type UserInstructionAction =
  | 'manage'
  | 'pause'
  | 'resume'
  | 'stopManaging'
  | 'remove'
  | 'compare'
  | 'adopt'
  | 'restore'
  | 'deleteAsset'
  | 'openFile';

/** 用户级指令的原生/回退来源。 */
export interface UserInstructionSourceDto {
  sourceId: string;
  path: string;
  role: UserInstructionSourceRole;
  active: boolean;
  exists: boolean;
  nonEmpty: boolean;
  hash: string | null;
  modifiedAt: string | null;
  ownership: UserInstructionSourceOwnership;
  /** source resolver 的稳定诊断，例如 fallback 被环境变量禁用或文件过大。 */
  reasonCode?: string | null;
  /**
   * 磁盘 UTF-8 正文（有界）。
   * Business Logic: 打开提示词原始栏直接展示本机已有文件。
   * Code Logic: 仅 active（或无 active 时首个现存）源携带；过大/非 UTF-8 时为 null/undefined。
   */
  content?: string | null;
  /** 正文被截断时不得用于覆盖写回。 */
  contentTruncated?: boolean;
}

/** 单 Agent 的动作级能力。 */
export interface UserInstructionCapabilityDto {
  scan: 'supported' | 'readOnly' | 'blocked';
  write: 'supported' | 'blocked';
  remove: 'supported' | 'blocked';
  activate: 'immediate' | 'newSession' | 'restart' | 'unknown' | 'blocked';
  reasonCode: string | null;
  evidenceIds: string[];
}

/** 单 Agent 的当前投影事实。 */
export interface UserInstructionProjectionDto {
  state: UserInstructionProjectionState;
  desiredRevisionId: string | null;
  appliedRevisionId: string | null;
  observedHash: string | null;
  lastErrorCode: string | null;
}

/** 用户级指令 target overview。 */
export interface UserInstructionTargetDto {
  target: AgentTarget;
  cli: {
    installed: boolean;
    version: string | null;
    configRoot: string;
  };
  sources: UserInstructionSourceDto[];
  effectiveSourceId: string | null;
  managedTargetPath: string | null;
  managementMode: UserInstructionManagementMode;
  capability: UserInstructionCapabilityDto;
  projection: UserInstructionProjectionDto;
  availableActions: UserInstructionAction[];
}

/** 用户级指令 canonical 内容。 */
export interface UserInstructionCanonicalDto {
  assetId: string;
  displayName: string;
  headRevisionId: string | null;
  commonContent: string;
  targetExtensions: Partial<Record<AgentTarget, string>>;
  deleted: boolean;
  /** 受 256 KiB 内容上限截断时，当前正文不得用于生成覆盖 plan。 */
  contentTruncated: boolean;
  /**
   * 块模型（与 commonContent/targetExtensions 同源 InstructionDocument）。
   * Business Logic: 三栏据此 hydrate 块/预览，无需从原文 parse。
   */
  blocks?: InstructionBlockDto[];
}

/**
 * 用户级指令专用工作区 DTO。
 *
 * Business Logic: 首屏只消费事实化来源、管理模式、能力和投影状态。
 * Code Logic: 对齐 Spec 10.2；不携带 portable ledger 内部字段。
 */
export interface UserInstructionWorkspaceDto {
  scopeId: string;
  setupState: UserInstructionSetupState;
  healthState: UserInstructionHealthState;
  canonical: UserInstructionCanonicalDto | null;
  targets: UserInstructionTargetDto[];
  inventorySnapshotHash: string;
  refreshedAt: string;
}

/** 读取用户级原生提示词文件。 */
export interface ReadUserNativeInstructionFileRequest {
  path: string;
}

/** 写入用户级原生提示词文件。 */
export interface WriteUserNativeInstructionFileRequest {
  path: string;
  content: string;
  expectedHash: string | null;
}

/** 用户级原生提示词文件快照。 */
export interface UserNativeInstructionFileDto {
  path: string;
  exists: boolean;
  content: string;
  hash: string | null;
  truncated: boolean;
  created: boolean;
}

/** 首次设置/日常更新中用户对 target 的明确选择（简写三态）。 */
export type UserInstructionTargetSelectionMode = 'managed' | 'unmanaged' | 'inherit';

/**
 * 用户级 target 选择：简写 mode，或带 adoptExisting 的详细形状。
 * Business Logic: 编辑本机已有 external 文件并写回时必须 adoptExisting，否则 apply 会 OWNERSHIP_REQUIRED。
 */
export type UserInstructionTargetSelection =
  | UserInstructionTargetSelectionMode
  | {
      managementMode: UserInstructionManagementMode;
      adoptExisting?: boolean;
      manageOverride?: boolean;
    };

/** 用户级指令公共与各 Agent 专属草稿。 */
export interface UserInstructionDraft {
  commonContent: string;
  targetExtensions: Partial<Record<AgentTarget, string>>;
  targetSelections: Record<AgentTarget, UserInstructionTargetSelection>;
}

/** 生成 setup/update preview 的输入。 */
export interface UserInstructionPreviewRequest extends UserInstructionDraft {
  baseRevisionId: string | null;
  inventorySnapshotHash: string;
}

/**
 * 保存块文档请求（baseRevisionId head CAS + inventorySnapshotHash 防 stale）。
 * Business Logic: 三栏编辑块后持久化 canonical，独立于 CLI 写入门禁；返回新 canonical。
 */
export interface SaveUserInstructionBlocksRequest {
  blocks: InstructionBlockDto[];
  baseRevisionId: string | null;
  inventorySnapshotHash: string;
}

/** 用户级指令 plan 中单 target 的路径级变化。 */
export interface UserInstructionPlanChangeDto {
  target: AgentTarget;
  path: string;
  operation: 'create' | 'update' | 'delete' | 'leave';
  currentHash: string | null;
  expectedHash: string | null;
  renderedHash: string | null;
  unifiedDiff: string | null;
  ownershipRequired: boolean;
  willShadowSourcePath: string | null;
  willReplaceFallbackSourcePath: string | null;
  emptyDueToTargetOnly: boolean;
  activation: 'immediate' | 'newSession' | 'restart' | 'unknown';
  warnings: string[];
  /** sidecar 1 MiB response ceiling 下，diff 可被安全截断。 */
  diffTruncated?: boolean;
}

/** 用户确认前的零写入 plan。 */
export interface UserInstructionPlanDto {
  planToken: string;
  expiresAt: string;
  baseRevisionId: string | null;
  inventorySnapshotHash: string;
  changes: UserInstructionPlanChangeDto[];
  blockingReasons: string[];
  /** 响应接近 sidecar hard limit 时，UI 必须明确提示预览不完整。 */
  truncated?: boolean;
  warnings?: string[];
}

/** plan apply 的单 target 真实结果。 */
export interface UserInstructionApplyTargetResultDto {
  target: AgentTarget;
  path: string;
  status: 'queued' | 'applied' | 'noChange' | 'stalePreview' | 'blocked' | 'conflict' | 'failed';
  errorCode: string | null;
  activation: 'immediate' | 'newSession' | 'restart' | 'unknown' | 'blocked';
}

/** plan apply 结果，不能压缩成单一 success。 */
export interface UserInstructionApplyResultDto {
  planToken: string;
  setupState: UserInstructionSetupState;
  healthState: UserInstructionHealthState;
  targets: UserInstructionApplyTargetResultDto[];
}

/** target-local preview 的统一请求。 */
export interface UserInstructionTargetPreviewRequest {
  target: AgentTarget;
  baseRevisionId: string | null;
  inventorySnapshotHash: string;
}

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
 * 资产聚合状态（Gate B Task 7/8）。
 *
 * Business Logic: 未选择任何目标时必须显示“未配置”，不能误报 partial。
 * Code Logic: 严格 wire token。
 */
export type AssetAggregateStatus =
  | 'unconfigured'
  | 'full'
  | 'partial'
  | 'sourceOnly'
  | 'activationRequired'
  | 'externalCollision'
  | 'detached'
  | 'blocked';

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
 * Business Logic: 列表行按 Claude/Codex/OpenCode 三列展示投影状态与聚合输入。
 * Code Logic: desired + materialization + requested/supported/sourceOnly/verified。
 */
export interface AgentHubTargetCell {
  target: AgentTarget;
  desiredPresence: DesiredPresence;
  desiredEnabled: boolean;
  materializationStatus?: MaterializationStatus | null;
  lastError?: string | null;
  /** 是否在 requested 集合（有 binding 行） */
  requested: boolean;
  /** 目标当前是否 supported */
  supported: boolean;
  /** 是否仅 sourceOnly（无可投影 materialization） */
  sourceOnly: boolean;
  /** 是否 verified（package activation/list 通过；指令 Synced 即 verified） */
  verified: boolean;
  /**
   * 可选 materialized invocation alias（若后端未来透传）。
   * Business Logic: 与 canonical displayName 分开展示。
   */
  invocationAlias?: string | null;
}

/**
 * 资产列表摘要。
 *
 * Business Logic: Hub 列表以 instruction/portable 资产为主展示逻辑资产。
 * Code Logic: 含 targets 三元单元格、冲突标记与 aggregateStatus。
 */
export interface AgentHubAssetSummary {
  assetId: string;
  scopeId: string;
  kind: string;
  /** Canonical 名称（与 materialized alias 分离） */
  displayName: string;
  logicalKey: string;
  originNamespace: string;
  policy: string;
  currentRevisionId?: string | null;
  targets: AgentHubTargetCell[];
  hasConflict?: boolean;
  /**
   * 派生聚合状态：full|partial|sourceOnly|activationRequired|externalCollision|detached|blocked
   */
  aggregateStatus: AssetAggregateStatus;
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
 * 三槽历史快照的逻辑槽位标识（与后端 `InstructionSlotKey` 1:1）。
 *
 * Business Logic: 公共槽不分 agent；适配/独有槽按 lane × agent 各 1 个
 *   item_id；前端用此打开历史抽屉、定位 `content_versions.item_id`。
 *
 * Code Logic: discriminated union（tag = `kind`）；adapted/targetOnly 携带 agent；
 *   与后端 serde tag 对齐，wire 直接通过 camelCase 解析。
 */
export type InstructionSlotKey =
  | { kind: 'shared' }
  | { kind: 'adapted'; agent: AgentTarget }
  | { kind: 'targetOnly'; agent: AgentTarget };

/**
 * 三槽历史列表请求 / 恢复请求。
 *
 * Business Logic: 前端按 lane × agent 打开历史抽屉与恢复版本时统一传入；
 *   `inventorySnapshotHash` 与 `baseRevisionId` 与 `saveBlocks` 同口径 CAS。
 *
 * Code Logic: 与后端 Rust request DTO 字段顺序 + 命名一致；恢复请求
 *   `baseRevisionId` 可空（首版 canonical head 为 null）。
 */
export interface ListUserInstructionSlotVersionsRequest {
  assetId: string;
  slot: InstructionSlotKey;
}

export interface RestoreUserInstructionSlotRequest {
  assetId: string;
  slot: InstructionSlotKey;
  versionId: string;
  baseRevisionId: string | null;
  inventorySnapshotHash: string;
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
  /**
   * Plugin package 投影报告（kind=plugin 时可选透传）。
   * Business Logic: Drawer 消费 per-component matrix / delete preview。
   */
  pluginReport?: PluginPackageReport | null;
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

/**
 * externalCollision / adoption 预览（UI 侧从资产单元格派生）。
 *
 * Business Logic: Task 8 碰撞对话框展示来源与诊断；不发明后端 adopt IPC。
 * Code Logic: pure view model。
 */
export interface AgentHubAdoptionPreview {
  assetId: string;
  displayName: string;
  logicalKey: string;
  originNamespace: string;
  target: AgentTarget;
  diagnostics: string[];
  aggregateStatus: AssetAggregateStatus;
}


/**
 * LAN push selection mode.
 */
export type AgentHubPushSelectionMode = 'fullHub' | 'userScope' | 'project' | 'explicitAssets';

/**
 * LAN push preview (zero transfer).
 */
export interface AgentHubLanPushPreview {
  previewToken: string;
  snapshotHash: string;
  snapshotId: string;
  selectionHash: string;
  assetCount: number;
  revisionCount: number;
  credentialBearingAssetCount: number;
  peerDeviceIds: string[];
  mode: AgentHubPushSelectionMode | string;
  plaintextBackupDisclosure: string;
  hasCredentialBearingAssets: boolean;
}

/**
 * 单目标 push 状态。
 */
export type AgentHubTargetPushStatus =
  | 'pending'
  | 'prepared'
  | 'transferred'
  | 'committed'
  | 'failed';

/**
 * 单目标 push outcome。
 */
export interface AgentHubTargetPushOutcome {
  peerDeviceId: string;
  peerLabel: string;
  clientRequestId: string;
  status: AgentHubTargetPushStatus | string;
  retryable: boolean;
  errorCode?: string | null;
  transferId?: string | null;
  missingObjectCount: number;
  transferredObjectCount: number;
  updatedAt: string;
}

/**
 * multi-target push report。
 */
export interface AgentHubMultiTargetPushReport {
  requestId: string;
  selectionHash: string;
  snapshotHash: string;
  status: string;
  targets: AgentHubTargetPushOutcome[];
}

/**
 * Git lane 清单条目。
 */
export interface AgentHubGitLaneSummary {
  laneDeviceId: string;
  snapshotHash: string;
  snapshotId: string;
  sourceReplicaId: string;
  assetCount: number;
  revisionCount: number;
  status: string;
  errorCode?: string | null;
}

/**
 * Git lanes inspect report。
 */
export interface AgentHubGitLaneInspectReport {
  workdirPresent: boolean;
  lanes: AgentHubGitLaneSummary[];
  localDeviceId: string;
}

/**
 * Git asset change kind。
 */
export type AgentHubGitAssetChangeKind =
  | 'added'
  | 'modified'
  | 'deleted'
  | 'conflict'
  | 'unchanged';

/**
 * Git asset diff entry（无 secret）。
 */
export interface AgentHubGitAssetDiffEntry {
  assetId: string;
  kind: string;
  logicalKey: string;
  displayName: string;
  changeKind: AgentHubGitAssetChangeKind | string;
  hasCredential: boolean;
  localHead?: string | null;
  remoteHead?: string | null;
  remoteDeleted: boolean;
}

/**
 * Git import change counts。
 */
export interface AgentHubGitAssetChangeCounts {
  added: number;
  modified: number;
  deleted: number;
  conflict: number;
  unchanged: number;
  credentialBearing: number;
}

/**
 * Project mapping candidate。
 */
export interface AgentHubProjectMappingCandidate {
  hubProjectId: string;
  candidateKind: string;
  candidateExternalId: string;
  localWorkbenchProjectId?: string | null;
}

/**
 * Resolved project mapping。
 */
export interface AgentHubResolvedProjectMapping {
  hubProjectId: string;
  localWorkbenchProjectId?: string | null;
  optedIn: boolean;
}

/**
 * Git import preview。
 */
export interface AgentHubGitImportPreview {
  laneDeviceId: string;
  snapshotId: string;
  snapshotHash: string;
  sourceReplicaId: string;
  assetCount: number;
  revisionCount: number;
  changeCounts: AgentHubGitAssetChangeCounts;
  assets: AgentHubGitAssetDiffEntry[];
  projectCandidates: AgentHubProjectMappingCandidate[];
  resolvedMappings: AgentHubResolvedProjectMapping[];
  plaintextBackupDisclosure: string;
  hasCredentialBearingAssets: boolean;
}

/**
 * Confirmed project mapping input。
 */
export interface AgentHubConfirmedProjectMapping {
  hubProjectId: string;
  localWorkbenchProjectId?: string | null;
  gitRemoteFingerprint?: string | null;
  optedIn?: boolean;
}

/**
 * Confirm git import request。
 */
export interface AgentHubConfirmGitImportRequest {
  laneDeviceId: string;
  snapshotHash: string;
  selectedAssetIds?: string[];
  projectMappings?: AgentHubConfirmedProjectMapping[];
  importUnmappedProjects?: boolean;
}

/**
 * Snapshot import outcome。
 */
export interface AgentHubSnapshotImportOutcome {
  snapshotId: string;
  snapshotHash: string;
  importedAssetIds: string[];
  insertedRevisions: number;
  dedupedRevisions: number;
  headsAdvanced: number;
  conflictsOpened: number;
  projectionsScheduled: number;
  importedObjectHashes: string[];
}

/**
 * Confirm git import outcome。
 */
export interface AgentHubConfirmGitImportOutcome {
  laneDeviceId: string;
  snapshotHash: string;
  import: AgentHubSnapshotImportOutcome;
  resolvedMappings: AgentHubResolvedProjectMapping[];
}

/**
 * Confirm project mapping request。
 */
export interface AgentHubConfirmProjectMappingRequest {
  hubProjectId: string;
  localWorkbenchProjectId?: string | null;
  gitRemoteFingerprint?: string | null;
  optedIn?: boolean;
}

/**
 * Push selection request。
 */
export interface AgentHubPushSelectionRequest {
  peerDeviceIds: string[];
  mode: AgentHubPushSelectionMode;
  scopeIds?: string[];
  assetIds?: string[];
  hubProjectIds?: string[];
  includeHistory?: boolean;
  previewToken?: string | null;
  requestId?: string | null;
}

/**
 * Plugin component 在 destination target 上的投影状态。
 *
 * Business Logic: per-component 矩阵不得压成 package 级 green synced。
 * Code Logic: camelCase wire tokens。
 */
export type PluginComponentTargetStatus =
  | 'verified'
  | 'partial'
  | 'sourceOnly'
  | 'activationRequired'
  | 'externalCollision'
  | 'blocked';

/**
 * Component 相对 package 的所有权。
 *
 * Business Logic: 删除 preview 区分 tombstone / preserve。
 * Code Logic: packageOwned | shared | standalone。
 */
export type PluginComponentOwnership = 'packageOwned' | 'shared' | 'standalone';

/**
 * Residual 类别。
 */
export type PluginResidualKind = 'runtime' | 'hooks' | 'assets' | 'npm' | 'customTool';

/**
 * Package 删除时 component 处置。
 *
 * Business Logic: 独占才 tombstone；共享/standalone 保留。
 * Code Logic: tombstoneOwned | preserveShared | preserveStandalone。
 */
export type PluginComponentDeleteDecision =
  | 'tombstoneOwned'
  | 'preserveShared'
  | 'preserveStandalone';

/**
 * 单 component 在某 destination 上的投影单元格。
 */
export interface PluginComponentTargetCell {
  target: AgentTarget;
  status: PluginComponentTargetStatus;
  reasons: string[];
  projectedPaths: string[];
  materializedAlias?: string | null;
}

/**
 * 固定 revision 的 component 报告行。
 *
 * Business Logic: Drawer 按 component 展示 target matrix / ownership / residual reason。
 * Code Logic: 固定 revisionId + 三端 cells。
 */
export interface PluginComponentReport {
  kind: string;
  assetId: string;
  displayName: string;
  canonicalRevisionId: string;
  ownership: PluginComponentOwnership;
  sourceTarget: AgentTarget;
  targets: PluginComponentTargetCell[];
  residualReason?: string | null;
}

/**
 * Residual 投影报告。
 */
export interface PluginResidualReport {
  residualTarget: AgentTarget;
  residualKind: PluginResidualKind;
  treeManifestHash: string;
  included: boolean;
  reasons: string[];
}

/**
 * 删除 preview 中单个 component 的处置行。
 */
export interface PluginDeletePreviewComponent {
  assetId: string;
  displayName: string;
  kind: string;
  ownership: PluginComponentOwnership;
  decision: PluginComponentDeleteDecision;
}

/**
 * Package 删除 preview。
 *
 * Business Logic: 列出将 tombstone vs 因引用保留的 component。
 * Code Logic: package 级 + components 决策表。
 */
export interface PluginDeletePreview {
  packageAssetId: string;
  packageDisplayName: string;
  components: PluginDeletePreviewComponent[];
}

/**
 * Package 级聚合投影报告（UI 入口）。
 *
 * Business Logic: mixed package 不得 compress 为 synced；partial 必须点名 blockers。
 * Code Logic: components + residuals + aggregate + optional deletePreview。
 */
export interface PluginPackageReport {
  packageAssetId: string;
  packageDisplayName: string;
  sourceTarget: AgentTarget;
  destinationTarget?: AgentTarget | null;
  aggregateStatus: AssetAggregateStatus;
  activationState: string;
  diagnostics: string[];
  components: PluginComponentReport[];
  residuals: PluginResidualReport[];
  /** partial blockers 精确 token 列表（target:reason） */
  partialBlockers: string[];
  deletePreview?: PluginDeletePreview | null;
}

/**
 * OpenCode project runtime bridge 状态。
 *
 * Business Logic: openCodeVisible 选择前必须 fail-closed 展示 bridge 状态。
 * Code Logic: ready | previewRequired | conflict | unsupported。
 */
export type OpenCodeBridgeStatus =
  | 'ready'
  | 'previewRequired'
  | 'conflict'
  | 'unsupported';

/**
 * 派生 bridge 视图模型（无 secret）。
 */
export interface OpenCodeBridgeView {
  status: OpenCodeBridgeStatus;
  relativePath: string;
  blockedReason?: string | null;
  requiresProjectPreview: boolean;
}
