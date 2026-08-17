/**
 * Workbench 依赖提示：本机走 Context，远端项目探测 owning device。
 *
 * Business Logic（为什么需要这个组件）:
 *   选中远端项目时必须展示对端 tmux 状态，确认框写明设备名 + argv；
 *   tmux 已就绪时不得占用终端工作台（就绪详情只在 Settings）；
 *   Settings 卡仍走本机 Context，本组件不得被 Settings 复用错装控制端。
 *
 * Code Logic（这个组件做什么）:
 *   local / remote 仅在非 ready、非 checking 时渲染 WorkbenchDependencyCard；
 *   remote 用 deviceId 调 API 并传入 source。
 */

import { useCallback, useEffect, useState } from 'react';
import { WorkbenchDependencyCard } from '@/components/domain/WorkbenchDependencyCard';
import { workbenchDependencyApi } from '@/api/workbenchDependency';
import {
  dependencyStatusFromError,
  shouldShowWorkbenchDependencyNotice,
} from '@/lib/workbenchDependency';
import type { WorkbenchDependencyStatus, WorkbenchProject } from '@/lib/types';

export interface WorkbenchDependencyNoticeProps {
  compact?: boolean;
  className?: string;
  project: WorkbenchProject;
  localStatus: WorkbenchDependencyStatus['status'];
  remoteWriteDisabled?: boolean;
}

const EMPTY_STATUS: WorkbenchDependencyStatus = {
  status: 'checking',
  available: false,
  version: null,
  backend: '',
  path: null,
  installable: false,
  installCommandPreview: [],
  error: null,
  output: [],
  statusChangedAt: '',
};

/**
 * Business Logic（为什么需要这个组件）:
 *   Workbench 页不能把本机 tmux 卡误当成对端状态，也不能在 tmux 已可用时挡住终端。
 *
 * Code Logic（这个函数做什么）:
 *   remote → 轮询对端 check/status；local → 复用 Context 卡；ready/checking 不渲染。
 */
export function WorkbenchDependencyNotice(props: WorkbenchDependencyNoticeProps) {
  const { compact, className, project, localStatus, remoteWriteDisabled = false } = props;
  const remote = project.kind === 'remote';
  const deviceId = remote ? project.deviceId : undefined;
  const [status, setStatus] = useState<WorkbenchDependencyStatus>(EMPTY_STATUS);
  const [checking, setChecking] = useState(false);
  const [installing, setInstalling] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const check = useCallback(async () => {
    if (!deviceId) return;
    setChecking(true);
    try {
      const next = await workbenchDependencyApi.check(deviceId);
      setStatus(next);
      setError(next.error);
    } catch (reason: unknown) {
      const mapped = dependencyStatusFromError(reason);
      setStatus(mapped);
      setError(mapped.error);
    } finally {
      setChecking(false);
    }
  }, [deviceId]);

  const install = useCallback(async () => {
    if (!deviceId) return;
    setInstalling(true);
    try {
      const next = await workbenchDependencyApi.install(deviceId);
      setStatus(next);
      setError(next.error);
    } catch (reason: unknown) {
      const mapped = dependencyStatusFromError(reason);
      setStatus(mapped);
      setError(mapped.error);
    } finally {
      setInstalling(false);
    }
  }, [deviceId]);

  const cancel = useCallback(async () => {
    if (!deviceId) return;
    const next = await workbenchDependencyApi.cancel(deviceId);
    setStatus(next);
    setError(next.error);
  }, [deviceId]);

  useEffect(() => {
    if (!remote || !deviceId) return undefined;
    const timer = window.setTimeout(() => {
      void check();
    }, 0);
    return () => window.clearTimeout(timer);
  }, [check, deviceId, remote]);

  useEffect(() => {
    if (!remote || !deviceId || status.status !== 'installing') return undefined;
    const timer = window.setInterval(() => {
      void workbenchDependencyApi.status(deviceId).then(setStatus).catch(() => undefined);
    }, 1500);
    return () => window.clearInterval(timer);
  }, [deviceId, remote, status.status]);

  if (!remote) {
    if (!shouldShowWorkbenchDependencyNotice(localStatus)) {
      return null;
    }
    return <WorkbenchDependencyCard compact={compact} className={className} />;
  }

  if (!shouldShowWorkbenchDependencyNotice(status.status)) {
    return null;
  }

  return (
    <WorkbenchDependencyCard
      compact={compact}
      className={className}
      deviceName={project.deviceName || project.deviceId}
      remoteWriteDisabled={remoteWriteDisabled}
      source={{ status, checking, installing, error, check, install, cancel }}
    />
  );
}
