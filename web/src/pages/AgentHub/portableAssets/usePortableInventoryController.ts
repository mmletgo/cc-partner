/**
 * Portable inventory 页面控制器。
 *
 * Business Logic（为什么需要这个 hook）:
 *   Agent 资产列表必须以 observed inventory 为真源；refresh 失败保留 stale 并禁止 mutation；
 *   与 canonical AgentHub matrix 状态分离，避免塞入巨型 controller。
 *
 * Code Logic（这个 hook 做什么）:
 *   inspect + refresh generation + mounted ref；纯 filter 计算 visibleItems；
 *   selection / pending action / per-item lock；hooks 全在 early return 前。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { portableAssetApi } from '@/api/portableInventory';
import type {
  PortableAssetActionKind,
  PortableInventoryItemDto,
  PortableInventorySnapshotDto,
} from '@/lib/types/portableInventory';
import {
  DEFAULT_PORTABLE_INVENTORY_FILTERS,
  countPortableItemsByKind,
  filterPortableInventoryItems,
  resolvePortablePrimaryAction,
  type PortableInventoryFilters,
  type PortableKindCounts,
} from './portableInventoryPresentation';

/** Controller 打开动作的待确认状态（F3 Dialog 消费；F2 仅持有）。 */
export interface PortableInventoryPendingAction {
  itemId: string;
  action: PortableAssetActionKind;
}

/** usePortableInventoryController 对 pure view 的返回合同。 */
export interface UsePortableInventoryControllerResult {
  snapshot: PortableInventorySnapshotDto | null;
  visibleItems: PortableInventoryItemDto[];
  kindCounts: PortableKindCounts;
  filters: PortableInventoryFilters;
  setFilters: (patch: Partial<PortableInventoryFilters>) => void;
  loading: boolean;
  refreshing: boolean;
  stale: boolean;
  mutationBlocked: boolean;
  error: string | null;
  selectedItemId: string | null;
  selectItem: (id: string | null) => void;
  lockedItemIds: ReadonlySet<string>;
  setItemLocked: (itemId: string, locked: boolean) => void;
  pendingAction: PortableInventoryPendingAction | null;
  openAction: (itemId: string, action: PortableAssetActionKind) => void;
  clearPendingAction: () => void;
  getPrimaryAction: (item: PortableInventoryItemDto) => PortableAssetActionKind | null;
  refresh: () => Promise<void>;
}

/**
 * Business Logic: 列表/筛选/选择的单一状态源，F5 再挂到 AgentHub composer。
 * Code Logic: 首屏与 refresh 共用 generation；失败保 snapshot + stale。
 */
export function usePortableInventoryController(): UsePortableInventoryControllerResult {
  const [snapshot, setSnapshot] = useState<PortableInventorySnapshotDto | null>(null);
  const [filters, setFiltersState] = useState<PortableInventoryFilters>(
    DEFAULT_PORTABLE_INVENTORY_FILTERS,
  );
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [staleFlag, setStaleFlag] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [selectedItemId, setSelectedItemId] = useState<string | null>(null);
  const [lockedItemIds, setLockedItemIds] = useState<Set<string>>(() => new Set());
  const [pendingAction, setPendingAction] = useState<PortableInventoryPendingAction | null>(
    null,
  );

  const mountedRef = useRef(true);
  const refreshSeqRef = useRef(0);
  const hasSnapshotRef = useRef(false);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  const refresh = useCallback(async () => {
    const seq = ++refreshSeqRef.current;
    if (hasSnapshotRef.current) {
      setRefreshing(true);
    } else {
      setLoading(true);
    }
    try {
      const next = await portableAssetApi.inspect();
      if (!mountedRef.current || seq !== refreshSeqRef.current) return;
      hasSnapshotRef.current = true;
      setSnapshot(next);
      setStaleFlag(Boolean(next.stale));
      setError(null);
    } catch (reason) {
      if (!mountedRef.current || seq !== refreshSeqRef.current) return;
      const message =
        reason instanceof Error && reason.message
          ? reason.message
          : 'portable inventory refresh failed';
      setError(message);
      // 保留旧 snapshot；标 stale 并禁止 mutation
      setStaleFlag(true);
    } finally {
      if (mountedRef.current && seq === refreshSeqRef.current) {
        setLoading(false);
        setRefreshing(false);
      }
    }
  }, []);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- initial inventory load
    void refresh();
  }, [refresh]);

  const setFilters = useCallback((patch: Partial<PortableInventoryFilters>) => {
    setFiltersState((prev) => ({ ...prev, ...patch }));
  }, []);

  const selectItem = useCallback((id: string | null) => {
    setSelectedItemId(id);
  }, []);

  const setItemLocked = useCallback((itemId: string, locked: boolean) => {
    setLockedItemIds((prev) => {
      const next = new Set(prev);
      if (locked) next.add(itemId);
      else next.delete(itemId);
      return next;
    });
  }, []);

  const stale = Boolean(snapshot?.stale) || staleFlag;
  const mutationBlocked = stale || !snapshot;

  const visibleItems = useMemo(
    () => filterPortableInventoryItems(snapshot?.items ?? [], filters),
    [snapshot, filters],
  );

  const kindCounts = useMemo(
    () => countPortableItemsByKind(snapshot?.items ?? []),
    [snapshot],
  );

  const getPrimaryAction = useCallback(
    (item: PortableInventoryItemDto) =>
      resolvePortablePrimaryAction(item, {
        stale,
        mutationBlocked,
        lockedItemIds,
      }),
    [stale, mutationBlocked, lockedItemIds],
  );

  const openAction = useCallback(
    (itemId: string, action: PortableAssetActionKind) => {
      if (mutationBlocked || stale || lockedItemIds.has(itemId)) {
        setPendingAction(null);
        return;
      }
      const item = snapshot?.items.find((entry) => entry.inventoryItemId === itemId);
      if (!item || isReadOnlyBlocked(item)) {
        setPendingAction(null);
        return;
      }
      const caps = item.capabilities;
      // discover-as-managed：不再开放 adopt 入口；其余 mutation 仍 capability 门闩。
      const allowed =
        (action === 'enable' && caps.canEnable) ||
        (action === 'disable' && caps.canDisable) ||
        (action === 'uninstall' && caps.canUninstall) ||
        (action === 'installToSourceTarget' && caps.canInstallToSourceTarget);
      if (!allowed) {
        setPendingAction(null);
        return;
      }
      setSelectedItemId(itemId);
      setPendingAction({ itemId, action });
    },
    [mutationBlocked, stale, snapshot, lockedItemIds],
  );

  const clearPendingAction = useCallback(() => {
    setPendingAction(null);
  }, []);

  return {
    snapshot,
    visibleItems,
    kindCounts,
    filters,
    setFilters,
    loading,
    refreshing,
    stale,
    mutationBlocked,
    error,
    selectedItemId,
    selectItem,
    lockedItemIds,
    setItemLocked,
    pendingAction,
    openAction,
    clearPendingAction,
    getPrimaryAction,
    refresh,
  };
}

/**
 * Business Logic: 未 opt-in 与 unsupported 禁止任何 mutation open。
 * Code Logic: 复用 presentation 只读判定 + managementState。
 */
function isReadOnlyBlocked(item: PortableInventoryItemDto): boolean {
  if (item.managementState === 'unsupported') return true;
  if (item.scopeKind === 'project' && !item.projectOptedIn) return true;
  return false;
}
