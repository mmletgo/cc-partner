/**
 * useBattery — 充电快照、焦点扣时上报与入账 toast。
 *
 * Business Logic（为什么需要这个 hook）:
 *   主窗/卫星必须共用后端权威余额；任一前台工作台窗只扣一份墙钟。
 *
 * Code Logic（这个 hook 做什么）:
 *   拉快照、听 battery:changed、按 visible+focused+pathname 上报 report_battery_focus，
 *   2s 可见轮询兜底；creditMinutes 上升沿推入 toast。
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import { useLocation } from 'react-router-dom';

import { batteryApi } from '@/api/battery';
import { useVisibilityPolling } from '@/hooks/useVisibilityPolling';
import { readCurrentWindowLabel } from '@/hooks/useWorkbenchWindowRole';
import type { BatteryCreditSource, BatteryMode, BatterySnapshot } from '@/lib/types/battery';

export const BATTERY_CHANGE_EVENT = 'battery:changed';

const CONSUMING_PATHS = new Set(['/workbench', '/attention']);

type TauriInternalsWindow = Window & {
  __TAURI_INTERNALS__?: { transformCallback?: unknown };
};

/**
 * Business Logic（为什么需要这个函数）:
 *   普通 Vite / 移动浏览器没有 Tauri internals，不能 listen。
 *
 * Code Logic（这个函数做什么）:
 *   检测 transformCallback 是否为函数。
 */
