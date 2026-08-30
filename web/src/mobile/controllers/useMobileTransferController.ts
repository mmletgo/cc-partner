/**
 * 移动端文件传输 controller（无 JSX）。
 *
 * Business Logic（为什么需要这个模块）:
 *   手机经主机中转发送/取消/续传/下载；刷新失败不得用空数组覆盖；
 *   lost ACK 必须 get-operation 对账，禁止 mint 新 id 盲重试。
 *
 * Code Logic（这个模块做什么）:
 *   设备 5s / 任务 3s useVisibilityPolling；单文件 input + file.slice 分块上传；
 *   mint/复用 clientOperationId；返回 view model，不渲染 UI。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { deviceSupportsTransferResume } from '@/api/devices';
import {
  MOBILE_TRANSFER_CHUNK_SIZE,
  transferHttp,
  type MobileTransferDevice,
} from '@/api/transferHttp';
import { useVisibilityPolling } from '@/hooks/useVisibilityPolling';
import type { TransferOperationStatus, TransferTask } from '@/lib/types';
import {
  groupTransferTasks,
  isLogicalTransferRecoveryLocked,
  isTransferOutcomeUncertain,
  mintTransferClientOperationId,
} from '@/pages/Transfer/transferHistory';
import { errorMessage } from '@/pages/Transfer/transferPageUtils';

const TASK_REFRESH_INTERVAL_MS = 3000;
const DEVICE_REFRESH_INTERVAL_MS = 5000;
const RECONCILE_MAX_ATTEMPTS = 12;
const RECONCILE_DELAY_MS = 1500;

type LoadState = 'loading' | 'success' | 'error';
type RecoveryKind = 'retry' | 'resume';

interface PendingRecovery {
  clientOperationId: string;
  kind: RecoveryKind;
}

interface PendingSendIntent {
  intentKey: string;
  clientOperationId: string;
}

/**
 * 本地上传进度（complete 前）。
 *
 * Business Logic（为什么需要这个类型）:
 *   主机任务在 complete 之后才出现；上传阶段需要先展示本地 bytes。
 *
 * Code Logic（字段说明）:
 *   uploadedBytes / fileSize 驱动进度条。
 */
export interface MobileTransferLocalUpload {
  fileName: string;
  fileSize: number;
  uploadedBytes: number;
  deviceName: string;
}

