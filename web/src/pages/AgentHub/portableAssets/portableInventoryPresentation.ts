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

import { allHubTargets, isHubTarget } from '@/lib/agentCatalog';
import type { AgentTarget } from '@/lib/types/agentHub';
import type {
  PortableAssetActionKind,
  PortableAssetKind,
  PortableInventoryItemDto,
  PortableInventoryManagementState,
  PortableInventoryOwnedBy,
} from '@/lib/types/portableInventory';
import type { PortableAssetLane } from '../context/agentHubContext';

/** 列表筛选状态（前端本地，不回传后端自由文本）。 */
export type PortableInventoryFilters = {
  kind: PortableAssetKind;
  target: 'all' | AgentTarget;
  scope: 'all' | 'user' | 'project';
  actualState: 'all' | 'enabled' | 'disabled' | 'problem';
  management: 'all' | PortableInventoryManagementState;
  search: string;
  /** Skill/Command 已装备 / 仓库；MCP/Plugin 忽略。 */
  assetLane: PortableAssetLane;
};

/** actual 状态分类（enabled/disabled/problem/unknown）。 */
export type PortableActualStateClass = 'enabled' | 'disabled' | 'problem' | 'unknown';

/** primary action 上下文（stale / lock / global mutation gate）。 */
export interface PortablePrimaryActionContext {
  stale: boolean;
  mutationBlocked: boolean;
  lockedItemIds: ReadonlySet<string>;
  /**
   * 当前 Skill/Command 存放面。已装备不暴露 destroyStore（删真树只在仓库页）。
   * 缺省视为非 equipped，保持旧调用方仍能测到 destroyStore。
   */
  assetLane?: PortableAssetLane;
}

/** kind tab 计数（排除 pluginComponent）。 */
export type PortableKindCounts = Partial<Record<PortableAssetKind, number>>;

/** borrowedFrom i18n 片段：Agent target / 共享 ~/.agents / 未知。 */
export type PortableBorrowedOwnerLabelKey = PortableInventoryOwnedBy;

/** 已安装在此 vs 运行时借用分区。 */
export interface PortableInventoryPartition {
  installed: PortableInventoryItemDto[];
  borrowed: PortableInventoryItemDto[];
}

/** 仓库页：同一 storeId（或 nativeId）跨 Agent 合成一行。 */
export interface PortableStoreCatalogGroup {
  key: string;
  displayName: string;
  representative: PortableInventoryItemDto;
  byTarget: Partial<Record<AgentTarget, PortableInventoryItemDto>>;
}

/** 仓库行上某个 Agent 的启用芯片。 */
export interface PortableStoreAgentChipState {
  target: AgentTarget;
  enabled: boolean;
  derived: boolean;
  derivedFrom: AgentTarget | null;
  canToggle: boolean;
  item: PortableInventoryItemDto | null;
}

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
  assetLane: 'equipped',
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
  'store_loaded_via_other_path',
  'borrowed_runtime_origin',
  // 无根 SKILL.md 的包展开成子项：提示来源，不是损坏。
  'nested_skill_package',
  // Codex 缓存包未写入 `[plugins."id@market"]`：等同未启用，不是损坏。
  'codex_plugin_not_in_config',
]);

/** 判断 warning token 是否只是提示，不应把主列表标成异常。 */
function isPortabilityAdvisory(code: string): boolean {
  return PORTABILITY_ADVISORY_CODES.has(code) || code.startsWith('transport:');
}

/** 返回会影响本机健康态的 warning；可移植性提示不标红主列表。 */
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
 * Business Logic: 运行时从其他 Agent / 共享目录加载的项分到「借用」分区。
 *   Plugin 启停跟当前 Agent 自己的开关；Skill/Command/MCP 启停与卸载仍改所有者磁盘。
 *   漂移是 Hub 一致性状态，不是外借：ownedBy===target 的 native/legacy 即使
 *   nativeOutputCandidate=false 也留在「已安装在此」。sharedAgents 一律借用。
 *   portableStore + 本机软链（含 Codex legacy 根）算已安装；兼容路径仍被加载则借用。
 * Code Logic: compatibility、sharedAgents、或其他 Hub owner → borrowed。
 *   不以 nativeOutputCandidate / legacyStandalone / managementState 判借用。
 */
