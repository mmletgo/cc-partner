/**
 * Portable inventory / action / pull 运行时 schema。
 *
 * Business Logic（为什么需要这个模块）:
 *   IPC 边界可能损坏或混合版本；写入页面状态前 fail-closed，
 *   ContractDecodeError 不得序列化 payload；MCP 仅 present/hash。
 *
 * Code Logic（这个模块做什么）:
 *   严格 enumDecoder + objectDecoder；未知额外字段前向兼容忽略；禁止 loose typing。
 */

import type {
  ApplyPortableAssetActionRequest,
  ApplyPortablePullRequest,
  ListRemotePortableInventoryRequest,
  PortableAssetActionChangeDto,
  PortableAssetActionItemResultDto,
  PortableAssetActionItemState,
  PortableAssetActionKind,
  PortableAssetActionPlanDto,
  PortableAssetActionResultDto,
  PortableAssetBackupPolicy,
  PortableAssetCanonicalEffect,
  PortableAssetConflictPolicy,
  PortableAssetKind,
  PortableAssetPlanOperation,
  PortableInventoryItemCapabilitiesDto,
  PortableInventoryItemDto,
  PortableInventoryManagementState,
  PortableInventoryOriginKind,
  PortableInventoryOwnedBy,
  PortableInventoryMutationCapability,
  PortableInventoryScanCapability,
  PortableInventorySnapshotDto,
  PortableInventorySourceOrigin,
  PortableInventoryTargetDto,
  PortableMcpCredentialFactDto,
  PortablePullChangeDto,
  PortablePullInstallMode,
  PortablePullItemResultDto,
  PortablePullItemState,
  PortablePullPlanDto,
  PortablePullResultDto,
  PortableScopeKind,
  PreviewPortableAssetActionRequest,
  PreviewPortablePullRequest,
  RemotePortableInventoryDto,
  RemotePortableInventoryItemDto,
} from '../types/portableInventory';
import {
  arrayDecoder,
  booleanDecoder,
  enumDecoder,
  numberDecoder,
  nullableDecoder,
  objectDecoder,
  optionalDecoder,
  stringDecoder,
  type Decoder,
} from '../runtimeSchema';
import { agentTargetDecoder, desiredPresenceDecoder } from './agentHub';

/** Portable 四类 kind。 */
export const portableAssetKindDecoder: Decoder<PortableAssetKind> = enumDecoder(
  'PortableAssetKind',
  ['skill', 'command', 'plugin', 'mcp'] as const,
);

/** 管理状态。 */
export const portableInventoryManagementStateDecoder: Decoder<PortableInventoryManagementState> =
  enumDecoder('PortableInventoryManagementState', [
    'unmanaged',
    'hubManaged',
    'drifted',
    'externalCollision',
    'unsupported',
  ] as const);

/** 源 origin。 */
export const portableInventorySourceOriginDecoder: Decoder<PortableInventorySourceOrigin> =
  enumDecoder('PortableInventorySourceOrigin', [
    'standalone',
    'pluginComponent',
    'nativeConfig',
  ] as const);

/** 发现/归属 originKind。 */
export const portableInventoryOriginKindDecoder: Decoder<PortableInventoryOriginKind> =
  enumDecoder('PortableInventoryOriginKind', [
    'native',
    'compatibility',
    'legacyStandalone',
    'plugin',
  ] as const);

/** 所有权：Hub target / 共享 ~/.agents / 未知。 */
export const portableInventoryOwnedByDecoder: Decoder<PortableInventoryOwnedBy> = enumDecoder(
  'PortableInventoryOwnedBy',
  [
    'claude',
    'codex',
    'opencode',
    'grok',
    'gemini',
    'cursor',
    'pi',
    'sharedAgents',
    'unknown',
  ] as const);

/** 扫描能力。 */
export const portableInventoryScanCapabilityDecoder: Decoder<PortableInventoryScanCapability> =
  enumDecoder('PortableInventoryScanCapability', [
    'supported',
    'readOnly',
    'blocked',
  ] as const);

/** 写入能力。 */
export const portableInventoryMutationCapabilityDecoder: Decoder<PortableInventoryMutationCapability> =
  enumDecoder('PortableInventoryMutationCapability', [
    'supported',
    'previewOnly',
    'blocked',
  ] as const);

