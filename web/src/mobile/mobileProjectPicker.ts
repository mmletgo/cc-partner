import type { MobileTransferDevice } from '@/api/transferHttp';
import type {
  WorkbenchRemoteDirectoryEntry,
  WorkbenchRemotePathInfo,
  WorkbenchRemoteRoot,
} from '@/lib/types';

export type MobileProjectPickerMode = 'closed' | 'local' | 'lan-devices' | 'lan-browse';

export interface MobileProjectPickerState {
  mode: MobileProjectPickerMode;
  devices: MobileTransferDevice[];
  devicesLoading: boolean;
  selectedDeviceId: string | null;
  roots: WorkbenchRemoteRoot[];
  rootsLoading: boolean;
  currentPath: string | null;
  entries: WorkbenchRemoteDirectoryEntry[];
  entriesLoading: boolean;
  selectedPath: string | null;
  pathInfo: WorkbenchRemotePathInfo | null;
  pathInfoPath: string | null;
  pathInfoLoading: boolean;
  openBusy: boolean;
  error: string | null;
}

export type MobileProjectPickerAction =
  | { type: 'openLocal' }
  | { type: 'openLan' }
  | { type: 'close' }
  | { type: 'devicesLoading' }
  | { type: 'devicesLoaded'; devices: MobileTransferDevice[] }
  | { type: 'devicesFailed'; error: string }
  | { type: 'deviceSelected'; deviceId: string }
  | { type: 'rootsLoading' }
  | { type: 'rootsLoaded'; roots: WorkbenchRemoteRoot[] }
  | { type: 'rootsFailed'; error: string }
  | { type: 'pathBrowsed'; path: string }
  | { type: 'entriesLoading' }
  | { type: 'entriesLoaded'; entries: WorkbenchRemoteDirectoryEntry[] }
  | { type: 'entriesFailed'; error: string }
  | { type: 'entrySelected'; path: string }
  | { type: 'pathInfoLoading'; path: string }
  | { type: 'pathInfoLoaded'; path: string; info: WorkbenchRemotePathInfo }
  | { type: 'pathInfoFailed'; path: string }
  | { type: 'openStarted' }
  | { type: 'openFinished' }
  | { type: 'openFailed'; error: string };

export const initialMobileProjectPickerState: MobileProjectPickerState = {
  mode: 'closed',
  devices: [],
  devicesLoading: false,
  selectedDeviceId: null,
  roots: [],
  rootsLoading: false,
  currentPath: null,
  entries: [],
  entriesLoading: false,
  selectedPath: null,
  pathInfo: null,
  pathInfoPath: null,
  pathInfoLoading: false,
  openBusy: false,
  error: null,
};

/**
 * Business Logic（为什么需要这个函数）:
 *   局域网添加入口必须排除主机自己和离线设备，避免把本机当对端打开。
 *
 * Code Logic（这个函数做什么）:
 *   保留 status=online 且 isSelf 不为 true 的设备。
 */
export function filterOnlineLanDevices(
  devices: MobileTransferDevice[],
): MobileTransferDevice[] {
  return devices.filter((device) => device.status === 'online' && device.isSelf !== true);
}

function clearBrowseFields(): Pick<
  MobileProjectPickerState,
  | 'currentPath'
  | 'entries'
  | 'selectedPath'
  | 'pathInfo'
  | 'pathInfoPath'
  | 'pathInfoLoading'
> {
  return {
    currentPath: null,
    entries: [],
    selectedPath: null,
    pathInfo: null,
    pathInfoPath: null,
    pathInfoLoading: false,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   本机浏览与局域网浏览共用一套目录状态，分散 setState 会在切设备/路径时留下 stale 数据。
 *
 * Code Logic（这个函数做什么）:
 *   用 reducer 串联打开模式、设备、根目录、目录项和打开忙态；openBusy 时忽略选择变更。
 */
export function mobileProjectPickerReducer(
  state: MobileProjectPickerState,
  action: MobileProjectPickerAction,
): MobileProjectPickerState {
  switch (action.type) {
    case 'openLocal':
      return {
        ...initialMobileProjectPickerState,
        mode: 'local',
      };
    case 'openLan':
      return {
        ...initialMobileProjectPickerState,
        mode: 'lan-devices',
        devicesLoading: true,
      };
    case 'close':
      if (state.openBusy) return state;
      return { ...initialMobileProjectPickerState };
    case 'devicesLoading':
      return { ...state, devicesLoading: true, error: null };
    case 'devicesLoaded':
      return {
        ...state,
        devices: action.devices,
        devicesLoading: false,
      };
    case 'devicesFailed':
      return { ...state, devicesLoading: false, error: action.error };
    case 'deviceSelected':
      if (state.openBusy) return state;
      return {
        ...state,
        mode: 'lan-browse',
        selectedDeviceId: action.deviceId,
        roots: [],
        rootsLoading: false,
        ...clearBrowseFields(),
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
        pathInfoPath: null,
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
        selectedPath: firstPath,
        entries: [],
        pathInfo: null,
        pathInfoPath: null,
        pathInfoLoading: false,
      };
    }
    case 'rootsFailed':
      return { ...state, rootsLoading: false, error: action.error };
    case 'pathBrowsed':
      if (state.openBusy) return state;
      return {
        ...state,
        currentPath: action.path,
        selectedPath: action.path,
        entries: [],
        pathInfo: null,
        pathInfoPath: null,
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
        pathInfoPath: null,
        pathInfoLoading: false,
        error: null,
      };
    case 'pathInfoLoading':
      return {
        ...state,
        pathInfo: null,
        pathInfoPath: action.path,
        pathInfoLoading: true,
      };
    case 'pathInfoLoaded':
      if (state.selectedPath !== action.path) return state;
      return {
        ...state,
        pathInfo: action.info,
        pathInfoPath: action.path,
        pathInfoLoading: false,
      };
    case 'pathInfoFailed':
      if (state.selectedPath !== action.path) return state;
      return { ...state, pathInfo: null, pathInfoPath: action.path, pathInfoLoading: false };
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
