/**
 * Same-agent remote portable pull controller.
 *
 * Business Logic（为什么需要这个 hook）:
 *   用户从远端设备按同类 Agent 选择性 Pull 资产；destination 固定等于 sourceTarget；
 *   device/target 切换必须取消 stale inventory 并清 selection；stale 禁止 mutation；
 *   partial/outcomeUnknown 走 reconcile，重复 apply 复用 clientRequestId。
 *
 * Code Logic（这个 hook 做什么）:
 *   持有 devices/selection/plan/result/sequence/clientRequestId；调用 devicesApi + portablePullApi；
 *   pure views 只消费 narrow props。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { devicesApi } from '@/api/devices';
import { portablePullApi as defaultPortablePullApi } from '@/api/portableInventory';
import type { AgentTarget } from '@/lib/types/agentHub';
import type { Device } from '@/lib/types';
import type {
  PortableAssetConflictPolicy,
  PortablePullApi,
  PortablePullPlanDto,
  PortablePullResultDto,
  RemotePortableInventoryDto,
  RemotePortableInventoryItemDto,
} from '@/lib/types/portableInventory';
import {
  canConfirmPortablePull,
  filterRemotePortableItems,
  needsPullReconcile,
  selectVisibleRemoteItemIds,
  type PortablePullFilters,
} from './portablePullPresentation';

export interface UsePortablePullControllerOptions {
  open: boolean;
  pullApi?: PortablePullApi;
  listDevices?: () => Promise<Device[]>;
  initialSourceTarget?: AgentTarget;
}

export interface UsePortablePullControllerResult {
  devices: Device[];
  selectedDeviceId: string;
  sourceTarget: AgentTarget;
  remoteInventory: RemotePortableInventoryDto | null;
  visibleItems: RemotePortableInventoryItemDto[];
  selectedItemIds: Set<string>;
  filters: PortablePullFilters;
  conflictPolicy: PortableAssetConflictPolicy;
  plan: PortablePullPlanDto | null;
  result: PortablePullResultDto | null;
  clientRequestId: string | null;
  busy: boolean;
  error: string | null;
  mutationBlocked: boolean;
  canApply: boolean;
  canReconcile: boolean;
  loadInventory(): Promise<void>;
  preview(): Promise<void>;
  apply(): Promise<void>;
  reconcile(): Promise<void>;
  selectDevice(deviceId: string): void;
  selectSourceTarget(target: AgentTarget): void;
  setFilters(next: PortablePullFilters): void;
  setConflictPolicy(policy: PortableAssetConflictPolicy): void;
  toggleItem(inventoryItemId: string): void;
  selectVisible(): void;
  clearSelection(): void;
}

const DEFAULT_FILTERS: PortablePullFilters = {
  kind: 'all',
  scope: 'all',
  actualState: 'all',
  search: '',
};

/**
 * Business Logic: apply 幂等键在同一 plan 内复用，新 plan 才 mint。
 * Code Logic: crypto.randomUUID 优先，否则时间+随机串。
 */
