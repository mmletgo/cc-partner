/**
 * Workbench project/worktree/session/path/save/file 运行时 schema。
 *
 * Business Logic（为什么需要这个模块）:
 *   工作台关键 DTO 损坏不得覆盖 active project/worktree 状态或文件保存基线。
 *
 * Code Logic（这个模块做什么）:
 *   解码 Project/Worktree/Session/PathInfo/SaveTextResult/FileNode/OpenFile 关键结构。
 */

import type {
  WorkbenchCsvPreview,
  WorkbenchFileCapabilities,
  WorkbenchFileNode,
  WorkbenchGitStatus,
  WorkbenchImagePreview,
  WorkbenchOpenFile,
  WorkbenchPathInfo,
  WorkbenchProject,
  WorkbenchSaveTextResult,
  WorkbenchSession,
  WorkbenchSqlitePreview,
  WorkbenchTextContent,
  WorkbenchWorktree,
} from '../types/workbench';
import {
  arrayDecoder,
  booleanDecoder,
  nullableDecoder,
  numberDecoder,
  objectDecoder,
  optionalDecoder,
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

/**
 * Business Logic（为什么需要这个 decoder）:
 *   文件树叶子节点只需必填 metadata，避免深度 children 递归膨胀。
 *
 * Code Logic（这个 decoder 做什么）:
 *   校验 name/path/kind/size/modifiedAt；不解码嵌套 children。
 */
const workbenchFileNodeLeafDecoder: Decoder<WorkbenchFileNode> = objectDecoder(
  'WorkbenchFileNodeLeaf',
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
 *   目录列表写入文件树前必须拒绝残缺节点，避免 path/kind 污染展开态。
 *
 * Code Logic（这个 decoder 做什么）:
 *   校验必填 metadata；children 仅一层浅校验（叶子无再嵌套 children）。
 */
export const workbenchFileNodeDecoder: Decoder<WorkbenchFileNode> = objectDecoder(
  'WorkbenchFileNode',
  {
    name: stringDecoder,
    path: stringDecoder,
    kind: stringDecoder,
    size: nullableDecoder(numberDecoder),
    modifiedAt: nullableDecoder(stringDecoder),
    children: optionalDecoder(nullableDecoder(arrayDecoder(workbenchFileNodeLeafDecoder))),
  },
);

/** 文件节点列表 decoder。 */
export const workbenchFileNodesDecoder: Decoder<WorkbenchFileNode[]> = arrayDecoder(
  workbenchFileNodeDecoder,
);

const workbenchFileCapabilitiesDecoder: Decoder<WorkbenchFileCapabilities> = objectDecoder(
  'WorkbenchFileCapabilities',
  {
    canPreview: booleanDecoder,
    canEdit: booleanDecoder,
    canFormat: booleanDecoder,
    mustValidateBeforeSave: booleanDecoder,
    // 前向兼容 string，再收敛为 WorkbenchFileMode 联合。
    defaultMode: stringDecoder as Decoder<WorkbenchFileCapabilities['defaultMode']>,
    availableModes: arrayDecoder(stringDecoder) as Decoder<
      WorkbenchFileCapabilities['availableModes']
    >,
  },
);

const workbenchTextContentDecoder: Decoder<WorkbenchTextContent> = objectDecoder(
  'WorkbenchTextContent',
  {
    content: stringDecoder,
    baseHash: stringDecoder,
    baseModifiedAt: nullableDecoder(stringDecoder),
  },
);

const workbenchImagePreviewDecoder: Decoder<WorkbenchImagePreview> = objectDecoder(
  'WorkbenchImagePreview',
  {
    dataUrl: stringDecoder,
    mime: stringDecoder,
    width: nullableDecoder(numberDecoder),
    height: nullableDecoder(numberDecoder),
  },
);

const workbenchCsvPreviewDecoder: Decoder<WorkbenchCsvPreview> = objectDecoder(
  'WorkbenchCsvPreview',
  {
    columns: arrayDecoder(stringDecoder),
    rows: arrayDecoder(arrayDecoder(stringDecoder)),
    truncated: booleanDecoder,
  },
);

const workbenchSqlitePreviewDecoder: Decoder<WorkbenchSqlitePreview> = objectDecoder(
  'WorkbenchSqlitePreview',
  {
    tables: arrayDecoder(stringDecoder),
    selectedTable: nullableDecoder(stringDecoder),
    columns: arrayDecoder(stringDecoder),
    rows: arrayDecoder(arrayDecoder(stringDecoder)),
    truncated: booleanDecoder,
  },
);

/**
 * Business Logic（为什么需要这个 decoder）:
 *   打开文件结果驱动 tab 能力与保存基线，损坏 payload 不得写入编辑器状态。
 *
 * Code Logic（这个 decoder 做什么）:
 *   解码 metadata/detectedType/capabilities 与可空 text/image/csv/sqlite 载荷。
 */
export const workbenchOpenFileDecoder: Decoder<WorkbenchOpenFile> = objectDecoder(
  'WorkbenchOpenFile',
  {
    metadata: workbenchPathInfoDecoder,
    // 前向兼容 string，再收敛为 WorkbenchDetectedFileType 联合。
    detectedType: stringDecoder as Decoder<WorkbenchOpenFile['detectedType']>,
    capabilities: workbenchFileCapabilitiesDecoder,
    text: nullableDecoder(workbenchTextContentDecoder),
    image: nullableDecoder(workbenchImagePreviewDecoder),
    csv: nullableDecoder(workbenchCsvPreviewDecoder),
    sqlite: nullableDecoder(workbenchSqlitePreviewDecoder),
    truncated: booleanDecoder,
    notice: nullableDecoder(stringDecoder),
  },
);
