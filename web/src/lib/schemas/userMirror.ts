/**
 * 用户级镜像 inventory / plan / result 运行时 schema。
 *
 * Business Logic（为什么需要这个模块）:
 *   IPC 边界可能损坏或混合版本；写入页面状态前 fail-closed，
 *   ContractDecodeError 不得序列化 payload；MCP 若带 token/value/secret 必须拒绝。
 *
 * Code Logic（这个模块做什么）:
 *   严格 enumDecoder + objectDecoder；未知额外字段前向兼容忽略；
 *   MCP 结构出现 secret 键 fail-closed。
 */

import type {
  UserMirrorAgentInventoryDto,
  UserMirrorAgentPlanDto,
  UserMirrorAgentResultDto,
  UserMirrorChangeOp,
  UserMirrorDirection,
  UserMirrorFileChangeDto,
  UserMirrorInventoryDto,
  UserMirrorItemState,
  UserMirrorMcpCredentialFactDto,
  UserMirrorNativeFileFactDto,
  UserMirrorPlanDto,
  UserMirrorPortableChangeDto,
  UserMirrorPortableItemDto,
  UserMirrorPortableKeyDto,
  UserMirrorResultDto,
  UserMirrorSelectionFilterDto,
  UserMirrorSlotHashesDto,
} from '../types/userMirror';
import {
  arrayDecoder,
  booleanDecoder,
  ContractDecodeError,
  defineDecoder,
  enumDecoder,
  numberDecoder,
  nullableDecoder,
  objectDecoder,
  optionalDecoder,
  stringDecoder,
  actualKindOf,
  type Decoder,
} from '../runtimeSchema';
import { agentTargetDecoder } from './agentHub';
import { portableAssetKindDecoder } from './portableInventory';

/** MCP 结构禁止出现的明文凭据键。 */
const MCP_SECRET_KEYS = ['token', 'value', 'secret'] as const;

/**
 * Business Logic（为什么需要这个函数）:
 *   inventory/UI/log 不得包含明文 env；MCP JSON 一旦带 token/value/secret 必须 fail-closed。
 *
 * Code Logic（这个函数做什么）:
 *   对象若自有 token/value/secret 键则抛 ContractDecodeError（不序列化值）；否则委托 inner。
 */
function rejectMcpSecretKeys<T>(contract: string, inner: Decoder<T>): Decoder<T> {
  return defineDecoder(contract, (value, path) => {
    if (value !== null && typeof value === 'object' && !Array.isArray(value)) {
      const record = value as Record<string, unknown>;
      for (const key of MCP_SECRET_KEYS) {
        if (Object.prototype.hasOwnProperty.call(record, key)) {
          throw new ContractDecodeError(contract, `${path}.${key}`, actualKindOf(record[key]));
        }
      }
    }
    return inner.decode(value, path);
  });
}

/** 镜像方向。 */
export const userMirrorDirectionDecoder: Decoder<UserMirrorDirection> = enumDecoder(
  'UserMirrorDirection',
  ['pull', 'push'] as const,
);

/** 预览变更动作。 */
export const userMirrorChangeOpDecoder: Decoder<UserMirrorChangeOp> = enumDecoder(
  'UserMirrorChangeOp',
  ['write', 'replace', 'clear', 'delete', 'disable'] as const,
);

/** 单条落地状态。 */
export const userMirrorItemStateDecoder: Decoder<UserMirrorItemState> = enumDecoder(
  'UserMirrorItemState',
  ['succeeded', 'failed', 'skipped', 'outcomeUnknown'] as const,
);

/**
 * Business Logic: MCP 凭据只允许 present/hash；出现 token/value/secret 必须拒绝。
 * Code Logic: 先扫禁止键，再 objectDecoder；其它未知字段忽略。
 */
export const userMirrorMcpCredentialFactDecoder: Decoder<UserMirrorMcpCredentialFactDto> =
  rejectMcpSecretKeys(
    'UserMirrorMcpCredentialFactDto',
    objectDecoder<UserMirrorMcpCredentialFactDto>('UserMirrorMcpCredentialFactDto', {
      present: booleanDecoder,
      hash: nullableDecoder(stringDecoder),
    }),
  );

/** 原生提示词文件事实。 */
export const userMirrorNativeFileFactDecoder: Decoder<UserMirrorNativeFileFactDto> = objectDecoder(
  'UserMirrorNativeFileFactDto',
  {
    logicalId: stringDecoder,
    contentHash: nullableDecoder(stringDecoder),
    exists: booleanDecoder,
    size: numberDecoder,
  },
);

/**
 * Business Logic: portable 项（含 MCP）不得把明文凭据键带进 metadata。
 * Code Logic: 顶层 token/value/secret fail；mcpCredential 委托凭据 decoder。
 */
export const userMirrorPortableItemDecoder: Decoder<UserMirrorPortableItemDto> = rejectMcpSecretKeys(
  'UserMirrorPortableItemDto',
  objectDecoder<UserMirrorPortableItemDto>('UserMirrorPortableItemDto', {
    kind: portableAssetKindDecoder,
    nativeId: stringDecoder,
    displayName: stringDecoder,
    contentHash: nullableDecoder(stringDecoder),
    treeHash: nullableDecoder(stringDecoder),
    actualEnabled: nullableDecoder(booleanDecoder),
    mcpCredential: nullableDecoder(userMirrorMcpCredentialFactDecoder),
    warnings: arrayDecoder(stringDecoder),
  }),
);

