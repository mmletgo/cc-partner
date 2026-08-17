/**
 * Same-agent portable pull pure presentation helpers.
 *
 * Business Logic（为什么需要这个模块）:
 *   Pull Drawer 需要在本地筛选远端 inventory、固定 same-target 文案、
 *   显式展示 canonical-only mapping、skip/replace 差异与 credential boolean；
 *   这些规则不能散落在 React 组件里。
 *
 * Code Logic（这个模块做什么）:
 *   纯函数：过滤、selection helper、plan/result 摘要、confirm gate；无 React/API/i18n 实例。
 */

import type { AgentTarget } from '@/lib/types/agentHub';
import type {
  PortableAssetConflictPolicy,
  PortableAssetKind,
  PortablePullChangeDto,
  PortablePullInstallMode,
  PortablePullItemState,
  PortablePullPlanDto,
  PortablePullResultDto,
  RemotePortableInventoryDto,
  RemotePortableInventoryItemDto,
} from '@/lib/types/portableInventory';

/** 远端 Pull 列表本地筛选（不传给后端自由文本 kind）。 */
export type PortablePullFilters = {
  kind: 'all' | PortableAssetKind;
  scope: 'all' | 'user' | 'project';
  actualState: 'all' | 'enabled' | 'disabled' | 'problem';
  search: string;
};

export interface PortablePullConflictDiffSummary {
  conflictCount: number;
  skippedByPolicy: number;
  replaceCandidates: number;
}

export interface PortablePullCredentialDisclosure {
  hasCredentialBearingAssets: boolean;
  credentialBearingCount: number;
}

export interface PortablePullProgressSummary {
  total: number;
  succeeded: number;
  skipped: number;
  failed: number;
  blocked: number;
  importedCanonicalOnly: number;
  outcomeUnknown: number;
  partial: boolean;
}

export interface PortablePullConfirmGateInput {
  remoteInventory: RemotePortableInventoryDto | null;
  selectedItemIds: Set<string>;
  plan: PortablePullPlanDto | null;
  busy: boolean;
}

export interface PortablePullConfirmGate {
  ok: boolean;
  reason:
    | 'ok'
    | 'busy'
    | 'missingInventory'
    | 'staleInventory'
    | 'emptySelection'
    | 'missingPlan'
    | 'planHashMismatch';
}

/**
 * Business Logic: 远端列表筛选只消费 inventory read model。
 * Code Logic: kind/scope/actual/search 本地过滤；problem = warnings 或 actualEnabled null。
 */
export function filterRemotePortableItems(
  items: RemotePortableInventoryItemDto[],
  filters: PortablePullFilters,
): RemotePortableInventoryItemDto[] {
  const search = filters.search.trim().toLowerCase();
  return items.filter((item) => {
    if (filters.kind !== 'all' && item.kind !== filters.kind) return false;

    if (filters.scope === 'user') {
      if (item.projectId) return false;
    } else if (filters.scope === 'project') {
      if (!item.projectId) return false;
    }

    if (filters.actualState === 'enabled') {
      if (item.actualEnabled !== true) return false;
    } else if (filters.actualState === 'disabled') {
      if (item.actualEnabled !== false) return false;
    } else if (filters.actualState === 'problem') {
      const problem = item.actualEnabled === null || item.warnings.length > 0;
      if (!problem) return false;
    }

    if (search) {
      const haystack = [
        item.displayName,
        item.nativeId,
        item.inventoryItemId,
        item.description ?? '',
        item.projectId ?? '',
        item.scopeId,
        item.kind,
      ]
        .join(' ')
        .toLowerCase();
      if (!haystack.includes(search)) return false;
    }

    return true;
  });
}

/**
 * Business Logic: “选择可见”只勾当前筛选结果。
 * Code Logic: 返回可见 inventoryItemId 集合。
 */
export function selectVisibleRemoteItemIds(
  visibleItems: RemotePortableInventoryItemDto[],
): Set<string> {
  return new Set(visibleItems.map((item) => item.inventoryItemId));
}

/**
 * Business Logic: destination 固定为 sourceTarget，UI 只展示 same-agent 文案。
 * Code Logic: 返回 i18n key，不渲染跨 target picker。
 */
export function sameAgentDestinationLabelKey(sourceTarget: AgentTarget): string {
  switch (sourceTarget) {
    case 'claude':
      return 'agentHub:portablePull.destination.sameAsClaude';
    case 'codex':
      return 'agentHub:portablePull.destination.sameAsCodex';
    case 'opencode':
      return 'agentHub:portablePull.destination.sameAsOpenCode';
    case 'grok':
      return 'agentHub:portablePull.destination.sameAsGrok';
    case 'gemini':
      return 'agentHub:portablePull.destination.sameAsGemini';
    case 'cursor':
      return 'agentHub:portablePull.destination.sameAsCursor';
    case 'pi':
      return 'agentHub:portablePull.destination.sameAsPi';
  }
}

/**
 * Business Logic: 未映射项目只能 importedCanonicalOnly，必须显式展示。
 * Code Logic: 过滤 installMode === importedCanonicalOnly 的 changes。
 */
