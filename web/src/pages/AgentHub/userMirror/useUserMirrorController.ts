/**
 * 用户级镜像 Pull/Push controller。
 *
 * Business Logic（为什么需要这个 hook）:
 *   Agent Hub 一次镜像全部已登记 Agent 的用户级指令与资产；
 *   必须 preview 后勾选破坏性确认才能 apply；换设备作废 plan；部分成功走 reconcile。
 *
 * Code Logic（这个 hook 做什么）:
 *   持有 direction / 设备选择 / plan / result / confirmed / stale；
 *   preview/apply/get 走 userMirrorApi；pure views 只消费 narrow props。
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import { devicesApi } from '@/api/devices';
import { userMirrorApi as defaultUserMirrorApi } from '@/api/userMirror';
import type { Device } from '@/lib/types';
import {
  USER_MIRROR_PREVIEW_REQUIRED,
  type UserMirrorApi,
  type UserMirrorDirection,
  type UserMirrorPlanDto,
  type UserMirrorResultDto,
} from '@/lib/types/userMirror';
import {
  buildPreviewRequest,
  canApplyUserMirror,
  formatUserMirrorError,
  isUserMirrorPlanExpired,
  isUserMirrorStaleError,
  needsUserMirrorReconcile,
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
  error: string | null;
  stale: boolean;
  canApply: boolean;
  canReconcile: boolean;
  preview(): Promise<void>;
  apply(): Promise<void>;
  reconcile(): Promise<void>;
  selectSourceDevice(deviceId: string): void;
  togglePeer(deviceId: string): void;
  setConfirmed(value: boolean): void;
}

/**
 * Business Logic: apply 幂等键在同一 plan 内复用，新 plan 才 mint。
 * Code Logic: crypto.randomUUID 优先，否则时间+随机串。
 */
function createClientRequestId(): string {
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
    return crypto.randomUUID();
  }
  return `user-mirror-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
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

  const mountedRef = useRef(true);
  const previewSeqRef = useRef(0);
  const applySeqRef = useRef(0);
  const planRequestIdRef = useRef<{ planToken: string; clientRequestId: string } | null>(null);
  const sourceDeviceIdRef = useRef(sourceDeviceId);
  const selectedPeerIdsRef = useRef(selectedPeerIds);
  const directionRef = useRef(direction);
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

  useEffect(() => {
    sourceDeviceIdRef.current = sourceDeviceId;
  }, [sourceDeviceId]);

  useEffect(() => {
    selectedPeerIdsRef.current = selectedPeerIds;
  }, [selectedPeerIds]);

  useEffect(() => {
    directionRef.current = direction;
  }, [direction]);

  const invalidatePlan = useCallback(() => {
    previewSeqRef.current += 1;
    planRequestIdRef.current = null;
    setPlan(null);
    setResult(null);
    setClientRequestId(null);
    setConfirmedState(false);
    setStaleFlag(false);
    setBusy(false);
  }, []);

  useEffect(() => {
    if (!open) {
      previewSeqRef.current += 1;
      applySeqRef.current += 1;
      planRequestIdRef.current = null;
      return;
    }

    previewSeqRef.current += 1;
    applySeqRef.current += 1;
    planRequestIdRef.current = null;
    /* eslint-disable react-hooks/set-state-in-effect -- open-session hydration is the effect's contract. */
    setPlan(null);
    setResult(null);
    setClientRequestId(null);
    setConfirmedState(false);
    setStaleFlag(false);
    setError(null);
    setBusy(false);
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
          sourceDeviceIdRef.current = nextSource;
          setSelectedPeerIds([]);
          selectedPeerIdsRef.current = [];
        } else {
          setSourceDeviceId('');
          sourceDeviceIdRef.current = '';
          const nextPeers = preferred ? [preferred] : [];
          setSelectedPeerIds(nextPeers);
          selectedPeerIdsRef.current = nextPeers;
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
  const canApply = canApplyUserMirror({ plan, confirmed, busy, stale });
  const canReconcile = needsUserMirrorReconcile(result);

  const selectSourceDevice = useCallback(
    (deviceId: string) => {
      setSourceDeviceId(deviceId);
      sourceDeviceIdRef.current = deviceId;
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
        selectedPeerIdsRef.current = next;
        return next;
      });
      invalidatePlan();
    },
    [invalidatePlan],
  );

  const setConfirmed = useCallback((value: boolean) => {
    setConfirmedState(value);
  }, []);

  const preview = useCallback(async () => {
    const request = buildPreviewRequest(
      directionRef.current,
      sourceDeviceIdRef.current,
      selectedPeerIdsRef.current,
    );
    if (!request) {
      setError(USER_MIRROR_PREVIEW_REQUIRED);
      return;
    }
    const seq = ++previewSeqRef.current;
    setBusy(true);
    setError(null);
    setStaleFlag(false);
    try {
      const nextPlan = await mirrorApiRef.current.preview(request);
      if (!mountedRef.current || seq !== previewSeqRef.current) return;
      setPlan(nextPlan);
      setResult(null);
      planRequestIdRef.current = null;
      setClientRequestId(null);
      setConfirmedState(false);
    } catch (reason) {
      if (!mountedRef.current || seq !== previewSeqRef.current) return;
      setPlan(null);
      if (isUserMirrorStaleError(reason)) setStaleFlag(true);
      setError(formatUserMirrorError(reason));
    } finally {
      if (mountedRef.current && seq === previewSeqRef.current) {
        setBusy(false);
      }
    }
  }, []);

  const apply = useCallback(async () => {
    if (!plan) {
      setError(USER_MIRROR_PREVIEW_REQUIRED);
      return;
    }
    if (!canApplyUserMirror({ plan, confirmed, busy: false, stale })) {
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
      const nextResult = await mirrorApiRef.current.apply({
        planToken: plan.planToken,
        clientRequestId: requestId,
      });
      if (!mountedRef.current || seq !== applySeqRef.current) return;
      setResult(nextResult);
      setClientRequestId(nextResult.clientRequestId);
    } catch (reason) {
      if (!mountedRef.current || seq !== applySeqRef.current) return;
      if (isUserMirrorStaleError(reason)) setStaleFlag(true);
      setError(formatUserMirrorError(reason));
    } finally {
      if (mountedRef.current && seq === applySeqRef.current) {
        setBusy(false);
      }
    }
  }, [plan, confirmed, stale]);

  const reconcile = useCallback(async () => {
    const requestId = clientRequestId ?? planRequestIdRef.current?.clientRequestId ?? null;
    if (!requestId) {
      setError(USER_MIRROR_PREVIEW_REQUIRED);
      return;
    }
    const seq = ++applySeqRef.current;
    setBusy(true);
    setError(null);
    try {
      const nextResult = await mirrorApiRef.current.get(requestId);
      if (!mountedRef.current || seq !== applySeqRef.current) return;
      setResult(nextResult);
      setClientRequestId(nextResult.clientRequestId);
    } catch (reason) {
      if (!mountedRef.current || seq !== applySeqRef.current) return;
      if (isUserMirrorStaleError(reason)) setStaleFlag(true);
      setError(formatUserMirrorError(reason));
    } finally {
      if (mountedRef.current && seq === applySeqRef.current) {
        setBusy(false);
      }
    }
  }, [clientRequestId]);

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
    error,
    stale,
    canApply,
    canReconcile,
    preview,
    apply,
    reconcile,
    selectSourceDevice,
    togglePeer,
    setConfirmed,
  };
}
