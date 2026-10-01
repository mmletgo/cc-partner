/**
 * 用户级镜像 Pull/Push controller。
 *
 * Business Logic（为什么需要这个 hook）:
 *   Agent Hub 一次镜像全部已登记 Agent 的用户级指令与资产；
 *   必须 preview 后勾选破坏性确认才能 apply；用户可选择同步哪些资产（默认全选）；
 *   换设备作废 plan；部分成功走 reconcile。
 *
 * Code Logic（这个 hook 做什么）:
 *   持有 direction / 设备选择 / plan / result / confirmed / stale 与同步内容选择
 *   （includeInstructions + 跨 Agent 去重的资产勾选，默认全选）；
 *   preview/apply/get 走 userMirrorApi；共享同步操作门闩；新预览保留已缩小的选择范围。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { devicesApi } from '@/api/devices';
import { userMirrorApi as defaultUserMirrorApi } from '@/api/userMirror';
import type { Device } from '@/lib/types';
import {
  USER_MIRROR_PREVIEW_REQUIRED,
  type UserMirrorApi,
  type UserMirrorDirection,
  type UserMirrorPlanDto,
  type UserMirrorResultDto,
  type UserMirrorSelectionFilterDto,
} from '@/lib/types/userMirror';
import {
  buildPreviewRequest,
  buildSelectionFilter,
  canApplyUserMirror,
  collectPlanAssetOptions,
  formatUserMirrorError,
  isUserMirrorPlanExpired,
  isUserMirrorStaleError,
  needsUserMirrorReconcile,
  type UserMirrorAssetOption,
} from './userMirrorPresentation';

export interface UseUserMirrorControllerOptions {
  open: boolean;
  direction: UserMirrorDirection;
  /**
   * 壳层当前 peer。Pull 预填源设备；Push 预填已选对端（在线时）。
   */
  initialSourceDeviceId?: string | null;
  mirrorApi?: UserMirrorApi;
  listDevices?: () => Promise<Device[]>;
}

export interface UseUserMirrorControllerResult {
  direction: UserMirrorDirection;
  devices: Device[];
  sourceDeviceId: string;
  selectedPeerIds: string[];
  plan: UserMirrorPlanDto | null;
  result: UserMirrorResultDto | null;
  clientRequestId: string | null;
  confirmed: boolean;
  busy: boolean;
  submitted: boolean;
  error: string | null;
  stale: boolean;
  canApply: boolean;
  canReconcile: boolean;
  /** 预览 plan 中可勾选的 portable 资产（跨 Agent 按 (kind, nativeId) 去重）。 */
  assetOptions: UserMirrorAssetOption[];
  /** 当前勾选的资产键（默认全选）。 */
  selectedAssetKeys: string[];
  includeInstructions: boolean;
  toggleAsset(key: string): void;
  selectAllAssets(): void;
  deselectAllAssets(): void;
  setIncludeInstructions(value: boolean): void;
  preview(): Promise<void>;
  apply(): Promise<void>;
  reconcile(): Promise<void>;
  selectSourceDevice(deviceId: string): void;
  togglePeer(deviceId: string): void;
  setConfirmed(value: boolean): void;
}

/**
 * Business Logic: apply 幂等键在同一 plan 内复用，新 plan 才 mint。
 * Code Logic: Tauri WebView 与项目测试运行时均提供 Web Crypto randomUUID。
 */
function createClientRequestId(): string {
  return globalThis.crypto.randomUUID();
}

/**
 * Business Logic: 只列出在线对端；离线设备不得进入镜像选择。
 * Code Logic: status === 'online'。
 */
function onlinePeers(list: Device[]): Device[] {
  return list.filter((device) => device.status === 'online');
}

/**
 * Business Logic: open 时加载在线对端并预填壳层 peer；关闭抬 sequence。
 * Code Logic: sequence + mounted ref 防 stale 写入；hooks 全在 early return 前。
 */
