/**
 * Portable inventory pure view（列表 + 筛选，无详情 Drawer）。
 *
 * Business Logic（为什么需要这个组件）:
 *   Agent 与 kind 由 AgentHubShell 顶层选择；本视图只做状态/管理态/搜索 + 列表。
 *
 * Code Logic（这个组件做什么）:
 *   只消费 controller result 与 labels；禁止 @/api；hooks 不在本视图。
 *   不再渲染 skill/command/plugin/mcp 子导航（避免与壳层 tab 重复）。
 */

import type { JSX } from 'react';
import { Button, Input, StatusMessage } from '@/components/primitives';
import type {
  PortableAssetActionKind,
  PortableInventoryItemDto,
} from '@/lib/types/portableInventory';
import {
  isPortableStoreAssetKind,
  groupPortableStoreCatalog,
  matchesPortableStoreCatalogGroup,
  portableStoreAgentChipStates,
  partitionPortableInventoryItems,
  type PortableInventoryFilters,
} from './portableInventoryPresentation';
import type { UsePortableInventoryControllerResult } from './usePortableInventoryController';
import {
  PortableInventoryRow,
  type PortableInventoryRowLabels,
  type PortableStoreAgentChip,
} from './PortableInventoryRow';
import styles from '../AgentHub.module.css';

const ACTUAL_OPTIONS = ['all', 'enabled', 'disabled', 'problem'] as const;
/** 管理态筛选：主心智 = 一致/漂移/冲突/不支持；unmanaged 置末作历史兜底。 */
const MANAGEMENT_OPTIONS = [
  'all',
  'hubManaged',
  'drifted',
  'externalCollision',
  'unsupported',
  'unmanaged',
] as const;

export interface PortableInventoryViewLabels extends PortableInventoryRowLabels {
  title: string;
  subtitle: string;
  loading: string;
  empty: string;
  migrateAllToStore: string;
  confirmAllVersions: string;
  materializeAllEscapeLinks: string;
  retry: string;
  staleBanner: string;
  searchPlaceholder: string;
  filterActual: string;
  filterManagement: string;
  actualFilter: Record<(typeof ACTUAL_OPTIONS)[number], string>;
  managementFilter: Record<(typeof MANAGEMENT_OPTIONS)[number], string>;
  groupInstalled: string;
  groupBorrowed: string;
  groupStoreAttached: string;
  groupStoreAvailable: string;
  emptyRuntimeHint: string;
}

export interface PortableInventoryViewProps {
  controller: UsePortableInventoryControllerResult;
  labels: PortableInventoryViewLabels;
  onOpenOwner?: (item: PortableInventoryItemDto) => void;
  /** 项目 Agent 冻结身份后隐藏行内范围。 */
  hideScope?: boolean;
}

/**
 * 渲染 portable inventory workspace 主体（F2 范围，不含 details/pull）。
 */
