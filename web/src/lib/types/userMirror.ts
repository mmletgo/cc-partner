/**
 * 用户级镜像 Pull/Push 域类型。
 *
 * Business Logic（为什么需要这个模块）:
 *   Agent Hub 一次镜像全部已登记 Agent 的用户级指令与 portable 资产；
 *   前端必须消费后端冻结的 camelCase DTO；MCP 仅 present/hash，无 secret 原文。
 *
 * Code Logic（这个模块做什么）:
 *   从 `src-tauri/src/agent_hub/user_mirror/models.rs` 原样编码 wire types；
 *   禁止 loose typing、禁止发明 optional default。
 */

import type { AgentTarget } from './agentHub';
import type { PortableAssetKind } from './portableInventory';

/** Preview plan 有效期（分钟），与 Rust `USER_MIRROR_PLAN_TTL_MINUTES` 对齐。 */
export const USER_MIRROR_PLAN_TTL_MINUTES = 15;

/** 镜像传输累计上限（字节），与 Rust `USER_MIRROR_DEST_MAX_TOTAL_BYTES` 对齐。 */
export const USER_MIRROR_DEST_MAX_TOTAL_BYTES = 512 * 1024 * 1024;

/** 对端未宣告 `agent-hub.user-mirror.v1`。 */
export const USER_MIRROR_CAPABILITY_UNSUPPORTED = 'USER_MIRROR_CAPABILITY_UNSUPPORTED';

/** 所选对端离线。 */
export const USER_MIRROR_PEER_OFFLINE = 'USER_MIRROR_PEER_OFFLINE';

/** 源/目标 inventory 或 plan 绑定已漂移。 */
export const USER_MIRROR_STALE = 'USER_MIRROR_STALE';

/** 未预览或预览与当前选择不一致。 */
export const USER_MIRROR_PREVIEW_REQUIRED = 'USER_MIRROR_PREVIEW_REQUIRED';

/** 对象累计超过 512 MiB。 */
export const USER_MIRROR_TRANSFER_LIMIT = 'USER_MIRROR_TRANSFER_LIMIT';

/** 解析结果落到白名单外路径。 */
export const USER_MIRROR_NATIVE_PATH_FORBIDDEN = 'USER_MIRROR_NATIVE_PATH_FORBIDDEN';

/** MCP 占位凭据不得覆盖目标已有真凭据。 */
export const USER_MIRROR_LEGACY_LOSSY_BLOCKED = 'USER_MIRROR_LEGACY_LOSSY_BLOCKED';

/**
 * 镜像方向。
 *
 * Business Logic: apply 端永远是 destination；UI 方向决定谁是 source。
 * Code Logic: camelCase wire；Pull 对端覆盖本机，Push 本机覆盖所选对端。
 */
export type UserMirrorDirection = 'pull' | 'push';

/**
 * 预览中单条文件/资产将执行的动作。
 *
 * Business Logic: 用户必须在确认框看到写/替换/清空/删除/停用，而不是笼统「同步」。
 * Code Logic: Plugin 多余为 Disable，Skill/MCP 多余为 Delete，空原生文件为 Clear。
 */
export type UserMirrorChangeOp = 'write' | 'replace' | 'clear' | 'delete' | 'disable';

/**
 * 单 Agent / 条目落地结果。
 *
 * Business Logic: 崩溃后未知不得标成功；部分成功不回滚。
 * Code Logic: `outcomeUnknown` 表示未完成。
 */
export type UserMirrorItemState = 'succeeded' | 'failed' | 'skipped' | 'outcomeUnknown';

/**
 * MCP 凭据仅暴露 present/hash，永不回显 secret。
 *
 * Business Logic: inventory/UI/log 不得包含明文 env；凭据只在 CAS 对象里。
 * Code Logic: `present` + 可选内容 hash。
 */
export interface UserMirrorMcpCredentialFactDto {
  present: boolean;
  hash: string | null;
}