/** Scope kind。 */
export const portableScopeKindDecoder: Decoder<PortableScopeKind> = enumDecoder(
  'PortableScopeKind',
  ['user', 'project', 'directory'] as const,
);

/** 本机动作 kind。 */
export const portableAssetActionKindDecoder: Decoder<PortableAssetActionKind> = enumDecoder(
  'PortableAssetActionKind',
  ['adopt', 'enable', 'disable', 'uninstall', 'installToSourceTarget'] as const,
);

/** 动作结果状态。 */
export const portableAssetActionItemStateDecoder: Decoder<PortableAssetActionItemState> =
  enumDecoder('PortableAssetActionItemState', [
    'succeeded',
    'skipped',
    'failed',
    'blocked',
    'outcomeUnknown',
  ] as const);

/** Canonical 影响。 */
export const portableAssetCanonicalEffectDecoder: Decoder<PortableAssetCanonicalEffect> =
  enumDecoder('PortableAssetCanonicalEffect', [
    'none',
    'createOwnership',
    'updateDesired',
    'tombstoneComponents',
  ] as const);

/** Plan 操作。 */
export const portableAssetPlanOperationDecoder: Decoder<PortableAssetPlanOperation> = enumDecoder(
  'PortableAssetPlanOperation',
  ['leave', 'enable', 'disable', 'uninstall', 'install', 'adopt'] as const,
);

/** 备份策略。 */
export const portableAssetBackupPolicyDecoder: Decoder<PortableAssetBackupPolicy> = enumDecoder(
  'PortableAssetBackupPolicy',
  ['none', 'recoverableBeforeDelete'] as const,
);

/** 冲突策略。 */
export const portableAssetConflictPolicyDecoder: Decoder<PortableAssetConflictPolicy> =
  enumDecoder('PortableAssetConflictPolicy', [
    'skipExisting',
    'replaceAfterPreview',
  ] as const);

/** Pull 安装模式。 */
export const portablePullInstallModeDecoder: Decoder<PortablePullInstallMode> = enumDecoder(
  'PortablePullInstallMode',
  ['installToTarget', 'importedCanonicalOnly', 'skipExisting', 'blocked'] as const,
);

/** Pull 项状态。 */
export const portablePullItemStateDecoder: Decoder<PortablePullItemState> = enumDecoder(
  'PortablePullItemState',
  [
    'succeeded',
    'skipped',
    'failed',
    'blocked',
    'importedCanonicalOnly',
    'outcomeUnknown',
  ] as const,
);

/** MCP 凭据事实（仅 present/hash；未知 secret 字段忽略）。 */
export const portableMcpCredentialFactDecoder: Decoder<PortableMcpCredentialFactDto> =
  objectDecoder('PortableMcpCredentialFactDto', {
    present: booleanDecoder,
    hash: nullableDecoder(stringDecoder),
  });

/** 单项能力。 */
export const portableInventoryItemCapabilitiesDecoder: Decoder<PortableInventoryItemCapabilitiesDto> =
  objectDecoder('PortableInventoryItemCapabilitiesDto', {
    canEnable: booleanDecoder,
    canDisable: booleanDecoder,
    canUninstall: booleanDecoder,
    canAdopt: booleanDecoder,
    canInstallToSourceTarget: booleanDecoder,
    reasonCode: nullableDecoder(stringDecoder),
    evidenceIds: arrayDecoder(stringDecoder),
  });

/** Target DTO。 */
export const portableInventoryTargetDecoder: Decoder<PortableInventoryTargetDto> = objectDecoder(
  'PortableInventoryTargetDto',
  {
    target: agentTargetDecoder,
    installed: booleanDecoder,
    version: nullableDecoder(stringDecoder),
    executable: nullableDecoder(stringDecoder),
    configRoot: stringDecoder,
    scanCapability: portableInventoryScanCapabilityDecoder,
    mutationCapability: portableInventoryMutationCapabilityDecoder,
    reasonCode: nullableDecoder(stringDecoder),
    evidenceIds: arrayDecoder(stringDecoder),
  },
);

/**
 * Business Logic: inventory 必须带 loadedBy/ownedBy/originKind/nativeOutputCandidate，
 *   缺字段不得默认为 native，避免兼容根被当成可卸载写出目标。
 * Code Logic: 四字段均为 required enum/boolean。
 */