export function PortableInventoryView(props: PortableInventoryViewProps): JSX.Element {
  const { controller, labels, onOpenOwner, hideScope = false } = props;
  const {
    loading,
    refreshing,
    error,
    snapshot,
    visibleItems,
    filters,
    setFilters,
    stale,
    getRowActions,
    openAction,
    lockedItemIds,
    refresh,
    confirmableCurrentVersionItems,
    openConfirmAllCurrentVersions,
    migratableToStoreItems,
    openMigrateAllToStore,
    materializableEscapeLinkItems,
    openMaterializeAllEscapeLinks,
    pendingAction,
    mutationBlocked,
  } = controller;

  const showMigrateAllToStore =
    isPortableStoreAssetKind(filters.kind) && filters.assetLane !== 'store';
  const migrateAllCount = migratableToStoreItems.length;
  const migrateAllDisabled =
    migrateAllCount === 0 ||
    Boolean(pendingAction) ||
    refreshing ||
    stale ||
    mutationBlocked;
  const confirmAllCount = confirmableCurrentVersionItems.length;
  const confirmAllDisabled =
    confirmAllCount === 0 ||
    Boolean(pendingAction) ||
    refreshing ||
    stale ||
    mutationBlocked;
  const showMaterializeAllEscapeLinks =
    isPortableStoreAssetKind(filters.kind) && filters.assetLane !== 'store';
  const materializeAllCount = materializableEscapeLinkItems.length;
  const materializeAllDisabled =
    materializeAllCount === 0 ||
    Boolean(pendingAction) ||
    refreshing ||
    stale ||
    mutationBlocked;

  if (loading && !snapshot) {
    return (
      <StatusMessage tone="info" data-testid="portable-inventory-loading">
        {labels.loading}
      </StatusMessage>
    );
  }

  if (error && !snapshot) {
    return (
      <StatusMessage
        tone="danger"
        data-testid="portable-inventory-error"
        action={
          <Button size="sm" onClick={() => void refresh()}>
            {labels.retry}
          </Button>
        }
      >
        {error}
      </StatusMessage>
    );
  }

  return (
    <div className={styles.userWorkspace} data-testid="portable-inventory-workspace">
      <section className={styles.userHero} aria-labelledby="portable-inventory-title">
        <div className={styles.userHeroCopy}>
          <h2 id="portable-inventory-title" className={styles.title}>
            {labels.title}
          </h2>
          <p className={styles.subtitle}>{labels.subtitle}</p>
          {snapshot ? (
            <p className={styles.userRefreshTime}>
              {snapshot.refreshedAt}
            </p>
          ) : null}
        </div>
        <div className={styles.portableHeroActions}>
          {showMigrateAllToStore ? (
            <Button
              variant="secondary"
              size="sm"
              disabled={migrateAllDisabled}
              onClick={() => openMigrateAllToStore()}
              data-testid="portable-inventory-migrate-all-to-store"
            >
              {labels.migrateAllToStore}
            </Button>
          ) : null}
          {showMaterializeAllEscapeLinks ? (
            <Button
              variant="secondary"
              size="sm"
              disabled={materializeAllDisabled}
              onClick={() => openMaterializeAllEscapeLinks()}
              data-testid="portable-inventory-materialize-all-escape-links"
            >
              {labels.materializeAllEscapeLinks}
            </Button>
          ) : null}
          <Button
            variant="secondary"
            size="sm"
            disabled={confirmAllDisabled}
            onClick={() => openConfirmAllCurrentVersions()}
            data-testid="portable-inventory-confirm-all-versions"
          >
            {labels.confirmAllVersions}
          </Button>
        </div>
      </section>

      {stale ? (
        <StatusMessage tone="warn" data-testid="portable-inventory-stale">
          {labels.staleBanner}
        </StatusMessage>
      ) : null}
      {error && snapshot ? (
        <StatusMessage
          tone="danger"
          action={
            <Button size="sm" onClick={() => void refresh()}>
              {labels.retry}
            </Button>
          }
        >
          {error}
        </StatusMessage>
      ) : null}

      <div className={styles.filters} data-testid="portable-inventory-filters">
        <div className={styles.filterField}>
          <Input
            value={filters.search}
            onChange={(event) => setFilters({ search: event.currentTarget.value })}
            placeholder={labels.searchPlaceholder}
            aria-label={labels.searchPlaceholder}
            data-testid="portable-inventory-search"
          />
        </div>
        <label className={styles.filterField}>
          <span className={styles.metaLabel}>{labels.filterActual}</span>
          <select
            className={styles.filterSelect}
            value={filters.actualState}
            onChange={(event) =>
              setFilters({
                actualState: event.currentTarget.value as typeof filters.actualState,
              })
            }
            data-testid="portable-filter-actual"
          >
            {ACTUAL_OPTIONS.map((option) => (
              <option key={option} value={option}>
                {labels.actualFilter[option]}
              </option>
            ))}
          </select>
        </label>
        <label className={styles.filterField}>
          <span className={styles.metaLabel}>{labels.filterManagement}</span>
          <select
            className={styles.filterSelect}
            value={filters.management}
            onChange={(event) =>
              setFilters({
                management: event.currentTarget.value as typeof filters.management,
              })
            }
            data-testid="portable-filter-management"
          >
            {MANAGEMENT_OPTIONS.map((option) => (
              <option key={option} value={option}>
                {labels.managementFilter[option]}
              </option>
            ))}
          </select>
        </label>
      </div>

      <div className={styles.list} data-testid="portable-inventory-list">
        {renderInventoryGroups({
          visibleItems,
          lockedItemIds,
          getRowActions,
          labels,
          onOpenOwner,
          openAction,
          storeLane: filters.assetLane === 'store',
          filters,
          hideScope,
        })}
      </div>
    </div>
  );
}

interface InventoryGroupRenderProps {
  visibleItems: PortableInventoryItemDto[];
  lockedItemIds: ReadonlySet<string>;
  getRowActions: (item: PortableInventoryItemDto) => PortableAssetActionKind[];
  labels: PortableInventoryViewLabels;
  onOpenOwner?: (item: PortableInventoryItemDto) => void;
  openAction: (itemId: string, action: PortableAssetActionKind) => void;
  storeLane: boolean;
  filters: PortableInventoryFilters;
  hideScope: boolean;
}

