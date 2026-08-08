/**
 * Portable inventory 页面控制器。
 *
 * Business Logic（为什么需要这个 hook）:
 *   Agent 资产列表必须以 observed inventory 为真源；refresh 失败保留 stale 并禁止 mutation；
 *   与 canonical AgentHub matrix 状态分离，避免塞入巨型 controller。
 *   T7：deviceId / projectRef 进入 inspect；切换上下文带 sequence 防竞态，peer 不得静默本机。
 *
 * Code Logic（这个 hook 做什么）:
 *   inspect + refresh generation + mounted ref；纯 filter 计算 visibleItems；
 *   selection / pending action / per-item lock；hooks 全在 early return 前。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { portableAssetApi, type AgentHubRequestContext } from '@/api/portableInventory';

/** peer 上下文稳定错误码（与 api 层常量同字面量；不 import 避免 mock 缺导出）。 */
const PEER_CONTEXT_UNAVAILABLE = 'AGENT_HUB_PEER_CONTEXT_UNAVAILABLE';
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

/** usePortableInventoryController 入参（壳层 device/project 上下文）。 */
export type UsePortableInventoryControllerArgs = AgentHubRequestContext;

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
  /** 当前 inspect 使用的上下文（便于页面层 mutation 透传）。 */
  requestContext: AgentHubRequestContext;
}

/**
 * Business Logic: 列表/筛选/选择的单一状态源；上下文切换 re-inspect。
 * Code Logic: 首屏与 refresh 共用 generation；失败保 snapshot + stale（同上下文）；
 *   deviceId/projectRef 变化时清空 snapshot 再拉，避免本机数据冒充 peer。
 */
export function usePortableInventoryController(
  context: UsePortableInventoryControllerArgs = {},
): UsePortableInventoryControllerResult {
  const deviceId = context.deviceId ?? null;
  const projectRef = context.projectRef ?? null;
  const requestContext = useMemo(
    (): AgentHubRequestContext => ({ deviceId, projectRef }),
    [deviceId, projectRef],
  );

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
  const contextKeyRef = useRef(`${deviceId ?? ''}\0${projectRef ?? ''}`);

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
      const next = await portableAssetApi.inspect(requestContext);
      if (!mountedRef.current || seq !== refreshSeqRef.current) return;
      hasSnapshotRef.current = true;
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
  }, [requestContext]);

  useEffect(() => {
    const nextKey = `${deviceId ?? ''}\0${projectRef ?? ''}`;
    const contextChanged = contextKeyRef.current !== nextKey;
    contextKeyRef.current = nextKey;
    if (contextChanged) {
      // 切换设备/项目：丢弃旧 snapshot，禁止用本机数据冒充 peer
      hasSnapshotRef.current = false;
      // eslint-disable-next-line react-hooks/set-state-in-effect -- context switch reset
      setSnapshot(null);
      setSelectedItemId(null);
      setPendingAction(null);
      setStaleFlag(false);
      setError(null);
    }
    // eslint-disable-next-line react-hooks/set-state-in-effect -- initial / context-driven inventory load
    void refresh();
  }, [deviceId, projectRef, refresh]);

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
    requestContext,
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