export function isPortableBorrowedRuntimeItem(item: PortableInventoryItemDto): boolean {
  if (item.ownedBy === 'portableStore') {
    return Boolean(item.store?.loadedViaOtherPath) || item.originKind === 'compatibility';
  }
  if (item.originKind === 'compatibility') return true;
  if (item.ownedBy === 'sharedAgents') return true;
  if (isHubTarget(item.ownedBy) && item.ownedBy !== item.target) return true;
  return false;
}

/**
 * Business Logic: 借用徽标要落到具体所有者文案（Claude / 共享 ~/.agents）。
 *   仓库真树本身不是「来自谁」；经其他 Agent 路径加载时用 loadedViaTarget。
 * Code Logic: loadedViaTarget 优先；否则 ownedBy；portableStore 仅作兜底。
 */
export function portableBorrowedOwnerLabelKey(
  item: PortableInventoryItemDto,
): PortableBorrowedOwnerLabelKey {
  const via = item.store?.loadedViaTarget;
  if (via && isHubTarget(via)) return via;
  if (item.ownedBy === 'sharedAgents' || item.ownedBy === 'unknown') {
    return item.ownedBy;
  }
  if (isHubTarget(item.ownedBy)) return item.ownedBy;
  if (item.ownedBy === 'portableStore') return 'portableStore';
  return 'unknown';
}

/**
 * Business Logic: 「在所有者 Agent 中打开」要跳到实际加载源，而不是仓库抽象所有者。
 * Code Logic: 徽标 key 若是 Hub target 则用之。
 */
export function portableBorrowedOwnerJumpTarget(
  item: PortableInventoryItemDto,
): AgentTarget | null {
  const key = portableBorrowedOwnerLabelKey(item);
  return isHubTarget(key) ? key : null;
}

/**
 * Business Logic: 主列表拆成「已安装在此」与「运行时从其他 Agent 加载」。
 * Code Logic: 保持输入顺序；plugin component 排除由 filter 负责，本函数不二次过滤。
 */
export function partitionPortableInventoryItems(
  items: readonly PortableInventoryItemDto[],
): PortableInventoryPartition {
  const installed: PortableInventoryItemDto[] = [];
  const borrowed: PortableInventoryItemDto[] = [];
  for (const item of items) {
    if (isPortableBorrowedRuntimeItem(item)) {
      borrowed.push(item);
    } else {
      installed.push(item);
    }
  }
  return { installed, borrowed };
}

/**
 * Business Logic: 仓库目录项（含已附加软链与未附加注入）。
 * Code Logic: 有 storeId 或 ownedBy=portableStore。
 */
export function isPortableStoreCatalogItem(item: PortableInventoryItemDto): boolean {
  return Boolean(item.store?.storeId) || item.ownedBy === 'portableStore';
}

/**
 * Business Logic: 只有 Skill/Command 进 portable-store 软链；MCP 是各家配置 leaf，Plugin 是 viewing 开关。
 * Code Logic: skill/command 为真。
 */
export function isPortableStoreAssetKind(kind: PortableAssetKind): boolean {
  return kind === 'skill' || kind === 'command';
}

/**
 * Business Logic: 逃逸软链不是「已禁用」，必须作为已装备问题行可见，才能一键恢复进仓库。
 * Code Logic: Skill/Command 上 canMaterializeEscapeLink 或 `store_symlink_escape` warning。
 */
export function isPortableEscapeLinkItem(item: PortableInventoryItemDto): boolean {
  if (!isPortableStoreAssetKind(item.kind)) return false;
  if (item.capabilities.canMaterializeEscapeLink) return true;
  return item.warnings.includes('store_symlink_escape');
}

/**
 * Business Logic: 只有本 Agent 自己的 native / Codex ~/.agents（legacyStandalone）能迁入仓库。
 *   Grok/Pi 等运行时从其他 Agent 加载的 compatibility 项不得出现「迁入仓库」。
 * Code Logic: 后端 canMigrateToStore 再加 originKind 闸，capability 泄漏也不会露出按钮。
 */
