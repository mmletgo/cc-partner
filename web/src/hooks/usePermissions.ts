/**
 * usePermissions - macOS 权限状态轮询与逐项请求
 *
 * Business Logic（为什么需要这个 hook）:
 *   Welcome 引导页、设置页权限管理与侧栏授权徽标都需要：持续获取屏幕录制/辅助功能/
 *   通知权限状态，并在用户点击对应动作时分别 Request 或打开设置。首轮失败必须结束
 *   loading 并给出可重试错误，避免永久「检查中」；刷新失败保留旧状态。
 *
 * Code Logic（这个 hook 做什么）:
 *   - 基于 useVisibilityPolling 每 2s 拉取 configApi.permissions（四项含真实通知状态）；
 *     页面隐藏暂停，恢复可见立即刷新，single-flight 防重叠
 *   - stopWhenGranted=true 时，产品展示的三项权限全部授权后停止轮询（Welcome 用）
 *   - loading 仅表示从未拿到过首轮结果；refreshing 表示已有状态的后台刷新
 *   - request(type) 只在用户点击时请求，同 type 并发合并；**禁止**挂载时自动 request
 *   - allRequiredGranted / allGranted 看 screenCapture/accessibility/notification
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import { configApi } from '@/api/config';
import type {
  PermissionActionResult,
  PermissionType,
  PermissionsStatus,
} from '@/lib/types';
import { useVisibilityPolling } from './useVisibilityPolling';

/** 发布版 localStorage key：标记权限引导已完成（全部 required 已授权）。 */
export const PERMISSION_ONBOARDED_KEY = 'cp-permission-onboarded';
/** 发布版：用户点了「暂时跳过」。 */
export const PERMISSION_SKIPPED_KEY = 'cp-permission-skipped';

/** 应用发行通道（与 Rust `AppFlavor` 对齐）。 */
export type AppFlavor = 'dev' | 'release';

/**
 * Business Logic（为什么需要这个函数）:
 *   开发壳与发布版必须使用不同的 onboarding 标记，避免互相跳过 Welcome。
 *
 * Code Logic（这个函数做什么）:
 *   release → `cp-permission-onboarded`；dev → `cp-permission-onboarded.dev`。
 */
export function permissionOnboardedKey(flavor: AppFlavor = 'release'): string {
  return flavor === 'dev' ? `${PERMISSION_ONBOARDED_KEY}.dev` : PERMISSION_ONBOARDED_KEY;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   「暂时跳过」与「已全部授权」分开记账，缺权限时仍应进 Welcome，除非用户明确跳过。
 *
 * Code Logic（这个函数做什么）:
 *   release → `cp-permission-skipped`；dev → `cp-permission-skipped.dev`。
 */
export function permissionSkippedKey(flavor: AppFlavor = 'release'): string {
  return flavor === 'dev' ? `${PERMISSION_SKIPPED_KEY}.dev` : PERMISSION_SKIPPED_KEY;
}

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
 *   引导完成条件只包含产品实际展示的屏幕录制、辅助功能与通知。
 *
 * Code Logic（这个函数做什么）:
 *   当 status 存在且三项 granted 均为 true 时返回 true；输入监控字段仅作后端协议兼容。
 */
function isRequiredGranted(status: PermissionsStatus | null): boolean {
  return (
    !!status &&
    status.screenCapture.granted &&
    status.accessibility.granted &&
    status.notification.granted
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
   * 三项引导权限是否已全部授权（与 allRequiredGranted 同义，保留兼容徽标/旧调用方）。
   * 含 notification。
   */
  allGranted: boolean;
  /** 三项引导权限是否已全部授权（屏幕录制 + 辅助功能 + 通知） */
  allRequiredGranted: boolean;
  /**
   * 请求单项权限；同 type 并发调用复用同一 Promise。
   * @param type 权限类型
   * Request 与 Open Settings 是两条独立入口；本函数只触发公开 Request API。
   * @returns 后端权限操作结果
   */
  request: (type: PermissionType) => Promise<PermissionActionResult>;
  /** 显式打开单项系统设置；与 Request 使用不同 IPC。 */
  openSettings: (type: PermissionType) => Promise<PermissionActionResult>;
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
  const requestingPromisesRef = useRef<
    Map<PermissionType, Promise<PermissionActionResult>>
  >(new Map());
  const settingsPromisesRef = useRef<Map<PermissionType, Promise<PermissionActionResult>>>(
    new Map(),
  );
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
   *   一次 check_permissions 拉取四项（通知按 Bundle 身份独立，非 plugin stub）；
   *   成功写 status 并清 error；失败写 error 且不覆盖旧 status；
   *   finally 结束 loading/refreshing；卸载后不 setState。仅查询、不 request。
   */
  const loadStatus = useCallback(async () => {
    const hadStatus = statusRef.current !== null;
    if (hadStatus && mountedRef.current) {
      setRefreshing(true);
    }
    try {
      const next = await configApi.permissions();
      if (!mountedRef.current) return;
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
   *   用户对单项权限点“请求授权”时，只应触发公开 Request，且重复点击不得并行弹多次。
   *   进入 Welcome **不得**自动调用（辅助功能等禁止自动弹系统框）。
   *   这里不得重置 TCC、打开设置或重启。
   *
   * Code Logic（这个函数做什么）:
   *   同 type 返回 in-flight Promise；展示权限均走 requestPermission（含 notification）；
   *   结束后 runNow 刷新；请求失败写 error 并 rethrow；成功返回 PermissionActionResult。
   */
  const request = useCallback(
    (type: PermissionType): Promise<PermissionActionResult> => {
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
          const result = await configApi.requestPermission(type);
          await runNow({ force: true });
          return result;
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
   *   Denied 权限必须由用户显式打开设置，不得复用 Request 或批量触发副作用。
   *
   * Code Logic（这个函数做什么）:
   *   同 type 的设置跳转合并为一个 Promise；结束后强制刷新状态。
   */
  const openSettings = useCallback(
    (type: PermissionType): Promise<PermissionActionResult> => {
      const existing = settingsPromisesRef.current.get(type);
      if (existing) return existing;

      const promise = (async () => {
        setRequesting((prev) => new Set(prev).add(type));
        try {
          const result = await configApi.openPermissionSettings(type);
          await runNow({ force: true });
          return result;
        } catch (err) {
          if (mountedRef.current) setError(toErrorMessage(err));
          throw err;
        } finally {
          settingsPromisesRef.current.delete(type);
          if (mountedRef.current) {
            setRequesting((prev) => {
              const next = new Set(prev);
              next.delete(type);
              return next;
            });
          }
        }
      })();

      settingsPromisesRef.current.set(type, promise);
      return promise;
    },
    [runNow],
  );

  return {
    status,
    loading,
    refreshing,
    error,
    requesting,
    allGranted: allRequiredGranted,
    allRequiredGranted,
    request,
    openSettings,
    refresh,
  };
}
