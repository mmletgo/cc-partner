/**
 * Agent Hub 目标矩阵 pure helpers。
 *
 * Business Logic（为什么需要这个模块）:
 *   Task 8 矩阵需要把 aggregateStatus / cell 输入映射为可测 UI 语义，
 *   与 controller 解耦，禁止 views 直连 API。
 *
 * Code Logic（这个模块做什么）:
 *   导出 cell 查找、聚合标签、动作可用性与 partial 原因列表。
 */

import type {
  AgentHubAssetSummary,
  AgentHubTargetCell,
  AgentTarget,
  AssetAggregateStatus,
  MaterializationStatus,
} from '@/lib/types/agentHub';

export const AGENT_TARGET_ORDER: AgentTarget[] = ['claude', 'codex', 'opencode'];

export const AGGREGATE_STATUSES: AssetAggregateStatus[] = [
  'unconfigured',
  'full',
  'partial',
  'sourceOnly',
  'activationRequired',
  'externalCollision',
  'detached',
  'blocked',
];

/**
 * Business Logic: 按固定顺序取 target cell。
 * Code Logic: find or null。
 */
export function cellForTarget(
  asset: AgentHubAssetSummary,
  target: AgentTarget,
): AgentHubTargetCell | null {
  return asset.targets.find((cell) => cell.target === target) ?? null;
}

/**
 * Business Logic: 列出 partial 的缺失/不等组件原因。
 * Code Logic: 扫描 requested 单元格的 supported/verified/sourceOnly/mat 状态。
 */
export function listPartialReasons(asset: AgentHubAssetSummary): string[] {
  const reasons: string[] = [];
  for (const target of AGENT_TARGET_ORDER) {
    const cell = cellForTarget(asset, target);
    if (!cell) {
      reasons.push(`${target}:missing-binding`);
      continue;
    }
    if (cell.desiredPresence !== 'present') continue;
    if (!cell.supported) reasons.push(`${target}:unsupported`);
    if (cell.sourceOnly) reasons.push(`${target}:sourceOnly`);
    if (!cell.verified) reasons.push(`${target}:not-verified`);
    if (cell.materializationStatus === 'blocked' || cell.materializationStatus === 'failed') {
      reasons.push(`${target}:${cell.materializationStatus}`);
    }
    if (cell.lastError) reasons.push(`${target}:error`);
  }
  return reasons;
}

/**
 * Business Logic: full 表示 verified invocation，不应再展示 install 动作。
 * Code Logic: aggregate === full。
 */
export function isVerifiedInvocation(status: AssetAggregateStatus): boolean {
  return status === 'full';
}

/**
 * Business Logic: sourceOnly 仅展示源 target，其它 target 不提供 install。
 * Code Logic: 仅 originNamespace 对应 target 视为源。
 */
export function isSourceTarget(asset: AgentHubAssetSummary, target: AgentTarget): boolean {
  return asset.originNamespace === target;
}

/**
 * Business Logic: 是否允许 enable/disable 该 target。
 * Code Logic: 有 cell、supported、非 sourceOnly；sourceOnly 聚合仅源 target 可 toggle。
 *   writeBlocked 由调用方叠加。
 */
export function canToggleEnabled(
  asset: AgentHubAssetSummary,
  target: AgentTarget,
  cell: AgentHubTargetCell | null,
): boolean {
  if (!cell) return false;
  if (!cell.supported) return false;
  if (cell.sourceOnly) return false;
  // aggregate sourceOnly 时其它 CLI 不提供 install/toggle
  if (asset.aggregateStatus === 'sourceOnly' && !isSourceTarget(asset, target)) {
    return false;
  }
  return true;
}

/**
 * Business Logic: detached 提供 restore/remove 选择。
 * Code Logic: 仅该 cell 的 mat 为 detached（不因 row aggregate 波及其它 target）。
 */
export function isDetachedCell(
  _asset: AgentHubAssetSummary,
  cell: AgentHubTargetCell | null,
): boolean {
  return cell?.materializationStatus === 'detached';
}

/**
 * Business Logic: activationRequired 展示手动激活说明。
 * Code Logic: 仅该 cell mat 命中（不因 row aggregate 波及其它 target）。
 */
export function needsActivation(
  _asset: AgentHubAssetSummary,
  cell: AgentHubTargetCell | null,
): boolean {
  return cell?.materializationStatus === 'activationRequired';
}

/**
 * Business Logic: externalCollision 打开 adoption/collision 预览。
 * Code Logic: 仅该 cell mat 命中（不因 row aggregate 波及其它 target）。
 */
export function hasExternalCollision(
  _asset: AgentHubAssetSummary,
  cell: AgentHubTargetCell | null,
): boolean {
  return cell?.materializationStatus === 'externalCollision';
}

/**
 * Business Logic: blocked 展示 support/evidence reason。
 * Code Logic: lastError 优先，否则 cell mat blocked（不因 row aggregate 波及其它 target）。
 */
export function blockedReason(
  _asset: AgentHubAssetSummary,
  cell: AgentHubTargetCell | null,
): string | null {
  if (cell?.lastError) return cell.lastError;
  if (cell?.materializationStatus === 'blocked') return 'blocked';
  return null;
}

/**
 * Business Logic: 矩阵 Pill tone。
 * Code Logic: 按 aggregate/mat 映射。
 */
export function aggregateTone(
  status: AssetAggregateStatus,
): 'success' | 'warn' | 'danger' | 'neutral' {
  switch (status) {
    case 'unconfigured':
      return 'neutral';
    case 'full':
      return 'success';
    case 'partial':
    case 'sourceOnly':
    case 'activationRequired':
    case 'detached':
      return 'warn';
    case 'externalCollision':
    case 'blocked':
      return 'danger';
    default:
      return 'neutral';
  }
}

/**
 * Business Logic: materialization 状态映射 Pill tone。
 * Code Logic: switch。
 */
export function materializationTone(
  status: MaterializationStatus | null | undefined,
): 'success' | 'warn' | 'danger' | 'neutral' {
  if (!status) return 'neutral';
  switch (status) {
    case 'synced':
      return 'success';
    case 'pending':
    case 'writing':
    case 'activationRequired':
    case 'drifted':
    case 'drift':
    case 'detached':
      return 'warn';
    case 'blocked':
    case 'failed':
    case 'conflict':
    case 'unsupported':
    case 'externalCollision':
      return 'danger';
    default:
      return 'neutral';
  }
}

/**
 * Business Logic: 选中源 target 的 invocation alias 展示。
 * Code Logic: 优先 cell.invocationAlias，否则 logicalKey 末段。
 */
export function resolveInvocationLabel(
  asset: AgentHubAssetSummary,
  target: AgentTarget,
): string {
  const cell = cellForTarget(asset, target);
  if (cell?.invocationAlias && cell.invocationAlias.trim()) {
    return cell.invocationAlias.trim();
  }
  const key = asset.logicalKey;
  const slash = key.lastIndexOf('/');
  return slash >= 0 ? key.slice(slash + 1) : key;
}
