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
import type { UsePortableInventoryControllerResult } from './usePortableInventoryController';
import {
  PortableInventoryRow,
  type PortableInventoryRowLabels,
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
  refresh: string;
  retry: string;
  staleBanner: string;
  searchPlaceholder: string;
  filterActual: string;
  filterManagement: string;
  actualFilter: Record<(typeof ACTUAL_OPTIONS)[number], string>;
  managementFilter: Record<(typeof MANAGEMENT_OPTIONS)[number], string>;
}

export interface PortableInventoryViewProps {
  controller: UsePortableInventoryControllerResult;
  labels: PortableInventoryViewLabels;
}

/**
 * 渲染 portable inventory workspace 主体（F2 范围，不含 details/pull）。
 */
export function PortableInventoryView(props: PortableInventoryViewProps): JSX.Element {
  const { controller, labels } = props;
  const {
    loading,
    refreshing,
    error,
    snapshot,
    visibleItems,
    filters,
    setFilters,
    stale,
    selectedItemId,
    selectItem,
    getPrimaryAction,
    openAction,
    lockedItemIds,
    refresh,
  } = controller;

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
        <div className={styles.userHeroActions}>
          <Button
            variant="secondary"
            size="sm"
            loading={refreshing}
            onClick={() => void refresh()}
            data-testid="portable-inventory-refresh"
          >
            {labels.refresh}
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
        {visibleItems.length === 0 ? (
          <p className={styles.empty} data-testid="portable-inventory-empty">
            {labels.empty}
          </p>
        ) : (
          visibleItems.map((item) => (
            <PortableInventoryRow
              key={item.inventoryItemId}
              item={item}
              selected={selectedItemId === item.inventoryItemId}
              busy={lockedItemIds.has(item.inventoryItemId)}
              primaryAction={getPrimaryAction(item)}
              labels={labels}
              onSelect={(selected) => selectItem(selected.inventoryItemId)}
              onPrimaryAction={(selected, action) =>
                openAction(selected.inventoryItemId, action)
              }
            />
          ))
        )}
      </div>
    </div>
  );
}
