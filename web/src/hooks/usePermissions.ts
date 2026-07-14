/**
 * usePermissions - macOS 权限状态轮询与逐项请求
 *
 * Business Logic（为什么需要这个 hook）:
 *   Welcome 引导页、设置页权限管理与侧栏授权徽标都需要：持续获取屏幕录制/辅助功能/
 *   输入监控/通知权限状态，并在用户点击「去设置」时触发单项授权。首轮失败必须结束
 *   loading 并给出可重试错误，避免永久「检查中」；刷新失败保留旧状态。
 *
 * Code Logic（这个 hook 做什么）:
 *   - 基于 useVisibilityPolling 每 2s 拉取 configApi.permissions + 通知权限；
 *     页面隐藏暂停，恢复可见立即刷新，single-flight 防重叠
 *   - stopWhenGranted=true 时，required 三项全部授权后停止轮询（Welcome 用）
 *   - loading 仅表示从未拿到过首轮结果；refreshing 表示已有状态的后台刷新
 *   - request(type) 只请求一个权限，同 type 并发合并为同一 Promise；requestMissing 顺序请求
 *   - allRequiredGranted / allGranted 只看 screenCapture/accessibility/inputMonitoring
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import { configApi } from '@/api/config';
import { checkNotificationGranted, requestNotificationPermission } from '@/lib/notification';
import type { PermissionType, PermissionsStatus } from '@/lib/types';
import { useVisibilityPolling } from './useVisibilityPolling';

/** localStorage key：标记已完成首次权限引导，避免每次启动都跳 Welcome */
export const PERMISSION_ONBOARDED_KEY = 'cp-permission-onboarded';

const POLL_INTERVAL_MS = 2000;

/**
 * Business Logic（为什么需要这个函数）:
 *   错误对象形态不统一，UI 需要稳定可读文案。
 *
 * Code Logic（这个函数做什么）:
 *   Error 取 message，其余 String()。
 */
function toErrorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   引导完成条件只看三项 TCC 权限，通知不阻塞 onboarding。
 *
 * Code Logic（这个函数做什么）:
 *   当 status 存在且三项 granted 均为 true 时返回 true。
 */
function isRequiredGranted(status: PermissionsStatus | null): boolean {
  return (
    !!status &&
    status.screenCapture.granted &&
    status.accessibility.granted &&
    status.inputMonitoring.granted
  );
}

export interface UsePermissionsResult {
  status: PermissionsStatus | null;
  /** 首轮结果尚未结束（成功或失败） */
  loading: boolean;
  /** 已有状态时的后台刷新中 */
  refreshing: boolean;
  /** 最近一次刷新或请求失败文案；成功刷新后清空 */
  error: string | null;
  /** 正在请求中的权限类型集合 */
  requesting: ReadonlySet<PermissionType>;
  /**
   * 三项 TCC 权限是否已全部授权（与 allRequiredGranted 同义，保留兼容徽标/旧调用方）。
   * 不含 notification。
   */
  allGranted: boolean;
  /** 三项 TCC 权限是否已全部授权；notification 不计入 */
  allRequiredGranted: boolean;
  /**
   * 请求单项权限；同 type 并发调用复用同一 Promise。
   * @param type 权限类型
   * @param openSettings 是否打开系统设置面板（仅 TCC 路径透传）
   */
  request: (type: PermissionType, openSettings?: boolean) => Promise<void>;
  /**
   * 顺序请求所有未授权权限（侧栏徽标批量入口仍需要）。
   * 单项失败不中断后续项。
   */
  requestMissing: () => Promise<void>;
  /** 手动触发一次权限状态刷新（single-flight） */
  refresh: () => Promise<void>;
}

/**
 * 权限状态轮询与逐项请求 hook
 *
 * @param options.stopWhenGranted 全部 required 授权后停止轮询（Welcome 页用 true）
 * @returns UsePermissionsResult
 */