export const portableInventoryItemDecoder: Decoder<PortableInventoryItemDto> = objectDecoder(
  'PortableInventoryItemDto',
  {
    inventoryItemId: stringDecoder,
    target: agentTargetDecoder,
    loadedBy: agentTargetDecoder,
    ownedBy: portableInventoryOwnedByDecoder,
    originKind: portableInventoryOriginKindDecoder,
    nativeOutputCandidate: booleanDecoder,
    kind: portableAssetKindDecoder,
    nativeId: stringDecoder,
    displayName: stringDecoder,
    description: nullableDecoder(stringDecoder),
    version: nullableDecoder(stringDecoder),
    scopeId: stringDecoder,
    scopeKind: portableScopeKindDecoder,
    projectId: nullableDecoder(stringDecoder),
    projectOptedIn: booleanDecoder,
    sourcePath: nullableDecoder(stringDecoder),
    sourceOrigin: portableInventorySourceOriginDecoder,
    parentPluginInventoryItemId: nullableDecoder(stringDecoder),
    actualEnabled: nullableDecoder(booleanDecoder),
    contentHash: nullableDecoder(stringDecoder),
    treeHash: nullableDecoder(stringDecoder),
    canonicalAssetId: nullableDecoder(stringDecoder),
    canonicalRevisionId: nullableDecoder(stringDecoder),
    managementState: portableInventoryManagementStateDecoder,
    desiredPresence: nullableDecoder(desiredPresenceDecoder),
    desiredEnabled: nullableDecoder(booleanDecoder),
    materializationStatus: nullableDecoder(stringDecoder),
    capabilities: portableInventoryItemCapabilitiesDecoder,
    warnings: arrayDecoder(stringDecoder),
    mcpCredential: optionalDecoder(
      nullableDecoder(portableMcpCredentialFactDecoder),
    ),
  },
);

/** 库存快照。 */
export const portableInventorySnapshotDecoder: Decoder<PortableInventorySnapshotDto> =
  objectDecoder('PortableInventorySnapshotDto', {
    inventorySnapshotHash: stringDecoder,
    refreshedAt: stringDecoder,
    stale: booleanDecoder,
    targets: arrayDecoder(portableInventoryTargetDecoder),
    items: arrayDecoder(portableInventoryItemDecoder),
  });

/** 动作 change 行。 */
export const portableAssetActionChangeDecoder: Decoder<PortableAssetActionChangeDto> =
  objectDecoder('PortableAssetActionChangeDto', {
    inventoryItemId: stringDecoder,
    target: agentTargetDecoder,
    kind: portableAssetKindDecoder,
    path: nullableDecoder(stringDecoder),
    operation: portableAssetPlanOperationDecoder,
    expectedSourceHash: nullableDecoder(stringDecoder),
    expectedTreeHash: nullableDecoder(stringDecoder),
    expectedCanonicalRevisionId: nullableDecoder(stringDecoder),
    backupPolicy: portableAssetBackupPolicyDecoder,
    createsOwnership: booleanDecoder,
    canonicalEffect: portableAssetCanonicalEffectDecoder,
    blockingReasons: arrayDecoder(stringDecoder),
    warnings: arrayDecoder(stringDecoder),
  });

/** Preview plan。 */
export const portableAssetActionPlanDecoder: Decoder<PortableAssetActionPlanDto> = objectDecoder(
  'PortableAssetActionPlanDto',
  {
    planToken: stringDecoder,
    expiresAt: stringDecoder,
    inventorySnapshotHash: stringDecoder,
    action: portableAssetActionKindDecoder,
    keepData: booleanDecoder,
    conflictPolicy: portableAssetConflictPolicyDecoder,
    changes: arrayDecoder(portableAssetActionChangeDecoder),
    blockingReasons: arrayDecoder(stringDecoder),
  },
);

/** 动作项结果。 */
export const portableAssetActionItemResultDecoder: Decoder<PortableAssetActionItemResultDto> =
  objectDecoder('PortableAssetActionItemResultDto', {
    inventoryItemId: stringDecoder,
    state: portableAssetActionItemStateDecoder,
    errorCode: nullableDecoder(stringDecoder),
    message: nullableDecoder(stringDecoder),
  });

/** Apply 聚合结果。 */
export const portableAssetActionResultDecoder: Decoder<PortableAssetActionResultDto> =
  objectDecoder('PortableAssetActionResultDto', {
    planToken: stringDecoder,
    clientRequestId: stringDecoder,
    items: arrayDecoder(portableAssetActionItemResultDecoder),
  });

