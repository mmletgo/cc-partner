/**
 * Portable inventory pure filter / status presentation helpers.
 *
 * Business Logic（为什么需要这个模块）:
 *   列表筛选、问题分类与主行动作必须从 observed inventory 纯函数推导，
 *   不依赖 React/API；Plugin component 不得与 standalone 合并计数。
 *
 * Code Logic（这个模块做什么）:
 *   暴露 filters 默认值、match/filter、kind count、actualState 分类、primary action 解析。
 */

import type { AgentTarget } from '@/lib/types/agentHub';
import type {
  PortableAssetActionKind,
  PortableAssetKind,
  PortableInventoryItemDto,
  PortableInventoryManagementState,
} from '@/lib/types/portableInventory';

/** 列表筛选状态（前端本地，不回传后端自由文本）。 */
export type PortableInventoryFilters = {
  kind: PortableAssetKind;
  target: 'all' | AgentTarget;
  scope: 'all' | 'user' | 'project';
  actualState: 'all' | 'enabled' | 'disabled' | 'problem';
  management: 'all' | PortableInventoryManagementState;
  search: string;
};

/** actual 状态分类（enabled/disabled/problem/unknown）。 */
export type PortableActualStateClass = 'enabled' | 'disabled' | 'problem' | 'unknown';

/** primary action 上下文（stale / lock / global mutation gate）。 */
export interface PortablePrimaryActionContext {
  stale: boolean;
  mutationBlocked: boolean;
  lockedItemIds: ReadonlySet<string>;
}

/** kind tab 计数（排除 pluginComponent）。 */
export type PortableKindCounts = Record<PortableAssetKind, number>;

/** 默认筛选：Skill tab，其余 all。 */
export const DEFAULT_PORTABLE_INVENTORY_FILTERS: PortableInventoryFilters = {
  kind: 'skill',
  target: 'all',
  scope: 'all',
  actualState: 'all',
  management: 'all',
  search: '',
};

const PROBLEM_MANAGEMENT: ReadonlySet<PortableInventoryManagementState> = new Set([
  'drifted',
  'externalCollision',
  'unsupported',
]);

const PORTABILITY_ADVISORY_CODES: ReadonlySet<string> = new Set([
  'absolutePath',
  'targetExecutable',
  'unsupportedInterpolation',
  'modelNotPortable',
  'permissionNotPortable',
  'unknownSourceField',
  'materializedAlias',
  'plugin_has_components',
]);

/** 判断 warning token 是否只是跨设备/跨 CLI 可移植性提示。 */
function isPortabilityAdvisory(code: string): boolean {
  return PORTABILITY_ADVISORY_CODES.has(code) || code.startsWith('transport:');
}

/** 返回会影响本机健康态的 warning；可移植性提示留在详情，不污染主列表。 */
export function portableInventoryProblemWarnings(
  item: PortableInventoryItemDto,
): string[] {
  return item.warnings.filter((warning) => !isPortabilityAdvisory(warning));
}

/**
 * Business Logic: Plugin component 在 standalone 列表中不展示/不计数。
 * Code Logic: sourceOrigin === 'pluginComponent' 即视为 component。
 */
export function isPortablePluginComponent(item: PortableInventoryItemDto): boolean {
  return item.sourceOrigin === 'pluginComponent';
}

/**
 * Business Logic: 异常态优先于 enabled/disabled，避免把 collision/drift 显示成健康。
 * Code Logic: management/warnings 命中 problem；否则按 actualEnabled。
 */
export function isPortableInventoryProblem(item: PortableInventoryItemDto): boolean {
  if (PROBLEM_MANAGEMENT.has(item.managementState)) return true;
  if (portableInventoryProblemWarnings(item).length > 0) return true;
  return false;
}

/**
 * Business Logic: actualEnabled 只能来自扫描；null 表示无原生 enable 语义。
 * Code Logic: problem 优先，再映射 true/false，null → unknown。
 */
export function classifyPortableActualState(
  item: PortableInventoryItemDto,
): PortableActualStateClass {
  if (isPortableInventoryProblem(item)) return 'problem';
  if (item.actualEnabled === true) return 'enabled';
  if (item.actualEnabled === false) return 'disabled';
  return 'unknown';
}

