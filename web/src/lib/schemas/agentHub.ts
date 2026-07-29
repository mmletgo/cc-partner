/**
 * Agent Hub 运行时 schema。
 *
 * Business Logic（为什么需要这个模块）:
 *   IPC 边界可能损坏或混合版本；写入页面状态前 fail-closed，
 *   且 ContractDecodeError 不得序列化 payload。
 *
 * Code Logic（这个模块做什么）:
 *   严格 enumDecoder 校验 required status 相关枚举；
 *   导出 status / list / detail / snapshot / preview decoder。
 */

import type {
  AgentHubAssetDetail,
  AgentHubAssetSummary,
  AgentHubConflictDto,
  AgentHubProbe,
  AgentHubProjectPreview,
  AgentHubProjectStatus,
  AgentHubSnapshot,
  AgentHubStatus,
  AgentHubTargetCell,
  AgentTarget,
  AssetAggregateStatus,
  DesiredPresence,
  InstructionBlockDto,
  InstructionBlockMode,
  MaterializationStatus,
} from '../types/agentHub';
import {
  arrayDecoder,
  booleanDecoder,
  enumDecoder,
  numberDecoder,
  nullableDecoder,
  objectDecoder,
  optionalDecoder,
  recordDecoder,
  stringDecoder,
  type Decoder,
} from '../runtimeSchema';

/**
 * Business Logic: target 三端枚举必须稳定。
 * Code Logic: claude | codex | opencode。
 */
export const agentTargetDecoder: Decoder<AgentTarget> = enumDecoder('AgentTarget', [
  'claude',
  'codex',
  'opencode',
] as const);

/**
 * Business Logic: presence 决定是否投影。
 * Code Logic: present | absent。
 */
export const desiredPresenceDecoder: Decoder<DesiredPresence> = enumDecoder('DesiredPresence', [
  'present',
  'absent',
] as const);

/**
 * Business Logic: 指令块模式是 Drawer 真源。
 * Code Logic: shared | adapted | targetOnly。
 */
export const instructionBlockModeDecoder: Decoder<InstructionBlockMode> = enumDecoder(
  'InstructionBlockMode',
  ['shared', 'adapted', 'targetOnly'] as const,
);

/**
 * Business Logic: 资产聚合态是矩阵汇总真源，非法值拒绝整包。
 * Code Logic: 严格 enum。
 */
export const assetAggregateStatusDecoder: Decoder<AssetAggregateStatus> = enumDecoder(
  'AssetAggregateStatus',
  [
    'full',
    'partial',
    'sourceOnly',
    'activationRequired',
    'externalCollision',
    'detached',
    'blocked',
  ] as const,
);

/**
 * Business Logic: 已知 materialization 状态严格校验，未知前向兼容为 string。
 * Code Logic: enum 优先，失败则 stringDecoder（仅可选字段路径使用）。
 */
const knownMaterializationStatusDecoder: Decoder<MaterializationStatus> = enumDecoder(
  'MaterializationStatus',
  [
    'synced',
    'pending',
    'blocked',
    'drifted',
    'drift',
    'detached',
    'failed',
    'writing',
    'conflict',
    'unsupported',
    'activationRequired',
    'externalCollision',
  ] as const,
);

/**
 * Business Logic: probe.support 是 status 必需枚举字段。
 * Code Logic: 严格 enum（含 backend supported 与 UI full/partial）。
 */
export const agentHubSupportDecoder: Decoder<string> = enumDecoder('AgentHubSupportLevel', [
  'full',
  'partial',
  'scanOnly',
  'unsupported',
  'supported',
] as const);

/**
 * Business Logic: CLI probe 汇总在状态条展示。
 * Code Logic: objectDecoder probe。
 */
export const agentHubProbeDecoder: Decoder<AgentHubProbe> = objectDecoder('AgentHubProbe', {
  target: agentTargetDecoder,
  executable: optionalDecoder(nullableDecoder(stringDecoder)),
  version: optionalDecoder(nullableDecoder(stringDecoder)),
  support: agentHubSupportDecoder,
  configRoot: optionalDecoder(nullableDecoder(stringDecoder)),
});

/**
 * Business Logic: 首屏 status 必须完整，非法 support enum 拒绝整包。
 * Code Logic: objectDecoder AgentHubStatus。
 */
export const agentHubStatusDecoder: Decoder<AgentHubStatus> = objectDecoder('AgentHubStatus', {
  enabled: booleanDecoder,
  backgroundEnabled: booleanDecoder,
  agentHubApiVersion: numberDecoder,
  ownerInstanceId: optionalDecoder(nullableDecoder(stringDecoder)),
  writeCompatible: booleanDecoder,
  probes: arrayDecoder(agentHubProbeDecoder),
  conflictCount: numberDecoder,
  blockedMaterializationCount: numberDecoder,
});

/**
 * Business Logic: 目标单元格 presence 与 Gate B 聚合输入为 required。
 * Code Logic: objectDecoder target cell。
 */
export const agentHubTargetCellDecoder: Decoder<AgentHubTargetCell> = objectDecoder(
  'AgentHubTargetCell',
  {
    target: agentTargetDecoder,
    desiredPresence: desiredPresenceDecoder,
    desiredEnabled: booleanDecoder,
    materializationStatus: optionalDecoder(
      nullableDecoder(knownMaterializationStatusDecoder),
    ),
    lastError: optionalDecoder(nullableDecoder(stringDecoder)),
    requested: booleanDecoder,
    supported: booleanDecoder,
    sourceOnly: booleanDecoder,
    verified: booleanDecoder,
    invocationAlias: optionalDecoder(nullableDecoder(stringDecoder)),
  },
);

/**
 * Business Logic: 列表行摘要，aggregateStatus 严格校验。
 * Code Logic: objectDecoder asset summary。
 */
