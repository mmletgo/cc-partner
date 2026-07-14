/**
 * Workbench project/worktree/session/path/save 运行时 schema。
 *
 * Business Logic（为什么需要这个模块）:
 *   工作台关键 DTO 损坏不得覆盖 active project/worktree 状态或文件保存基线。
 *
 * Code Logic（这个模块做什么）:
 *   解码 Project/Worktree/Session/PathInfo/SaveTextResult 关键结构。
 */

import type {
  WorkbenchGitStatus,
  WorkbenchPathInfo,
  WorkbenchProject,
  WorkbenchSaveTextResult,
  WorkbenchSession,
  WorkbenchWorktree,
} from '../types/workbench';
import {
  arrayDecoder,
  booleanDecoder,
  nullableDecoder,
  numberDecoder,
  objectDecoder,
  stringDecoder,
  type Decoder,
} from '../runtimeSchema';

/**
 * Business Logic（为什么需要这个 decoder）:
 *   项目列表/打开结果是 Workbench 入口契约。
 *
 * Code Logic（这个 decoder 做什么）:
 *   解码 id/name/kind/device/path/时间戳字段；kind 允许前向 string。
 */
export const workbenchProjectDecoder: Decoder<WorkbenchProject> = objectDecoder(
  'WorkbenchProject',
  {
    id: stringDecoder,
    name: stringDecoder,
    kind: stringDecoder,
    deviceId: stringDecoder,
    deviceName: stringDecoder,
    path: stringDecoder,
    lastOpenedAt: stringDecoder,
    createdAt: stringDecoder,
    updatedAt: stringDecoder,
  },
);

/** 项目列表 decoder。 */
export const workbenchProjectsDecoder: Decoder<WorkbenchProject[]> =
  arrayDecoder(workbenchProjectDecoder);

const gitStatusDecoder: Decoder<WorkbenchGitStatus> = objectDecoder('WorkbenchGitStatus', {
  branch: nullableDecoder(stringDecoder),
  changed: numberDecoder,
  ahead: numberDecoder,
  behind: numberDecoder,
  conflicts: numberDecoder,
  clean: booleanDecoder,
  canPush: booleanDecoder,
});

/**
 * Business Logic（为什么需要这个 decoder）:
 *   worktree strip 与 Git 操作依赖 status 摘要。
 *
 * Code Logic（这个 decoder 做什么）:
 *   解码 worktree 元数据 + nested status。
 */
export const workbenchWorktreeDecoder: Decoder<WorkbenchWorktree> = objectDecoder(
  'WorkbenchWorktree',
  {
    id: stringDecoder,
    projectId: stringDecoder,
    name: stringDecoder,
    branch: nullableDecoder(stringDecoder),
    baseBranch: nullableDecoder(stringDecoder),
    path: stringDecoder,
    isMain: booleanDecoder,
    status: gitStatusDecoder,
    createdAt: stringDecoder,
    updatedAt: stringDecoder,
  },
);

/** worktree 列表 decoder。 */
export const workbenchWorktreesDecoder: Decoder<WorkbenchWorktree[]> = arrayDecoder(
  workbenchWorktreeDecoder,
);

/**
 * Business Logic（为什么需要这个 decoder）:
 *   terminal window tab 元数据驱动 focus/输入。
 *
 * Code Logic（这个 decoder 做什么）:
 *   解码 session 字段；status 允许前向 string。
 */
export const workbenchSessionDecoder: Decoder<WorkbenchSession> = objectDecoder(
  'WorkbenchSession',
  {
    id: stringDecoder,
    projectId: stringDecoder,
    worktreeId: nullableDecoder(stringDecoder),
    name: stringDecoder,
    command: stringDecoder,
    cwd: stringDecoder,
    status: stringDecoder,
    cols: numberDecoder,
    rows: numberDecoder,
    startedAt: stringDecoder,
    exitedAt: nullableDecoder(stringDecoder),
    exitCode: nullableDecoder(numberDecoder),
    supportsPanes: booleanDecoder,
    paneCount: numberDecoder,
  },
);

/** session 列表 decoder。 */
export const workbenchSessionsDecoder: Decoder<WorkbenchSession[]> =
  arrayDecoder(workbenchSessionDecoder);

/**
 * Business Logic（为什么需要这个 decoder）:
 *   路径 info 用于文件树/保存后刷新。
 *
 * Code Logic（这个 decoder 做什么）:
 *   解码 name/path/kind/size/modifiedAt。
 */
export const workbenchPathInfoDecoder: Decoder<WorkbenchPathInfo> = objectDecoder(
  'WorkbenchPathInfo',
  {
    name: stringDecoder,
    path: stringDecoder,
    kind: stringDecoder,
    size: nullableDecoder(numberDecoder),
    modifiedAt: nullableDecoder(stringDecoder),
  },
);

/**
 * Business Logic（为什么需要这个 decoder）:
 *   保存结果提供下一轮 baseHash，损坏会破坏乐观锁。
 *
 * Code Logic（这个 decoder 做什么）:
 *   解码 metadata + baseHash + baseModifiedAt。
 */
export const workbenchSaveTextResultDecoder: Decoder<WorkbenchSaveTextResult> = objectDecoder(
  'WorkbenchSaveTextResult',
  {
    metadata: workbenchPathInfoDecoder,
    baseHash: stringDecoder,
    baseModifiedAt: nullableDecoder(stringDecoder),
  },
);