export function canOfferPortableMigrateToStore(item: PortableInventoryItemDto): boolean {
  if (!isPortableStoreAssetKind(item.kind)) return false;
  if (!item.capabilities.canMigrateToStore) return false;
  if (item.originKind === 'compatibility') return false;
  return true;
}

/**
 * Business Logic: 借用视图不得删共享仓库真树；本 Agent 已附加或仓库目录项仍可销毁。
 * Code Logic: compatibility / loadedViaOtherPath 隐藏 destroyStore。
 */
export function canOfferPortableDestroyStore(item: PortableInventoryItemDto): boolean {
  if (!isPortableStoreAssetKind(item.kind)) return false;
  if (!item.capabilities.canDestroyStore) return false;
  if (item.originKind === 'compatibility' || item.store?.loadedViaOtherPath) return false;
  return true;
}

/**
 * Business Logic: 运行时从其他 Agent 加载的仓库项已经在用源软链，不必再给当前 Agent 附加。
 * Code Logic: borrowed runtime 隐藏 attach，即使 capability 泄漏。
 */
export function canOfferPortableAttach(item: PortableInventoryItemDto): boolean {
  if (!isPortableStoreAssetKind(item.kind)) return false;
  if (!item.capabilities.canAttach) return false;
  if (isPortableBorrowedRuntimeItem(item)) return false;
  return true;
}

/**
 * Business Logic: 本 Agent 自己挂了软链才可「从此 Agent 卸下」。
 *   借用且无本机链（loadedViaOtherPath / compatibility）不得拆源 Agent 软链。
 * Code Logic: 先看 canDetach；borrowed runtime 且 storeAttached 不为 true 一律 false
 *   （capability 泄漏也不露按钮）。本 Agent storeAttached 时不隐藏。
 */
export function canOfferPortableDetach(item: PortableInventoryItemDto): boolean {
  if (!isPortableStoreAssetKind(item.kind)) return false;
  if (!item.capabilities.canDetach) return false;
  if (isPortableBorrowedRuntimeItem(item) && item.store?.storeAttached !== true) {
    return false;
  }
  return true;
}

/**
 * Business Logic: 仓库按 Skill/Command 去重，不按 Agent 拆行。
 * Code Logic: 优先 storeId；否则 kind+nativeId+scope。
 */
export function portableStoreCatalogGroupKey(item: PortableInventoryItemDto): string {
  if (item.store?.storeId) return item.store.storeId;
  return `${item.kind}:${item.nativeId}:${item.scopeKind}:${item.scopeId}`;
}

/**
 * Business Logic: 同一 Agent 可能同时有本机软链、兼容路径扫描和未附加注入；芯片要看真正生效的那条。
 * Code Logic: 本机已附加 > 运行时借用/经其他路径加载 > 未附加目录项。
 */
function rankPortableStoreGroupItem(item: PortableInventoryItemDto): number {
  if (item.store?.storeAttached === true && !isPortableBorrowedRuntimeItem(item)) return 3;
  if (item.store?.loadedViaOtherPath || isPortableBorrowedRuntimeItem(item)) return 2;
  if (item.store?.storeAttached === true) return 2;
  return 1;
}

function preferPortableStoreGroupItem(
  current: PortableInventoryItemDto | undefined,
  next: PortableInventoryItemDto,
): PortableInventoryItemDto {
  if (!current) return next;
  return rankPortableStoreGroupItem(next) > rankPortableStoreGroupItem(current) ? next : current;
}

/**
 * Business Logic: 仓库列出本机每一份 Skill/Command，一行代表所有 Agent 的启用态。
 * Code Logic: 按 group key 合并；byTarget 保留每个 Agent 最高优先级的库存项。
 */
