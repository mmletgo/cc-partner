import { classifyTransportFault } from './faultRecovery';
import type {
  WorkbenchProject,
  WorkbenchRemoteDirectoryEntry,
  WorkbenchRemotePathInfo,
} from './types';

/** 后端固定中文离线文案（legacy 兼容，优先仍走 typed NETWORK_OFFLINE）。 */
const REMOTE_WORKBENCH_OFFLINE_ERROR = '远端设备不在线';

/**
 * Business Logic（为什么需要这个函数）:
 *   侧栏项目列表是用户的空间记忆锚点：选中或重新打开已有项目时不得把它挪到顶部，
 *   否则用户每次点击都要重新寻找相邻项目。只有真正新增的项目才追加到顶部
 *   （与后端 list 的 created_at DESC 顺序一致）。
 *
 * Code Logic（这个函数做什么）:
 *   已存在同 id 时按原索引就地替换为最新 DTO；不存在时插入数组开头。不修改传入数组。
 */
export function upsertWorkbenchProjectInPlace(
  projects: WorkbenchProject[],
  project: WorkbenchProject,
): WorkbenchProject[] {
  const index = projects.findIndex((item) => item.id === project.id);
  if (index < 0) return [project, ...projects];
  const next = [...projects];
  next[index] = project;
  return next;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   侧栏拖拽需要把源项目插到目标项目之前或之后，生成新的 id 顺序。
 *
 * Code Logic（这个函数做什么）:
 *   从 ids 移除 sourceId，再插入到 target 前（before）或后（after）；id 缺失时返回原数组副本。
 */
export function moveProjectId(
  ids: string[],
  sourceId: string,
  targetId: string,
  position: 'before' | 'after',
): string[] {
  if (sourceId === targetId) return [...ids];
  const from = ids.indexOf(sourceId);
  const to = ids.indexOf(targetId);
  if (from < 0 || to < 0) return [...ids];
  const next = [...ids];
  next.splice(from, 1);
  const targetIndex = next.indexOf(targetId);
  if (targetIndex < 0) return [...ids];
  next.splice(position === 'before' ? targetIndex : targetIndex + 1, 0, sourceId);
  return next;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   拖拽乐观更新 / 后端回写需要把 id 顺序投影到项目对象列表。
 *
 * Code Logic（这个函数做什么）:
 *   按 orderedIds 收集存在的项目，再把未出现的项目按原序追加。
 */
export function orderProjectsByIds(
  projects: WorkbenchProject[],
  orderedIds: string[],
): WorkbenchProject[] {
  const byId = new Map(projects.map((project) => [project.id, project]));
  const next: WorkbenchProject[] = [];
  const seen = new Set<string>();
  for (const id of orderedIds) {
    const project = byId.get(id);
    if (project && !seen.has(id)) {
      next.push(project);
      seen.add(id);
    }
  }
  for (const project of projects) {
    if (!seen.has(project.id)) next.push(project);
  }
  return next;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   远端目录选择器需要提供上一级导航，并兼容 macOS/Linux 与 Windows 设备路径。
 *
 * Code Logic（这个函数做什么）:
 *   根据路径中最后出现的分隔符判断 Unix 或 Windows 风格；根目录或盘符根返回 null。
 */
export function remoteParentPath(path: string): string | null {
  const trimmed = path.trim();
  if (!trimmed) return null;

  const lastSlash = trimmed.lastIndexOf('/');
  const lastBackslash = trimmed.lastIndexOf('\\');
  if (lastBackslash > lastSlash) {
    const withoutTrailing = trimmed.replace(/\\+$/, '');
    if (/^[A-Za-z]:$/.test(withoutTrailing) || /^[A-Za-z]:\\?$/.test(trimmed)) return null;
    const parentIndex = withoutTrailing.lastIndexOf('\\');
    if (parentIndex < 0) return null;
    const parent = withoutTrailing.slice(0, parentIndex);
    return /^[A-Za-z]:$/.test(parent) ? `${parent}\\` : parent;
  }

  if (trimmed === '/') return null;
  const withoutTrailing = trimmed.replace(/\/+$/, '');
  if (withoutTrailing === '') return null;
  const parentIndex = withoutTrailing.lastIndexOf('/');
  if (parentIndex < 0) return null;
  if (parentIndex === 0) return '/';
  return withoutTrailing.slice(0, parentIndex);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用户浏览远端目录时优先继续进入文件夹，文件只作为上下文参考展示。
 *
 * Code Logic（这个函数做什么）:
 *   返回目录优先、名称升序的新数组；排序不改变原始 entries。
 */
export function sortRemoteDirectoryEntries(
  entries: WorkbenchRemoteDirectoryEntry[],
): WorkbenchRemoteDirectoryEntry[] {
  return [...entries].sort((left, right) => {
    const leftRank = left.kind === 'dir' ? 0 : 1;
    const rightRank = right.kind === 'dir' ? 0 : 1;
    if (leftRank !== rightRank) return leftRank - rightRank;
    return left.name.localeCompare(right.name, undefined, {
      numeric: true,
      sensitivity: 'base',
    });
  });
}

/**
 * Business Logic（为什么需要这个函数）:
 *   远端项目打开前必须确认当前路径信息仍匹配用户选中的远端目录，避免 stale 请求打开旧路径或不可读文件。
 *
 * Code Logic（这个函数做什么）:
 *   校验 device/path/pathInfo 一致、路径是可读目录，并且没有路径信息请求或打开请求正在进行。
 */
export function canOpenRemoteProjectSelection(
  selectedDeviceId: string | null,
  selectedPath: string | null,
  pathInfo: WorkbenchRemotePathInfo | null,
  pathInfoDeviceId: string | null,
  pathInfoLoading: boolean,
  openBusy: boolean,
): boolean {
  return Boolean(
    selectedDeviceId &&
      selectedPath &&
      pathInfo &&
      pathInfoDeviceId === selectedDeviceId &&
      pathInfo.path === selectedPath &&
      pathInfo.kind === 'dir' &&
      pathInfo.readable &&
      !pathInfoLoading &&
      !openBusy,
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   手机添加本机项目没有 deviceId，打开前仍必须确认当前选中路径是可读目录。
 *
 * Code Logic（这个函数做什么）:
 *   校验 path/pathInfo 一致、路径是可读目录，并且没有路径信息或打开请求在途。
 */
export function canOpenHostProjectSelection(
  selectedPath: string | null,
  pathInfo: WorkbenchRemotePathInfo | null,
  pathInfoPath: string | null,
  pathInfoLoading: boolean,
  openBusy: boolean,
): boolean {
  return Boolean(
    selectedPath &&
      pathInfo &&
      pathInfoPath === selectedPath &&
      pathInfo.path === selectedPath &&
      pathInfo.kind === 'dir' &&
      pathInfo.readable &&
      !pathInfoLoading &&
      !openBusy,
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   远端设备掉线时，前端需要识别它来展示离线提示并禁用远端写操作；
 *   优先 typed 故障码，中文固定文案仅作 backend legacy 回退。
 *
 * Code Logic（这个函数做什么）:
 *   先用 classifyTransportFault 判断 NETWORK_OFFLINE / networkOffline；
 *   否则从 Error/message/string 中匹配后端固定中文「远端设备不在线」。
 */
export function isRemoteWorkbenchOfflineError(error: unknown): boolean {
  const classification = classifyTransportFault(error);
  if (
    classification.code === 'NETWORK_OFFLINE' ||
    classification.kind === 'networkOffline'
  ) {
    return true;
  }
  const message =
    error instanceof Error
      ? error.message
      : typeof error === 'string'
        ? error
        : error === null || error === undefined
          ? ''
          : String(error);
  return message.includes(REMOTE_WORKBENCH_OFFLINE_ERROR);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 需要把“当前远端项目离线”作为页面级状态，而不是影响本机项目或其他远端项目。
 *
 * Code Logic（这个函数做什么）:
 *   当前项目为 remote 且 id 匹配离线项目 id 时返回 true。
 */
export function isRemoteWorkbenchProjectOffline(
  project: WorkbenchProject | null | undefined,
  offlineProjectId: string | null,
): boolean {
  return Boolean(project?.kind === 'remote' && offlineProjectId && project.id === offlineProjectId);
}
