/**
 * Transfer 页面 - 局域网文件传输
 *
 * Business Logic（为什么需要这个页面）:
 *   用户需要在一个屏幕里完成：选目标设备、选本机文件路径、发送、监控进度与取消，
 *   并对失败任务执行 resume/retry，对本机已接收 completed 任务 Open/Reveal；
 *   结果不确定时必须先对账，禁止 blind retry。
 *
 * Code Logic（这个页面做什么）:
 *   - 设备 5s / 任务 3s visibility-aware polling；刷新失败保留已有数组
 *   - 选中文件存 {path,name}；Enter/Space/点击走 pickTransferFile；native drop 只取首路径
 *   - handleSendClick 用 sendingRef 同步门闩；mint/复用稳定 clientOperationId 调 send
 *   - pending/transferring 传 onCancel；failed 传 resume 或 retry；receive completed 传 open/reveal
 *   - 首次 send 与 retry/resume 均稳定 clientOperationId；uncertain → getOperation，不盲重放
 *   - 历史按 active/needs-attention/recent-completed 分区，空组省略
 *   - Tauri 环境下 listen transfer:progress 与 completed/failed/cancelled，fail-closed 解码后 merge
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ChangeEvent, KeyboardEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { listen } from '@tauri-apps/api/event';
import { Button, Card, Pill } from '@/components/primitives';
import { TransferItem } from '@/components/domain';
import { deviceSupportsTransferResume, devicesApi } from '@/api/devices';
import { transferApi } from '@/api/transfer';
import { useVisibilityPolling } from '@/hooks/useVisibilityPolling';
import { classifyTransportFault, planFaultRecovery } from '@/lib/faultRecovery';
import type { Device, TransferTask } from '@/lib/types';
import { SendIcon, UploadIcon } from '@/lib/icons';
import { ContractDecodeError } from '@/lib/runtimeSchema';
import {
  decodeTransferProgressEvent,
  decodeTransferStatusEvent,
  mergeTransferProgressEvent,
  mergeTransferStatusEvent,
} from '@/lib/transferProgress';
import { pickTransferFile, subscribeTransferFileDrops } from './transferFileSelection';
import {
  canOpenRevealTransfer,
  groupTransferTasks,
  isLogicalTransferRecoveryLocked,
  isTransferOutcomeUncertain,
  isTransferResumable,
  isTransferRetryable,
  mintTransferClientOperationId,
} from './transferHistory';
import styles from './Transfer.module.css';

const TASK_REFRESH_INTERVAL_MS = 3000;
const DEVICE_REFRESH_INTERVAL_MS = 5000;

/** 终态事件名：completed / failed / cancelled 共用 StatusPayload。 */
const TRANSFER_STATUS_EVENTS = [
  'transfer:completed',
  'transfer:failed',
  'transfer:cancelled',
] as const;

type LoadState = 'loading' | 'success' | 'error';

/** 用户意图级 recovery 操作（稳定 clientOperationId）。 */
type RecoveryKind = 'retry' | 'resume';

interface PendingRecovery {
  clientOperationId: string;
  kind: RecoveryKind;
}

/** 首次 send 意图：同 device+path 在 pending/uncertain 期间复用 clientOperationId。 */
interface PendingSendIntent {
  intentKey: string;
  clientOperationId: string;
}