export function mapCanonicalOnlyChanges(
  changes: PortablePullChangeDto[],
): PortablePullChangeDto[] {
  return changes.filter((change) => change.installMode === 'importedCanonicalOnly');
}

/**
 * Business Logic: install mode 文案 key 固定映射，避免自由文本。
 * Code Logic: 返回 agentHub:portablePull.installMode.* key。
 */
export function formatPullInstallModeLabelKey(mode: PortablePullInstallMode): string {
  return `agentHub:portablePull.installMode.${mode}`;
}

/**
 * Business Logic: skip/replace 策略差异必须可读。
 * Code Logic: 统计 conflict 项与策略下 skip/replace 候选。
 */
export function summarizeConflictPolicyDiff(
  policy: PortableAssetConflictPolicy,
  changes: PortablePullChangeDto[],
): PortablePullConflictDiffSummary {
  const conflictChanges = changes.filter((change) => change.conflict);
  const conflictCount = conflictChanges.length;
  if (policy === 'skipExisting') {
    return {
      conflictCount,
      skippedByPolicy: conflictChanges.filter((c) => c.installMode === 'skipExisting').length,
      replaceCandidates: 0,
    };
  }
  return {
    conflictCount,
    skippedByPolicy: 0,
    replaceCandidates: conflictChanges.filter((c) => c.installMode === 'installToTarget').length,
  };
}

/**
 * Business Logic: 凭据只披露 boolean/count，不暴露 secret。
 * Code Logic: 从 plan 的 hasCredentialBearingAssets/count 投影。
 */
export function credentialDisclosureFromPlan(
  plan: PortablePullPlanDto | null,
): PortablePullCredentialDisclosure {
  if (!plan) {
    return { hasCredentialBearingAssets: false, credentialBearingCount: 0 };
  }
  return {
    hasCredentialBearingAssets: plan.hasCredentialBearingAssets,
    credentialBearingCount: plan.credentialBearingCount,
  };
}

/**
 * Business Logic: partial/outcomeUnknown 不得显示全成功。
 * Code Logic: 统计 result items 各 state。
 */
export function summarizePullResultProgress(
  result: PortablePullResultDto | null,
): PortablePullProgressSummary {
  if (!result) {
    return {
      total: 0,
      succeeded: 0,
      skipped: 0,
      failed: 0,
      blocked: 0,
      importedCanonicalOnly: 0,
      outcomeUnknown: 0,
      partial: false,
    };
  }
  const counts = {
    total: result.items.length,
    succeeded: 0,
    skipped: 0,
    failed: 0,
    blocked: 0,
    importedCanonicalOnly: 0,
    outcomeUnknown: 0,
    partial: result.partial,
  };
  for (const item of result.items) {
    switch (item.state) {
      case 'succeeded':
        counts.succeeded += 1;
        break;
      case 'skipped':
        counts.skipped += 1;
        break;
      case 'failed':
        counts.failed += 1;
        break;
      case 'blocked':
        counts.blocked += 1;
        break;
      case 'importedCanonicalOnly':
        counts.importedCanonicalOnly += 1;
        break;
      case 'outcomeUnknown':
        counts.outcomeUnknown += 1;
        break;
    }
  }
  return counts;
}

/**
 * Business Logic: partial 或 outcomeUnknown 需要 reconcile，禁止标全成功。
 * Code Logic: partial || any outcomeUnknown。
 */
export function needsPullReconcile(result: PortablePullResultDto | null): boolean {
  if (!result) return false;
  if (result.partial) return true;
  return result.items.some((item) => item.state === 'outcomeUnknown');
}

/**
 * Business Logic: 结果行 tone 映射。
 * Code Logic: success/warn/danger/neutral。
 */
export function portablePullItemResultTone(
  state: PortablePullItemState,
): 'success' | 'warn' | 'danger' | 'neutral' {
  switch (state) {
    case 'succeeded':
      return 'success';
    case 'importedCanonicalOnly':
    case 'skipped':
    case 'outcomeUnknown':
      return 'warn';
    case 'failed':
    case 'blocked':
      return 'danger';
  }
}

/**
 * Business Logic: stale inventory / 空选择 / 无 plan 禁止 confirm。
 * Code Logic: pure gate；plan hash 必须匹配当前 remote inventory hash。
 */
export function canConfirmPortablePull(input: PortablePullConfirmGateInput): PortablePullConfirmGate {
  if (input.busy) return { ok: false, reason: 'busy' };
  if (!input.remoteInventory) return { ok: false, reason: 'missingInventory' };
  if (input.remoteInventory.stale) return { ok: false, reason: 'staleInventory' };
  if (input.selectedItemIds.size === 0) return { ok: false, reason: 'emptySelection' };
  if (!input.plan) return { ok: false, reason: 'missingPlan' };
  if (input.plan.remoteInventorySnapshotHash !== input.remoteInventory.inventorySnapshotHash) {
    return { ok: false, reason: 'planHashMismatch' };
  }
  return { ok: true, reason: 'ok' };
}
