/**
 * Portable inventory 页面控制器。
 *
 * Business Logic（为什么需要这个 hook）:
 *   Agent 资产列表必须以 observed inventory 为真源；refresh 失败保留 stale 并禁止 mutation；
 *   与 canonical AgentHub matrix 状态分离，避免塞入巨型 controller。
 *   按需加载：仅 enabled 时 inspect；enabled=false 时 retain 同 contextKey 的 snapshot。
 *
 * Code Logic（这个 hook 做什么）:
 *   inspect + refresh generation + mounted ref；纯 filter 计算 visibleItems；
 *   selection / pending action / per-item lock；hooks 全在 early return 前。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { portableAssetApi, type AgentHubRequestContext } from '@/api/portableInventory';

/** peer 上下文稳定错误码（与 api 层常量同字面量；不 import 避免 mock 缺导出）。 */
const PEER_CONTEXT_UNAVAILABLE = 'AGENT_HUB_PEER_CONTEXT_UNAVAILABLE';
const PROJECT_CONTEXT_UNAVAILABLE = 'AGENT_HUB_PROJECT_CONTEXT_UNAVAILABLE';
import type {
  PortableAssetActionKind,
  PortableInventoryItemDto,
  PortableInventoryQuery,
  PortableInventoryRequestContext,
  PortableInventorySnapshotDto,
} from '@/lib/types/portableInventory';
import {
  DEFAULT_PORTABLE_INVENTORY_FILTERS,
  countPortableItemsByKind,
  filterPortableInventoryItems,
  isPortableStoreAssetKind,
  listConfirmableCurrentVersionItems,
  resolvePortablePrimaryAction,
  resolvePortableRowActions,
  type PortableInventoryFilters,
  type PortableKindCounts,
} from './portableInventoryPresentation';

/** 同 contextKey 下 skill↔instructions 回切免 re-inspect 的 soft TTL（ms）。 */
export const PORTABLE_INVENTORY_SOFT_TTL_MS = 60_000;

/** Controller 打开动作的待确认状态（F3 Dialog 消费；F2 仅持有）。 */
export interface PortableInventoryPendingAction {
  /** 参与本次 preview/apply 的 inventory item id；单项动作为单元素。 */
  itemIds: string[];
  action: PortableAssetActionKind;
}

/**
 * usePortableInventoryController 入参。
 *
 * Business Logic: device/project 来自壳层；enabled 由父层 portableLaneActive 驱动。
 * Code Logic: enabled 默认 false，避免 Hub mount 就全量扫盘。
 */
export type UsePortableInventoryControllerArgs = AgentHubRequestContext & {
  /** 为 true 时才 inspect；false 时 retain 同 key snapshot。 */
  enabled?: boolean;
  /** 首次 render 的 URL/壳层筛选，避免 mount 后再改筛选触发两次扫描。 */
  initialFilters?: Partial<PortableInventoryFilters>;
};

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
  /** 当前快照里可「确认当前版本」的项（当前 Agent + 当前类别）。 */
  confirmableCurrentVersionItems: PortableInventoryItemDto[];
  /** 一键确认当前类别、当前 Agent 的全部漂移项。 */
  openConfirmAllCurrentVersions: () => void;
  clearPendingAction: () => void;
  getPrimaryAction: (item: PortableInventoryItemDto) => PortableAssetActionKind | null;
  /** 行内多动作（与 getPrimaryAction 共享同一组门闩，含 uninstall）。 */
  getRowActions: (item: PortableInventoryItemDto) => PortableAssetActionKind[];
  refresh: () => Promise<void>;
  /** 当前 inspect 使用的上下文（便于页面层 mutation 透传）。 */
  requestContext: AgentHubRequestContext;
  /** 当前快照的后端扫描过滤条件；preview/apply 重校验必须复用。 */
  inventoryQuery: PortableInventoryQuery;
}

/**
 * Business Logic: 列表/筛选/选择的单一状态源；仅 enabled 时拉库存；上下文切换防 peer 污染。
 * Code Logic: 首屏与 refresh 共用 generation；失败保 snapshot + stale（同上下文）；
 *   deviceId/projectRef 变化时清空 snapshot；enabled=false 不 inspect 且 loading=false。
 */
