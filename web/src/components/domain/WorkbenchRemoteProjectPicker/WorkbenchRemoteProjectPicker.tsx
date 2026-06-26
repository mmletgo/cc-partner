/**
 * WorkbenchRemoteProjectPicker（局域网远端项目选择器）
 *
 * Business Logic（为什么需要这个组件）:
 *   Workbench 需要允许用户直接从在线局域网设备选择项目文件夹，不要求该项目先被远端 Workbench 预添加。
 *
 * Code Logic（这个组件做什么）:
 *   加载在线设备、远端可浏览根目录和当前目录项；用户选择目录后调用 openProject 并把打开的项目回传给父组件。
 */

import { useCallback, useEffect, useMemo, useReducer, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { devicesApi } from '@/api/devices';
import { workbenchApi } from '@/api/workbench';
import { Button, Card, Pill, StatusDot } from '@/components/primitives';
import type {
  Device,
  WorkbenchProject,
  WorkbenchRemoteDirectoryEntry,
  WorkbenchRemotePathInfo,
  WorkbenchRemoteRoot,
} from '@/lib/types';
import { ChevronRightIcon, FileIcon, FolderIcon, XIcon } from '@/lib/icons';
import {
  canOpenRemoteProjectSelection,
  remoteParentPath,
  sortRemoteDirectoryEntries,
} from '@/lib/workbenchRemoteProjects';
import styles from './WorkbenchRemoteProjectPicker.module.css';

export interface WorkbenchRemoteProjectPickerProps {
  /** 打开成功后的项目 DTO 回调。 */
  onProjectOpened: (project: WorkbenchProject) => void;
  /** 关闭选择器。 */
  onCancel: () => void;
  /** 远端打开请求 pending 状态变化回调，供父级阻止关闭弹窗。 */
  onOpenBusyChange?: (openBusy: boolean) => void;
  /** 可注入的打开实现；默认直接调用 workbenchApi.remote.openProject。 */
  openProject?: (deviceId: string, path: string) => Promise<WorkbenchProject | null>;
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
  | { type: 'openFailed'; error: string };

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
  error: null,
};

/**
 * Business Logic（为什么需要这个函数）:
 *   远端项目选择器同时维护设备、根目录、目录项和路径信息，分散 setState 容易产生 stale 状态。
 *
 * Code Logic（这个函数做什么）:
 *   用 reducer 串联加载、选择、浏览和打开状态；每次切换 device/path 都清理不再匹配的下游数据。
 */
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
      if (state.openBusy) return state;
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
      if (state.openBusy) return state;
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
      if (state.openBusy) return state;
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
      if (state.selectedDeviceId !== action.deviceId || state.selectedPath !== action.path) {
        return state;
      }
      return {
        ...state,
        pathInfo: action.info,
        pathInfoDeviceId: action.deviceId,
        pathInfoLoading: false,
      };
    case 'pathInfoFailed':
      if (state.selectedDeviceId !== action.deviceId || state.selectedPath !== action.path) {
        return state;
      }
      return { ...state, pathInfo: null, pathInfoDeviceId: action.deviceId, pathInfoLoading: false };
    case 'openStarted':
      return { ...state, openBusy: true, error: null };
    case 'openFinished':
      return { ...state, openBusy: false };
    case 'openFailed':
      return { ...state, openBusy: false, error: action.error };
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
 *   使用 devicesApi 与 workbenchApi.remote 分层加载设备、根目录、目录项和路径信息，打开成功后调用 onProjectOpened。
 */
export function WorkbenchRemoteProjectPicker(props: WorkbenchRemoteProjectPickerProps) {
  const { onProjectOpened, onCancel, onOpenBusyChange, openProject } = props;
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
    error,
  } = state;

  const selectedDevice = useMemo(
    () => devices.find((device) => device.id === selectedDeviceId) ?? null,
    [devices, selectedDeviceId],
  );
  const sortedEntries = useMemo(() => sortRemoteDirectoryEntries(entries), [entries]);
  const parentPath = useMemo(() => (currentPath ? remoteParentPath(currentPath) : null), [currentPath]);
  const canOpenSelectedPath = canOpenRemoteProjectSelection(
    selectedDeviceId,
    selectedPath,
    pathInfo,
    pathInfoDeviceId,
    pathInfoLoading,
    openBusy,
  );
  const effectiveOpenProject = useCallback(
    (deviceId: string, path: string) =>
      openProject ? openProject(deviceId, path) : workbenchApi.remote.openProject(deviceId, path),
    [openProject],
  );
  const handleCancel = useCallback(() => {
    if (openBusy) return;
    onCancel();
  }, [onCancel, openBusy]);

  useEffect(() => {
    selectionRef.current = { deviceId: selectedDeviceId, path: selectedPath };
  }, [selectedDeviceId, selectedPath]);

  useEffect(() => {
    return deferEffect(() => {
      let cancelled = false;
      dispatch({ type: 'devicesLoading' });
      void devicesApi
        .list()
        .then((list) => {
          if (cancelled) return;
          dispatch({
            type: 'devicesLoaded',
            devices: list.filter((device) => device.status === 'online'),
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
  }, [t]);

  useEffect(() => {
    return deferEffect(() => {
      if (!selectedDeviceId) return;
      let cancelled = false;
      dispatch({ type: 'rootsLoading' });
      void workbenchApi.remote
        .roots(selectedDeviceId)
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
  }, [selectedDeviceId, t]);

  useEffect(() => {
    return deferEffect(() => {
      if (!selectedDeviceId || !currentPath) return;
      let cancelled = false;
      dispatch({ type: 'entriesLoading' });
      void workbenchApi.remote
        .listDir(selectedDeviceId, currentPath)
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
  }, [currentPath, selectedDeviceId, t]);

  useEffect(() => {
    return deferEffect(() => {
      if (!selectedDeviceId || !selectedPath) return;
      const deviceId = selectedDeviceId;
      const path = selectedPath;
      let cancelled = false;
      dispatch({ type: 'pathInfoLoading', deviceId, path });
      void workbenchApi.remote
        .info(deviceId, path)
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
  }, [selectedDeviceId, selectedPath]);

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
    if (!canOpenSelectedPath || !selectedDeviceId || !selectedPath) return;
    const requestSeq = openRequestSeqRef.current + 1;
    openRequestSeqRef.current = requestSeq;
    const requestDeviceId = selectedDeviceId;
    const requestPath = selectedPath;
    let shouldFinishRequest = true;
    try {
      onOpenBusyChange?.(true);
      dispatch({ type: 'openStarted' });
      const project = await effectiveOpenProject(requestDeviceId, requestPath);
      const currentSelection = selectionRef.current;
      const isCurrentRequest =
        openRequestSeqRef.current === requestSeq &&
        currentSelection.deviceId === requestDeviceId &&
        currentSelection.path === requestPath;
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
    effectiveOpenProject,
    onOpenBusyChange,
    onProjectOpened,
    selectedDeviceId,
    selectedPath,
    t,
  ]);

  return (
    <Card className={styles.picker} variant="elevated" padding="none">
      <Card.Header className={styles.header}>
        <div className={styles.heading}>
          <h2>{t('workbench:remoteProjectPicker.title')}</h2>
          <p>{t('workbench:remoteProjectPicker.subtitle')}</p>
        </div>
        <Button
          variant="icon"
          icon={<XIcon />}
          title={t('workbench:remoteProjectPicker.close')}
          aria-label={t('workbench:remoteProjectPicker.close')}
          disabled={openBusy}
          onClick={handleCancel}
        />
      </Card.Header>

      <Card.Body className={styles.body}>
        {error ? <div className={styles.errorBox}>{error}</div> : null}

        <section className={styles.section} aria-label={t('workbench:remoteProjectPicker.devices')}>
          <div className={styles.sectionHeader}>
            <span>{t('workbench:remoteProjectPicker.devices')}</span>
            {devicesLoading ? <Pill tone="neutral">{t('workbench:loading')}</Pill> : null}
          </div>
          <div className={styles.deviceList}>
            {!devicesLoading && devices.length === 0 ? (
              <div className={styles.empty}>{t('workbench:remoteProjectPicker.noDevices')}</div>
            ) : null}
            {devices.map((device) => (
              <button
                key={device.id}
                type="button"
                className={styles.deviceButton}
                data-active={device.id === selectedDeviceId || undefined}
                disabled={openBusy}
                onClick={() => handleDeviceSelect(device.id)}
              >
                <StatusDot status={device.status} size="sm" />
                <span className={styles.deviceName}>{device.name}</span>
                <span className={styles.deviceAddress}>{device.address}</span>
              </button>
            ))}
          </div>
        </section>

        <section className={styles.section} aria-label={t('workbench:remoteProjectPicker.roots')}>
          <div className={styles.sectionHeader}>
            <span>{t('workbench:remoteProjectPicker.roots')}</span>
            {rootsLoading ? <Pill tone="neutral">{t('workbench:loading')}</Pill> : null}
          </div>
          <div className={styles.rootList}>
            {!rootsLoading && selectedDevice && roots.length === 0 ? (
              <div className={styles.empty}>{t('workbench:remoteProjectPicker.noRoots')}</div>
            ) : null}
            {roots.map((root) => (
              <button
                key={`${root.kind}:${root.path}`}
                type="button"
                className={styles.rootButton}
                data-active={root.path === currentPath || undefined}
                disabled={openBusy}
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
              disabled={!parentPath || openBusy}
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
                    disabled={!isDirectory || openBusy}
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
                      disabled={openBusy}
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

      <Card.Footer className={styles.footer}>
        <Button variant="ghost" disabled={openBusy} onClick={handleCancel}>
          {t('workbench:remoteProjectPicker.close')}
        </Button>
        <Button
          variant="primary"
          loading={openBusy}
          disabled={!canOpenSelectedPath}
          onClick={() => void handleOpenProject()}
        >
          {t('workbench:remoteProjectPicker.openProject')}
        </Button>
      </Card.Footer>
    </Card>
  );
}