export function groupPortableStoreCatalog(
  items: readonly PortableInventoryItemDto[],
): PortableStoreCatalogGroup[] {
  const groups = new Map<string, PortableStoreCatalogGroup>();
  for (const item of items) {
    const key = portableStoreCatalogGroupKey(item);
    const existing = groups.get(key);
    if (!existing) {
      groups.set(key, {
        key,
        displayName: item.displayName,
        representative: item,
        byTarget: { [item.target]: item },
      });
      continue;
    }
    existing.byTarget[item.target] = preferPortableStoreGroupItem(
      existing.byTarget[item.target],
      item,
    );
    existing.representative = preferPortableStoreGroupItem(existing.representative, item);
    existing.displayName = existing.representative.displayName;
  }
  return [...groups.values()];
}

/**
 * Business Logic: 芯片 = 该 Agent 是否会加载这份仓库资产；Grok 等借用路径随源 Agent 变化。
 * Code Logic: 已附加或 loadedViaOtherPath / actualEnabled 视为启用；借用且无本机软链则只读。
 */
export function portableStoreAgentChipState(
  group: PortableStoreCatalogGroup,
  target: AgentTarget,
): PortableStoreAgentChipState {
  const item = group.byTarget[target] ?? null;
  if (!item) {
    return {
      target,
      enabled: false,
      derived: false,
      derivedFrom: null,
      canToggle: false,
      item: null,
    };
  }
  const derived =
    isPortableBorrowedRuntimeItem(item) && item.store?.storeAttached !== true;
  const enabled =
    item.store?.storeAttached === true ||
    item.store?.loadedViaOtherPath === true ||
    item.actualEnabled === true;
  const canToggle = derived
    ? false
    : enabled
      ? canOfferPortableDetach(item)
      : canOfferPortableAttach(item);
  return {
    target,
    enabled,
    derived,
    derivedFrom: derived ? portableBorrowedOwnerJumpTarget(item) : null,
    canToggle,
    item,
  };
}

/**
 * Business Logic: 仓库行展示全部 Hub Agent，缺扫描项的芯片保持未启用且不可点。
 * Code Logic: allHubTargets 固定顺序。
 */
export function portableStoreAgentChipStates(
  group: PortableStoreCatalogGroup,
): PortableStoreAgentChipState[] {
  return allHubTargets().map((target) => portableStoreAgentChipState(group, target));
}

/**
 * Business Logic: 仓库筛选作用在整份 Skill/Command 上，而不是某个 Agent 的副本。
 * Code Logic: management / actualState 看 representative 与「是否有 Agent 已启用」。
 */
export function matchesPortableStoreCatalogGroup(
  group: PortableStoreCatalogGroup,
  filters: PortableInventoryFilters,
): boolean {
  if (
    filters.management !== 'all' &&
    group.representative.managementState !== filters.management
  ) {
    return false;
  }
  if (filters.actualState === 'all') return true;
  const chips = portableStoreAgentChipStates(group);
  if (filters.actualState === 'enabled') return chips.some((chip) => chip.enabled);
  if (filters.actualState === 'disabled') return chips.every((chip) => !chip.enabled);
  if (filters.actualState === 'problem') {
    return isPortableInventoryProblem(group.representative);
  }
  return true;
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
  const storeCatalogLane =
    isPortableStoreAssetKind(item.kind) && filters.assetLane === 'store';
  if (!storeCatalogLane && filters.target !== 'all' && item.target !== filters.target) {
    return false;
  }
  if (filters.scope !== 'all') {
    if (filters.scope === 'user' && item.scopeKind !== 'user') return false;
    if (filters.scope === 'project' && item.scopeKind !== 'project') return false;
  }
  if (!storeCatalogLane) {
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
  }
  if (!matchesSearch(item, filters.search)) return false;
  if (isPortableStoreAssetKind(item.kind)) {
    const catalog = isPortableStoreCatalogItem(item);
    if (storeCatalogLane) {
      if (!catalog) return false;
    } else if (
      catalog &&
      item.store?.storeAttached !== true &&
      !isPortableBorrowedRuntimeItem(item)
    ) {
      return false;
    } else if (item.actualEnabled === false && !isPortableEscapeLinkItem(item)) {
      return false;
    }
  }
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
  if (
    canOfferPortableMigrateToStore(item) ||
    canOfferPortableAttach(item) ||
    canOfferPortableDetach(item) ||
    canOfferPortableDestroyStore(item)
  ) {
    return false;
  }
  if (caps.canMaterializeEscapeLink) return false;
  return true;
}

