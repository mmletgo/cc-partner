/**
 * WorkbenchRemoteProjectPicker（本机 / 局域网项目目录选择器）
 *
 * Business Logic（为什么需要这个组件）:
 *   Workbench 添加本机或局域网项目都走应用内目录浏览；可在当前目录新建一层文件夹后确认打开。
 *
 * Code Logic（这个组件做什么）:
 *   source=local 浏览本机 fs；source=remote 先选在线设备再浏览对端。新建成功后选中新目录，打开才登记项目。
 */

import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { devicesApi } from '@/api/devices';
import { workbenchApi } from '@/api/workbench';
import { Button, Card, Dialog, Input, Pill, StatusDot } from '@/components/primitives';
import type {
  Device,
  WorkbenchProject,
  WorkbenchRemoteDirectoryEntry,
  WorkbenchRemotePathInfo,
  WorkbenchRemoteRoot,
} from '@/lib/types';
import { ChevronRightIcon, FileIcon, FolderIcon, XIcon } from '@/lib/icons';
import {
  isRelayShadowDevice,
  pickRelayAwarePickerDevices,
} from '@/lib/relayDevices';
import {
  canOpenHostProjectSelection,
  canOpenRemoteProjectSelection,
  isValidBrowseChildName,
  peerSupportsBrowseMkdir,
  remoteParentPath,
  sortRemoteDirectoryEntries,
} from '@/lib/workbenchRemoteProjects';
import styles from './WorkbenchRemoteProjectPicker.module.css';

export type WorkbenchProjectPickerSource = 'local' | 'remote';