export const agentHubAssetSummaryDecoder: Decoder<AgentHubAssetSummary> = objectDecoder(
  'AgentHubAssetSummary',
  {
    assetId: stringDecoder,
    scopeId: stringDecoder,
    kind: stringDecoder,
    displayName: stringDecoder,
    logicalKey: stringDecoder,
    originNamespace: stringDecoder,
    policy: stringDecoder,
    currentRevisionId: optionalDecoder(nullableDecoder(stringDecoder)),
    targets: arrayDecoder(agentHubTargetCellDecoder),
    hasConflict: optionalDecoder(booleanDecoder),
    aggregateStatus: assetAggregateStatusDecoder,
  },
);

/**
 * Business Logic: list_assets 返回数组。
 * Code Logic: arrayDecoder(summary)。
 */
export const agentHubAssetSummaryListDecoder: Decoder<AgentHubAssetSummary[]> = arrayDecoder(
  agentHubAssetSummaryDecoder,
);

/**
 * Business Logic: 指令块 mode 为 required enum。
 * Code Logic: objectDecoder block。
 */
export const instructionBlockDtoDecoder: Decoder<InstructionBlockDto> = objectDecoder(
  'InstructionBlockDto',
  {
    id: stringDecoder,
    mode: instructionBlockModeDecoder,
    commonMarkdown: stringDecoder,
    variants: optionalDecoder(nullableDecoder(recordDecoder(stringDecoder))),
    headingPath: optionalDecoder(nullableDecoder(arrayDecoder(stringDecoder))),
    sourceTarget: optionalDecoder(nullableDecoder(agentTargetDecoder)),
    needsAdaptation: optionalDecoder(booleanDecoder),
  },
);

/**
 * Business Logic: 冲突条目供 resolve。
 * Code Logic: objectDecoder conflict。
 */
export const agentHubConflictDtoDecoder: Decoder<AgentHubConflictDto> = objectDecoder(
  'AgentHubConflictDto',
  {
    id: stringDecoder,
    target: optionalDecoder(nullableDecoder(agentTargetDecoder)),
    detailJson: optionalDecoder(stringDecoder),
    createdAt: stringDecoder,
  },
);

/**
 * Business Logic: 资产详情含 blocks 与 aggregateStatus。
 * Code Logic: summary 字段 + optional blocks/conflicts。
 */
export const agentHubAssetDetailDecoder: Decoder<AgentHubAssetDetail> = objectDecoder(
  'AgentHubAssetDetail',
  {
    assetId: stringDecoder,
    scopeId: stringDecoder,
    kind: stringDecoder,
    displayName: stringDecoder,
    logicalKey: stringDecoder,
    originNamespace: stringDecoder,
    policy: stringDecoder,
    currentRevisionId: optionalDecoder(nullableDecoder(stringDecoder)),
    targets: arrayDecoder(agentHubTargetCellDecoder),
    hasConflict: optionalDecoder(booleanDecoder),
    aggregateStatus: assetAggregateStatusDecoder,
    blocks: optionalDecoder(arrayDecoder(instructionBlockDtoDecoder)),
    contentMarkdown: optionalDecoder(nullableDecoder(stringDecoder)),
    conflicts: optionalDecoder(arrayDecoder(agentHubConflictDtoDecoder)),
  },
);

/**
 * Business Logic: status+assets 组合快照，用于 schema 合同与首屏解码。
 * Code Logic: objectDecoder snapshot；未知 support enum 拒绝且不序列化 payload。
 */
export const agentHubSnapshotDecoder: Decoder<AgentHubSnapshot> = objectDecoder(
  'AgentHubSnapshot',
  {
    status: agentHubStatusDecoder,
    assets: agentHubAssetSummaryListDecoder,
  },
);

/**
 * Business Logic: preview 字段随后端演进，仅要求 object 形状。
 * Code Logic: 宽松 object → record 透传 + 常见字段可选 decode。
 */
export const agentHubProjectPreviewDecoder: Decoder<AgentHubProjectPreview> = {
  name: 'AgentHubProjectPreview',
  decode(value, path = '$'): AgentHubProjectPreview {
    const obj = objectDecoder('AgentHubProjectPreviewLoose', {
      projectId: optionalDecoder(stringDecoder),
      hubProjectId: optionalDecoder(nullableDecoder(stringDecoder)),
      path: optionalDecoder(stringDecoder),
      optedIn: optionalDecoder(booleanDecoder),
      noCommitNotice: optionalDecoder(stringDecoder),
      gitRemoteFingerprint: optionalDecoder(nullableDecoder(stringDecoder)),
    }).decode(value, path);
    // 保留未知额外字段供 Dialog 展示
    if (value && typeof value === 'object' && !Array.isArray(value)) {
      return { ...(value as Record<string, unknown>), ...obj } as AgentHubProjectPreview;
    }
    return obj as AgentHubProjectPreview;
  },
};

/**
 * Business Logic: enable 结果宽松解码。
 * Code Logic: 同 preview 宽松 object。
 */
export const agentHubProjectStatusDecoder: Decoder<AgentHubProjectStatus> = {
  name: 'AgentHubProjectStatus',
  decode(value, path = '$'): AgentHubProjectStatus {
    const obj = objectDecoder('AgentHubProjectStatusLoose', {
      projectId: optionalDecoder(stringDecoder),
      hubProjectId: optionalDecoder(stringDecoder),
      optedIn: optionalDecoder(booleanDecoder),
    }).decode(value, path);
    if (value && typeof value === 'object' && !Array.isArray(value)) {
      return { ...(value as Record<string, unknown>), ...obj } as AgentHubProjectStatus;
    }
    return obj as AgentHubProjectStatus;
  },
};