/**
 * Business Logic: 已装备拆「已安装在此」与「运行时借用」；仓库按 Skill/Command 去重并展示各 Agent 启用芯片。
 *   已装备不渲染彻底删除仓库副本；该动作只留在仓库页。
 * Code Logic: 两边都空才 empty；已装备仅借用时附 emptyRuntimeHint；equipped 行过滤 destroyStore。
 */
function renderInventoryGroups(props: InventoryGroupRenderProps): JSX.Element {
  const {
    visibleItems,
    lockedItemIds,
    getRowActions,
    labels,
    onOpenOwner,
    openAction,
    storeLane,
    filters,
    hideScope,
  } = props;

  function renderRow(
    item: PortableInventoryItemDto,
    extra?: Pick<Parameters<typeof PortableInventoryRow>[0], 'onOpenOwner'>,
  ): JSX.Element {
    return (
      <PortableInventoryRow
        key={item.inventoryItemId}
        item={item}
        busy={lockedItemIds.has(item.inventoryItemId)}
        actions={getRowActions(item).filter((action) => action !== 'destroyStore')}
        labels={labels}
        onAction={(selected, action) => openAction(selected.inventoryItemId, action)}
        onOpenOwner={extra?.onOpenOwner}
        hideScope={hideScope}
      />
    );
  }

  if (storeLane) {
    const groups = groupPortableStoreCatalog(visibleItems).filter((group) =>
      matchesPortableStoreCatalogGroup(group, filters),
    );
    if (groups.length === 0) {
      return (
        <p className={styles.empty} data-testid="portable-inventory-empty">
          {labels.empty}
        </p>
      );
    }
    return (
      <section
        className={styles.inventoryGroup}
        data-testid="portable-inventory-group-store-catalog"
        aria-label={labels.title}
      >
        {groups.map((group) => {
          const item = group.representative;
          const chips: PortableStoreAgentChip[] = portableStoreAgentChipStates(group).map(
            (chip) => ({
              target: chip.target,
              enabled: chip.enabled,
              derived: chip.derived,
              derivedFrom: chip.derivedFrom,
              canToggle: chip.canToggle,
              busy: chip.item ? lockedItemIds.has(chip.item.inventoryItemId) : false,
            }),
          );
          const actions = getRowActions(item).filter(
            (action) => action !== 'attach' && action !== 'detach',
          );
          return (
            <PortableInventoryRow
              key={group.key}
              item={item}
              busy={lockedItemIds.has(item.inventoryItemId) || chips.some((chip) => chip.busy)}
              actions={actions}
              labels={labels}
              catalogAgentChips={chips}
              onAction={(selected, action) => openAction(selected.inventoryItemId, action)}
              hideScope={hideScope}
              onToggleCatalogAgent={(target) => {
                const chip = portableStoreAgentChipStates(group).find(
                  (entry) => entry.target === target,
                );
                if (!chip?.canToggle || !chip.item) return;
                openAction(chip.item.inventoryItemId, chip.enabled ? 'detach' : 'attach');
              }}
            />
          );
        })}
      </section>
    );
  }

  const { installed, borrowed } = partitionPortableInventoryItems(visibleItems);
  if (installed.length === 0 && borrowed.length === 0) {
    return (
      <p className={styles.empty} data-testid="portable-inventory-empty">
        {labels.empty}
      </p>
    );
  }

  return (
    <>
      {installed.length > 0 ? (
        <section
          className={styles.inventoryGroup}
          data-testid="portable-inventory-group-installed"
          aria-labelledby="portable-inventory-group-installed-title"
        >
          <h3 id="portable-inventory-group-installed-title" className={styles.inventoryGroupTitle}>
            {labels.groupInstalled}
          </h3>
          {installed.map((item) => renderRow(item))}
        </section>
      ) : null}
      {borrowed.length > 0 ? (
        <section
          className={styles.inventoryGroup}
          data-testid="portable-inventory-group-borrowed"
          aria-labelledby="portable-inventory-group-borrowed-title"
        >
          <h3 id="portable-inventory-group-borrowed-title" className={styles.inventoryGroupTitle}>
            {labels.groupBorrowed}
          </h3>
          {installed.length === 0 ? (
            <p className={styles.emptyInline} data-testid="portable-inventory-runtime-hint">
              {labels.emptyRuntimeHint}
            </p>
          ) : null}
          {borrowed.map((item) => renderRow(item, { onOpenOwner }))}
        </section>
      ) : null}
    </>
  );
}