/** 远端 inventory 项。 */
export const remotePortableInventoryItemDecoder: Decoder<RemotePortableInventoryItemDto> =
  objectDecoder('RemotePortableInventoryItemDto', {
    inventoryItemId: stringDecoder,
    target: agentTargetDecoder,
    kind: portableAssetKindDecoder,
    nativeId: stringDecoder,
    displayName: stringDecoder,
    description: nullableDecoder(stringDecoder),
    version: nullableDecoder(stringDecoder),
    scopeId: stringDecoder,
    projectId: nullableDecoder(stringDecoder),
    projectOptedIn: booleanDecoder,
    sourceOrigin: portableInventorySourceOriginDecoder,
    actualEnabled: nullableDecoder(booleanDecoder),
    contentHash: nullableDecoder(stringDecoder),
    treeHash: nullableDecoder(stringDecoder),
    warnings: arrayDecoder(stringDecoder),
    mcpCredential: optionalDecoder(
      nullableDecoder(portableMcpCredentialFactDecoder),
    ),
  });

/** 远端 inventory。 */
export const remotePortableInventoryDecoder: Decoder<RemotePortableInventoryDto> = objectDecoder(
  'RemotePortableInventoryDto',
  {
    sourceDeviceId: stringDecoder,
    sourceTarget: agentTargetDecoder,
    inventorySnapshotHash: stringDecoder,
    refreshedAt: stringDecoder,
    stale: booleanDecoder,
    items: arrayDecoder(remotePortableInventoryItemDecoder),
  },
);

/** Pull change。 */
export const portablePullChangeDecoder: Decoder<PortablePullChangeDto> = objectDecoder(
  'PortablePullChangeDto',
  {
    inventoryItemId: stringDecoder,
    kind: portableAssetKindDecoder,
    nativeId: stringDecoder,
    displayName: stringDecoder,
    installMode: portablePullInstallModeDecoder,
    conflict: booleanDecoder,
    legacyLossy: booleanDecoder,
    credentialBearing: booleanDecoder,
    blockingReasons: arrayDecoder(stringDecoder),
    warnings: arrayDecoder(stringDecoder),
  },
);

/** Pull plan。 */
export const portablePullPlanDecoder: Decoder<PortablePullPlanDto> = objectDecoder(
  'PortablePullPlanDto',
  {
    planToken: stringDecoder,
    expiresAt: stringDecoder,
    sourceDeviceId: stringDecoder,
    sourceTarget: agentTargetDecoder,
    destinationTarget: agentTargetDecoder,
    remoteInventorySnapshotHash: stringDecoder,
    localInventorySnapshotHash: stringDecoder,
    conflictPolicy: portableAssetConflictPolicyDecoder,
    selectionManifestHash: stringDecoder,
    credentialBearingCount: numberDecoder,
    hasCredentialBearingAssets: booleanDecoder,
    changes: arrayDecoder(portablePullChangeDecoder),
    blockingReasons: arrayDecoder(stringDecoder),
  },
);

/** Pull item result。 */
export const portablePullItemResultDecoder: Decoder<PortablePullItemResultDto> = objectDecoder(
  'PortablePullItemResultDto',
  {
    inventoryItemId: stringDecoder,
    state: portablePullItemStateDecoder,
    installMode: nullableDecoder(portablePullInstallModeDecoder),
    errorCode: nullableDecoder(stringDecoder),
    message: nullableDecoder(stringDecoder),
  },
);

/** Pull 聚合结果。 */
export const portablePullResultDecoder: Decoder<PortablePullResultDto> = objectDecoder(
  'PortablePullResultDto',
  {
    planToken: stringDecoder,
    clientRequestId: stringDecoder,
    sourceDeviceId: stringDecoder,
    sourceTarget: agentTargetDecoder,
    destinationTarget: agentTargetDecoder,
    partial: booleanDecoder,
    items: arrayDecoder(portablePullItemResultDecoder),
  },
);

/** 请求 shape 类型 re-export 占位（runtime 不解码 request，仅 response）。 */
export type {
  PreviewPortableAssetActionRequest,
  ApplyPortableAssetActionRequest,
  ListRemotePortableInventoryRequest,
  PreviewPortablePullRequest,
  ApplyPortablePullRequest,
};
