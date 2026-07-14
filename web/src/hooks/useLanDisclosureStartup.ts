/**
 * useLanDisclosureStartup — App 级 LAN 风险披露状态机。
 *
 * Business Logic（为什么需要这个 hook）:
 *   GUI 在用户确认 LAN 风险前不得进入产品路由；状态为 loading/required/starting/error/pass，
 *   失败 fail-closed 可重试，永不 fail-open。
 *
 * Code Logic（这个 hook 做什么）:
 *   挂载时 getLanDisclosureStatus；required 时等待 acknowledge；
 *   所有 hooks 在 early return 之前由调用方遵守；本 hook 自身无条件 hooks。
 */

import { useCallback, useEffect, useState } from 'react';
import {
  backendApi,
  type LanDisclosureStartResult,
  type LanDisclosureStatus,
} from '@/api/backend';

/** 前端披露状态机。 */
export type LanDisclosurePhase = 'loading' | 'required' | 'starting' | 'error' | 'pass';

export type UseLanDisclosureStartupResult = {
  phase: LanDisclosurePhase;
  status: LanDisclosureStatus | null;
  startResult: LanDisclosureStartResult | null;
  error: string | null;
  acknowledge: () => Promise<void>;
  retry: () => Promise<void>;
  openDiagnostics: () => Promise<void>;
};

/**
 * Business Logic（为什么需要这个函数）:
 *   App 级 gate 与测试需要共享同一状态机。
 *
 * Code Logic（这个函数做什么）:
 *   管理 phase/status/error；提供 acknowledge/retry/openDiagnostics。
 */
export function useLanDisclosureStartup(): UseLanDisclosureStartupResult {
  const [phase, setPhase] = useState<LanDisclosurePhase>('loading');
  const [status, setStatus] = useState<LanDisclosureStatus | null>(null);
  const [startResult, setStartResult] = useState<LanDisclosureStartResult | null>(null);
  const [error, setError] = useState<string | null>(null);

  /**
   * Business Logic（为什么需要这个函数）:
   *   挂载与重试都要重新读取披露状态。
   *
   * Code Logic（这个函数做什么）:
   *   调 getLanDisclosureStatus；required → required，否则 pass；失败 → error。
   */
  const loadStatus = useCallback(async () => {
    setPhase('loading');
    setError(null);
    try {
      const next = await backendApi.getLanDisclosureStatus();
      setStatus(next);
      if (next.required) {
        setPhase('required');
      } else {
        setPhase('pass');
      }
    } catch (reason) {
      setStatus(null);
      setError(reason instanceof Error ? reason.message : String(reason));
      setPhase('error');
    }
  }, []);

  useEffect(() => {
    void loadStatus();
  }, [loadStatus]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户显式确认后才启动 sidecar。
   *
   * Code Logic（这个函数做什么）:
   *   phase=starting → acknowledge invoke → pass；失败 error（可重试，不回滚确认文案态）。
   */
  const acknowledge = useCallback(async () => {
    setPhase('starting');
    setError(null);
    try {
      const result = await backendApi.acknowledgeLanDisclosureAndStartBackend();
      setStartResult(result);
      setStatus((prev) =>
        prev
          ? {
              ...prev,
              required: false,
              alreadyRunning: result.reusedExisting,
              actualHttpPort: result.actualHttpPort,
              localAddresses:
                result.localAddresses.length > 0 ? result.localAddresses : prev.localAddresses,
            }
          : prev,
      );
      setPhase('pass');
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
      setPhase('error');
    }
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   读状态失败或启动失败后需显式重试。
   *
   * Code Logic（这个函数做什么）:
   *   若已有 status 且曾 starting 失败，优先再 acknowledge；否则重新 loadStatus。
   */
  const retry = useCallback(async () => {
    // 启动失败或确认失败：bootstrap 可能已写入，再调 acknowledge（后端幂等/可重试）。
    if (phase === 'error' && status) {
      await acknowledge();
      return;
    }
    await loadStatus();
  }, [acknowledge, loadStatus, phase, status]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   启动失败时提供打开诊断/日志目录的动作。
   *
   * Code Logic（这个函数做什么）:
   *   best-effort invoke open_backend_log_dir；失败写入 error 不改 phase。
   */
  const openDiagnostics = useCallback(async () => {
    try {
      await import('@/api/client').then(({ invoke }) =>
        invoke<void>('open_backend_log_dir'),
      );
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }, []);

  return {
    phase,
    status,
    startResult,
    error,
    acknowledge,
    retry,
    openDiagnostics,
  };
}
