import { useCallback, useEffect, useReducer, useRef, useState } from 'react';
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { transferHttp } from '@/api/transferHttp';
import { httpWorkbenchTransport, workbenchHttp } from '@/api/workbenchHttp';
import { Button, Dialog, Drawer, Input } from '@/components/primitives';
import {
  canOpenHostProjectSelection,
  canOpenRemoteProjectSelection,
  isValidBrowseChildName,
  peerSupportsBrowseMkdir,
  remoteParentPath,
  sortRemoteDirectoryEntries,
} from '@/lib/workbenchRemoteProjects';
import type { WorkbenchProject, WorkbenchRemoteDirectoryEntry } from '@/lib/types';
import {
  filterOnlineLanDevices,
  initialMobileProjectPickerState,
  mobileProjectPickerReducer,
} from '../mobileProjectPicker';
import styles from '../MobileWorkbench.module.css';

export type MobileProjectPickerKind = 'local' | 'lan';

export interface MobileProjectPickerProps {
  open: boolean;
  kind: MobileProjectPickerKind;
  onClose: () => void;
  onProjectOpened: (project: WorkbenchProject) => void;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   React lint 禁止 effect 主体同步 setState；picker 仍需要 effect 启动异步加载。
 *
 * Code Logic（这个函数做什么）:
 *   将 effect 内工作延迟到下一轮 macrotask。
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

function errorMessage(error: unknown, fallback: string): string {
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === 'string' && error) return error;
  return fallback;
}

function isRemoteDirectory(entry: WorkbenchRemoteDirectoryEntry): boolean {
  return entry.kind === 'dir';
}

/**
 * MobileProjectPicker（移动端添加本机/局域网项目）
 *
 * Business Logic（为什么需要这个组件）:
 *   手机没有系统目录框，需要在 Drawer 里浏览主机或局域网对端目录并打开为 Workbench 项目。
 *
 * Code Logic（这个组件做什么）:
 *   kind=local 浏览主机 fs；kind=lan 先选在线设备再经 remote HTTP 浏览；打开成功后回调项目 DTO。
 */
