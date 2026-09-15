/**
 * useWorkbenchFreshRestart — 设备级「全新启动连接」弹窗共享状态 hook。
 *
 * Business Logic（为什么需要这个 hook）:
 *   Workbench 页面（状态卡入口，针对当前 active project）与侧栏
 *   WorkbenchProjectRail（悬停入口，针对悬停的项目）共用同一套
 *   preview → confirm → result 弹窗状态机；抽成窄 hook 避免两处复制。
 *
 * Code Logic（这个 hook 做什么）:
 *   - openFreshRestartDialog(target)：按 target.deviceId（remote → 设备 id；
 *     local/缺省 → undefined=本机）打开弹窗并自动 preview；
 *   - confirmFreshRestart(includeForeignSessions)：busy 双锁执行 execute，
 *     成功（含降级结果）后回调 onCompleted 供调用方刷新统计/session 列表；
 *   - 传输失败保留 error 供弹窗内重试，不回调 onCompleted。
 */

import { useCallback, useState } from 'react';

import { workbenchApi } from '@/api/workbench';
import type {
  WorkbenchFreshRestartPreview,
  WorkbenchFreshRestartResult,
} from '@/lib/types';

/** open 时的目标设备：remote 传对端 deviceId；本机传 undefined/null。 */
export interface WorkbenchFreshRestartTarget {
  deviceId?: string | null;
}

/** 弹窗状态切片（WorkbenchFreshRestartDialog 的 props 子集）。 */
export interface WorkbenchFreshRestartDialogState {
  open: boolean;
  busy: boolean;
  previewing: boolean;
  preview: WorkbenchFreshRestartPreview | null;
  result: WorkbenchFreshRestartResult | null;
  error: string | null;
}

export interface UseWorkbenchFreshRestartResult {
  dialog: WorkbenchFreshRestartDialogState;
  openFreshRestartDialog: (target: WorkbenchFreshRestartTarget) => void;
  closeFreshRestartDialog: () => void;
  confirmFreshRestart: (includeForeignSessions: boolean) => Promise<void>;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   invoke 失败时需要人类可读 message 写入弹窗错误态。
 *
 * Code Logic（这个函数做什么）:
 *   从 unknown 提取 Error.message / string，否则 fallback。
 */
function freshRestartErrorMessage(error: unknown, fallback: string): string {
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === 'string' && error) return error;
  return fallback;
}

/**
 * Business Logic（为什么是导出 hook）:
 *   两个入口（Workbench 页面 / 侧栏 Rail）共享同一弹窗状态机，避免复制；
 *   调用方各自持有实例（页面一份、Rail 一份），状态互不干扰。
 *
 * Code Logic（这个 hook 做什么）:
 *   见模块头注释；onCompleted 在 execute 成功返回（含降级结果）后触发一次。
 */
export function useWorkbenchFreshRestart(params: {
  onCompleted?: () => void;
}): UseWorkbenchFreshRestartResult {
  const { onCompleted } = params;
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [previewing, setPreviewing] = useState(false);
  const [preview, setPreview] = useState<WorkbenchFreshRestartPreview | null>(null);
  const [result, setResult] = useState<WorkbenchFreshRestartResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [deviceId, setDeviceId] = useState<string | null>(null);

  /**
   * Business Logic（为什么需要这个函数）:
   *   打开弹窗时按目标解析设备并自动预检影响面，让用户在确认前看到准确数字。
   *
   * Code Logic（这个函数做什么）:
   *   remote → target.deviceId；local/缺省 → undefined（本机）；置 open 并拉 preview。
   */
  const openFreshRestartDialog = useCallback((target: WorkbenchFreshRestartTarget) => {
    const resolvedDeviceId = target.deviceId?.trim() || null;
    setDeviceId(resolvedDeviceId);
    setOpen(true);
    setResult(null);
    setError(null);
    setPreview(null);
    setPreviewing(true);
    workbenchApi.freshRestart
      .preview(resolvedDeviceId ?? undefined)
      .then((nextPreview) => {
        setPreview(nextPreview);
      })
      .catch((nextError: unknown) => {
        setError(freshRestartErrorMessage(nextError, '全新启动连接预检失败'));
      })
      .finally(() => {
        setPreviewing(false);
      });
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   执行期间禁止关闭弹窗/重复触发（设备级单飞，后端也有 barrier）。
   *
   * Code Logic（这个函数做什么）:
   *   busy 时直接返回；关闭并清理全部弹窗状态。
   */
  const closeFreshRestartDialog = useCallback(() => {
    if (busy) return;
    setOpen(false);
    setPreview(null);
    setResult(null);
    setError(null);
  }, [busy]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户确认后执行设备级全新启动；成功（含降级结果）回调 onCompleted，
   *   传输失败保留弹窗并展示错误供重试。
   *
   * Code Logic（这个函数做什么）:
   *   busy 双锁 → execute → 写 result → 成功回调 onCompleted。
   */
  const confirmFreshRestart = useCallback(
    async (includeForeignSessions: boolean) => {
      if (busy) return;
      setBusy(true);
      setError(null);
      try {
        const nextResult = await workbenchApi.freshRestart.execute(
          deviceId ?? undefined,
          includeForeignSessions,
        );
        setResult(nextResult);
        onCompleted?.();
      } catch (executeError) {
        setError(freshRestartErrorMessage(executeError, '全新启动连接执行失败'));
      } finally {
        setBusy(false);
      }
    },
    [busy, deviceId, onCompleted],
  );

  return {
    dialog: { open, busy, previewing, preview, result, error },
    openFreshRestartDialog,
    closeFreshRestartDialog,
    confirmFreshRestart,
  };
}