export interface MobileTransferViewModel {
  devices: MobileTransferDevice[];
  selectedDeviceId: string;
  selectedFileName: string | null;
  sending: boolean;
  sendError: string | null;
  devicesState: LoadState;
  devicesError: string | null;
  tasks: TransferTask[];
  tasksState: LoadState;
  tasksError: string | null;
  groupedTasks: ReturnType<typeof groupTransferTasks>;
  localUpload: MobileTransferLocalUpload | null;
  reconcilingIds: ReadonlySet<string>;
  cancellingIds: ReadonlySet<string>;
  taskActionErrors: Record<string, string>;
  canSend: boolean;
  onDeviceChange: (deviceId: string) => void;
  onFileChosen: (file: File | null) => void;
  onSend: () => void;
  onCancel: (taskId: string) => void;
  onResume: (taskId: string) => void;
  onRetry: (taskId: string) => void;
  onDownload: (task: TransferTask) => void;
  onRetryDevices: () => void;
  onRetryTasks: () => void;
  peerSupportsResume: (task: TransferTask) => boolean;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   刷新失败必须保留上一份列表，禁止用空数组覆盖已成功数据。
 *
 * Code Logic（这个函数做什么）:
 *   failed 时返回 previous；成功且 incoming 为数组时返回 incoming。
 */
export function retainListOnRefreshFailure<T>(
  previous: T[],
  incoming: T[] | null | undefined,
  failed: boolean,
): T[] {
  if (failed) return previous;
  return Array.isArray(incoming) ? incoming : previous;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   同一设备+文件意图在 uncertain 期间必须复用 clientOperationId。
 *
 * Code Logic（这个函数做什么）:
 *   用 deviceId + name + size + lastModified 组成 intentKey，不含任何 path。
 */
export function buildMobileTransferSendIntentKey(
  deviceId: string,
  file: Pick<File, 'name' | 'size' | 'lastModified'>,
): string {
  return `${deviceId}\0${file.name}\0${file.size}\0${file.lastModified}`;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   timeout/network 后只能对账，不能把错误当确定性失败去 mint 新 id。
 *
 * Code Logic（这个函数做什么）:
 *   转调 isTransferOutcomeUncertain。
 */
export function shouldReconcileTransferError(error: unknown): boolean {
  return isTransferOutcomeUncertain(error);
}

/**
 * Business Logic（为什么需要这个 hook）:
 *   移动传输面板需要轮询、选设备/文件、分块上传与动作矩阵，且不得把 API 泄漏进 view。
 *
 * Code Logic（这个 hook 做什么）:
 *   持有全部 state/effects；返回 MobileTransferViewModel。
 */
export function useMobileTransferController(): MobileTransferViewModel {
  const { t } = useTranslation(['transfer', 'common']);

  const [devices, setDevices] = useState<MobileTransferDevice[]>([]);
  const [selectedDeviceId, setSelectedDeviceId] = useState<string>('');
  const [devicesState, setDevicesState] = useState<LoadState>('loading');
  const [devicesError, setDevicesError] = useState<string | null>(null);

  const [tasks, setTasks] = useState<TransferTask[]>([]);
  const [tasksState, setTasksState] = useState<LoadState>('loading');
  const [tasksError, setTasksError] = useState<string | null>(null);

  const [selectedFile, setSelectedFile] = useState<File | null>(null);
  const [sending, setSending] = useState(false);
  const [sendError, setSendError] = useState<string | null>(null);
  const [localUpload, setLocalUpload] = useState<MobileTransferLocalUpload | null>(null);
  const [cancellingIds, setCancellingIds] = useState<ReadonlySet<string>>(() => new Set());
  const [taskActionErrors, setTaskActionErrors] = useState<Record<string, string>>({});
  const [reconcilingIds, setReconcilingIds] = useState<ReadonlySet<string>>(() => new Set());

  const sendingRef = useRef(false);
  const cancellingIdsRef = useRef<Set<string>>(new Set());
  const recoveryBusyRef = useRef<Set<string>>(new Set());
  const pendingRecoveriesRef = useRef<Record<string, PendingRecovery>>({});
  const pendingSendIntentRef = useRef<PendingSendIntent | null>(null);
  const mountedRef = useRef(true);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   设备下拉刷新失败不得清空已选目标。
   *
   * Code Logic（这个函数做什么）:
   *   成功写数组并默认选中首项；失败 retain 旧列表。
   */
  const loadDevices = useCallback(async () => {
    try {
      const data = await transferHttp.listDevices();
      if (!mountedRef.current) return;
      const next = Array.isArray(data) ? data : [];
      setDevices(next);
      if (next.length > 0) {
        setSelectedDeviceId((prev) => prev || next[0]!.id);
      }
      setDevicesState('success');
      setDevicesError(null);
    } catch (err) {
      if (!mountedRef.current) return;
      setDevices((prev) => retainListOnRefreshFailure(prev, [], true));
      setDevicesState('error');
      setDevicesError(t('transfer:deviceLoadFailed', { error: errorMessage(err) }));
    }
  }, [t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   任务刷新失败必须保留上一份，即使 incoming 会被理解成空。
   *
   * Code Logic（这个函数做什么）:
   *   成功写数组；失败 retain prev。
   */
  const loadTasks = useCallback(async () => {
    try {
      const data = await transferHttp.listTasks();
      if (!mountedRef.current) return;
      setTasks(Array.isArray(data) ? data : []);
      setTasksState('success');
      setTasksError(null);
    } catch (err) {
      if (!mountedRef.current) return;
      setTasks((prev) => retainListOnRefreshFailure(prev, [], true));
      setTasksState('error');
      setTasksError(t('transfer:taskLoadFailed', { error: errorMessage(err) }));
    }
  }, [t]);

  const { runNow: runTasksNow } = useVisibilityPolling(loadTasks, {
    intervalMs: TASK_REFRESH_INTERVAL_MS,
  });

  useVisibilityPolling(loadDevices, {
    intervalMs: DEVICE_REFRESH_INTERVAL_MS,
  });

  const groupedTasks = useMemo(
    () => groupTransferTasks(tasks, reconcilingIds),
    [tasks, reconcilingIds],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户切换目标设备。
   *
   * Code Logic（这个函数做什么）:
   *   写 selectedDeviceId。
   */
  const handleDeviceChange = useCallback((deviceId: string): void => {
    setSelectedDeviceId(deviceId);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   清理行级错误。
   *
   * Code Logic（这个函数做什么）:
   *   从 taskActionErrors 删除指定 taskId。
   */
  const clearTaskActionError = useCallback((taskId: string): void => {
    setTaskActionErrors((prev) => {
      if (!(taskId in prev)) return prev;
      const next = { ...prev };
      delete next[taskId];
      return next;
    });
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   recovery 成功或对账终态后释放 clientOperationId。
   *
   * Code Logic（这个函数做什么）:
   *   删除 pendingRecoveriesRef 对应项。
   */
  const clearPendingRecovery = useCallback((taskId: string): void => {
    if (!(taskId in pendingRecoveriesRef.current)) return;
    const next = { ...pendingRecoveriesRef.current };
    delete next[taskId];
    pendingRecoveriesRef.current = next;
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   进入/离开对账态。
   *
   * Code Logic（这个函数做什么）:
   *   不可变 Set 更新。
   */
  const setReconciling = useCallback((taskId: string, on: boolean): void => {
    setReconcilingIds((prev) => {
      const has = prev.has(taskId);
      if (on && has) return prev;
      if (!on && !has) return prev;
      const next = new Set(prev);
      if (on) next.add(taskId);
      else next.delete(taskId);
      return next;
    });
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   对账 pending 必须有界轮询，禁止立即 mint 新 id 再发。
   *
   * Code Logic（这个函数做什么）:
   *   轮询 getOperation 至终态或耗尽。
   */
  const reconcileClientOperation = useCallback(
    async (clientOperationId: string): Promise<TransferOperationStatus | null> => {
      for (let attempt = 0; attempt < RECONCILE_MAX_ATTEMPTS; attempt += 1) {
        if (!mountedRef.current) return null;
        const status = await transferHttp.getOperation(clientOperationId);
        if (!mountedRef.current) return null;
        if (status.status !== 'pending') {
          return status;
        }
        if (attempt + 1 >= RECONCILE_MAX_ATTEMPTS) {
          return status;
        }
        await new Promise<void>((resolve) => {
          window.setTimeout(resolve, RECONCILE_DELAY_MS);
        });
      }
      return { status: 'pending' };
    },
    [],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户选完文件后必须 init → chunk → complete；uncertain 只对账。
   *
   * Code Logic（这个函数做什么）:
   *   sendingRef 门闩；按 intentKey 复用 clientOperationId；file.slice 分块；
   *   本地进度写入 localUpload；成功后 force 刷新任务。
   *   fileOverride 供“选完即传”路径直接传入 File（此时 selectedFile state 尚未落盘）。
   */
  const handleSend = useCallback(async (fileOverride?: File): Promise<void> => {
    const file = fileOverride ?? selectedFile;
    if (!file || !selectedDeviceId || sendingRef.current) return;
    sendingRef.current = true;
    setSending(true);
    setSendError(null);

    const intentKey = buildMobileTransferSendIntentKey(selectedDeviceId, file);
    const existing = pendingSendIntentRef.current;
    const clientOperationId =
      existing && existing.intentKey === intentKey
        ? existing.clientOperationId
        : mintTransferClientOperationId();
    pendingSendIntentRef.current = { intentKey, clientOperationId };

    const deviceName =
      devices.find((device) => device.id === selectedDeviceId)?.name ?? selectedDeviceId;

    /**
     * Business Logic（为什么需要这个函数）:
     *   complete/uncertain 后需要统一刷新并解释对账终态。
     *
     * Code Logic（这个函数做什么）:
     *   调 reconcileClientOperation；succeeded 清空选择。
     */
    const finishUncertainSend = async (): Promise<void> => {
      try {
        const status = await reconcileClientOperation(clientOperationId);
        if (!mountedRef.current) return;
        if (!status || status.status === 'pending') {
          setSendError(
            t('transfer:sendFailed', {
              error: 'operation still pending; retry reconcile',
            }),
          );
          return;
        }
        pendingSendIntentRef.current = null;
        setLocalUpload(null);
        await runTasksNow({ force: true });
        if (status.status === 'failed') {
          setSendError(t('transfer:sendFailed', { error: status.code }));
        } else if (status.status === 'succeeded') {
          setSelectedFile(null);
          setSendError(null);
        }
      } catch (reconcileErr) {
        if (!mountedRef.current) return;
        setSendError(t('transfer:sendFailed', { error: errorMessage(reconcileErr) }));
      }
    };

    try {
      const init = await transferHttp.initUpload({
        filename: file.name,
        size: file.size,
        deviceId: selectedDeviceId,
        clientOperationId,
      });
      if (!mountedRef.current) return;

      let uploadedBytes = Math.max(0, init.receivedBytes);
      setLocalUpload({
        fileName: file.name,
        fileSize: file.size,
        uploadedBytes,
        deviceName,
      });

      while (uploadedBytes < file.size) {
        const chunk = file.slice(uploadedBytes, uploadedBytes + MOBILE_TRANSFER_CHUNK_SIZE);
        const result = await transferHttp.uploadChunk(init.id, uploadedBytes, chunk);
        if (!mountedRef.current) return;
        uploadedBytes = Math.max(result.receivedBytes, uploadedBytes + chunk.size);
        setLocalUpload({
          fileName: file.name,
          fileSize: file.size,
          uploadedBytes: Math.min(uploadedBytes, file.size),
          deviceName,
        });
      }

      await transferHttp.completeUpload(init.id);
      if (!mountedRef.current) return;
      pendingSendIntentRef.current = null;
      setLocalUpload(null);
      setSelectedFile(null);
      await runTasksNow({ force: true });
    } catch (err) {
      if (!mountedRef.current) return;
      if (shouldReconcileTransferError(err)) {
        await finishUncertainSend();
        return;
      }
      pendingSendIntentRef.current = null;
      setLocalUpload(null);
      setSendError(t('transfer:sendFailed', { error: errorMessage(err) }));
    } finally {
      sendingRef.current = false;
      setSending(false);
    }
  }, [devices, reconcileClientOperation, runTasksNow, selectedDeviceId, selectedFile, t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   浏览器只能提供 File，不能给主机绝对路径；且移动端打开系统文件选择器可能触发
   *   页面整页重载（dev 态 Vite 断线 reload / 生产态浏览器后台回收），等待手动点
   *   “发送”的窗口期内 File 引用会随重载丢失，所以选完文件必须立即自动上传。
   *
   * Code Logic（这个函数做什么）:
   *   保留单个 File、清空发送错误，并直接把 File 传给 handleSend 立即开始
   *   init → chunk → complete（不依赖尚未落盘的 selectedFile state）。
   */
  const handleFileChosen = useCallback(
    (file: File | null): void => {
      setSelectedFile(file);
      setSendError(null);
      if (file) {
        void handleSend(file);
      }
    },
    [handleSend],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   进行中任务只能取消；双击不得二次 cancel。
   *
   * Code Logic（这个函数做什么）:
   *   cancellingIdsRef 门闩；成功 force 刷新。
   */
  const handleCancel = useCallback(
    async (taskId: string): Promise<void> => {
      if (cancellingIdsRef.current.has(taskId)) return;
      cancellingIdsRef.current.add(taskId);
      setCancellingIds(new Set(cancellingIdsRef.current));
      clearTaskActionError(taskId);
      try {
        await transferHttp.cancel(taskId);
        await runTasksNow({ force: true });
      } catch (err) {
        setTaskActionErrors((prev) => ({
          ...prev,
          [taskId]: t('transfer:cancelFailed', { error: errorMessage(err) }),
        }));
      } finally {
        cancellingIdsRef.current.delete(taskId);
        setCancellingIds(new Set(cancellingIdsRef.current));
      }
    },
    [clearTaskActionError, runTasksNow, t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   recovery uncertain 时保持 reconciling，直到 getOperation 收敛。
   *
   * Code Logic（这个函数做什么）:
   *   有界轮询；终态清 pending 并 force 刷新。
   */
  const reconcilePendingRecovery = useCallback(
    async (taskId: string, clientOperationId: string): Promise<void> => {
      setReconciling(taskId, true);
      try {
        const status = await reconcileClientOperation(clientOperationId);
        if (!mountedRef.current) return;
        if (!status || status.status === 'pending') {
          setReconciling(taskId, false);
          setTaskActionErrors((prev) => ({
            ...prev,
            [taskId]: t('transfer:retryFailed', {
              error: 'operation still pending; retry reconcile',
            }),
          }));
          return;
        }
        clearPendingRecovery(taskId);
        setReconciling(taskId, false);
        await runTasksNow({ force: true });
        if (status.status === 'failed') {
          setTaskActionErrors((prev) => ({
            ...prev,
            [taskId]: t('transfer:retryFailed', { error: status.code }),
          }));
        }
      } catch (err) {
        if (!mountedRef.current) return;
        setTaskActionErrors((prev) => ({
          ...prev,
          [taskId]: t('transfer:retryFailed', { error: errorMessage(err) }),
        }));
      }
    },
    [clearPendingRecovery, reconcileClientOperation, runTasksNow, setReconciling, t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   retry/resume 必须复用同 kind 的 clientOperationId；uncertain 只对账。
   *
   * Code Logic（这个函数做什么）:
   *   逻辑锁 + recoveryBusy；成功清 pending；uncertain → getOperation。
   */
  const handleRecovery = useCallback(
    async (taskId: string, kind: RecoveryKind): Promise<void> => {
      if (recoveryBusyRef.current.has(taskId)) return;
      if (reconcilingIds.has(taskId)) return;

      const target = tasks.find((item) => item.id === taskId);
      if (target && isLogicalTransferRecoveryLocked(target, tasks, reconcilingIds)) {
        return;
      }

      recoveryBusyRef.current.add(taskId);
      clearTaskActionError(taskId);

      const existing = pendingRecoveriesRef.current[taskId];
      const clientOperationId =
        existing && existing.kind === kind
          ? existing.clientOperationId
          : mintTransferClientOperationId();
      pendingRecoveriesRef.current = {
        ...pendingRecoveriesRef.current,
        [taskId]: { clientOperationId, kind },
      };

      try {
        if (kind === 'resume') {
          await transferHttp.resume(taskId, clientOperationId);
        } else {
          await transferHttp.retry(taskId, clientOperationId);
        }
        if (!mountedRef.current) return;
        clearPendingRecovery(taskId);
        setReconciling(taskId, false);
        await runTasksNow({ force: true });
      } catch (err) {
        if (!mountedRef.current) return;
        if (shouldReconcileTransferError(err)) {
          await reconcilePendingRecovery(taskId, clientOperationId);
          return;
        }
        clearPendingRecovery(taskId);
        setReconciling(taskId, false);
        const key = kind === 'resume' ? 'transfer:resumeFailed' : 'transfer:retryFailed';
        setTaskActionErrors((prev) => ({
          ...prev,
          [taskId]: t(key, { error: errorMessage(err) }),
        }));
      } finally {
        recoveryBusyRef.current.delete(taskId);
      }
    },
    [
      clearPendingRecovery,
      clearTaskActionError,
      reconcilePendingRecovery,
      reconcilingIds,
      runTasksNow,
      setReconciling,
      t,
      tasks,
    ],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   已完成 Receive 只能下载，不能 Open/Reveal。
   *
   * Code Logic（这个函数做什么）:
   *   调 transferHttp.download(taskId, fileName)；失败写行级错误。
   */
  const handleDownload = useCallback(
    async (task: TransferTask): Promise<void> => {
      clearTaskActionError(task.id);
      try {
        await transferHttp.download(task.id, task.fileName);
      } catch (err) {
        setTaskActionErrors((prev) => ({
          ...prev,
          [task.id]: t('transfer:downloadFailed', { error: errorMessage(err) }),
        }));
      }
    },
    [clearTaskActionError, t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   旧 peer 无 resume.v1 时必须回退「重新传输」；本机目标视为支持。
   *
   * Code Logic（这个函数做什么）:
   *   isSelf 或 capabilities 含 transfer.resume.v1。
   */
  const peerSupportsResume = useCallback(
    (task: TransferTask): boolean => {
      const peer = task.peerDeviceId
        ? devices.find((device) => device.id === task.peerDeviceId)
        : undefined;
      if (peer?.isSelf) return true;
      return deviceSupportsTransferResume(peer);
    },
    [devices],
  );

  const canSend = Boolean(selectedFile && selectedDeviceId) && !sending;

  return {
    devices,
    selectedDeviceId,
    selectedFileName: selectedFile?.name ?? null,
    sending,
    sendError,
    devicesState,
    devicesError,
    tasks,
    tasksState,
    tasksError,
    groupedTasks,
    localUpload,
    reconcilingIds,
    cancellingIds,
    taskActionErrors,
    canSend,
    onDeviceChange: handleDeviceChange,
    onFileChosen: handleFileChosen,
    onSend: () => {
      void handleSend();
    },
    onCancel: (taskId) => {
      void handleCancel(taskId);
    },
    onResume: (taskId) => {
      void handleRecovery(taskId, 'resume');
    },
    onRetry: (taskId) => {
      void handleRecovery(taskId, 'retry');
    },
    onDownload: (task) => {
      void handleDownload(task);
    },
    onRetryDevices: () => {
      void loadDevices();
    },
    onRetryTasks: () => {
      void runTasksNow({ force: true });
    },
    peerSupportsResume,
  };
}