function canUseTauriEvents(): boolean {
  if (typeof window === 'undefined') return false;
  const internals = (window as TauriInternalsWindow).__TAURI_INTERNALS__;
  return typeof internals?.transformCallback === 'function';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   只有工作台与 Inbox 前台才消耗；健康/游戏/失焦不扣。
 *
 * Code Logic（这个函数做什么）:
 *   pathname 取首段；visible && focused && 消耗路由。
 */
export function isBatteryConsumingRoute(
  pathname: string,
  visible: boolean,
  focused: boolean,
): boolean {
  if (!visible || !focused) return false;
  const path = pathname.split('?')[0] ?? pathname;
  if (path === '/workbench' || path.startsWith('/workbench/')) return true;
  if (path === '/attention' || path.startsWith('/attention/')) return true;
  return CONSUMING_PATHS.has(path);
}

export interface BatteryCreditToast {
  id: number;
  minutes: number;
  source?: BatteryCreditSource;
}

export interface UseBatteryResult {
  snapshot: BatterySnapshot | null;
  loading: boolean;
  error: string | null;
  toast: BatteryCreditToast | null;
  setMode: (mode: BatteryMode) => Promise<void>;
  dismissToast: () => void;
}

/**
 * Business Logic（为什么需要这个 hook）:
 *   footer 环、遮罩、设置开关都读同一份快照，且每个窗各自上报焦点。
 *
 * Code Logic（这个 hook 做什么）:
 *   见文件头；hooks 全部无条件调用。
 */
export function useBattery(): UseBatteryResult {
  const location = useLocation();
  const windowLabelRef = useRef(readCurrentWindowLabel());
  const [snapshot, setSnapshot] = useState<BatterySnapshot | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [toast, setToast] = useState<BatteryCreditToast | null>(null);
  const toastSeq = useRef(0);
  const lastCreditKey = useRef<string | null>(null);
  const lastConsuming = useRef<boolean | null>(null);

  /**
   * Business Logic（为什么需要这个函数）:
   *   入账后环要涨，并在侧栏旁弹出 +Xm。
   *
   * Code Logic（这个函数做什么）:
   *   用 remaining+creditMinutes+source 去重后写 toast。
   */
  const applySnapshot = useCallback((next: BatterySnapshot): void => {
    setSnapshot(next);
    setError(null);
    if (next.creditMinutes && next.creditMinutes > 0) {
      const key = `${next.remainingMs}:${next.creditMinutes}:${next.creditSource ?? ''}`;
      if (lastCreditKey.current !== key) {
        lastCreditKey.current = key;
        toastSeq.current += 1;
        setToast({
          id: toastSeq.current,
          minutes: next.creditMinutes,
          source: next.creditSource,
        });
      }
    }
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   首屏与轮询兜底都要读权威快照。
   *
   * Code Logic（这个函数做什么）:
   *   getSnapshot；失败保留旧快照。
   */
  const refresh = useCallback(async (): Promise<void> => {
    try {
      const next = await batteryApi.getSnapshot();
      applySnapshot(next);
    } catch (reason) {
      const message = reason instanceof Error ? reason.message : String(reason);
      setError(message);
    } finally {
      setLoading(false);
    }
  }, [applySnapshot]);

  useVisibilityPolling(refresh, {
    intervalMs: 3000,
    enabled: true,
    runImmediately: true,
    refreshOnVisible: true,
  });

  /**
   * Business Logic（为什么需要这个函数）:
   *   入账 / 模式 / 结算后后端 emit battery:changed，轮询不够快。
   *
   * Code Logic（这个函数做什么）:
   *   有 Tauri internals 时 listen 并 applySnapshot。
   */
  useEffect(() => {
    if (!canUseTauriEvents()) return undefined;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void import('@tauri-apps/api/event')
      .then(({ listen }) => {
        if (disposed) return undefined;
        return listen<BatterySnapshot>(BATTERY_CHANGE_EVENT, (event) => {
          if (event.payload && typeof event.payload === 'object') {
            applySnapshot(event.payload);
          }
        });
      })
      .then((fn) => {
        if (!fn) return;
        if (disposed) {
          fn();
          return;
        }
        unlisten = fn;
      })
      .catch(() => undefined);
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [applySnapshot]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   本窗是否消耗必须随路由 / 可见性 / 焦点变化上报。
   *
   * Code Logic（这个函数做什么）:
   *   计算 consuming；变化或每 1s 心跳 reportFocus。
   */
  useEffect(() => {
    let cancelled = false;

    const compute = (): boolean => {
      const visible = typeof document === 'undefined' ? false : document.visibilityState === 'visible';
      const focused = typeof document === 'undefined' ? false : document.hasFocus();
      return isBatteryConsumingRoute(location.pathname, visible, focused);
    };

    const report = async (force: boolean): Promise<void> => {
      const consuming = compute();
      if (!force && lastConsuming.current === consuming) {
        if (!consuming) return;
      }
      lastConsuming.current = consuming;
      try {
        const next = await batteryApi.reportFocus(windowLabelRef.current, consuming);
        if (!cancelled) applySnapshot(next);
      } catch {
        // 扣时失败留给下一拍心跳；不把焦点错误盖掉余额
      }
    };

    const onChange = (): void => {
      void report(true);
    };

    void report(true);
    const timer = window.setInterval(() => {
      void report(false);
    }, 1000);
    window.addEventListener('focus', onChange);
    window.addEventListener('blur', onChange);
    document.addEventListener('visibilitychange', onChange);

    return () => {
      cancelled = true;
      window.clearInterval(timer);
      window.removeEventListener('focus', onChange);
      window.removeEventListener('blur', onChange);
      document.removeEventListener('visibilitychange', onChange);
      if (lastConsuming.current) {
        lastConsuming.current = false;
        void batteryApi.reportFocus(windowLabelRef.current, false).catch(() => undefined);
      }
    };
  }, [applySnapshot, location.pathname]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   footer 与设置页切换模式必须写后端权威行。
   *
   * Code Logic（这个函数做什么）:
   *   setMode 成功后 applySnapshot。
   */
  const setMode = useCallback(
    async (mode: BatteryMode): Promise<void> => {
      const next = await batteryApi.setMode(mode);
      applySnapshot(next);
    },
    [applySnapshot],
  );

  const dismissToast = useCallback((): void => {
    setToast(null);
  }, []);

  return { snapshot, loading, error, toast, setMode, dismissToast };
}