/**
 * Business Logic: 未 opt-in 项目只允许只读扫描。
 * Code Logic: project scope 且 projectOptedIn=false → 只读。
 */
export function isPortableItemReadOnly(item: PortableInventoryItemDto): boolean {
  if (item.scopeKind === 'project' && !item.projectOptedIn) return true;
  return false;
}

/**
 * Business Logic: 列表搜索对名称/描述/nativeId/path 做不区分大小写子串匹配。
 * Code Logic: trim + lowercase；空串恒 true。
 */
function matchesSearch(item: PortableInventoryItemDto, search: string): boolean {
  const needle = search.trim().toLowerCase();
  if (!needle) return true;
  const haystacks = [
    item.displayName,
    item.nativeId,
    item.description ?? '',
    item.sourcePath ?? '',
    item.scopeId,
  ];
  return haystacks.some((value) => value.toLowerCase().includes(needle));
}

/**
 * Business Logic: 单个 item 是否满足当前筛选（不含 plugin component 排除）。
 * Code Logic: 各维度 AND；actualState problem 用 isPortableInventoryProblem。
 */
export function matchesPortableInventoryItem(
  item: PortableInventoryItemDto,
  filters: PortableInventoryFilters,
): boolean {
  if (item.kind !== filters.kind) return false;
  if (filters.target !== 'all' && item.target !== filters.target) return false;
  if (filters.scope !== 'all') {
    if (filters.scope === 'user' && item.scopeKind !== 'user') return false;
    if (filters.scope === 'project' && item.scopeKind !== 'project') return false;
  }
  if (filters.management !== 'all' && item.managementState !== filters.management) {
    return false;
  }
  if (filters.actualState !== 'all') {
    const actual = classifyPortableActualState(item);
    if (filters.actualState === 'problem') {
      if (actual !== 'problem') return false;
    } else if (actual !== filters.actualState) {
      return false;
    }
  }
  if (!matchesSearch(item, filters.search)) return false;
  return true;
}

/**
 * Business Logic: 可见列表以 observed inventory 为真源，排除 plugin components。
 * Code Logic: filter standalone 再 apply matches。
 */
export function filterPortableInventoryItems(
  items: readonly PortableInventoryItemDto[],
  filters: PortableInventoryFilters,
): PortableInventoryItemDto[] {
  return items.filter(
    (item) => !isPortablePluginComponent(item) && matchesPortableInventoryItem(item, filters),
  );
}

/**
 * Business Logic: kind tab 计数反映 standalone 主列表体量。
 * Code Logic: 跳过 pluginComponent，按 kind 累加。
 */
export function countPortableItemsByKind(
  items: readonly PortableInventoryItemDto[],
): PortableKindCounts {
  const counts: PortableKindCounts = {
    skill: 0,
    command: 0,
    plugin: 0,
    mcp: 0,
  };
  for (const item of items) {
    if (isPortablePluginComponent(item)) continue;
    counts[item.kind] += 1;
  }
  return counts;
}

/**
 * Business Logic: 行上只暴露一个主动作；stale/未 opt-in/unsupported 不得 mutation。
 * Code Logic: capability 驱动 adopt → enable/disable → installToSourceTarget 优先级。
 */
export function resolvePortablePrimaryAction(
  item: PortableInventoryItemDto,
  context: PortablePrimaryActionContext,
): PortableAssetActionKind | null {
  if (context.stale || context.mutationBlocked) return null;
  if (context.lockedItemIds.has(item.inventoryItemId)) return null;
  if (isPortableItemReadOnly(item)) return null;
  if (item.managementState === 'unsupported') return null;

  const caps = item.capabilities;
  if (caps.canAdopt) return 'adopt';
  if (item.actualEnabled === true && caps.canDisable) return 'disable';
  if (item.actualEnabled === false && caps.canEnable) return 'enable';
  if (item.actualEnabled === null || item.actualEnabled === undefined) {
    if (caps.canInstallToSourceTarget) return 'installToSourceTarget';
  }
  if (caps.canInstallToSourceTarget && !caps.canEnable && !caps.canDisable) {
    return 'installToSourceTarget';
  }
  return null;
}