export function usePermissions(
  options: { stopWhenGranted?: boolean } = {},
): UsePermissionsResult {
  const { stopWhenGranted = false } = options;
  const [status, setStatus] = useState<PermissionsStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [requesting, setRequesting] = useState<ReadonlySet<PermissionType>>(() => new Set());

  const statusRef = useRef<PermissionsStatus | null>(null);
  const requestingPromisesRef = useRef<Map<PermissionType, Promise<void>>>(new Map());
  const mountedRef = useRef(true);

  // 在 effect 中同步 ref，避免 render 期间写 ref（react-hooks/refs 规则）
  useEffect(() => {
    statusRef.current = status;
  }, [status]);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   轮询与手动刷新共用同一拉取逻辑，首轮失败要结束 loading，后续失败保留旧状态。
   *
   * Code Logic（这个函数做什么）:
   *   并行拉取 TCC + 通知权限；成功写 status 并清 error；失败写 error 且不覆盖旧 status；
   *   finally 结束 loading/refreshing；卸载后不 setState。
   */
  const loadStatus = useCallback(async () => {
    const hadStatus = statusRef.current !== null;
    if (hadStatus && mountedRef.current) {
      setRefreshing(true);
    }
    try {
      const [tcc, notifyGranted] = await Promise.all([
        configApi.permissions(),
        checkNotificationGranted(),
      ]);
      if (!mountedRef.current) return;
      const next: PermissionsStatus = {
        ...tcc,
        notification: { granted: notifyGranted },
      };
      statusRef.current = next;
      setStatus(next);
      setError(null);
    } catch (err) {
      if (!mountedRef.current) return;
      setError(toErrorMessage(err));
      // 已有状态时不覆盖 status，保留 stale 投影
    } finally {
      if (mountedRef.current) {
        setLoading(false);
        setRefreshing(false);
      }
    }
  }, []);

  const allRequiredGranted = isRequiredGranted(status);
  const pollingEnabled = !(stopWhenGranted && allRequiredGranted);

  const { runNow } = useVisibilityPolling(loadStatus, {
    intervalMs: POLL_INTERVAL_MS,
    enabled: pollingEnabled,
    runImmediately: true,
  });

  /**
   * Business Logic（为什么需要这个函数）:
   *   UI「重新检查」与 request 完成后需要立即对齐后端状态。
   *
   * Code Logic（这个函数做什么）:
   *   委托 runNow 做 single-flight 刷新。
   */
  const refresh = useCallback(async () => {
    await runNow({ force: true });
  }, [runNow]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户对单项权限点「去设置」时，只应触发该项授权，且重复点击不得并行弹多次。
   *
   * Code Logic（这个函数做什么）:
   *   同 type 返回 in-flight Promise；notification 走 JS API，其余走 requestPermission；
   *   结束后 runNow 刷新；请求失败写 error 并 rethrow。
   */
  const request = useCallback(
    (type: PermissionType, openSettings?: boolean): Promise<void> => {
      // 注意：外层不可标 async，否则 return existing 会被再包一层 Promise，破坏去重。
      const existing = requestingPromisesRef.current.get(type);
      if (existing) {
        return existing;
      }

      const promise = (async () => {
        setRequesting((prev) => {
          const next = new Set(prev);
          next.add(type);
          return next;
        });
        try {
          if (type === 'notification') {
            await requestNotificationPermission();
          } else {
            await configApi.requestPermission(type, openSettings);
          }
          await runNow({ force: true });
        } catch (err) {
          if (mountedRef.current) {
            setError(toErrorMessage(err));
          }
          throw err;
        } finally {
          requestingPromisesRef.current.delete(type);
          if (mountedRef.current) {
            setRequesting((prev) => {
              const next = new Set(prev);
              next.delete(type);
              return next;
            });
          }
        }
      })();

      requestingPromisesRef.current.set(type, promise);
      return promise;
    },
    [runNow],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   侧栏徽标仍提供「一次点开请求所有未授权项」的批量入口。
   *
   * Code Logic（这个函数做什么）:
   *   按固定顺序对未授权项 await request；单项失败继续后续项。
   */
  const requestMissing = useCallback(async () => {
    const current = statusRef.current;
    if (!current) return;

    const types: PermissionType[] = [];
    if (!current.screenCapture.granted) types.push('screenCapture');
    if (!current.accessibility.granted) types.push('accessibility');
    if (!current.inputMonitoring.granted) types.push('inputMonitoring');
    if (!current.notification.granted) types.push('notification');

    for (const type of types) {
      try {
        await request(type);
      } catch {
        // 顺序继续，不因单项失败中断
      }
    }
  }, [request]);

  return {
    status,
    loading,
    refreshing,
    error,
    requesting,
    allGranted: allRequiredGranted,
    allRequiredGranted,
    request,
    requestMissing,
    refresh,
  };
}