/**
 * Business Logic: 行上只暴露一个主动作；stale/未 opt-in/unsupported 不得 mutation。
 *   Skill/Command 只走仓库（迁入/附加/卸下），不再启停；Plugin/MCP 仍走 enable/disable。
 *   漂移项优先「确认当前版本」；发现即管理后 **永不** 以 adopt 作为主动作（可保留 API kind 给遗留 apply）。
 * Code Logic: capability 驱动 materializeEscapeLink → confirmCurrentVersion → store → enable/disable → installToSourceTarget；跳过 canAdopt。
 */
export function resolvePortablePrimaryAction(
  item: PortableInventoryItemDto,
  context: PortablePrimaryActionContext,
): PortableAssetActionKind | null {
  if (context.stale || context.mutationBlocked) return null;
  if (context.lockedItemIds.has(item.inventoryItemId)) return null;
  if (isPortableItemReadOnly(item)) return null;

  const caps = item.capabilities;
  if (caps.canMaterializeEscapeLink) return 'materializeEscapeLink';

  if (item.managementState === 'unsupported') return null;
  // 历史 unmanaged 且尚无启停能力：不伪装 Adopt；父层展示刷新文案。
  if (needsPortableEnsureManagedRefresh(item)) return null;

  if (caps.canConfirmCurrentVersion) return 'confirmCurrentVersion';
  // 故意不返回 'adopt'：discover-as-managed 已取代纳入主路径。
  if (isPortableStoreAssetKind(item.kind)) {
    if (canOfferPortableDetach(item)) return 'detach';
    if (canOfferPortableAttach(item)) return 'attach';
    if (canOfferPortableMigrateToStore(item)) return 'migrateToStore';
    return null;
  }
  const borrowedMcp = item.kind === 'mcp' && isPortableBorrowedRuntimeItem(item);
  if (!borrowedMcp && item.actualEnabled === true && caps.canDisable) return 'disable';
  if (!borrowedMcp && item.actualEnabled === false && caps.canEnable) return 'enable';
  if (item.actualEnabled === null || item.actualEnabled === undefined) {
    if (caps.canInstallToSourceTarget) return 'installToSourceTarget';
  }
  if (caps.canInstallToSourceTarget && !caps.canEnable && !caps.canDisable) {
    return 'installToSourceTarget';
  }
  return null;
}

/**
 * Business Logic: 列表行同时暴露仓库或启停动作，无需详情侧栏。
 *   Skill/Command：迁入仓库 / 附加 / 从此 Agent 卸下；彻底删除仓库项只在仓库页。
 *   运行时从其他 Agent 加载的 compatibility Skill/Command 不出现迁入/附加/销毁，
 *   也不得卸下源软链（仅本 Agent storeAttached 时可 detach）。
 *   Plugin：仍走 enable/disable/uninstall；借用 Plugin 按 capability 暴露 viewing 开关。
 *   MCP：自身走 enable/disable/uninstall；借用 MCP 不得暴露所有者启停/卸载。
 *   发现即管理后 **永不** 返回 adopt 作为行内动作；与 resolvePortablePrimaryAction 同样的安全门闩。
 *
 * Code Logic: 复用 stale/mutationBlocked/locked/readOnly/unsupported/unmanaged 判定，
 *   Skill/Command 只累加 store 动作；其余 kind 按 enable/disable → install → uninstall。
 *   borrowed MCP 跳过 enable/disable/uninstall（即使 capability 泄漏）。
 *   返回的数组可能为空（无任何 mutation 资格）或含多项；调用方按顺序渲染按钮。
 */