export function MobileProjectPicker(props: MobileProjectPickerProps): ReactElement {
  const { open, kind, onClose, onProjectOpened } = props;
  const { t } = useTranslation(['workbench']);
  const [state, dispatch] = useReducer(
    mobileProjectPickerReducer,
    initialMobileProjectPickerState,
  );
  const closeButtonRef = useRef<HTMLButtonElement>(null);
  const {
    mode,
    devices,
    devicesLoading,
    selectedDeviceId,
    currentPath,
    entries,
    entriesLoading,
    selectedPath,
    pathInfo,
    pathInfoPath,
    pathInfoLoading,
    openBusy,
    createBusy,
    createError,
    error,
  } = state;
  const pickerBusy = openBusy || createBusy;
  const [createDialogOpen, setCreateDialogOpen] = useState(false);
  const [createName, setCreateName] = useState('');
  const sortedEntries = sortRemoteDirectoryEntries(entries);
  const parentPath = currentPath ? remoteParentPath(currentPath) : null;
  const canOpen =
    kind === 'local'
      ? canOpenHostProjectSelection(
          selectedPath,
          pathInfo,
          pathInfoPath,
          pathInfoLoading,
          pickerBusy,
        )
      : canOpenRemoteProjectSelection(
          selectedDeviceId,
          selectedPath,
          pathInfo,
          selectedDeviceId,
          pathInfoLoading,
          pickerBusy,
        );
  const selectedLanDevice = devices.find((device) => device.id === selectedDeviceId);
  const browsing = kind === 'local' || mode === 'lan-browse';
  const canCreateFolder =
    browsing &&
    Boolean(currentPath) &&
    !pickerBusy &&
    (kind === 'local' || peerSupportsBrowseMkdir(selectedLanDevice?.capabilities));
  const title =
    kind === 'local'
      ? t('workbench:mobile.projectPanel.pickerLocalTitle')
      : mode === 'lan-devices'
        ? t('workbench:mobile.projectPanel.pickerDevicesTitle')
        : t('workbench:mobile.projectPanel.pickerRemoteTitle');

  useEffect(() => {
    if (!open) {
      dispatch({ type: 'close' });
      return;
    }
    dispatch(kind === 'local' ? { type: 'openLocal' } : { type: 'openLan' });
  }, [kind, open]);

  useEffect(() => {
    if (!open || kind !== 'lan' || mode !== 'lan-devices') return undefined;
    return deferEffect(() => {
      let cancelled = false;
      dispatch({ type: 'devicesLoading' });
      void transferHttp
        .listDevices()
        .then((list) => {
          if (!cancelled) {
            dispatch({ type: 'devicesLoaded', devices: filterOnlineLanDevices(list) });
          }
        })
        .catch((loadError: unknown) => {
          if (!cancelled) {
            dispatch({
              type: 'devicesFailed',
              error: errorMessage(loadError, t('workbench:mobile.projectPanel.errors.devices')),
            });
          }
        });
      return () => {
        cancelled = true;
      };
    });
  }, [kind, mode, open, t]);

  useEffect(() => {
    if (!open) return undefined;
    const browsingLocal = kind === 'local' && mode === 'local';
    const browsingLan = kind === 'lan' && mode === 'lan-browse' && selectedDeviceId;
    if (!browsingLocal && !browsingLan) return undefined;
    return deferEffect(() => {
      let cancelled = false;
      dispatch({ type: 'rootsLoading' });
      const request = browsingLocal
        ? workbenchHttp.fs.roots()
        : workbenchHttp.remote.roots(selectedDeviceId as string);
      void request
        .then((list) => {
          if (!cancelled) dispatch({ type: 'rootsLoaded', roots: list });
        })
        .catch((loadError: unknown) => {
          if (!cancelled) {
            dispatch({
              type: 'rootsFailed',
              error: errorMessage(loadError, t('workbench:mobile.projectPanel.errors.roots')),
            });
          }
        });
      return () => {
        cancelled = true;
      };
    });
  }, [kind, mode, open, selectedDeviceId, t]);

  useEffect(() => {
    if (!open || !currentPath) return undefined;
    const browsingLocal = kind === 'local' && mode === 'local';
    const browsingLan = kind === 'lan' && mode === 'lan-browse' && selectedDeviceId;
    if (!browsingLocal && !browsingLan) return undefined;
    return deferEffect(() => {
      let cancelled = false;
      dispatch({ type: 'entriesLoading' });
      const request = browsingLocal
        ? workbenchHttp.fs.listDir(currentPath)
        : workbenchHttp.remote.listDir(selectedDeviceId as string, currentPath);
      void request
        .then((list) => {
          if (!cancelled) dispatch({ type: 'entriesLoaded', entries: list });
        })
        .catch((loadError: unknown) => {
          if (!cancelled) {
            dispatch({
              type: 'entriesFailed',
              error: errorMessage(loadError, t('workbench:mobile.projectPanel.errors.dir')),
            });
          }
        });
      return () => {
        cancelled = true;
      };
    });
  }, [currentPath, kind, mode, open, selectedDeviceId, t]);

  useEffect(() => {
    if (!open || !selectedPath) return undefined;
    const browsingLocal = kind === 'local' && mode === 'local';
    const browsingLan = kind === 'lan' && mode === 'lan-browse' && selectedDeviceId;
    if (!browsingLocal && !browsingLan) return undefined;
    const path = selectedPath;
    return deferEffect(() => {
      let cancelled = false;
      dispatch({ type: 'pathInfoLoading', path });
      const request = browsingLocal
        ? workbenchHttp.fs.info(path)
        : workbenchHttp.remote.info(selectedDeviceId as string, path);
      void request
        .then((info) => {
          if (!cancelled) dispatch({ type: 'pathInfoLoaded', path, info });
        })
        .catch(() => {
          if (!cancelled) dispatch({ type: 'pathInfoFailed', path });
        });
      return () => {
        cancelled = true;
      };
    });
  }, [kind, mode, open, selectedDeviceId, selectedPath]);

  const handleClose = useCallback(() => {
    if (pickerBusy) return;
    onClose();
  }, [onClose, pickerBusy]);

  const handleCreateFolder = useCallback(async () => {
    const name = createName.trim();
    if (!currentPath || !isValidBrowseChildName(name) || pickerBusy) return;
    if (kind === 'lan' && !selectedDeviceId) return;
    dispatch({ type: 'createStarted' });
    try {
      const created =
        kind === 'local'
          ? await workbenchHttp.fs.createDir(currentPath, name)
          : await workbenchHttp.remote.createDir(selectedDeviceId as string, currentPath, name);
      dispatch({ type: 'createFinished' });
      dispatch({ type: 'pathBrowsed', path: created.path });
      setCreateDialogOpen(false);
      setCreateName('');
    } catch (createErr: unknown) {
      dispatch({
        type: 'createFailed',
        error: errorMessage(createErr, t('workbench:mobile.projectPanel.errors.create')),
      });
    }
  }, [createName, currentPath, kind, pickerBusy, selectedDeviceId, t]);

  const handleOpen = useCallback(async () => {
    if (!canOpen || !selectedPath) return;
    dispatch({ type: 'openStarted' });
    try {
      const project =
        kind === 'local'
          ? await httpWorkbenchTransport.projects.open(selectedPath)
          : await workbenchHttp.remote.openProject(selectedDeviceId as string, selectedPath);
      dispatch({ type: 'openFinished' });
      onProjectOpened(project);
    } catch (openError: unknown) {
      dispatch({
        type: 'openFailed',
        error: errorMessage(openError, t('workbench:mobile.projectPanel.errors.open')),
      });
    }
  }, [canOpen, kind, onProjectOpened, selectedDeviceId, selectedPath, t]);

  const showBrowser = kind === 'local' || mode === 'lan-browse';

  return (
    <Drawer
      open={open}
      titleId="mobile-project-picker-title"
      side="right"
      closeOnEscape={!pickerBusy}
      closeOnBackdrop={!pickerBusy}
      onClose={handleClose}
      initialFocusRef={closeButtonRef}
      className={styles.pickerDrawer}
    >
      <div className={styles.pickerHeader}>
        <h2 id="mobile-project-picker-title">{title}</h2>
        <Button ref={closeButtonRef} variant="ghost" size="sm" disabled={pickerBusy} onClick={handleClose}>
          {t('workbench:mobile.projectPanel.pickerCancel')}
        </Button>
      </div>
      <div className={styles.pickerBody}>
        {error ? <p className={styles.panelError}>{error}</p> : null}
        {kind === 'lan' && mode === 'lan-devices' ? (
          <>
            {devicesLoading ? <p className={styles.panelState}>{t('workbench:loading')}</p> : null}
            {!devicesLoading && devices.length === 0 ? (
              <p className={styles.panelState}>
                {t('workbench:mobile.projectPanel.pickerEmptyDevices')}
              </p>
            ) : null}
            {devices.map((device) => (
              <button
                key={device.id}
                type="button"
                className={styles.mobileListItem}
                disabled={pickerBusy}
                onClick={() => dispatch({ type: 'deviceSelected', deviceId: device.id })}
              >
                <strong className={styles.mobileListTitle}>{device.name}</strong>
                <span className={styles.mobileListPath}>{device.address}</span>
              </button>
            ))}
          </>
        ) : null}
        {showBrowser ? (
          <>
            <p className={styles.pickerPath}>{currentPath ?? t('workbench:emptyValue')}</p>
            <Button
              variant="ghost"
              size="sm"
              disabled={!parentPath || pickerBusy}
              onClick={() => {
                if (parentPath) dispatch({ type: 'pathBrowsed', path: parentPath });
              }}
            >
              {t('workbench:mobile.projectPanel.pickerParent')}
            </Button>
            {entriesLoading ? <p className={styles.panelState}>{t('workbench:loading')}</p> : null}
            {!entriesLoading && sortedEntries.length === 0 ? (
              <p className={styles.panelState}>{t('workbench:mobile.projectPanel.pickerEmptyDir')}</p>
            ) : null}
            {sortedEntries.map((entry) => {
              const dir = isRemoteDirectory(entry);
              return (
                <button
                  key={entry.path}
                  type="button"
                  className={styles.mobileListItem}
                  disabled={!dir || pickerBusy}
                  onClick={() => {
                    if (!dir) return;
                    dispatch({ type: 'pathBrowsed', path: entry.path });
                  }}
                >
                  <span className={styles.mobileListTitleRow}>
                    <strong className={styles.mobileListTitle}>{entry.name}</strong>
                    {entry.isGitRepo ? (
                      <span className={styles.pickerMeta}>
                        {t('workbench:mobile.projectPanel.pickerGit')}
                      </span>
                    ) : null}
                  </span>
                  <span className={styles.mobileListPath}>{entry.path}</span>
                </button>
              );
            })}
          </>
        ) : null}
      </div>
      {showBrowser ? (
        <div className={styles.pickerFooter}>
          {canCreateFolder ? (
            <Button
              variant="secondary"
              disabled={pickerBusy}
              onClick={() => {
                setCreateName('');
                setCreateDialogOpen(true);
              }}
            >
              {t('workbench:mobile.projectPanel.createFolder')}
            </Button>
          ) : null}
          <Button variant="primary" disabled={!canOpen} loading={openBusy} onClick={() => void handleOpen()}>
            {t('workbench:mobile.projectPanel.pickerOpen')}
          </Button>
        </div>
      ) : null}
      <Dialog
        open={createDialogOpen}
        titleId="mobile-browse-create-dir-title"
        onClose={() => {
          if (createBusy) return;
          setCreateDialogOpen(false);
        }}
        closeOnEscape={!createBusy}
        closeOnBackdrop={!createBusy}
      >
        <h2 id="mobile-browse-create-dir-title">{t('workbench:mobile.projectPanel.createFolder')}</h2>
        <Input
          value={createName}
          onChange={(event) => setCreateName(event.target.value)}
          placeholder={t('workbench:mobile.projectPanel.createFolderPlaceholder')}
          disabled={createBusy}
        />
        {createError ? <p className={styles.panelError}>{createError}</p> : null}
        <div className={styles.pickerFooter}>
          <Button variant="ghost" disabled={createBusy} onClick={() => setCreateDialogOpen(false)}>
            {t('workbench:mobile.projectPanel.pickerCancel')}
          </Button>
          <Button
            variant="primary"
            loading={createBusy}
            disabled={!isValidBrowseChildName(createName) || createBusy}
            onClick={() => void handleCreateFolder()}
          >
            {t('workbench:mobile.projectPanel.createFolderConfirm')}
          </Button>
        </div>
      </Dialog>
    </Drawer>
  );
}
