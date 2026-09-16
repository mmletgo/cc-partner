/**
 * 同仓库跨设备主分支同步的纯规划。
 *
 * Business Logic（为什么需要这个模块）:
 *   Git 历史「同步」要把当前仓库主分支推到 origin，再在其他设备的主工作区拉取；
 *   目标集合必须与侧栏分组同一套 fingerprint，且排除本机其它 clone。
 *
 * Code Logic（这个模块做什么）:
 *   选出其他设备上的同 fingerprint 项目、主 worktree，以及按钮是否可点。
 */

import type { WorkbenchProject, WorkbenchWorktree } from './types';
import { projectGroupKey } from './workbenchProjectGroups';

/**
 * Business Logic（为什么需要这个函数）:
 *   同步只打「别的设备」上的同一仓库，避免把同机第二条 clone 当成远端。
 *
 * Code Logic（这个函数做什么）:
 *   fingerprint 为空则无兄弟；否则同 group key、不同 deviceId、排除自身。
 */
export function siblingProjectsOnOtherDevices(
  current: WorkbenchProject | null,
  projects: WorkbenchProject[],
): WorkbenchProject[] {
  if (!current) return [];
  const key = projectGroupKey(current);
  if (!key.startsWith('fp:')) return [];
  return projects.filter(
    (project) =>
      project.id !== current.id
      && project.deviceId !== current.deviceId
      && projectGroupKey(project) === key,
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   同步对象是仓库主工作区，不是当前功能分支 worktree。
 *
 * Code Logic（这个函数做什么）:
 *   返回 isMain 的第一项，没有则 null。
 */
export function pickMainWorktree(
  worktrees: WorkbenchWorktree[],
): WorkbenchWorktree | null {
  return worktrees.find((worktree) => worktree.isMain) ?? null;
}

export interface CanSyncProjectMainInput {
  mainWorktree: WorkbenchWorktree | null;
  siblingCount: number;
  worktreeBusy: string | null;
  unknownMutationLock?: { kind: string } | null;
  remoteWriteDisabled: boolean;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   没有其他设备、主分支不能 push、busy 或远端只读时，同步按钮必须禁用。
 *
 * Code Logic（这个函数做什么）:
 *   siblingCount>0 且 canPushWorktree(main) 且未 remoteWriteDisabled。
 */
export function canSyncProjectMain(input: CanSyncProjectMainInput): boolean {
  if (input.remoteWriteDisabled) return false;
  if (input.siblingCount <= 0) return false;
  if (input.worktreeBusy !== null) return false;
  if (input.unknownMutationLock) return false;
  const main = input.mainWorktree;
  return Boolean(main?.branch && main.status.canPush);
}