function createClientRequestId(): string {
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
    return crypto.randomUUID();
  }
  return `portable-pull-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
}

/**
 * Business Logic: 错误展示保留稳定 code 优先。
 * Code Logic: Error.message；有 code 时拼前缀。
 */
function formatError(reason: unknown): string {
  if (!reason) return 'unknown_error';
  if (reason instanceof Error) {
    const code = (reason as { code?: unknown }).code;
    if (typeof code === 'string' && code.length > 0) {
      return `${code}: ${reason.message}`;
    }
    return reason.message || 'unknown_error';
  }
  if (typeof reason === 'object') {
    const obj = reason as { code?: unknown; error?: unknown; message?: unknown };
    if (typeof obj.code === 'string') {
      const msg =
        typeof obj.error === 'string'
          ? obj.error
          : typeof obj.message === 'string'
            ? obj.message
            : '';
      return msg ? `${obj.code}: ${msg}` : obj.code;
    }
    if (typeof obj.error === 'string') return obj.error;
    if (typeof obj.message === 'string') return obj.message;
  }
  return String(reason);
}

/**
 * Business Logic: open 时加载 devices；destination 恒等于 sourceTarget。
 * Code Logic: sequence + mounted ref 防 stale 写入；hooks 全在 early return 前。
 */
export function usePortablePullController(
  options: UsePortablePullControllerOptions,
): UsePortablePullControllerResult {
  const {
    open,
    pullApi = defaultPortablePullApi,
    listDevices = devicesApi.list,
    initialSourceTarget = 'claude',
  } = options;

  const [devices, setDevices] = useState<Device[]>([]);
  const [selectedDeviceId, setSelectedDeviceId] = useState('');
  const [sourceTarget, setSourceTarget] = useState<AgentTarget>(initialSourceTarget);
  const [remoteInventory, setRemoteInventory] = useState<RemotePortableInventoryDto | null>(null);
  const [selectedItemIds, setSelectedItemIds] = useState<Set<string>>(() => new Set());
  const [filters, setFiltersState] = useState<PortablePullFilters>(DEFAULT_FILTERS);
  const [conflictPolicy, setConflictPolicyState] =
    useState<PortableAssetConflictPolicy>('skipExisting');
  const [plan, setPlan] = useState<PortablePullPlanDto | null>(null);
  const [result, setResult] = useState<PortablePullResultDto | null>(null);
  const [clientRequestId, setClientRequestId] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const mountedRef = useRef(true);
  const inventorySeqRef = useRef(0);
  const previewSeqRef = useRef(0);
  const applySeqRef = useRef(0);
  const planRequestIdRef = useRef<{ planToken: string; clientRequestId: string } | null>(null);
  const selectedDeviceIdRef = useRef(selectedDeviceId);
  const sourceTargetRef = useRef(sourceTarget);
  const remoteInventoryRef = useRef(remoteInventory);
  const filtersRef = useRef(filters);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  useEffect(() => {
    selectedDeviceIdRef.current = selectedDeviceId;
  }, [selectedDeviceId]);

  useEffect(() => {
    sourceTargetRef.current = sourceTarget;
  }, [sourceTarget]);

  useEffect(() => {
    remoteInventoryRef.current = remoteInventory;
  }, [remoteInventory]);

  useEffect(() => {
    filtersRef.current = filters;
  }, [filters]);

  /** open 时加载 devices；关闭时抬 sequence 作废 in-flight inventory。 */
  useEffect(() => {
    if (!open) {
      inventorySeqRef.current += 1;
      return;
    }
    let cancelled = false;
    void (async () => {
      try {
        const list = await listDevices();
        if (cancelled || !mountedRef.current) return;
        const peers = list.filter((d) => d.status === 'online');
        setDevices(peers);
        setSelectedDeviceId((prev) => {
          if (prev && peers.some((d) => d.id === prev)) return prev;
          return peers[0]?.id ?? '';
        });
      } catch (reason) {
        if (cancelled || !mountedRef.current) return;
        setError(formatError(reason));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [open, listDevices]);

  const visibleItems = useMemo(
    () => filterRemotePortableItems(remoteInventory?.items ?? [], filters),
    [remoteInventory, filters],
  );

  const mutationBlocked = Boolean(remoteInventory?.stale);
  const canApply = canConfirmPortablePull({
    remoteInventory,
    selectedItemIds,
    plan,
    busy,
  }).ok;
  const canReconcile = needsPullReconcile(result);

  const resetPullWorkspaceForContextChange = useCallback(() => {
    inventorySeqRef.current += 1;
    previewSeqRef.current += 1;
    applySeqRef.current += 1;
    planRequestIdRef.current = null;
    setRemoteInventory(null);
    setSelectedItemIds(new Set());
    setPlan(null);
    setResult(null);
    setClientRequestId(null);
    setError(null);
  }, []);

  const selectDevice = useCallback(
    (deviceId: string) => {
      setSelectedDeviceId(deviceId);
      resetPullWorkspaceForContextChange();
    },
    [resetPullWorkspaceForContextChange],
  );

  const selectSourceTarget = useCallback(
    (target: AgentTarget) => {
      setSourceTarget(target);
      resetPullWorkspaceForContextChange();
    },
    [resetPullWorkspaceForContextChange],
  );

  const setFilters = useCallback((next: PortablePullFilters) => {
    filtersRef.current = next;
    setFiltersState(next);
  }, []);

  const setConflictPolicy = useCallback((policy: PortableAssetConflictPolicy) => {
    setConflictPolicyState(policy);
    // policy 变化使旧 plan 失效
    planRequestIdRef.current = null;
    setPlan(null);
    setResult(null);
    setClientRequestId(null);
  }, []);

  const toggleItem = useCallback((inventoryItemId: string) => {
    setSelectedItemIds((prev) => {
      const next = new Set(prev);
      if (next.has(inventoryItemId)) next.delete(inventoryItemId);
      else next.add(inventoryItemId);
      return next;
    });
    planRequestIdRef.current = null;
    setPlan(null);
    setResult(null);
    setClientRequestId(null);
  }, []);

  const selectVisible = useCallback(() => {
    // 从最新 inventory + filters 重算，避免与 setFilters 同批 act 时闭包过期
    const visible = filterRemotePortableItems(
      remoteInventoryRef.current?.items ?? [],
      filtersRef.current,
    );
    setSelectedItemIds(selectVisibleRemoteItemIds(visible));
    planRequestIdRef.current = null;
    setPlan(null);
    setResult(null);
    setClientRequestId(null);
  }, []);

  const clearSelection = useCallback(() => {
    setSelectedItemIds(new Set());
    planRequestIdRef.current = null;
    setPlan(null);
    setResult(null);
    setClientRequestId(null);
  }, []);

  const loadInventory = useCallback(async () => {
    const deviceId = selectedDeviceIdRef.current;
    const target = sourceTargetRef.current;
    if (!deviceId) {
      setError('missing_device');
      return;
    }
    const seq = ++inventorySeqRef.current;
    setBusy(true);
    setError(null);
    try {
      const snapshot = await pullApi.listRemote({
        sourceDeviceId: deviceId,
        sourceTarget: target,
      });
      if (!mountedRef.current || seq !== inventorySeqRef.current) return;
      // 丢弃与当前选择不匹配的响应
      if (
        snapshot.sourceDeviceId !== selectedDeviceIdRef.current ||
        snapshot.sourceTarget !== sourceTargetRef.current
      ) {
        return;
      }
      setRemoteInventory(snapshot);
      setSelectedItemIds((prev) => {
        if (prev.size === 0) return prev;
        const valid = new Set(snapshot.items.map((item) => item.inventoryItemId));
        const next = new Set([...prev].filter((id) => valid.has(id)));
        return next;
      });
      // inventory 刷新后旧 plan 失效
      planRequestIdRef.current = null;
      setPlan(null);
      setResult(null);
      setClientRequestId(null);
    } catch (reason) {
      if (!mountedRef.current || seq !== inventorySeqRef.current) return;
      setError(formatError(reason));
    } finally {
      if (mountedRef.current && seq === inventorySeqRef.current) {
        setBusy(false);
      }
    }
  }, [pullApi]);

  const preview = useCallback(async () => {
    const inventory = remoteInventory;
    const deviceId = selectedDeviceIdRef.current;
    const target = sourceTargetRef.current;
    if (!inventory || !deviceId) {
      setError('missing_inventory');
      return;
    }
    if (inventory.stale) {
      setError('stale_remote_inventory');
      return;
    }
    if (selectedItemIds.size === 0) {
      setError('empty_selection');
      return;
    }
    const seq = ++previewSeqRef.current;
    setBusy(true);
    setError(null);
    try {
      const nextPlan = await pullApi.preview({
        sourceDeviceId: deviceId,
        sourceTarget: target,
        destinationTarget: target,
        remoteInventorySnapshotHash: inventory.inventorySnapshotHash,
        inventoryItemIds: [...selectedItemIds],
        conflictPolicy,
      });
      if (!mountedRef.current || seq !== previewSeqRef.current) return;
      setPlan(nextPlan);
      setResult(null);
      planRequestIdRef.current = null;
      setClientRequestId(null);
    } catch (reason) {
      if (!mountedRef.current || seq !== previewSeqRef.current) return;
      // preview 失败保留 selection/policy；清 plan
      setPlan(null);
      setError(formatError(reason));
    } finally {
      if (mountedRef.current && seq === previewSeqRef.current) {
        setBusy(false);
      }
    }
  }, [pullApi, remoteInventory, selectedItemIds, conflictPolicy]);

  const apply = useCallback(async () => {
    if (!plan) {
      setError('missing_plan');
      return;
    }
    const gate = canConfirmPortablePull({
      remoteInventory,
      selectedItemIds,
      plan,
      busy: false,
    });
    if (!gate.ok) {
      setError(gate.reason);
      return;
    }
    const existing = planRequestIdRef.current;
    const requestId =
      existing && existing.planToken === plan.planToken
        ? existing.clientRequestId
        : createClientRequestId();
    planRequestIdRef.current = { planToken: plan.planToken, clientRequestId: requestId };
    setClientRequestId(requestId);

    const seq = ++applySeqRef.current;
    setBusy(true);
    setError(null);
    try {
      const nextResult = await pullApi.apply({
        planToken: plan.planToken,
        clientRequestId: requestId,
      });
      if (!mountedRef.current || seq !== applySeqRef.current) return;
      setResult(nextResult);
      setClientRequestId(nextResult.clientRequestId);
    } catch (reason) {
      if (!mountedRef.current || seq !== applySeqRef.current) return;
      setError(formatError(reason));
    } finally {
      if (mountedRef.current && seq === applySeqRef.current) {
        setBusy(false);
      }
    }
  }, [pullApi, plan, remoteInventory, selectedItemIds]);

  const reconcile = useCallback(async () => {
    const requestId = clientRequestId ?? planRequestIdRef.current?.clientRequestId ?? null;
    if (!requestId) {
      setError('missing_client_request_id');
      return;
    }
    const seq = ++applySeqRef.current;
    setBusy(true);
    setError(null);
    try {
      const nextResult = await pullApi.get(requestId);
      if (!mountedRef.current || seq !== applySeqRef.current) return;
      setResult(nextResult);
      setClientRequestId(nextResult.clientRequestId);
    } catch (reason) {
      if (!mountedRef.current || seq !== applySeqRef.current) return;
      setError(formatError(reason));
    } finally {
      if (mountedRef.current && seq === applySeqRef.current) {
        setBusy(false);
      }
    }
  }, [pullApi, clientRequestId]);

  return {
    devices,
    selectedDeviceId,
    sourceTarget,
    remoteInventory,
    visibleItems,
    selectedItemIds,
    filters,
    conflictPolicy,
    plan,
    result,
    clientRequestId,
    busy,
    error,
    mutationBlocked,
    canApply,
    canReconcile,
    loadInventory,
    preview,
    apply,
    reconcile,
    selectDevice,
    selectSourceTarget,
    setFilters,
    setConflictPolicy,
    toggleItem,
    selectVisible,
    clearSelection,
  };
}