export function useUserMirrorController(
  options: UseUserMirrorControllerOptions,
): UseUserMirrorControllerResult {
  const {
    open,
    direction,
    initialSourceDeviceId = null,
    mirrorApi = defaultUserMirrorApi,
    listDevices = devicesApi.list,
  } = options;

  const [devices, setDevices] = useState<Device[]>([]);
  const [sourceDeviceId, setSourceDeviceId] = useState('');
  const [selectedPeerIds, setSelectedPeerIds] = useState<string[]>([]);
  const [plan, setPlan] = useState<UserMirrorPlanDto | null>(null);
  const [result, setResult] = useState<UserMirrorResultDto | null>(null);
  const [clientRequestId, setClientRequestId] = useState<string | null>(null);
  const [confirmed, setConfirmedState] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [staleFlag, setStaleFlag] = useState(false);
  const [submitted, setSubmitted] = useState(false);
  const [reconcilePending, setReconcilePending] = useState(false);
  const [includeInstructions, setIncludeInstructionsState] = useState(true);
  const [deselectedAssetKeys, setDeselectedAssetKeys] = useState<ReadonlySet<string>>(new Set());

  const mountedRef = useRef(true);
  const previewSeqRef = useRef(0);
  const applySeqRef = useRef(0);
  const operationInFlightRef = useRef<symbol | null>(null);
  const planRequestIdRef = useRef<{ planToken: string; clientRequestId: string } | null>(null);
  const listDevicesRef = useRef(listDevices);
  const mirrorApiRef = useRef(mirrorApi);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  useEffect(() => {
    listDevicesRef.current = listDevices;
  }, [listDevices]);

  useEffect(() => {
    mirrorApiRef.current = mirrorApi;
  }, [mirrorApi]);

  /**
   * Business Logic: preview/apply/reconcile 共享同一同步门闩，阻止同一 tick 双击与跨动作竞态。
   * Code Logic: 返回本次唯一 token；已有操作时返回 null。
   */
  const beginOperation = useCallback((): symbol | null => {
    if (operationInFlightRef.current) return null;
    const token = Symbol('user-mirror-operation');
    operationInFlightRef.current = token;
    setBusy(true);
    return token;
  }, []);

  /**
   * Business Logic: 旧会话 finally 不得清掉新会话的 busy 门闩。
   * Code Logic: 仅 token 仍匹配时清 ref 与 busy。
   */
  const endOperation = useCallback((token: symbol): void => {
    if (operationInFlightRef.current !== token) return;
    operationInFlightRef.current = null;
    if (mountedRef.current) setBusy(false);
  }, []);

  const invalidatePlan = useCallback(() => {
    previewSeqRef.current += 1;
    applySeqRef.current += 1;
    planRequestIdRef.current = null;
    setPlan(null);
    setResult(null);
    setClientRequestId(null);
    setSubmitted(false);
    setReconcilePending(false);
    setConfirmedState(false);
    setStaleFlag(false);
  }, []);

  useEffect(() => {
    if (!open) {
      previewSeqRef.current += 1;
      applySeqRef.current += 1;
      operationInFlightRef.current = null;
      planRequestIdRef.current = null;
      return;
    }

    previewSeqRef.current += 1;
    applySeqRef.current += 1;
    operationInFlightRef.current = null;
    planRequestIdRef.current = null;
    /* eslint-disable react-hooks/set-state-in-effect -- open-session hydration is the effect's contract. */
    setDevices([]);
    setSourceDeviceId('');
    setSelectedPeerIds([]);
    setPlan(null);
    setResult(null);
    setClientRequestId(null);
    setConfirmedState(false);
    setStaleFlag(false);
    setSubmitted(false);
    setReconcilePending(false);
    setError(null);
    setBusy(false);
    setIncludeInstructionsState(true);
    setDeselectedAssetKeys(new Set());
    /* eslint-enable react-hooks/set-state-in-effect */

    let cancelled = false;
    void (async () => {
      try {
        const list = await listDevicesRef.current();
        if (cancelled || !mountedRef.current) return;
        const peers = onlinePeers(list);
        setDevices(peers);
        const preferred =
          initialSourceDeviceId && peers.some((device) => device.id === initialSourceDeviceId)
            ? initialSourceDeviceId
            : '';
        if (direction === 'pull') {
          const nextSource = preferred || peers[0]?.id || '';
          setSourceDeviceId(nextSource);
          setSelectedPeerIds([]);
        } else {
          setSourceDeviceId('');
          const nextPeers = preferred ? [preferred] : [];
          setSelectedPeerIds(nextPeers);
        }
      } catch (reason) {
        if (cancelled || !mountedRef.current) return;
        setError(formatUserMirrorError(reason));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [open, direction, initialSourceDeviceId]);

  const stale = staleFlag || isUserMirrorPlanExpired(plan);
  const assetOptions = useMemo(() => collectPlanAssetOptions(plan), [plan]);
  const selectedAssetKeys = useMemo(
    () => assetOptions.filter((option) => !deselectedAssetKeys.has(option.key)).map((option) => option.key),
    [assetOptions, deselectedAssetKeys],
  );
  const hasSelection = includeInstructions || selectedAssetKeys.length > 0;
  const canApply = canApplyUserMirror({
    plan,
    confirmed,
    busy,
    stale,
    submitted,
    hasSelection,
  });
  const canReconcile =
    Boolean(clientRequestId) && (reconcilePending || needsUserMirrorReconcile(result));

  const selectSourceDevice = useCallback(
    (deviceId: string) => {
      setSourceDeviceId(deviceId);
      invalidatePlan();
    },
    [invalidatePlan],
  );

  const togglePeer = useCallback(
    (deviceId: string) => {
      setSelectedPeerIds((prev) => {
        const next = prev.includes(deviceId)
          ? prev.filter((id) => id !== deviceId)
          : [...prev, deviceId];
        return next;
      });
      invalidatePlan();
    },
    [invalidatePlan],
  );

  const setConfirmed = useCallback((value: boolean) => {
    setConfirmedState(value);
  }, []);

  /**
   * Business Logic: 资产勾选跨 Agent 联动，取消勾选项记录在案，其余默认全选。
   * Code Logic: 维护「被取消勾选」集合：已取消则恢复，未取消则加入。
   */
  const toggleAsset = useCallback((key: string) => {
    setDeselectedAssetKeys((previous) => {
      const next = new Set(previous);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
    setConfirmedState(false);
  }, []);

  /** Business Logic: 全选快捷操作 = 清空取消勾选集合（回到默认全量）。 */
  const selectAllAssets = useCallback(() => {
    setDeselectedAssetKeys(new Set());
    setConfirmedState(false);
  }, []);

  /** Business Logic: 全不选快捷操作 = 取消全部资产（指令仍由 includeInstructions 控制）。 */
  const deselectAllAssets = useCallback(() => {
    setDeselectedAssetKeys(new Set(assetOptions.map((option) => option.key)));
    setConfirmedState(false);
  }, [assetOptions]);

  const setIncludeInstructions = useCallback((value: boolean) => {
    setIncludeInstructionsState(value);
    setConfirmedState(false);
  }, []);

  const preview = useCallback(async () => {
    const request = buildPreviewRequest(
      direction,
      sourceDeviceId,
      selectedPeerIds,
    );
    if (!request) {
      setError(USER_MIRROR_PREVIEW_REQUIRED);
      return;
    }
    const operationToken = beginOperation();
    if (!operationToken) return;
    const seq = ++previewSeqRef.current;
    setError(null);
    setStaleFlag(false);
    const preserveSelection = plan !== null;
    const previouslySelectedKeys = new Set(selectedAssetKeys);
    try {
      const nextPlan = await mirrorApiRef.current.preview(request);
      if (!mountedRef.current || seq !== previewSeqRef.current) return;
      const nextOptions = collectPlanAssetOptions(nextPlan);
      let nextDeselectedKeys: ReadonlySet<string>;
      if (preserveSelection) {
        nextDeselectedKeys = new Set(
          nextOptions
            .filter((option) => !previouslySelectedKeys.has(option.key))
            .map((option) => option.key),
        );
      } else {
        nextDeselectedKeys = new Set();
      }
      setPlan(nextPlan);
      setResult(null);
      planRequestIdRef.current = null;
      setClientRequestId(null);
      setSubmitted(false);
      setReconcilePending(false);
      setConfirmedState(false);
      setDeselectedAssetKeys(nextDeselectedKeys);
    } catch (reason) {
      if (!mountedRef.current || seq !== previewSeqRef.current) return;
      if (isUserMirrorStaleError(reason)) {
        setStaleFlag(true);
      }
      setError(formatUserMirrorError(reason));
    } finally {
      endOperation(operationToken);
    }
  }, [
    beginOperation,
    direction,
    endOperation,
    plan,
    selectedAssetKeys,
    selectedPeerIds,
    sourceDeviceId,
  ]);

  const apply = useCallback(async () => {
    if (!plan) {
      setError(USER_MIRROR_PREVIEW_REQUIRED);
      return;
    }
    if (
      !canApplyUserMirror({
        plan,
        confirmed,
        busy: Boolean(operationInFlightRef.current),
        stale,
        submitted,
        hasSelection,
      })
    ) {
      return;
    }
    const operationToken = beginOperation();
    if (!operationToken) return;
    const existing = planRequestIdRef.current;
    const requestId =
      existing && existing.planToken === plan.planToken
        ? existing.clientRequestId
        : createClientRequestId();
    planRequestIdRef.current = { planToken: plan.planToken, clientRequestId: requestId };
    setClientRequestId(requestId);
    setSubmitted(true);
    setReconcilePending(true);

    const seq = ++applySeqRef.current;
    setError(null);
    // 全选 + 指令开 → selection 为 null，apply 请求不带该字段（等价默认全量）。
    const selection: UserMirrorSelectionFilterDto | null = buildSelectionFilter({
      includeInstructions,
      options: assetOptions,
      deselectedKeys: deselectedAssetKeys,
    });
    try {
      const requestPayload = selection
        ? {
            planToken: plan.planToken,
            clientRequestId: requestId,
            selection,
          }
        : {
            planToken: plan.planToken,
            clientRequestId: requestId,
          };
      const nextResult = await mirrorApiRef.current.apply(requestPayload);
      if (!mountedRef.current || seq !== applySeqRef.current) return;
      setResult(nextResult);
      setClientRequestId(nextResult.clientRequestId);
      setReconcilePending(false);
    } catch (reason) {
      if (!mountedRef.current || seq !== applySeqRef.current) return;
      if (isUserMirrorStaleError(reason)) {
        setStaleFlag(true);
        setReconcilePending(false);
      }
      setError(formatUserMirrorError(reason));
    } finally {
      endOperation(operationToken);
    }
  }, [
    assetOptions,
    beginOperation,
    confirmed,
    deselectedAssetKeys,
    endOperation,
    hasSelection,
    includeInstructions,
    plan,
    stale,
    submitted,
  ]);

  const reconcile = useCallback(async () => {
    const requestId = clientRequestId ?? planRequestIdRef.current?.clientRequestId ?? null;
    if (!requestId) {
      setError(USER_MIRROR_PREVIEW_REQUIRED);
      return;
    }
    const operationToken = beginOperation();
    if (!operationToken) return;
    const seq = ++applySeqRef.current;
    setError(null);
    try {
      const nextResult = await mirrorApiRef.current.get(requestId);
      if (!mountedRef.current || seq !== applySeqRef.current) return;
      setResult(nextResult);
      setClientRequestId(nextResult.clientRequestId);
      setReconcilePending(false);
    } catch (reason) {
      if (!mountedRef.current || seq !== applySeqRef.current) return;
      if (isUserMirrorStaleError(reason)) {
        setStaleFlag(true);
      }
      setReconcilePending(true);
      setError(formatUserMirrorError(reason));
    } finally {
      endOperation(operationToken);
    }
  }, [beginOperation, clientRequestId, endOperation]);

  return {
    direction,
    devices,
    sourceDeviceId,
    selectedPeerIds,
    plan,
    result,
    clientRequestId,
    confirmed,
    busy,
    submitted,
    error,
    stale,
    canApply,
    canReconcile,
    assetOptions,
    selectedAssetKeys,
    includeInstructions,
    toggleAsset,
    selectAllAssets,
    deselectAllAssets,
    setIncludeInstructions,
    preview,
    apply,
    reconcile,
    selectSourceDevice,
    togglePeer,
    setConfirmed,
  };
}