interface TauriInternalsWindow extends Window {
  __TAURI_INTERNALS__?: {
    transformCallback?: unknown;
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   普通浏览器调试环境没有 Tauri event internals，页面不得注册不可用的桌面事件。
 *
 * Code Logic（这个函数做什么）:
 *   检测 window.__TAURI_INTERNALS__.transformCallback 是否为函数。
 */
function canListenToTauriEvents(): boolean {
  const internals = (window as TauriInternalsWindow).__TAURI_INTERNALS__;
  return typeof internals?.transformCallback === 'function';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   解码失败只允许日志暴露 contract/path，禁止打印 payload。
 *
 * Code Logic（这个函数做什么）:
 *   ContractDecodeError 输出 contract + path；其它错误仅输出安全 message。
 */
function warnTransferEventDecodeFailure(eventName: string, reason: unknown): void {
  if (reason instanceof ContractDecodeError) {
    console.warn(
      `[transfer] skip ${eventName}: contract=${reason.contract} path=${reason.path}`,
    );
    return;
  }
  const message = reason instanceof Error ? reason.message : String(reason);
  console.warn(`[transfer] skip ${eventName}: ${message}`);
}

/**
 * Business Logic（为什么需要这个类型）:
 *   发送只需要绝对路径与展示用 basename，不能用浏览器 File（无可靠路径）。
 *
 * Code Logic（这个类型做什么）:
 *   path 为不透明 UTF-8 绝对路径；name 为 basename 展示。
 */
interface SelectedTransferFile {
  path: string;
  name: string;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   UI 只能展示 basename，不得暴露完整绝对路径。
 *
 * Code Logic（这个函数做什么）:
 *   按最后一次 / 或 \\ 切分路径，得到文件名；路径本身不做改写。
 */
function basenameFromPath(path: string): string {
  const normalized = path.replace(/\\/g, '/');
  const parts = normalized.split('/');
  const last = parts[parts.length - 1] ?? '';
  return last.length > 0 ? last : path;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   错误对象可能是 Error 或未知 reject，需要稳定可读文案。
 *
 * Code Logic（这个函数做什么）:
 *   Error 取 message，其余 String()。
 */
function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/**
 * Business Logic（为什么需要这个组件）:
 *   局域网文件传输主视图：设备选择、路径选择、发送、任务监控与恢复动作。
 *
 * Code Logic（这个组件做什么）:
 *   挂载 visibility polling、native drop 与 transfer progress/status 事件订阅，
 *   管理 send/cancel/retry/resume/open/reveal 状态机并按分区渲染列表。
 */
export function Transfer() {
  const { t } = useTranslation(['transfer', 'common']);

  // ── 设备列表（目标设备下拉数据源） ──
  const [devices, setDevices] = useState<Device[]>([]);
  const [selectedDeviceId, setSelectedDeviceId] = useState<string>('');
  const [devicesState, setDevicesState] = useState<LoadState>('loading');
  const [devicesError, setDevicesError] = useState<string | null>(null);

  // ── 任务列表 ──
  const [tasks, setTasks] = useState<TransferTask[]>([]);
  const [tasksState, setTasksState] = useState<LoadState>('loading');
  const [tasksError, setTasksError] = useState<string | null>(null);

  // ── 文件选择 / 拖拽 ──
  const [selectedFile, setSelectedFile] = useState<SelectedTransferFile | null>(null);
  const [isDragOver, setIsDragOver] = useState(false);
  const [selectionNotice, setSelectionNotice] = useState<string | null>(null);

  // ── 发送 / 取消 / recovery 动作状态 ──
  const [sending, setSending] = useState(false);
  const [sendError, setSendError] = useState<string | null>(null);
  const [cancellingIds, setCancellingIds] = useState<ReadonlySet<string>>(() => new Set());
  const [taskActionErrors, setTaskActionErrors] = useState<Record<string, string>>({});
  /** 结果不确定、正在 getOperation 对账的 taskId */
  const [reconcilingIds, setReconcilingIds] = useState<ReadonlySet<string>>(() => new Set());
  /** 同步门闩：双击 send 在 re-render 前也不可重入（仅 React state 不够） */
  const sendingRef = useRef(false);
  /** 同步门闩：双击 cancel 在 re-render 前也不可重入 */
  const cancellingIdsRef = useRef<Set<string>>(new Set());
  /** 同步门闩：同一 task 的 recovery 不可并发 */
  const recoveryBusyRef = useRef<Set<string>>(new Set());
  /** 稳定 clientOperationId：user intent 在 pending/unknown 期间复用 */
  const pendingRecoveriesRef = useRef<Record<string, PendingRecovery>>({});
  /** 首次 send 意图：同 device+path 在 uncertain 期间复用 clientOperationId */
  const pendingSendIntentRef = useRef<PendingSendIntent | null>(null);
  /** 组件挂载守卫：异步 loader 写 state 前检查 */
  const mountedRef = useRef(true);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   设备下拉需要与后端发现列表对齐；刷新失败不得清空已成功数据。
   *
   * Code Logic（这个函数做什么）:
   *   调用 devicesApi.list；成功写数组；失败保留 prev 并标 error；卸载后不 setState。
   */
  const loadDevices = useCallback(async () => {
    try {
      const data = await devicesApi.list();
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
      setDevicesState('error');
      setDevicesError(t('transfer:deviceLoadFailed', { error: errorMessage(err) }));
      // 保留已有 devices，不覆盖为空
    }
  }, [t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   任务列表是发送/取消后的权威进度源；刷新失败按故障分类决定 keepStale 或 fail-closed 清空。
   *
   * Code Logic（这个函数做什么）:
   *   调用 transferApi.list；成功写数组；失败经 classifyTransportFault + planFaultRecovery：
   *   clear 清空 tasks，keepStale/none 保留 prev；错误文案优先用稳定 code；卸载后不 setState。
   */
  const loadTasks = useCallback(async () => {
    try {
      const data = await transferApi.list();
      if (!mountedRef.current) return;
      setTasks(Array.isArray(data) ? data : []);
      setTasksState('success');
      setTasksError(null);
    } catch (err) {
      if (!mountedRef.current) return;
      const classification = classifyTransportFault(err);
      setTasks((prev) => {
        const plan = planFaultRecovery({
          classification,
          hasCache: prev.length > 0,
          optimisticApplied: false,
        });
        if (plan.clearCache) {
          return [];
        }
        return prev;
      });
      setTasksState('error');
      const displayError =
        classification.code !== 'UNKNOWN_FAULT' ? classification.code : errorMessage(err);
      setTasksError(t('transfer:taskLoadFailed', { error: displayError }));
    }
  }, [t]);

  const { runNow: runTasksNow } = useVisibilityPolling(loadTasks, {
    intervalMs: TASK_REFRESH_INTERVAL_MS,
  });

  useVisibilityPolling(loadDevices, {
    intervalMs: DEVICE_REFRESH_INTERVAL_MS,
  });

  // ── 状态计数（按 status 分组） ──
  const statusCounts = useMemo(() => {
    return tasks.reduce(
      (acc, task) => {
        if (task.status === 'transferring' || task.status === 'pending') acc.active += 1;
        else if (task.status === 'completed') acc.completed += 1;
        else if (task.status === 'failed' || task.status === 'cancelled') acc.failed += 1;
        return acc;
      },
      { active: 0, completed: 0, failed: 0 },
    );
  }, [tasks]);

  const groupedTasks = useMemo(
    () => groupTransferTasks(tasks, reconcilingIds),
    [tasks, reconcilingIds],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   原生路径选中后需要只展示 basename，完整路径仅用于 send。
   *
   * Code Logic（这个函数做什么）:
   *   写入 selectedFile，可选清理/设置多文件 notice。
   */
  const applySelectedPath = useCallback((path: string, multi: boolean) => {
    setSelectedFile({ path, name: basenameFromPath(path) });
    setSendError(null);
    setSelectionNotice(multi ? t('transfer:firstFileOnly') : null);
  }, [t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   浏览按钮与 dropzone 键盘操作需要打开原生单文件选择器。
   *
   * Code Logic（这个函数做什么）:
   *   await pickTransferFile；取消(null)保留原选择；成功写 {path,name}。
   */
  const handlePickClick = useCallback(async () => {
    const path = await pickTransferFile();
    if (path == null) return;
    applySelectedPath(path, false);
  }, [applySelectedPath]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   dropzone 需支持键盘可达，Enter/Space 打开原生选择器。
   *
   * Code Logic（这个函数做什么）:
   *   拦截 Enter/Space，preventDefault 后触发 handlePickClick。
   */
  const handleDropzoneKeyDown = useCallback(
    (event: KeyboardEvent<HTMLDivElement>) => {
      if (event.key !== 'Enter' && event.key !== ' ') return;
      event.preventDefault();
      void handlePickClick();
    },
    [handlePickClick],
  );

  // 挂载时订阅 native drag-drop；卸载时 unlisten
  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | undefined;

    void (async () => {
      const stop = await subscribeTransferFileDrops((paths) => {
        if (!active) return;
        const first = paths[0];
        if (!first) return;
        setIsDragOver(false);
        applySelectedPath(first, paths.length > 1);
      });
      if (!active) {
        stop();
        return;
      }
      unlisten = stop;
    })();

    return () => {
      active = false;
      unlisten?.();
    };
  }, [applySelectedPath]);

  // 挂载时订阅 transfer progress / 终态事件；解码失败 fail-closed 跳过
  useEffect(() => {
    if (!canListenToTauriEvents()) return undefined;

    const unlistenProgress = listen('transfer:progress', (event) => {
      setTasks((prev) => {
        const next = mergeTransferProgressEvent(prev, event.payload);
        if (next != null) {
          return next;
        }
        // merge 返回 null：再 decode 一次仅用于安全日志（不打印 payload）
        try {
          decodeTransferProgressEvent(event.payload);
        } catch (reason) {
          warnTransferEventDecodeFailure('transfer:progress', reason);
          return prev;
        }
        console.warn('[transfer] skip transfer:progress: merge rejected');
        return prev;
      });
    });

    const unlistenStatuses = TRANSFER_STATUS_EVENTS.map((eventName) =>
      listen(eventName, (event) => {
        setTasks((prev) => {
          const next = mergeTransferStatusEvent(prev, event.payload);
          if (next != null) {
            return next;
          }
          try {
            decodeTransferStatusEvent(event.payload);
            // 结构合法但 status 非 TransferStatus 枚举 → fail-closed
            console.warn(
              `[transfer] skip ${eventName}: unknown status (contract=TransferStatusEvent)`,
            );
          } catch (reason) {
            warnTransferEventDecodeFailure(eventName, reason);
          }
          return prev;
        });
      }),
    );

    return () => {
      void unlistenProgress.then((fn) => fn());
      for (const pending of unlistenStatuses) {
        void pending.then((fn) => fn());
      }
    };
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户切换目标设备时更新下拉选中项。
   *
   * Code Logic（这个函数做什么）:
   *   写 selectedDeviceId。
   */
  const handleDeviceChange = useCallback((e: ChangeEvent<HTMLSelectElement>) => {
    setSelectedDeviceId(e.target.value);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   清理行级错误文案。
   *
   * Code Logic（这个函数做什么）:
   *   从 taskActionErrors 删除指定 taskId。
   */
  const clearTaskActionError = useCallback((taskId: string) => {
    setTaskActionErrors((prev) => {
      if (!(taskId in prev)) return prev;
      const next = { ...prev };
      delete next[taskId];
      return next;
    });
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   recovery 成功或对账终态后释放 clientOperationId 占用。
   *
   * Code Logic（这个函数做什么）:
   *   同步删 pendingRecoveriesRef 与 state。
   */
  const clearPendingRecovery = useCallback((taskId: string) => {
    if (!(taskId in pendingRecoveriesRef.current)) return;
    const next = { ...pendingRecoveriesRef.current };
    delete next[taskId];
    pendingRecoveriesRef.current = next;
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   进入/离开对账态时更新 reconciling 集合。
   *
   * Code Logic（这个函数做什么）:
   *   不可变 Set 更新。
   */
  const setReconciling = useCallback((taskId: string, on: boolean) => {
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
   *   用户确认发送后必须真实调用 send_transfer，成功后强制刷新任务列表（不得只 join 旧 poll）；
   *   双击不得在 re-render 前发出第二次 send；lost ACK 必须复用稳定 clientOperationId。
   *
   * Code Logic（这个函数做什么）:
   *   用 sendingRef 同步门闩 + sending 状态；按 device+path mint/复用 clientOperationId；
   *   await transferApi.send；成功清空选择与 pending intent 并 force runTasksNow；
   *   uncertain 保留 intent 并 getOperation 对账；definitive 失败保留选择；finally 释放门闩。
   */
  const handleSendClick = useCallback(async () => {
    if (!selectedFile || !selectedDeviceId || sendingRef.current) return;
    sendingRef.current = true;
    setSending(true);
    setSendError(null);

    const intentKey = `${selectedDeviceId}\0${selectedFile.path}`;
    const existing = pendingSendIntentRef.current;
    const clientOperationId =
      existing && existing.intentKey === intentKey
        ? existing.clientOperationId
        : mintTransferClientOperationId();
    pendingSendIntentRef.current = { intentKey, clientOperationId };

    try {
      await transferApi.send(selectedDeviceId, selectedFile.path, clientOperationId);
      pendingSendIntentRef.current = null;
      setSelectedFile(null);
      setSelectionNotice(null);
      await runTasksNow({ force: true });
    } catch (err) {
      if (isTransferOutcomeUncertain(err)) {
        // lost ACK：保留 clientOperationId，先对账再 force 刷新，禁止 mint 新 id 盲重放
        try {
          const maxAttempts = 12;
          const delayMs = 1500;
          for (let attempt = 0; attempt < maxAttempts; attempt += 1) {
            if (!mountedRef.current) return;
            const status = await transferApi.getOperation(clientOperationId);
            if (!mountedRef.current) return;
            if (status.status === 'pending') {
              if (attempt + 1 >= maxAttempts) break;
              await new Promise<void>((resolve) => {
                window.setTimeout(resolve, delayMs);
              });
              continue;
            }
            // 终态（succeeded/failed/notFound）清 intent 并刷新列表
            pendingSendIntentRef.current = null;
            await runTasksNow({ force: true });
            if (status.status === 'failed') {
              setSendError(t('transfer:sendFailed', { error: status.code }));
            } else if (status.status === 'succeeded') {
              setSelectedFile(null);
              setSelectionNotice(null);
            }
            return;
          }
          setSendError(
            t('transfer:sendFailed', {
              error: 'operation still pending; retry reconcile',
            }),
          );
        } catch (reconcileErr) {
          setSendError(t('transfer:sendFailed', { error: errorMessage(reconcileErr) }));
        }
        return;
      }
      // definitive 失败：清 intent，允许用户改文件/设备后重新 mint
      pendingSendIntentRef.current = null;
      setSendError(t('transfer:sendFailed', { error: errorMessage(err) }));
    } finally {
      sendingRef.current = false;
      setSending(false);
    }
  }, [selectedFile, selectedDeviceId, runTasksNow, t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户可取消 pending/transferring 任务，失败需行级提示且保留任务；
   *   双击不得在 re-render 前发出第二次 cancel。
   *
   * Code Logic（这个函数做什么）:
   *   用 cancellingIdsRef 同步门闩；await cancel；成功 force runTasksNow；失败写 taskActionErrors。
   */
  const handleCancelTask = useCallback(
    async (taskId: string) => {
      if (cancellingIdsRef.current.has(taskId)) return;
      cancellingIdsRef.current.add(taskId);
      setCancellingIds(new Set(cancellingIdsRef.current));
      clearTaskActionError(taskId);
      try {
        await transferApi.cancel(taskId);
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
   *   timeout/network 后先 getOperation 对账，终态才释放动作锁；pending 有界轮询直至收敛。
   *
   * Code Logic（这个函数做什么）:
   *   有界轮询 transferApi.getOperation；succeeded/failed/notFound 清 reconciling 并 force 刷新；
   *   pending 退避重试，耗尽后释放 reconciling 但保留 clientOperationId 供再次对账；
   *   查询失败保持 reconciling 并展示错误。
   */
  const reconcilePendingRecovery = useCallback(
    async (taskId: string, clientOperationId: string) => {
      setReconciling(taskId, true);
      const maxAttempts = 12;
      const delayMs = 1500;
      try {
        for (let attempt = 0; attempt < maxAttempts; attempt += 1) {
          if (!mountedRef.current) return;
          const status = await transferApi.getOperation(clientOperationId);
          if (!mountedRef.current) return;
          if (status.status === 'pending') {
            if (attempt + 1 >= maxAttempts) {
              // 耗尽后仍 pending：释放 reconciling、保留 clientOperationId，避免永久卡死
              setReconciling(taskId, false);
              setTaskActionErrors((prev) => ({
                ...prev,
                [taskId]: t('transfer:retryFailed', {
                  error: 'operation still pending; retry reconcile',
                }),
              }));
              return;
            }
            await new Promise<void>((resolve) => {
              window.setTimeout(resolve, delayMs);
            });
            continue;
          }
          // 终态（succeeded/failed/notFound）释放意图并刷新列表
          clearPendingRecovery(taskId);
          setReconciling(taskId, false);
          await runTasksNow({ force: true });
          if (status.status === 'failed') {
            setTaskActionErrors((prev) => ({
              ...prev,
              [taskId]: t('transfer:retryFailed', { error: status.code }),
            }));
          }
          return;
        }
      } catch (err) {
        if (!mountedRef.current) return;
        // 对账本身失败：仍保持 reconciling，仅展示错误
        setTaskActionErrors((prev) => ({
          ...prev,
          [taskId]: t('transfer:retryFailed', { error: errorMessage(err) }),
        }));
      }
    },
    [clearPendingRecovery, runTasksNow, setReconciling, t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户对失败任务点「继续传输」/「重新传输」时必须带稳定 clientOperationId；
   *   uncertain 不得 mint 新 id 盲重放。
   *
   * Code Logic（这个函数做什么）:
   *   复用 pendingRecoveries 中同 kind 的 id，否则 mint；调用 retry/resume；
   *   成功清 pending 并 force 刷新；uncertain → reconciling + getOperation。
   */
  const handleRecovery = useCallback(
    async (taskId: string, kind: RecoveryKind) => {
      if (recoveryBusyRef.current.has(taskId)) return;
      if (reconcilingIds.has(taskId)) return;

      const target = tasks.find((item) => item.id === taskId);
      // 同 logical 已有 child 活跃/对账时禁止再 mint 新 clientOperationId
      if (
        target &&
        isLogicalTransferRecoveryLocked(target, tasks, reconcilingIds)
      ) {
        return;
      }

      recoveryBusyRef.current.add(taskId);
      clearTaskActionError(taskId);

      const existing = pendingRecoveriesRef.current[taskId];
      const clientOperationId =
        existing && existing.kind === kind
          ? existing.clientOperationId
          : mintTransferClientOperationId();
      const nextPending: PendingRecovery = { clientOperationId, kind };
      pendingRecoveriesRef.current = {
        ...pendingRecoveriesRef.current,
        [taskId]: nextPending,
      };

      try {
        if (kind === 'resume') {
          await transferApi.resume(taskId, clientOperationId);
        } else {
          await transferApi.retry(taskId, clientOperationId);
        }
        if (!mountedRef.current) return;
        clearPendingRecovery(taskId);
        setReconciling(taskId, false);
        await runTasksNow({ force: true });
      } catch (err) {
        if (!mountedRef.current) return;
        if (isTransferOutcomeUncertain(err)) {
          // N3：uncertain → 正在确认结果，不 blind retry
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
   *   本机已接收 completed 任务可打开文件。
   *
   * Code Logic（这个函数做什么）:
   *   await transferApi.open；失败写行级错误。
   */
  const handleOpenTask = useCallback(
    async (taskId: string) => {
      clearTaskActionError(taskId);
      try {
        await transferApi.open(taskId);
      } catch (err) {
        setTaskActionErrors((prev) => ({
          ...prev,
          [taskId]: t('transfer:openFailed', { error: errorMessage(err) }),
        }));
      }
    },
    [clearTaskActionError, t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   本机已接收 completed 任务可在文件夹中显示。
   *
   * Code Logic（这个函数做什么）:
   *   await transferApi.reveal；失败写行级错误。
   */
  const handleRevealTask = useCallback(
    async (taskId: string) => {
      clearTaskActionError(taskId);
      try {
        await transferApi.reveal(taskId);
      } catch (err) {
        setTaskActionErrors((prev) => ({
          ...prev,
          [taskId]: t('transfer:revealFailed', { error: errorMessage(err) }),
        }));
      }
    },
    [clearTaskActionError, t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   分区列表需要按合法动作矩阵组装 TransferItem props。
   *
   * Code Logic（这个函数做什么）:
   *   根据 status/resumable/retryable/receive completed 选择性传入回调。
   */
  const renderTaskRow = useCallback(
    (task: TransferTask) => {
      const reconciling = reconcilingIds.has(task.id);
      const recoveryLocked = isLogicalTransferRecoveryLocked(task, tasks, reconcilingIds);
      const canCancel = task.status === 'pending' || task.status === 'transferring';
      const peer = task.peerDeviceId
        ? devices.find((d) => d.id === task.peerDeviceId)
        : undefined;
      const peerSupportsResume = deviceSupportsTransferResume(peer);
      // 同 logical 活跃/对账期间禁用旧 failed 行的 resume/retry
      const canResume =
        !reconciling &&
        !recoveryLocked &&
        isTransferResumable(task, peerSupportsResume);
      const canRetry =
        !reconciling &&
        !recoveryLocked &&
        isTransferRetryable(task, peerSupportsResume);
      const canOpenReveal = !reconciling && canOpenRevealTransfer(task);
      const actionError = taskActionErrors[task.id];
      return (
        <li key={task.id} className={styles.taskRow}>
          <TransferItem
            task={{
              id: task.id,
              fileName: task.fileName,
              fileSize: task.fileSize,
              direction: task.direction,
              status: task.status,
              progress: task.progress,
              peerDevice: task.peerDeviceName,
              speed: task.speed,
              errorMessage: task.failure?.message ?? task.errorMessage,
              phase: task.phase,
              reconciling,
            }}
            onCancel={canCancel ? () => void handleCancelTask(task.id) : undefined}
            onResume={canResume ? () => void handleRecovery(task.id, 'resume') : undefined}
            onRetry={canRetry ? () => void handleRecovery(task.id, 'retry') : undefined}
            onOpen={canOpenReveal ? () => void handleOpenTask(task.id) : undefined}
            onReveal={canOpenReveal ? () => void handleRevealTask(task.id) : undefined}
            cancelling={cancellingIds.has(task.id)}
          />
          {actionError ? (
            <p className={styles.rowAlert} role="alert">
              {actionError}
            </p>
          ) : null}
        </li>
      );
    },
    [
      cancellingIds,
      devices,
      handleCancelTask,
      handleOpenTask,
      handleRecovery,
      handleRevealTask,
      reconcilingIds,
      taskActionErrors,
      tasks,
    ],
  );

  const dropzoneClasses = [styles.dropzone, isDragOver ? styles.dropzoneOver : '']
    .filter(Boolean)
    .join(' ');

  const pickedName = selectedFile?.name ?? null;
  const canSend = Boolean(selectedFile && selectedDeviceId) && !sending;

  return (
    <div className={styles.page}>
      {/* 页面头部 */}
      <header className={styles.pageHeader}>
        <span className={styles.eyebrow}>{t('transfer:eyebrow')}</span>
        <h1 className={styles.title}>{t('transfer:title')}</h1>
        <p className={styles.lead}>{t('transfer:lead')}</p>
      </header>

      {/* 发送区 */}
      <Card variant="elevated" className={styles.sendCard}>
        <div className={styles.sendTop}>
          <label className={styles.field}>
            <span className={styles.fieldLabel}>{t('transfer:fieldLabel')}</span>
            <div className={styles.selectWrap}>
              <select
                className={styles.select}
                value={selectedDeviceId}
                onChange={handleDeviceChange}
                aria-label={t('transfer:selectDevice')}
                disabled={devicesState === 'loading' && devices.length === 0}
              >
                {devicesState === 'loading' && devices.length === 0 ? (
                  <option value="">{t('transfer:loading')}</option>
                ) : devices.length === 0 ? (
                  <option value="">{t('transfer:noDevices')}</option>
                ) : (
                  devices.map((d) => (
                    <option key={d.id} value={d.id}>
                      {d.name} · {d.address}:{d.port}
                    </option>
                  ))
                )}
              </select>
              <span className={styles.selectArrow} aria-hidden="true">
                ▾
              </span>
            </div>
          </label>

          <div className={styles.pickerCol}>
            <Button
              variant="primary"
              size="md"
              icon={<SendIcon />}
              onClick={() => void handleSendClick()}
              disabled={!canSend}
              loading={sending}
              aria-busy={sending || undefined}
            >
              {pickedName
                ? t('transfer:sendFile', { file: pickedName })
                : t('transfer:pickFile')}
            </Button>
            <Button
              variant="secondary"
              size="md"
              onClick={() => void handlePickClick()}
              disabled={sending}
            >
              {t('transfer:browse')}
            </Button>
          </div>
        </div>

        <div
          className={dropzoneClasses}
          onDragOver={(e) => {
            e.preventDefault();
            e.stopPropagation();
            setIsDragOver(true);
          }}
          onDragLeave={(e) => {
            e.preventDefault();
            e.stopPropagation();
            setIsDragOver(false);
          }}
          onDrop={(e) => {
            // 浏览器 HTML5 drop 不提供原生绝对路径；真实路径来自 Tauri onDragDropEvent。
            e.preventDefault();
            e.stopPropagation();
            setIsDragOver(false);
          }}
          onClick={() => void handlePickClick()}
          onKeyDown={handleDropzoneKeyDown}
          role="button"
          tabIndex={0}
          aria-label={t('transfer:dropAria')}
        >
          <span className={styles.dropIcon} aria-hidden="true">
            <UploadIcon size={20} />
          </span>
          <p className={styles.dropTitle}>
            {pickedName
              ? t('transfer:dropTitlePicked', { file: pickedName })
              : t('transfer:dropTitleEmpty')}
          </p>
          <p className={styles.dropHint}>{t('transfer:chunkHint')}</p>
        </div>

        {selectionNotice ? (
          <p className={styles.notice} role="alert">
            {selectionNotice}
          </p>
        ) : null}

        {sendError ? (
          <p className={styles.alert} role="alert">
            {sendError}
          </p>
        ) : null}

        {devicesState === 'error' ? (
          <p className={styles.notice} role="status">
            {devicesError}{' '}
            <Button variant="secondary" size="sm" onClick={() => void loadDevices()}>
              {t('common:action.retry')}
            </Button>
          </p>
        ) : null}
      </Card>

      {/* 任务列表 */}
      <section className={styles.tasksSection}>
        <div className={styles.sectionHead}>
          <h2 className={styles.sectionTitle}>
            {t('transfer:tasksTitle')}{' '}
            <span className={styles.sectionCount}>({tasks.length})</span>
          </h2>
          <div className={styles.statusPills}>
            <Pill tone="accent" dot>
              {t('transfer:active', { n: statusCounts.active })}
            </Pill>
            <Pill tone="success">
              {t('transfer:completed', { n: statusCounts.completed })}
            </Pill>
            <Pill tone="danger">{t('transfer:failed', { n: statusCounts.failed })}</Pill>
          </div>
        </div>

        {tasksState === 'loading' && tasks.length === 0 ? (
          <TaskListSkeleton />
        ) : tasks.length === 0 ? (
          <div className={styles.empty}>
            <p>{t('transfer:empty')}</p>
            <p className={styles.emptyHint}>{t('transfer:emptyHint')}</p>
          </div>
        ) : (
          <div className={styles.groupStack}>
            {groupedTasks.active.length > 0 ? (
              <section className={styles.taskGroup} aria-label={t('transfer:groupActive')}>
                <h3 className={styles.groupTitle}>{t('transfer:groupActive')}</h3>
                <ul className={styles.taskList}>{groupedTasks.active.map(renderTaskRow)}</ul>
              </section>
            ) : null}
            {groupedTasks.needsAttention.length > 0 ? (
              <section
                className={styles.taskGroup}
                aria-label={t('transfer:groupNeedsAttention')}
              >
                <h3 className={styles.groupTitle}>{t('transfer:groupNeedsAttention')}</h3>
                <ul className={styles.taskList}>
                  {groupedTasks.needsAttention.map(renderTaskRow)}
                </ul>
              </section>
            ) : null}
            {groupedTasks.completed.length > 0 ? (
              <section className={styles.taskGroup} aria-label={t('transfer:groupCompleted')}>
                <h3 className={styles.groupTitle}>{t('transfer:groupCompleted')}</h3>
                <ul className={styles.taskList}>{groupedTasks.completed.map(renderTaskRow)}</ul>
              </section>
            ) : null}
          </div>
        )}

        {tasksState === 'error' ? (
          <p className={styles.notice} role="status">
            {tasksError}{' '}
            <Button variant="secondary" size="sm" onClick={() => void loadTasks()}>
              {t('common:action.retry')}
            </Button>
          </p>
        ) : null}
      </section>
    </div>
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   首屏任务加载时需要骨架屏，避免空白闪烁。
 *
 * Code Logic（这个函数做什么）:
 *   渲染三条静态骨架行，aria-busy=true。
 */
function TaskListSkeleton() {
  const { t } = useTranslation(['transfer']);
  return (
    <ul className={styles.taskList} aria-busy="true" aria-label={t('transfer:skeletonAria')}>
      {[0, 1, 2].map((i) => (
        <li key={i} className={styles.skeletonRow}>
          <span
            className={styles.skeletonBlock}
            style={{ width: 32, height: 32, borderRadius: 'var(--radius-md)' }}
          />
          <span className={styles.skeletonLines}>
            <span className={styles.skeletonBlock} style={{ width: '40%', height: 12 }} />
            <span className={styles.skeletonBlock} style={{ width: '60%', height: 10 }} />
          </span>
        </li>
      ))}
    </ul>
  );
}