/**
 * 用户级原生提示词文件的元数据事实（无绝对路径）。
 *
 * Business Logic: 预览按逻辑 id 对号入座，禁止把路径泄漏到 LAN JSON。
 * Code Logic: `logicalId` 形如 `claude.native.CLAUDE.md` / `cursor.slot.adapted`。
 */
export interface UserMirrorNativeFileFactDto {
  logicalId: string;
  contentHash: string | null;
  exists: boolean;
  size: number;
}

/**
 * 用户级 portable 条目元数据。
 *
 * Business Logic: Skill/Command/Plugin/MCP 进入镜像选择；MCP 凭据只给 present/hash。
 * Code Logic: 复用 `PortableAssetKind`；warnings 为扫描诊断，不含 secret。
 */
export interface UserMirrorPortableItemDto {
  kind: PortableAssetKind;
  nativeId: string;
  displayName: string;
  contentHash: string | null;
  treeHash: string | null;
  actualEnabled: boolean | null;
  mcpCredential: UserMirrorMcpCredentialFactDto | null;
  warnings: string[];
}

/**
 * 三槽 canonical 内容 hash。
 *
 * Business Logic: inventory 只传 hash，正文走 CAS，避免把整份指令放进元数据快照。
 * Code Logic: 空槽为 null。
 */
export interface UserMirrorSlotHashesDto {
  common: string | null;
  adapted: string | null;
  exclusive: string | null;
}

/**
 * 单个已登记 Agent 的用户级 inventory。
 *
 * Business Logic: 同名 Agent 对号入座；一次覆盖 catalog 全部 Hub Agent。
 * Code Logic: slots + 原生文件事实 + portable 条目。
 */
export interface UserMirrorAgentInventoryDto {
  target: AgentTarget;
  slots: UserMirrorSlotHashesDto;
  nativeFiles: UserMirrorNativeFileFactDto[];
  items: UserMirrorPortableItemDto[];
}

/**
 * 全 Agent 用户级元数据快照。
 *
 * Business Logic: 源端暴露 inventory；无 path、无 secret、无 env。
 * Code Logic: `inventorySnapshotHash` 绑定 preview/apply，漂移则 STALE。
 */
export interface UserMirrorInventoryDto {
  sourceDeviceId: string;
  inventorySnapshotHash: string;
  refreshedAt: string;
  agents: UserMirrorAgentInventoryDto[];
  credentialBearingCount: number;
}

/**
 * 预览镜像请求。
 *
 * Business Logic: Pull 选一台源设备；Push 可多选对端；没有条目勾选或 mode。
 * Code Logic: `sourceDeviceId` 仅 Pull；`peerDeviceIds` 仅 Push。
 */
export interface PreviewUserMirrorRequest {
  direction: UserMirrorDirection;
  sourceDeviceId?: string | null;
  peerDeviceIds: string[];
}

/**
 * 原生提示词文件将发生的变更。
 *
 * Business Logic: 预览列出写/替换/清空，用户确认后才 apply。
 * Code Logic: 只含逻辑 id 与两侧 hash，无绝对路径。
 */
export interface UserMirrorFileChangeDto {
  logicalId: string;
  op: UserMirrorChangeOp;
  sourceHash: string | null;
  destHash: string | null;
}

/**
 * portable 资产将发生的变更。
 *
 * Business Logic: 预览按新增/替换/删除/停用列出，并标是否含凭据。
 * Code Logic: MCP 删除与 Plugin disable 分列于 plan 不同字段。
 */
export interface UserMirrorPortableChangeDto {
  kind: PortableAssetKind;
  nativeId: string;
  displayName: string;
  op: UserMirrorChangeOp;
  credentialBearing: boolean;
}

/**
 * 单个 Agent 的镜像 plan。
 *
 * Business Logic: 用户按 Agent 看到指令文件与资产变更数量。
 * Code Logic: 指令写、portable upsert/delete、plugin disable、MCP 删除分列。
 */
