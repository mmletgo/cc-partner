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
export type PortableKindCounts = Partial<Record<PortableAssetKind, number>>;

/**
 * 默认筛选：Skill tab + 当前工作台默认 Agent（claude）。
 * 壳层有 hubContext 时由父层 setFilters({ target: context.agent }) 覆盖；
 * controller 独立挂载时保持 claude，避免「全部 Agent」成为主心智模型。
 */
export const DEFAULT_PORTABLE_INVENTORY_FILTERS: PortableInventoryFilters = {
  kind: 'skill',
  target: 'claude',
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
    counts[item.kind] = (counts[item.kind] ?? 0) + 1;
  }
  return counts;
}

/**
 * Business Logic: 历史 unmanaged 项（T5 ensure_managed 前残留）应引导刷新纳入，
 * 而不是走 Adopt 主路径；若后端已给 enable/disable 能力则按 managed 处理。
 * Code Logic: managementState === 'unmanaged' 且无启停/安装能力 → 需要 refresh。
 */
export function needsPortableEnsureManagedRefresh(
  item: PortableInventoryItemDto,
): boolean {
  if (item.managementState !== 'unmanaged') return false;
  if (isPortableItemReadOnly(item)) return false;
  const caps = item.capabilities;
  if (caps.canEnable || caps.canDisable || caps.canInstallToSourceTarget) return false;
  return true;
}

/**
 * Business Logic: 行上只暴露一个主动作；stale/未 opt-in/unsupported 不得 mutation。
 *   发现即管理后 **永不** 以 adopt 作为主动作（可保留 API kind 给遗留 apply）。
 * Code Logic: capability 驱动 enable/disable → installToSourceTarget；跳过 canAdopt。
 */
export function resolvePortablePrimaryAction(
  item: PortableInventoryItemDto,
  context: PortablePrimaryActionContext,
): PortableAssetActionKind | null {
  if (context.stale || context.mutationBlocked) return null;
  if (context.lockedItemIds.has(item.inventoryItemId)) return null;
  if (isPortableItemReadOnly(item)) return null;
  if (item.managementState === 'unsupported') return null;
  // 历史 unmanaged 且尚无启停能力：不伪装 Adopt；父层展示刷新文案。
  if (needsPortableEnsureManagedRefresh(item)) return null;

  const caps = item.capabilities;
  // 故意不返回 'adopt'：discover-as-managed 已取代纳入主路径。
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

/**
 * Business Logic: 列表行同时暴露启用/禁用与卸载动作，让用户无需打开详情 Drawer 即可管理。
 *   发现即管理后 **永不** 返回 adopt 作为行内动作；与 resolvePortablePrimaryAction 同样的安全门闩。
 *
 * Code Logic: 复用 stale/mutationBlocked/locked/readOnly/unsupported/unmanaged 判定，
 *   按 capabilities 累加有序数组：enable/disable（互斥）→ installToSourceTarget → uninstall。
 *   返回的数组可能为空（无任何 mutation 资格）或含多项；调用方按顺序渲染按钮。
 */
export function resolvePortableRowActions(
  item: PortableInventoryItemDto,
  context: PortablePrimaryActionContext,
): PortableAssetActionKind[] {
  if (context.stale || context.mutationBlocked) return [];
  if (context.lockedItemIds.has(item.inventoryItemId)) return [];
  if (isPortableItemReadOnly(item)) return [];
  if (item.managementState === 'unsupported') return [];
  if (needsPortableEnsureManagedRefresh(item)) return [];

  const caps = item.capabilities;
  const actions: PortableAssetActionKind[] = [];
  // enable 与 disable 基于 actualEnabled 互斥；与详情 drawer 的展示一致。
  if (item.actualEnabled !== true && caps.canEnable) {
    actions.push('enable');
  }
  if (item.actualEnabled !== false && caps.canDisable) {
    actions.push('disable');
  }
  // installToSourceTarget 仅在无原生 enable 语义（actualEnabled 为 null/undefined）时出现。
  if (
    (item.actualEnabled === null || item.actualEnabled === undefined) &&
    caps.canInstallToSourceTarget
  ) {
    actions.push('installToSourceTarget');
  }
  // uninstall 始终在末尾，仅看 canUninstall。
  if (caps.canUninstall) {
    actions.push('uninstall');
  }
  return actions;
}
