/**
 * Plugin package 投影 pure helpers。
 *
 * Business Logic（为什么需要这个模块）:
 *   PluginComponentsDrawer 需要 per-component target matrix、partial blockers
 *   与 delete preview 分组，禁止把 mixed status 压成 green synced。
 *
 * Code Logic（这个模块做什么）:
 *   从 PluginPackageReport 派生 blockers、delete groups、tone 与 cell 查找。
 */

import type {
  AgentTarget,
  AssetAggregateStatus,
  PluginComponentDeleteDecision,
  PluginComponentReport,
  PluginComponentTargetCell,
  PluginComponentTargetStatus,
  PluginDeletePreview,
  PluginDeletePreviewComponent,
  PluginPackageReport,
} from '@/lib/types/agentHub';
import { AGENT_TARGET_ORDER } from './targetMatrix';

/**
 * Business Logic: partial 必须点名 exact blockers。
 * Code Logic: 优先 report.partialBlockers；否则扫描 component cells。
 */
export function listPluginPartialBlockers(report: PluginPackageReport): string[] {
  if (report.partialBlockers.length > 0) {
    return [...report.partialBlockers];
  }
  const blockers: string[] = [];
  for (const component of report.components) {
    for (const cell of component.targets) {
      if (cell.status === 'verified') continue;
      const reason = cell.reasons[0] ?? cell.status;
      blockers.push(`${component.displayName}@${cell.target}:${reason}`);
    }
  }
  for (const residual of report.residuals) {
    if (!residual.included) {
      const reason = residual.reasons[0] ?? 'omitted';
      blockers.push(`residual@${residual.residualTarget}:${reason}`);
    }
  }
  return blockers;
}

/**
 * Business Logic: 聚合 tone 不得把 partial 当 success。
 * Code Logic: full→success；partial/sourceOnly/activation→warn；collision/blocked→danger。
 */
export function pluginAggregateTone(
  status: AssetAggregateStatus,
): 'success' | 'warn' | 'danger' | 'neutral' {
  switch (status) {
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
 * Business Logic: component cell status → Pill tone。
 * Code Logic: verified success；partial/source/activation warn；其余 danger。
 */
export function pluginComponentStatusTone(
  status: PluginComponentTargetStatus,
): 'success' | 'warn' | 'danger' | 'neutral' {
  switch (status) {
    case 'verified':
      return 'success';
    case 'partial':
    case 'sourceOnly':
    case 'activationRequired':
      return 'warn';
    case 'externalCollision':
    case 'blocked':
      return 'danger';
    default:
      return 'neutral';
  }
}

/**
 * Business Logic: 查找 component 在 target 上的 cell。
 * Code Logic: find or null。
 */
export function componentCellForTarget(
  component: PluginComponentReport,
  target: AgentTarget,
): PluginComponentTargetCell | null {
  return component.targets.find((cell) => cell.target === target) ?? null;
}

/**
 * Business Logic: 固定三端顺序补齐 component matrix（缺 cell 不 silent full）。
 * Code Logic: AGENT_TARGET_ORDER map。
 */
export function orderedComponentTargets(
  component: PluginComponentReport,
): Array<{ target: AgentTarget; cell: PluginComponentTargetCell | null }> {
  return AGENT_TARGET_ORDER.map((target) => ({
    target,
    cell: componentCellForTarget(component, target),
  }));
}

/**
 * Business Logic: delete preview 分 tombstone / preserve 两组。
 * Code Logic: 按 decision 分组。
 */
export function groupDeletePreview(preview: PluginDeletePreview | null | undefined): {
  tombstone: PluginDeletePreviewComponent[];
  preserve: PluginDeletePreviewComponent[];
} {
  const tombstone: PluginDeletePreviewComponent[] = [];
  const preserve: PluginDeletePreviewComponent[] = [];
  for (const row of preview?.components ?? []) {
    if (row.decision === 'tombstoneOwned') {
      tombstone.push(row);
    } else {
      preserve.push(row);
    }
  }
  return { tombstone, preserve };
}

/**
 * Business Logic: 是否允许把 aggregate 当“synced green”。
 * Code Logic: 仅 full 为 true。
 */
export function isPluginFullySynced(status: AssetAggregateStatus): boolean {
  return status === 'full';
}

/**
 * Business Logic: delete decision 是否保留引用。
 * Code Logic: preserve* → true。
 */
export function isPreserveDeleteDecision(decision: PluginComponentDeleteDecision): boolean {
  return decision === 'preserveShared' || decision === 'preserveStandalone';
}

/**
 * Business Logic: portable Plugin 详情需要 tombstone/preserve 计数摘要。
 * Code Logic: 复用 groupDeletePreview 计数，不发明 ownership 决策。
 */
export function summarizeDeletePreview(preview: PluginDeletePreview | null | undefined): {
  tombstoneCount: number;
  preserveCount: number;
  total: number;
} {
  const groups = groupDeletePreview(preview);
  const tombstoneCount = groups.tombstone.length;
  const preserveCount = groups.preserve.length;
  return {
    tombstoneCount,
    preserveCount,
    total: tombstoneCount + preserveCount,
  };
}

// OpenCode bridge helpers live in shared lib so Settings/Workbench/Orchestrator share fail-closed rules.
export {
  OPENCODE_RUNTIME_BRIDGE_REL_PATH,
  isOpenCodeBridgeReady,
} from '@/lib/agentAdapterPresentation';