export interface UserMirrorAgentPlanDto {
  target: AgentTarget;
  instructionWrites: UserMirrorFileChangeDto[];
  portableUpserts: UserMirrorPortableChangeDto[];
  portableDeletes: UserMirrorPortableChangeDto[];
  pluginDisables: UserMirrorPortableChangeDto[];
  mcpDeletes: UserMirrorPortableChangeDto[];
}

/**
 * portable 资产选择键：跨 Agent 联动（同名 skill 在多个 Agent 上算同一资产）。
 *
 * Business Logic: 用户按 (kind, nativeId) 勾选要同步的资产；同名资产跨 Agent 一起同步。
 * Code Logic: 与 Rust `UserMirrorPortableKeyDto` 对齐；`(kind, nativeId)` 对号 inventory。
 */
export interface UserMirrorPortableKeyDto {
  kind: PortableAssetKind;
  nativeId: string;
}

/**
 * 镜像选择过滤器；undefined / null = 全部同步（默认行为不变）。
 *
 * Business Logic: pull/push 可只同步部分资产；缺省必须等价旧全量镜像。
 * Code Logic: `includeInstructions=false` 跳过指令文件与 Hub 三槽；
 * `portableKeys=null` = 全部 portable，数组 = 仅选中键（跨 Agent 联动）。
 */
export interface UserMirrorSelectionFilterDto {
  includeInstructions: boolean;
  portableKeys: UserMirrorPortableKeyDto[] | null;
}

/**
 * 绑定源/目标 inventory 的镜像 preview plan。
 *
 * Business Logic: apply 必须带本 plan；TTL 15 分钟；凭据条数用于确认框披露。
 * Code Logic: `planToken` + 两侧 snapshot hash + per-agent 变更。
 */
export interface UserMirrorPlanDto {
  planToken: string;
  expiresAt: string;
  direction: UserMirrorDirection;
  sourceDeviceId: string;
  destinationDeviceId: string;
  remoteInventorySnapshotHash: string;
  localInventorySnapshotHash: string;
  credentialBearingCount: number;
  hasCredentialBearingAssets: boolean;
  agents: UserMirrorAgentPlanDto[];
  blockingReasons: string[];
  /** apply 时写入的同步范围；缺省/null = 全量（preview 恒为 null）。 */
  selection?: UserMirrorSelectionFilterDto | null;
}

/**
 * 应用已预览镜像的请求。
 *
 * Business Logic: 同 `clientRequestId` 重放同一结果；不同 plan 冲突。
 * Code Logic: planToken + clientRequestId；selection 缺省 = 全量同步（旧语义不变）。
 */
export interface ApplyUserMirrorRequest {
  planToken: string;
  clientRequestId: string;
  selection?: UserMirrorSelectionFilterDto | null;
}

/**
 * 单个 Agent 的 apply 结果。
 *
 * Business Logic: 分项展示 succeeded/failed/unknown，已成功项保留。
 * Code Logic: 失败时带稳定 errorCode。
 */
export interface UserMirrorAgentResultDto {
  target: AgentTarget;
  state: UserMirrorItemState;
  errorCode: string | null;
  message: string | null;
}

/**
 * 一次镜像 apply 的整次结果。
 *
 * Business Logic: `partial=true` 当且仅当存在失败或 unknown。
 * Code Logic: 绑定 plan/request 与源/目标 device，按 Agent 列出状态。
 */
export interface UserMirrorResultDto {
  planToken: string;
  clientRequestId: string;
  sourceDeviceId: string;
  destinationDeviceId: string;
  partial: boolean;
  agents: UserMirrorAgentResultDto[];
}

/**
 * 用户级镜像 API 形状（controller 消费）。
 *
 * Business Logic: preview 绑定 plan；apply/get 用 planToken + clientRequestId 幂等对账。
 * Code Logic: 三条 invoke 命令；request 参数名 `request`；get 用 clientRequestId。
 */
export interface UserMirrorApi {
  preview(request: PreviewUserMirrorRequest): Promise<UserMirrorPlanDto>;
  apply(request: ApplyUserMirrorRequest): Promise<UserMirrorResultDto>;
  get(clientRequestId: string): Promise<UserMirrorResultDto>;
}