export interface WorkbenchRemoteProjectPickerProps {
  /** 打开成功后的项目 DTO 回调。 */
  onProjectOpened: (project: WorkbenchProject) => void;
  /** 关闭选择器。 */
  onCancel: () => void;
  /** 打开或新建请求 pending 状态变化回调，供父级阻止关闭弹窗。 */
  onOpenBusyChange?: (openBusy: boolean) => void;
  /** 可注入的打开实现；默认直接调用 workbenchApi.remote.openProject。 */
  openProject?: (deviceId: string, path: string) => Promise<WorkbenchProject | null>;
  /** 可注入的本机打开实现；默认直接调用 workbenchApi.projects.add。桌面侧栏/启动面必须注入 addProjectFromPath，否则共享列表不会更新。 */
  openLocalProject?: (path: string) => Promise<WorkbenchProject | null>;
  /** local=本机应用内浏览；remote=局域网设备。默认 remote。 */
  source?: WorkbenchProjectPickerSource;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   远端设备或目录 API 失败时，选择器需要显示用户可读的错误。
 *
 * Code Logic（这个函数做什么）:
 *   从 unknown 错误中提取 message；没有可用消息时返回 fallback。
 */
function errorMessage(error: unknown, fallback: string): string {
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === 'string' && error) return error;
  return fallback;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   远端文件只作为上下文展示，只有目录能被选择并打开为 Workbench 项目。
 *
 * Code Logic（这个函数做什么）:
 *   判断远端目录项 kind 是否为 dir。
 */
function isRemoteDirectory(entry: WorkbenchRemoteDirectoryEntry): boolean {
  return entry.kind === 'dir';
}

interface RemoteProjectPickerState {
  devices: Device[];
  devicesLoading: boolean;
  selectedDeviceId: string | null;
  roots: WorkbenchRemoteRoot[];
  rootsLoading: boolean;
  currentPath: string | null;
  entries: WorkbenchRemoteDirectoryEntry[];
  entriesLoading: boolean;
  selectedPath: string | null;
  pathInfo: WorkbenchRemotePathInfo | null;
  pathInfoDeviceId: string | null;
  pathInfoLoading: boolean;
  openBusy: boolean;
  createBusy: boolean;
  createError: string | null;
  error: string | null;
}

type RemoteProjectPickerAction =
  | { type: 'devicesLoading' }
  | { type: 'devicesLoaded'; devices: Device[] }
  | { type: 'devicesFailed'; error: string }
  | { type: 'deviceSelected'; deviceId: string }
  | { type: 'rootsLoading' }
  | { type: 'rootsLoaded'; roots: WorkbenchRemoteRoot[] }
  | { type: 'rootsFailed'; error: string }
  | { type: 'rootSelected'; path: string }
  | { type: 'entriesLoading' }
  | { type: 'entriesLoaded'; entries: WorkbenchRemoteDirectoryEntry[] }
  | { type: 'entriesFailed'; error: string }
  | { type: 'entrySelected'; path: string }
  | { type: 'entryBrowsed'; path: string }
  | { type: 'pathInfoLoading'; deviceId: string; path: string }
  | { type: 'pathInfoLoaded'; deviceId: string; path: string; info: WorkbenchRemotePathInfo }
  | { type: 'pathInfoFailed'; deviceId: string; path: string }
  | { type: 'openStarted' }
  | { type: 'openFinished' }
  | { type: 'openFailed'; error: string }
  | { type: 'createStarted' }
  | { type: 'createFinished' }
  | { type: 'createFailed'; error: string };

const initialPickerState: RemoteProjectPickerState = {
  devices: [],
  devicesLoading: true,
  selectedDeviceId: null,
  roots: [],
  rootsLoading: false,
  currentPath: null,
  entries: [],
  entriesLoading: false,
  selectedPath: null,
  pathInfo: null,
  pathInfoDeviceId: null,
  pathInfoLoading: false,
  openBusy: false,
  createBusy: false,
  createError: null,
  error: null,
};

/**
 * Business Logic（为什么需要这个函数）:
 *   远端项目选择器同时维护设备、根目录、目录项和路径信息，分散 setState 容易产生 stale 状态。
 *
 * Code Logic（这个函数做什么）:
 *   用 reducer 串联加载、选择、浏览和打开状态；每次切换 device/path 都清理不再匹配的下游数据。
 */
function isPickerBusy(state: RemoteProjectPickerState): boolean {
  return state.openBusy || state.createBusy;
}

function remoteProjectPickerReducer(
  state: RemoteProjectPickerState,
  action: RemoteProjectPickerAction,
): RemoteProjectPickerState {
  switch (action.type) {
    case 'devicesLoading':
      return { ...state, devicesLoading: true, error: null };
    case 'devicesLoaded': {
      const selectedDeviceId =
        state.selectedDeviceId && action.devices.some((device) => device.id === state.selectedDeviceId)
          ? state.selectedDeviceId
          : action.devices[0]?.id ?? null;
      const deviceChanged = selectedDeviceId !== state.selectedDeviceId;
      return {
        ...state,
        devices: action.devices,
        devicesLoading: false,
        selectedDeviceId,
        roots: deviceChanged ? [] : state.roots,
        currentPath: deviceChanged ? null : state.currentPath,
        entries: deviceChanged ? [] : state.entries,
        selectedPath: deviceChanged ? null : state.selectedPath,
        pathInfo: deviceChanged ? null : state.pathInfo,
        pathInfoDeviceId: deviceChanged ? null : state.pathInfoDeviceId,
        pathInfoLoading: deviceChanged ? false : state.pathInfoLoading,
      };
    }
    case 'devicesFailed':
      return { ...state, devicesLoading: false, error: action.error };
    case 'deviceSelected':
      if (isPickerBusy(state)) return state;
      return {
        ...state,
        selectedDeviceId: action.deviceId,
        roots: [],
        currentPath: null,
        entries: [],
        selectedPath: null,
        pathInfo: null,
        pathInfoDeviceId: null,
        pathInfoLoading: false,
        error: null,
      };
    case 'rootsLoading':
      return {
        ...state,
        rootsLoading: true,
        roots: [],
        currentPath: null,
        entries: [],
        selectedPath: null,
        pathInfo: null,
        pathInfoDeviceId: null,
        pathInfoLoading: false,
        error: null,
      };
    case 'rootsLoaded': {
      const firstPath = action.roots[0]?.path ?? null;
      return {
        ...state,
        roots: action.roots,
        rootsLoading: false,
        currentPath: firstPath,
        entries: [],
        selectedPath: firstPath,
        pathInfo: null,
        pathInfoDeviceId: null,
        pathInfoLoading: false,
      };
    }
    case 'rootsFailed':
      return { ...state, rootsLoading: false, error: action.error };
    case 'rootSelected':
    case 'entryBrowsed':
      if (isPickerBusy(state)) return state;
      return {
        ...state,
        currentPath: action.path,
        entries: [],
        selectedPath: action.path,
        pathInfo: null,
        pathInfoDeviceId: null,
        pathInfoLoading: false,
        error: null,
      };
    case 'entriesLoading':
      return { ...state, entriesLoading: true, entries: [], error: null };
    case 'entriesLoaded':
      return { ...state, entries: action.entries, entriesLoading: false };
    case 'entriesFailed':
      return { ...state, entriesLoading: false, error: action.error };
    case 'entrySelected':
      if (isPickerBusy(state)) return state;
      return {
        ...state,
        selectedPath: action.path,
        pathInfo: null,
        pathInfoDeviceId: null,
        pathInfoLoading: false,
        error: null,
      };
    case 'pathInfoLoading':
      return {
        ...state,
        pathInfo: null,
        pathInfoDeviceId: action.deviceId,
        pathInfoLoading: true,
      };
    case 'pathInfoLoaded':
      if (state.selectedPath !== action.path) {
        return state;
      }
      if (action.deviceId !== 'local' && state.selectedDeviceId !== action.deviceId) {
        return state;
      }
      return {
        ...state,
        pathInfo: action.info,
        pathInfoDeviceId: action.deviceId,
        pathInfoLoading: false,
      };
    case 'pathInfoFailed':
      if (state.selectedPath !== action.path) {
        return state;
      }
      if (action.deviceId !== 'local' && state.selectedDeviceId !== action.deviceId) {
        return state;
      }
      return { ...state, pathInfo: null, pathInfoDeviceId: action.deviceId, pathInfoLoading: false };
    case 'openStarted':
      return { ...state, openBusy: true, error: null };
    case 'openFinished':
      return { ...state, openBusy: false };
    case 'openFailed':
      return { ...state, openBusy: false, error: action.error };
    case 'createStarted':
      return { ...state, createBusy: true, createError: null, error: null };
    case 'createFinished':
      return { ...state, createBusy: false, createError: null };
    case 'createFailed':
      return { ...state, createBusy: false, createError: action.error };
    default:
      return state;
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   React lint 禁止 effect 主体同步 setState；远端 picker 仍需要 effect 启动异步加载。
 *
 * Code Logic（这个函数做什么）:
 *   将 effect 内工作延迟到下一轮 macrotask；清理时取消未启动任务并执行已启动任务的清理函数。
 */
function deferEffect(work: () => void | (() => void)): () => void {
  let cleanup: void | (() => void);
  const timer = window.setTimeout(() => {
    cleanup = work();
  }, 0);
  return () => {
    window.clearTimeout(timer);
    cleanup?.();
  };
}

/**
 * WorkbenchRemoteProjectPicker 组件
 *
 * Business Logic（为什么需要这个组件）:
 *   用户需要从侧栏添加入口选择“远端设备项目”，并直接把局域网设备上的目录加入 Workbench。
 *
 * Code Logic（这个组件做什么）:
 *   使用 devicesApi 与 workbenchApi.remote/fs 分层加载设备、根目录、目录项和路径信息；
 *   本机打开走 openLocalProject（缺省 projects.add），远端打开走 openProject，成功后调用 onProjectOpened。
 */
export function WorkbenchRemoteProjectPicker(props: WorkbenchRemoteProjectPickerProps) {
  const { onProjectOpened, onCancel, onOpenBusyChange, openProject, openLocalProject, source = 'remote' } = props;
  const isLocal = source === 'local';
  const { t } = useTranslation(['workbench']);
  const [state, dispatch] = useReducer(remoteProjectPickerReducer, initialPickerState);
  const selectionRef = useRef<{ deviceId: string | null; path: string | null }>({
    deviceId: null,
    path: null,
  });
  const openRequestSeqRef = useRef<number>(0);
  const {
    devices,
    devicesLoading,
    selectedDeviceId,
    roots,
    rootsLoading,
    currentPath,
    entries,
    entriesLoading,
    selectedPath,
    pathInfo,
    pathInfoDeviceId,
    pathInfoLoading,
    openBusy,
    createBusy,
    createError,
    error,
  } = state;
  const pickerBusy = openBusy || createBusy;
  const [createDialogOpen, setCreateDialogOpen] = useState(false);
  const [createName, setCreateName] = useState('');

  const selectedDevice = useMemo(
    () => devices.find((device) => device.id === selectedDeviceId) ?? null,
    [devices, selectedDeviceId],
  );
  const sortedEntries = useMemo(() => sortRemoteDirectoryEntries(entries), [entries]);
  const parentPath = useMemo(() => (currentPath ? remoteParentPath(currentPath) : null), [currentPath]);
  const canOpenSelectedPath = isLocal
    ? canOpenHostProjectSelection(
        selectedPath,
        pathInfo,
        pathInfoDeviceId === 'local' ? selectedPath : pathInfoDeviceId,
        pathInfoLoading,
        pickerBusy,
      )
    : canOpenRemoteProjectSelection(
        selectedDeviceId,
        selectedPath,
        pathInfo,
        pathInfoDeviceId,
        pathInfoLoading,
        pickerBusy,
      );
  const canCreateFolder =
    Boolean(currentPath) &&
    !pickerBusy &&
    (isLocal || peerSupportsBrowseMkdir(selectedDevice?.capabilities));
  const effectiveOpenProject = useCallback(
    (deviceId: string, path: string) =>
      openProject ? openProject(deviceId, path) : workbenchApi.remote.openProject(deviceId, path),
    [openProject],
  );
  const effectiveOpenLocalProject = useCallback(
    (path: string) =>
      openLocalProject ? openLocalProject(path) : workbenchApi.projects.add(path),
    [openLocalProject],
  );
  const handleCancel = useCallback(() => {
    if (pickerBusy) return;
    onCancel();
  }, [onCancel, pickerBusy]);

  useEffect(() => {
    selectionRef.current = { deviceId: selectedDeviceId, path: selectedPath };
  }, [selectedDeviceId, selectedPath]);

  useEffect(() => {
    return deferEffect(() => {
      if (isLocal) {
        dispatch({ type: 'devicesLoaded', devices: [] });
        return;
      }
      let cancelled = false;
      dispatch({ type: 'devicesLoading' });
      void devicesApi
        .list()
        .then((list) => {
          if (cancelled) return;
          dispatch({
            type: 'devicesLoaded',
            devices: pickRelayAwarePickerDevices(list),
          });
        })
        .catch((loadError: unknown) => {
          if (!cancelled) {
            dispatch({
              type: 'devicesFailed',
              error: errorMessage(loadError, t('workbench:remoteProjectPicker.errors.devices')),
            });
          }
        });
      return () => {
        cancelled = true;
      };
    });
  }, [isLocal, t]);

  useEffect(() => {
    return deferEffect(() => {
      if (!isLocal && !selectedDeviceId) return;
      let cancelled = false;
      dispatch({ type: 'rootsLoading' });
      const request = isLocal
        ? workbenchApi.fs.roots()
        : workbenchApi.remote.roots(selectedDeviceId as string);
      void request
        .then((list) => {
          if (!cancelled) dispatch({ type: 'rootsLoaded', roots: list });
        })
        .catch((loadError: unknown) => {
          if (!cancelled) {
            dispatch({
              type: 'rootsFailed',
              error: errorMessage(loadError, t('workbench:remoteProjectPicker.errors.roots')),
            });
          }
        });
      return () => {
        cancelled = true;
      };
    });
  }, [isLocal, selectedDeviceId, t]);

  useEffect(() => {
    return deferEffect(() => {
      if (!currentPath) return;
      if (!isLocal && !selectedDeviceId) return;
      let cancelled = false;
      dispatch({ type: 'entriesLoading' });
      const request = isLocal
        ? workbenchApi.fs.listDir(currentPath)
        : workbenchApi.remote.listDir(selectedDeviceId as string, currentPath);
      void request
        .then((list) => {
          if (!cancelled) dispatch({ type: 'entriesLoaded', entries: list });
        })
        .catch((loadError: unknown) => {
          if (!cancelled) {
            dispatch({
              type: 'entriesFailed',
              error: errorMessage(loadError, t('workbench:remoteProjectPicker.errors.dir')),
            });
          }
        });
      return () => {
        cancelled = true;
      };
    });
  }, [currentPath, isLocal, selectedDeviceId, t]);

  useEffect(() => {
    return deferEffect(() => {
      if (!selectedPath) return;
      if (!isLocal && !selectedDeviceId) return;
      const deviceId = isLocal ? 'local' : (selectedDeviceId as string);
      const path = selectedPath;
      let cancelled = false;
      dispatch({ type: 'pathInfoLoading', deviceId, path });
      const request = isLocal ? workbenchApi.fs.info(path) : workbenchApi.remote.info(deviceId, path);
      void request
        .then((info) => {
          if (!cancelled) dispatch({ type: 'pathInfoLoaded', deviceId, path, info });
        })
        .catch(() => {
          if (!cancelled) dispatch({ type: 'pathInfoFailed', deviceId, path });
        });
      return () => {
        cancelled = true;
      };
    });
  }, [isLocal, selectedDeviceId, selectedPath]);

  const handleDeviceSelect = useCallback((deviceId: string) => {
    dispatch({ type: 'deviceSelected', deviceId });
  }, []);

  const handleRootSelect = useCallback((path: string) => {
    dispatch({ type: 'rootSelected', path });
  }, []);

  const handleEntrySelect = useCallback((entry: WorkbenchRemoteDirectoryEntry) => {
    if (!isRemoteDirectory(entry)) return;
    dispatch({ type: 'entrySelected', path: entry.path });
  }, []);

  const handleEntryBrowse = useCallback((path: string) => {
    dispatch({ type: 'entryBrowsed', path });
  }, []);

  const handleOpenProject = useCallback(async () => {
    if (!canOpenSelectedPath || !selectedPath) return;
    if (!isLocal && !selectedDeviceId) return;
    const requestSeq = openRequestSeqRef.current + 1;
    openRequestSeqRef.current = requestSeq;
    const requestDeviceId = selectedDeviceId;
    const requestPath = selectedPath;
    let shouldFinishRequest = true;
    try {
      onOpenBusyChange?.(true);
      dispatch({ type: 'openStarted' });
      const project = isLocal
        ? await effectiveOpenLocalProject(requestPath)
        : await effectiveOpenProject(requestDeviceId as string, requestPath);
      const currentSelection = selectionRef.current;
      const isCurrentRequest =
        openRequestSeqRef.current === requestSeq &&
        currentSelection.path === requestPath &&
        (isLocal || currentSelection.deviceId === requestDeviceId);
      if (project && isCurrentRequest) {
        shouldFinishRequest = false;
        dispatch({ type: 'openFinished' });
        onOpenBusyChange?.(false);
        onProjectOpened(project);
      }
    } catch (openError: unknown) {
      if (openRequestSeqRef.current === requestSeq) {
        dispatch({
          type: 'openFailed',
          error: errorMessage(openError, t('workbench:remoteProjectPicker.errors.open')),
        });
        return;
      }
    } finally {
      if (shouldFinishRequest && openRequestSeqRef.current === requestSeq) {
        dispatch({ type: 'openFinished' });
        onOpenBusyChange?.(false);
      }
    }
  }, [
    canOpenSelectedPath,
    effectiveOpenLocalProject,
    effectiveOpenProject,
    isLocal,
    onOpenBusyChange,
    onProjectOpened,
    selectedDeviceId,
    selectedPath,
    t,
  ]);

  const handleCreateFolder = useCallback(async () => {
    const name = createName.trim();
    if (!currentPath || !isValidBrowseChildName(name) || pickerBusy) return;
    if (!isLocal && !selectedDeviceId) return;
    dispatch({ type: 'createStarted' });
    onOpenBusyChange?.(true);
    try {
      const created = isLocal
        ? await workbenchApi.fs.createDir(currentPath, name)
        : await workbenchApi.remote.createDir(selectedDeviceId as string, currentPath, name);
      const list = isLocal
        ? await workbenchApi.fs.listDir(currentPath)
        : await workbenchApi.remote.listDir(selectedDeviceId as string, currentPath);
      dispatch({ type: 'createFinished' });
      dispatch({ type: 'entriesLoaded', entries: list });
      dispatch({ type: 'entrySelected', path: created.path });
      setCreateDialogOpen(false);
      setCreateName('');
    } catch (createErr: unknown) {
      dispatch({
        type: 'createFailed',
        error: errorMessage(createErr, t('workbench:remoteProjectPicker.errors.create')),
      });
    } finally {
      onOpenBusyChange?.(false);
    }
  }, [
    createName,
    currentPath,
    isLocal,
    onOpenBusyChange,
    pickerBusy,
    selectedDeviceId,
    t,
  ]);

  return (
    <Card className={styles.picker} variant="elevated" padding="none">
      <Card.Header className={styles.header} padding="md">
        <div className={styles.heading}>
          <h2>
            {t(
              isLocal
                ? 'workbench:remoteProjectPicker.localTitle'
                : 'workbench:remoteProjectPicker.title',
            )}
          </h2>
          <p>
            {t(
              isLocal
                ? 'workbench:remoteProjectPicker.localSubtitle'
                : 'workbench:remoteProjectPicker.subtitle',
            )}
          </p>
        </div>
        <Button
          variant="icon"
          icon={<XIcon />}
          title={t('workbench:remoteProjectPicker.close')}
          aria-label={t('workbench:remoteProjectPicker.close')}
          disabled={pickerBusy}
          onClick={handleCancel}
        />
      </Card.Header>

      <Card.Body className={styles.body} padding="md">
        {error ? <div className={styles.errorBox}>{error}</div> : null}

        {isLocal ? null : (
        <section
          className={`${styles.section} ${styles.devicesSection}`}
          aria-label={t('workbench:remoteProjectPicker.devices')}
        >
          <div className={styles.sectionHeader}>
            <span>{t('workbench:remoteProjectPicker.devices')}</span>
            {devicesLoading ? <Pill tone="neutral">{t('workbench:loading')}</Pill> : null}
          </div>
          <div className={styles.deviceList}>
            {!devicesLoading && devices.length === 0 ? (
              <div className={styles.empty}>{t('workbench:remoteProjectPicker.noDevices')}</div>
            ) : null}
            {devices.map((device) => {
              const viaRelay = isRelayShadowDevice(device);
              const viaName = device.viaDeviceName ?? device.viaDeviceId ?? device.name;
              const relayOffline = viaRelay && device.status === 'offline';
              return (
                <button
                  key={device.id}
                  type="button"
                  className={styles.deviceButton}
                  data-active={device.id === selectedDeviceId || undefined}
                  data-relay={viaRelay || undefined}
                  data-relay-offline={relayOffline || undefined}
                  disabled={pickerBusy || relayOffline}
                  onClick={() => handleDeviceSelect(device.id)}
                >
                  <StatusDot status={device.status} size="sm" />
                  <span className={styles.deviceName}>{device.name}</span>
                  {viaRelay ? (
                    <Pill tone="neutral" className={styles.relayPill}>
                      {t('workbench:remoteProjectPicker.viaRelay', { device: viaName })}
                    </Pill>
                  ) : null}
                  <span className={styles.deviceAddress}>{device.address}</span>
                  {relayOffline ? (
                    <span className={styles.deviceRelayHint}>
                      {t('workbench:remoteProjectPicker.relayOffline', { device: viaName })}
                    </span>
                  ) : null}
                </button>
              );
            })}
          </div>
        </section>
        )}

        <section
          className={`${styles.section} ${styles.rootsSection}`}
          aria-label={t('workbench:remoteProjectPicker.roots')}
        >
          <div className={styles.sectionHeader}>
            <span>{t('workbench:remoteProjectPicker.roots')}</span>
            {rootsLoading ? <Pill tone="neutral">{t('workbench:loading')}</Pill> : null}
          </div>
          <div className={styles.rootList}>
            {!rootsLoading && (isLocal || selectedDevice) && roots.length === 0 ? (
              <div className={styles.empty}>{t('workbench:remoteProjectPicker.noRoots')}</div>
            ) : null}
            {roots.map((root) => (
              <button
                key={`${root.kind}:${root.path}`}
                type="button"
                className={styles.rootButton}
                data-active={root.path === currentPath || undefined}
                disabled={pickerBusy}
                onClick={() => handleRootSelect(root.path)}
              >
                <FolderIcon />
                <span className={styles.rootText}>
                  <span>{root.label}</span>
                  <span>{root.path}</span>
                </span>
              </button>
            ))}
          </div>
        </section>

        <section className={styles.browser} aria-label={t('workbench:remoteProjectPicker.browser')}>
          <div className={styles.pathBar}>
            <Button
              variant="ghost"
              size="sm"
              disabled={!parentPath || pickerBusy}
              onClick={() => {
                if (parentPath) handleEntryBrowse(parentPath);
              }}
            >
              {t('workbench:remoteProjectPicker.parent')}
            </Button>
            <span className={styles.currentPath}>{currentPath ?? t('workbench:emptyValue')}</span>
          </div>

          <div className={styles.entryList}>
            {entriesLoading ? <div className={styles.empty}>{t('workbench:remoteProjectPicker.loadingDir')}</div> : null}
            {!entriesLoading && currentPath && sortedEntries.length === 0 ? (
              <div className={styles.empty}>{t('workbench:remoteProjectPicker.emptyDirectory')}</div>
            ) : null}
            {sortedEntries.map((entry) => {
              const isDirectory = isRemoteDirectory(entry);
              return (
                <div
                  key={entry.path}
                  className={styles.entryRow}
                  data-selected={entry.path === selectedPath || undefined}
                  data-disabled={!isDirectory || undefined}
                >
                  <button
                    type="button"
                    className={styles.entrySelect}
                    disabled={!isDirectory || pickerBusy}
                    onClick={() => handleEntrySelect(entry)}
                  >
                    {isDirectory ? <FolderIcon /> : <FileIcon />}
                    <span className={styles.entryText}>
                      <span>{entry.name}</span>
                      <span>{entry.path}</span>
                    </span>
                    {entry.isGitRepo ? <Pill tone="accent">{t('workbench:remoteProjectPicker.gitRepo')}</Pill> : null}
                  </button>
                  {isDirectory ? (
                    <Button
                      variant="icon"
                      icon={<ChevronRightIcon />}
                      title={t('workbench:remoteProjectPicker.browse')}
                      aria-label={t('workbench:remoteProjectPicker.browse')}
                      disabled={pickerBusy}
                      onClick={() => handleEntryBrowse(entry.path)}
                    />
                  ) : null}
                </div>
              );
            })}
          </div>
        </section>

        <section className={styles.selection} aria-label={t('workbench:remoteProjectPicker.selection')}>
          <span>{t('workbench:remoteProjectPicker.selectedPath')}</span>
          <code>{selectedPath ?? t('workbench:emptyValue')}</code>
          {pathInfoLoading ? <Pill tone="neutral">{t('workbench:loading')}</Pill> : null}
          {pathInfo ? (
            <div className={styles.selectionMeta}>
              <Pill tone={pathInfo.readable ? 'success' : 'danger'}>
                {pathInfo.readable
                  ? t('workbench:remoteProjectPicker.readable')
                  : t('workbench:remoteProjectPicker.notReadable')}
              </Pill>
              {pathInfo.isGitRepo ? <Pill tone="accent">{t('workbench:remoteProjectPicker.gitRepo')}</Pill> : null}
              <span>{pathInfo.suggestedProjectName}</span>
            </div>
          ) : null}
        </section>
      </Card.Body>

      <Card.Footer className={styles.footer} padding="md">
        <Button variant="ghost" disabled={pickerBusy} onClick={handleCancel}>
          {t('workbench:remoteProjectPicker.close')}
        </Button>
        {canCreateFolder ? (
          <Button
            variant="secondary"
            disabled={pickerBusy}
            onClick={() => {
              setCreateName('');
              setCreateDialogOpen(true);
            }}
          >
            {t('workbench:remoteProjectPicker.createFolder')}
          </Button>
        ) : null}
        <Button
          variant="primary"
          loading={openBusy}
          disabled={!canOpenSelectedPath}
          onClick={() => void handleOpenProject()}
        >
          {t('workbench:remoteProjectPicker.openProject')}
        </Button>
      </Card.Footer>
      <Dialog
        open={createDialogOpen}
        titleId="workbench-browse-create-dir-title"
        onClose={() => {
          if (createBusy) return;
          setCreateDialogOpen(false);
        }}
        closeOnEscape={!createBusy}
        closeOnBackdrop={!createBusy}
      >
        <h2 id="workbench-browse-create-dir-title">{t('workbench:remoteProjectPicker.createFolder')}</h2>
        <Input
          value={createName}
          onChange={(event) => setCreateName(event.target.value)}
          placeholder={t('workbench:remoteProjectPicker.createFolderPlaceholder')}
          disabled={createBusy}
        />
        {createError ? <p className={styles.errorBox}>{createError}</p> : null}
        <div className={styles.footer} style={{ marginTop: 'var(--space-4)' }}>
          <Button
            variant="ghost"
            disabled={createBusy}
            onClick={() => setCreateDialogOpen(false)}
          >
            {t('workbench:remoteProjectPicker.close')}
          </Button>
          <Button
            variant="primary"
            loading={createBusy}
            disabled={!isValidBrowseChildName(createName) || createBusy}
            onClick={() => void handleCreateFolder()}
          >
            {t('workbench:remoteProjectPicker.createFolderConfirm')}
          </Button>
        </div>
      </Dialog>
    </Card>
  );
}
