/**
 * WorkbenchDependencyProvider - tmux 依赖共享状态。
 *
 * Business Logic（为什么需要这个模块）:
 *   Workbench 的真实 window/pane 功能依赖 tmux；应用需要自动检测、展示状态、引导安装并在安装后重新检测。
 *
 * Code Logic（这个模块做什么）:
 *   调用后端 dependency API 管理状态，安装中轮询安装状态，并通过 Context 提供给 Workbench/Settings。
 */

import { useCallback, useEffect, useMemo, useState } from 'react';
import type { ReactNode } from 'react';
import { workbenchDependencyApi } from '@/api/workbenchDependency';
import type { WorkbenchDependencyStatus } from '@/lib/types';
import {
  WorkbenchDependencyContext,
  type WorkbenchDependencyContextValue,
} from './workbenchDependencyContext';

const POLL_INTERVAL_MS = 1200;

const INITIAL_STATUS: WorkbenchDependencyStatus = {
  status: 'checking',
  available: false,
  version: null,
  backend: 'native',
  path: null,
  installable: false,
  installCommandPreview: [],
  error: null,
  output: [],
  statusChangedAt: new Date(0).toISOString(),
};

export interface WorkbenchDependencyProviderProps {
  children: ReactNode;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   普通浏览器调试环境没有 Tauri IPC，依赖检测失败时需要展示清晰降级状态。
 *
 * Code Logic（这个函数做什么）:
 *   将未知错误转换为 failed 状态 DTO，保留错误 message 供 UI 展示。
 */
function statusFromError(error: unknown): WorkbenchDependencyStatus {
  const message =
    error instanceof Error ? error.message : typeof error === 'string' ? error : String(error);
  return {
    ...INITIAL_STATUS,
    status: 'failed',
    error: message,
  };
}

/**
 * Business Logic（为什么需要这个组件）:
 *   Workbench 与 Settings 需要共享依赖状态，避免重复安装或重复检测。
 *
 * Code Logic（这个组件做什么）:
 *   维护依赖状态、检测/安装/cancel 动作；安装中轮询后端状态直到离开 installing。
 */
export function WorkbenchDependencyProvider({ children }: WorkbenchDependencyProviderProps) {
  const [status, setStatus] = useState<WorkbenchDependencyStatus>(INITIAL_STATUS);
  const [checking, setChecking] = useState<boolean>(false);
  const [installing, setInstalling] = useState<boolean>(false);
  const [error, setError] = useState<string | null>(null);

  const check = useCallback(async () => {
    try {
      setChecking(true);
      setError(null);
      const next = await workbenchDependencyApi.check();
      setStatus(next);
    } catch (err) {
      const failed = statusFromError(err);
      setError(failed.error);
      setStatus(failed);
    } finally {
      setChecking(false);
    }
  }, []);

  const install = useCallback(async () => {
    try {
      setInstalling(true);
      setError(null);
      const next = await workbenchDependencyApi.install();
      setStatus(next);
    } catch (err) {
      const failed = statusFromError(err);
      setError(failed.error);
      setStatus(failed);
      setInstalling(false);
    }
  }, []);

  const cancel = useCallback(async () => {
    try {
      const next = await workbenchDependencyApi.cancel();
      setStatus(next);
    } catch (err) {
      const failed = statusFromError(err);
      setError(failed.error);
      setStatus(failed);
    } finally {
      setInstalling(false);
    }
  }, []);

  useEffect(() => {
    const timer = window.setTimeout(() => {
      void check();
    }, 0);
    return () => window.clearTimeout(timer);
  }, [check]);

  useEffect(() => {
    const syncTimer = window.setTimeout(() => {
      setInstalling(status.status === 'installing');
    }, 0);
    if (status.status !== 'installing') {
      return () => window.clearTimeout(syncTimer);
    }
    const timer = window.setInterval(() => {
      void workbenchDependencyApi
        .status()
        .then((next) => {
          setStatus(next);
          if (next.status !== 'installing') setInstalling(false);
        })
        .catch((err) => {
          const failed = statusFromError(err);
          setError(failed.error);
          setStatus(failed);
          setInstalling(false);
        });
    }, POLL_INTERVAL_MS);
    return () => {
      window.clearTimeout(syncTimer);
      window.clearInterval(timer);
    };
  }, [status.status]);

  const value = useMemo<WorkbenchDependencyContextValue>(
    () => ({
      status,
      checking,
      installing,
      error,
      check,
      install,
      cancel,
    }),
    [cancel, check, checking, error, install, installing, status],
  );

  return (
    <WorkbenchDependencyContext.Provider value={value}>
      {children}
    </WorkbenchDependencyContext.Provider>
  );
}
