import type {
  WorkbenchProject,
  WorkbenchRemoteDirectoryEntry,
  WorkbenchRemotePathInfo,
} from './types';

/**
 * Business Logic（为什么需要这个函数）:
 *   本机和远端项目都进入同一侧栏最近项目列表，打开远端项目后应立即置顶且不重复。
 *
 * Code Logic（这个函数做什么）:
 *   按 project.id 移除旧项，再把最新项目 DTO 插入数组开头；不修改传入数组。
 */
export function insertWorkbenchProjectAtTop(
  projects: WorkbenchProject[],
  project: WorkbenchProject,
): WorkbenchProject[] {
  return [project, ...projects.filter((item) => item.id !== project.id)];
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