export function resolvePortableRowActions(
  item: PortableInventoryItemDto,
  context: PortablePrimaryActionContext,
): PortableAssetActionKind[] {
  if (context.stale || context.mutationBlocked) return [];
  if (context.lockedItemIds.has(item.inventoryItemId)) return [];
  if (isPortableItemReadOnly(item)) return [];

  const caps = item.capabilities;
  if (caps.canMaterializeEscapeLink) return ['materializeEscapeLink'];

  if (item.managementState === 'unsupported') return [];
  if (needsPortableEnsureManagedRefresh(item)) return [];

  const actions: PortableAssetActionKind[] = [];
  if (caps.canConfirmCurrentVersion) {
    actions.push('confirmCurrentVersion');
  }
  const storeKind = isPortableStoreAssetKind(item.kind);
  if (storeKind) {
    if (canOfferPortableMigrateToStore(item)) actions.push('migrateToStore');
    if (canOfferPortableAttach(item)) actions.push('attach');
    if (canOfferPortableDetach(item)) actions.push('detach');
    if (context.assetLane !== 'equipped' && canOfferPortableDestroyStore(item)) {
      actions.push('destroyStore');
    }
    return actions;
  }
  const borrowedMcp = item.kind === 'mcp' && isPortableBorrowedRuntimeItem(item);
  if (!borrowedMcp && item.actualEnabled !== true && caps.canEnable) {
    actions.push('enable');
  }
  if (!borrowedMcp && item.actualEnabled !== false && caps.canDisable) {
    actions.push('disable');
  }
  if (
    (item.actualEnabled === null || item.actualEnabled === undefined) &&
    caps.canInstallToSourceTarget
  ) {
    actions.push('installToSourceTarget');
  }
  if (!borrowedMcp && caps.canUninstall) {
    actions.push('uninstall');
  }
  return actions;
}

/**
 * Business Logic: 「全部确认版本」覆盖当前 Agent、当前类别快照里所有可确认项，
 *   不受搜索/一致性筛选裁切；Plugin component 不进主列表，也不进批量。
 * Code Logic: 复用行动作同一组 stale/lock/readOnly/unsupported 门闩。
 */
export function listConfirmableCurrentVersionItems(
  items: readonly PortableInventoryItemDto[],
  context: PortablePrimaryActionContext,
): PortableInventoryItemDto[] {
  return items.filter((item) => {
    if (isPortablePluginComponent(item)) return false;
    return resolvePortableRowActions(item, context).includes('confirmCurrentVersion');
  });
}

/**
 * Business Logic: 「全部迁入仓库」覆盖当前 Agent、当前类别快照里所有可迁入项，
 *   不受搜索/一致性筛选裁切；仅 Skill/Command；Plugin component 不进批量。
 * Code Logic: 复用行动作同一组 stale/lock/readOnly/unsupported 门闩与 canMigrateToStore。
 */
export function listMigratableToStoreItems(
  items: readonly PortableInventoryItemDto[],
  context: PortablePrimaryActionContext,
): PortableInventoryItemDto[] {
  return items.filter((item) => {
    if (!isPortableStoreAssetKind(item.kind)) return false;
    if (isPortablePluginComponent(item)) return false;
    return resolvePortableRowActions(item, context).includes('migrateToStore');
  });
}

/**
 * Business Logic: 「全部恢复为仓库资产」覆盖当前 Agent、当前类别快照里所有逃逸项，
 *   不受搜索/一致性筛选裁切；仅 Skill/Command；Plugin component 不进批量。
 * Code Logic: 复用行动作同一组 stale/lock/readOnly 门闩与 canMaterializeEscapeLink。
 */
export function listMaterializableEscapeLinkItems(
  items: readonly PortableInventoryItemDto[],
  context: PortablePrimaryActionContext,
): PortableInventoryItemDto[] {
  return items.filter((item) => {
    if (!isPortableStoreAssetKind(item.kind)) return false;
    if (isPortablePluginComponent(item)) return false;
    return resolvePortableRowActions(item, context).includes('materializeEscapeLink');
  });
}

/**
 * Business Logic: preview 请求的 id 集合必须与 pending 动作一致，顺序无关。
 * Code Logic: 拷贝排序后逐项比较。
 */
export function samePortableItemIds(
  left: readonly string[],
  right: readonly string[],
): boolean {
  if (left.length !== right.length) return false;
  const a = [...left].sort();
  const b = [...right].sort();
  return a.every((id, index) => id === b[index]);
}