/** 三槽 hash。 */
export const userMirrorSlotHashesDecoder: Decoder<UserMirrorSlotHashesDto> = objectDecoder(
  'UserMirrorSlotHashesDto',
  {
    common: nullableDecoder(stringDecoder),
    adapted: nullableDecoder(stringDecoder),
    exclusive: nullableDecoder(stringDecoder),
  },
);

/** 单 Agent inventory。 */
export const userMirrorAgentInventoryDecoder: Decoder<UserMirrorAgentInventoryDto> = objectDecoder(
  'UserMirrorAgentInventoryDto',
  {
    target: agentTargetDecoder,
    slots: userMirrorSlotHashesDecoder,
    nativeFiles: arrayDecoder(userMirrorNativeFileFactDecoder),
    items: arrayDecoder(userMirrorPortableItemDecoder),
  },
);

/** 全 Agent inventory 快照。 */
export const userMirrorInventoryDecoder: Decoder<UserMirrorInventoryDto> = objectDecoder(
  'UserMirrorInventoryDto',
  {
    sourceDeviceId: stringDecoder,
    inventorySnapshotHash: stringDecoder,
    refreshedAt: stringDecoder,
    agents: arrayDecoder(userMirrorAgentInventoryDecoder),
    credentialBearingCount: numberDecoder,
  },
);

/** 原生文件变更。 */
export const userMirrorFileChangeDecoder: Decoder<UserMirrorFileChangeDto> = objectDecoder(
  'UserMirrorFileChangeDto',
  {
    logicalId: stringDecoder,
    op: userMirrorChangeOpDecoder,
    sourceHash: nullableDecoder(stringDecoder),
    destHash: nullableDecoder(stringDecoder),
  },
);

/** portable 变更。 */
export const userMirrorPortableChangeDecoder: Decoder<UserMirrorPortableChangeDto> = objectDecoder(
  'UserMirrorPortableChangeDto',
  {
    kind: portableAssetKindDecoder,
    nativeId: stringDecoder,
    displayName: stringDecoder,
    op: userMirrorChangeOpDecoder,
    credentialBearing: booleanDecoder,
  },
);

/** 单 Agent plan。 */
export const userMirrorAgentPlanDecoder: Decoder<UserMirrorAgentPlanDto> = objectDecoder(
  'UserMirrorAgentPlanDto',
  {
    target: agentTargetDecoder,
    instructionWrites: arrayDecoder(userMirrorFileChangeDecoder),
    portableUpserts: arrayDecoder(userMirrorPortableChangeDecoder),
    portableDeletes: arrayDecoder(userMirrorPortableChangeDecoder),
    pluginDisables: arrayDecoder(userMirrorPortableChangeDecoder),
    mcpDeletes: arrayDecoder(userMirrorPortableChangeDecoder),
  },
);

/** portable 资产选择键（跨 Agent 联动）。 */
export const userMirrorPortableKeyDecoder: Decoder<UserMirrorPortableKeyDto> = objectDecoder(
  'UserMirrorPortableKeyDto',
  {
    kind: portableAssetKindDecoder,
    nativeId: stringDecoder,
  },
);

/** 镜像选择过滤器；plan 里缺省/null 均表示全量。 */
export const userMirrorSelectionFilterDecoder: Decoder<UserMirrorSelectionFilterDto> =
  objectDecoder('UserMirrorSelectionFilterDto', {
    includeInstructions: booleanDecoder,
    portableKeys: nullableDecoder(arrayDecoder(userMirrorPortableKeyDecoder)),
  });

/** Preview plan（selection 为后端 apply 时写入的可选字段）。 */
export const userMirrorPlanDecoder: Decoder<UserMirrorPlanDto> = objectDecoder(
  'UserMirrorPlanDto',
  {
    planToken: stringDecoder,
    expiresAt: stringDecoder,
    direction: userMirrorDirectionDecoder,
    sourceDeviceId: stringDecoder,
    destinationDeviceId: stringDecoder,
    remoteInventorySnapshotHash: stringDecoder,
    localInventorySnapshotHash: stringDecoder,
    credentialBearingCount: numberDecoder,
    hasCredentialBearingAssets: booleanDecoder,
    agents: arrayDecoder(userMirrorAgentPlanDecoder),
    blockingReasons: arrayDecoder(stringDecoder),
    selection: optionalDecoder(nullableDecoder(userMirrorSelectionFilterDecoder)),
  },
);

/** 单 Agent apply 结果。 */
export const userMirrorAgentResultDecoder: Decoder<UserMirrorAgentResultDto> = objectDecoder(
  'UserMirrorAgentResultDto',
  {
    target: agentTargetDecoder,
    state: userMirrorItemStateDecoder,
    errorCode: nullableDecoder(stringDecoder),
    message: nullableDecoder(stringDecoder),
  },
);

/** Apply 聚合结果。 */
export const userMirrorResultDecoder: Decoder<UserMirrorResultDto> = objectDecoder(
  'UserMirrorResultDto',
  {
    planToken: stringDecoder,
    clientRequestId: stringDecoder,
    sourceDeviceId: stringDecoder,
    destinationDeviceId: stringDecoder,
    partial: booleanDecoder,
    agents: arrayDecoder(userMirrorAgentResultDecoder),
  },
);