export function usePortableInventoryController(
  context: UsePortableInventoryControllerArgs = {},
): UsePortableInventoryControllerResult {
  const deviceId = context.deviceId ?? null;
  const projectRef = context.projectRef ?? null;
  const enabled = context.enabled === true;
  const requestContext = useMemo(
    (): AgentHubRequestContext => ({ deviceId, projectRef }),
    [deviceId, projectRef],
  );

  const [snapshot, setSnapshot] = useState<PortableInventorySnapshotDto | null>(null);
  const [snapshotRequestKey, setSnapshotRequestKey] = useState<string | null>(null);
  const [filters, setFiltersState] = useState<PortableInventoryFilters>(() => ({
    ...DEFAULT_PORTABLE_INVENTORY_FILTERS,
    ...context.initialFilters,
  }));
  // enabled 默认 false：初值 loading 必须 false，禁止停在 true
  const [loading, setLoading] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [staleFlag, setStaleFlag] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [selectedItemId, setSelectedItemId] = useState<string | null>(null);
  const [lockedItemIds, setLockedItemIds] = useState<Set<string>>(() => new Set());
  const [pendingAction, setPendingAction] = useState<PortableInventoryPendingAction | null>(
    null,
  );

  const inventoryQuery = useMemo(
    (): PortableInventoryQuery => ({
      ...(filters.target === 'all' ? {} : { target: filters.target }),
      kind: filters.kind,
      ...(filters.scope === 'all' ? {} : { scopeKind: filters.scope }),
      ...(projectRef && !projectRef.startsWith('remote:')
        ? { localProjectId: projectRef }
        : {}),
    }),
    [filters.kind, filters.scope, filters.target, projectRef],
  );
  const inspectRequest = useMemo(
    (): PortableInventoryRequestContext => ({ ...requestContext, ...inventoryQuery }),
    [inventoryQuery, requestContext],
  );
  const requestKey = useMemo(
    () =>
      [
        deviceId ?? '',
        projectRef ?? '',
        inventoryQuery.target ?? '',
        inventoryQuery.kind ?? '',
        inventoryQuery.scopeKind ?? '',
        inventoryQuery.localProjectId ?? '',
      ].join('\0'),
    [deviceId, inventoryQuery, projectRef],
  );

  const mountedRef = useRef(true);
  const refreshSeqRef = useRef(0);
  const hasSnapshotRef = useRef(false);
  const requestKeyRef = useRef(requestKey);
  const snapshotKeyRef = useRef<string | null>(null);
  const snapshotFetchedAtRef = useRef(0);
  const snapshotCacheRef = useRef(
    new Map<string, { snapshot: PortableInventorySnapshotDto; fetchedAt: number }>(),
  );

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  /**
   * Business Logic: 手动/自动刷新 observed inventory。
   * Code Logic: generation 丢弃过期响应；有 snapshot 时用 refreshing 而非整区 loading。
   */
  const refresh = useCallback(async () => {
    const seq = ++refreshSeqRef.current;
    if (hasSnapshotRef.current) {
      setRefreshing(true);
    } else {
      setLoading(true);
    }
    try {
      const next = await portableAssetApi.inspect(inspectRequest);
      if (!mountedRef.current || seq !== refreshSeqRef.current) return;
      hasSnapshotRef.current = true;
      snapshotFetchedAtRef.current = Date.now();
      snapshotKeyRef.current = requestKey;
      setSnapshotRequestKey(requestKey);
      snapshotCacheRef.current.set(requestKey, {
        snapshot: next,
        fetchedAt: snapshotFetchedAtRef.current,
      });
      setSnapshot(next);
      setStaleFlag(Boolean(next.stale));
      setError(null);
    } catch (reason) {
      if (!mountedRef.current || seq !== refreshSeqRef.current) return;
      const code =
        reason && typeof reason === 'object' && 'code' in reason
          ? String((reason as { code?: unknown }).code ?? '')
          : '';
        const message =
          code === PEER_CONTEXT_UNAVAILABLE
            ? PEER_CONTEXT_UNAVAILABLE
            : code === PROJECT_CONTEXT_UNAVAILABLE
              ? PROJECT_CONTEXT_UNAVAILABLE
            : reason instanceof Error && reason.message
            ? reason.message
            : 'portable inventory refresh failed';
      setError(message);
      // 同上下文失败：保留旧 snapshot 并标 stale；跨上下文由 effect 已清空
      setStaleFlag(true);
    } finally {
      if (mountedRef.current && seq === refreshSeqRef.current) {
        setLoading(false);
        setRefreshing(false);
      }
    }
  }, [inspectRequest, requestKey]);

  useEffect(() => {
    const requestChanged = requestKeyRef.current !== requestKey;
    requestKeyRef.current = requestKey;
    if (requestChanged) {
      // A disabled lane may not start a replacement request; still advance the
      // generation so an in-flight response from the previous context cannot land.
      refreshSeqRef.current += 1;
      const cached = snapshotCacheRef.current.get(requestKey);
      hasSnapshotRef.current = Boolean(cached);
      snapshotFetchedAtRef.current = cached?.fetchedAt ?? 0;
      snapshotKeyRef.current = cached ? requestKey : null;
      setSnapshotRequestKey(cached ? requestKey : null);
      setSnapshot(cached?.snapshot ?? null);
      setSelectedItemId(null);
      setPendingAction(null);
      setLockedItemIds(new Set());
      setStaleFlag(false);
      setError(null);
      setLoading(false);
      setRefreshing(false);
    }

    if (!enabled) {
      // R1: 不 inspect；retain 同 key snapshot；loading 保持 false
      setLoading(false);
      setRefreshing(false);
      return;
    }

    // R3/R4: enabled 且（无 snapshot 或 TTL 过期或 context 刚变）→ inspect
    const age = Date.now() - snapshotFetchedAtRef.current;
    const softFresh =
      hasSnapshotRef.current &&
      snapshotKeyRef.current === requestKey &&
      age >= 0 &&
      age < PORTABLE_INVENTORY_SOFT_TTL_MS;
    if (softFresh) {
      setLoading(false);
      return;
    }
     
    void refresh();
  }, [enabled, refresh, requestKey]);

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

  const snapshotMatchesQuery = snapshotRequestKey === requestKey;
  const stale = !snapshotMatchesQuery || Boolean(snapshot?.stale) || staleFlag;
  // projectRef 已包含在 inventoryQuery，并由后端解析为唯一 Hub project；
  // 未选择项目时父 controller 不启用扫描，不能回退成“全部项目”。
  const mutationBlocked = stale || !snapshot;

  const visibleItems = useMemo(
    () => filterPortableInventoryItems(snapshotMatchesQuery ? snapshot?.items ?? [] : [], filters),
    [snapshot, snapshotMatchesQuery, filters],
  );

  const kindCounts = useMemo(
    (): PortableKindCounts => {
      if (!snapshotMatchesQuery) return {};
      const counts = countPortableItemsByKind(snapshot?.items ?? []);
      return { [filters.kind]: counts[filters.kind] };
    },
    [filters.kind, snapshot, snapshotMatchesQuery],
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

  /**
   * Business Logic: 列表行内同时暴露启停/卸载，复用 getPrimaryAction 的安全门闩。
   * Code Logic: 调 resolvePortableRowActions 输出有序动作数组（enable/disable → install → uninstall）。
   */
  const getRowActions = useCallback(
    (item: PortableInventoryItemDto) =>
      resolvePortableRowActions(item, {
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
      const storeKind = isPortableStoreAssetKind(item.kind);
      const allowed =
        (action === 'enable' && caps.canEnable) ||
        (action === 'disable' && caps.canDisable) ||
        (action === 'uninstall' && caps.canUninstall) ||
        (action === 'installToSourceTarget' && caps.canInstallToSourceTarget) ||
        (action === 'attach' && storeKind && Boolean(caps.canAttach)) ||
        (action === 'detach' && storeKind && Boolean(caps.canDetach)) ||
        (action === 'destroyStore' && storeKind && Boolean(caps.canDestroyStore)) ||
        (action === 'migrateToStore' && storeKind && Boolean(caps.canMigrateToStore)) ||
        (action === 'confirmCurrentVersion' && Boolean(caps.canConfirmCurrentVersion));
      if (!allowed) {
        setPendingAction(null);
        return;
      }
      setSelectedItemId(itemId);
      setPendingAction({ itemIds: [itemId], action });
    },
    [mutationBlocked, stale, snapshot, lockedItemIds],
  );

  const confirmableCurrentVersionItems = useMemo(
    () =>
      listConfirmableCurrentVersionItems(snapshotMatchesQuery ? snapshot?.items ?? [] : [], {
        stale,
        mutationBlocked,
        lockedItemIds,
      }),
    [snapshot, snapshotMatchesQuery, stale, mutationBlocked, lockedItemIds],
  );

  /**
   * Business Logic: 一键确认当前 Agent、当前类别快照里全部可确认项，不跟搜索/一致性筛选。
   * Code Logic: 空集或 mutation 门闩时清 pending；否则打开 confirmCurrentVersion 批量 dialog。
   */
  const openConfirmAllCurrentVersions = useCallback(() => {
    const ids = confirmableCurrentVersionItems.map((item) => item.inventoryItemId);
    if (ids.length === 0 || mutationBlocked || stale) {
      setPendingAction(null);
      return;
    }
    setPendingAction({ itemIds: ids, action: 'confirmCurrentVersion' });
  }, [confirmableCurrentVersionItems, mutationBlocked, stale]);

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
    confirmableCurrentVersionItems,
    openConfirmAllCurrentVersions,
    clearPendingAction,
    getPrimaryAction,
    getRowActions,
    refresh,
    requestContext,
    inventoryQuery,
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
