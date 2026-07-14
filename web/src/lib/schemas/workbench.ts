/**
 * Workbench project/worktree/session/path/save/file 运行时 schema。
 *
 * Business Logic（为什么需要这个模块）:
 *   工作台关键 DTO 损坏不得覆盖 active project/worktree 状态或文件保存基线。
 *
 * Code Logic（这个模块做什么）:
 *   解码 Project/Worktree/Session/PathInfo/SaveTextResult/FileNode/OpenFile 关键结构，
 *   以及「继续工作」启动摘要 WorkbenchLaunchSummary 的 fail-closed decoder。
 */

import type {
  MutationIntent,
  MutationKind,
  MutationState,
  MutationTransportClass,
  WorkbenchCsvPreview,
  WorkbenchFileCapabilities,
  WorkbenchFileNode,
  WorkbenchGitStatus,
  WorkbenchImagePreview,
  WorkbenchMergeResult,
  WorkbenchMergeStage,
  WorkbenchMutationEnvelope,
  WorkbenchMutationOperation,
  WorkbenchOpenFile,
  WorkbenchPathInfo,
  WorkbenchProject,
  WorkbenchSaveTextResult,
  WorkbenchSession,
  WorkbenchSqlitePreview,
  WorkbenchTextContent,
  WorkbenchWorktree,
} from '../types/workbench';
import type {
  WorkbenchLaunchDevice,
  WorkbenchLaunchProject,
  WorkbenchLaunchSession,
  WorkbenchLaunchSummaryWire,
  WorkbenchLaunchTask,
  WorkbenchLaunchTransfer,
  WorkbenchLaunchSectionWire,
} from '../types/workbench';
import {
  arrayDecoder,
  booleanDecoder,
  defineDecoder,
  enumDecoder,
  literalDecoder,
  nullableDecoder,
  numberDecoder,
  objectDecoder,
  optionalDecoder,
  stringDecoder,
  unionDecoder,
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

const mutationTransportClassDecoder: Decoder<MutationTransportClass> = enumDecoder(
  'MutationTransportClass',
  ['timeout', 'network'] as const,
);

/**
 * Business Logic（为什么需要这个 decoder）:
 *   mutation 成功通道必须严格区分 succeeded / unknown，损坏 envelope 不得写入 controller。
 *
 * Code Logic（这个 decoder 做什么）:
 *   tag=kind 联合；succeeded 用 valueDecoder 解 value；unknown 可选 transportClass。
 */
export function workbenchMutationEnvelopeDecoder<T>(
  valueDecoder: Decoder<T>,
): Decoder<WorkbenchMutationEnvelope<T>> {
  return unionDecoder<WorkbenchMutationEnvelope<T>>('WorkbenchMutationEnvelope', [
    objectDecoder('WorkbenchMutationEnvelopeSucceeded', {
      kind: literalDecoder('succeeded'),
      value: valueDecoder,
      clientOperationId: stringDecoder,
    }),
    objectDecoder('WorkbenchMutationEnvelopeUnknown', {
      kind: literalDecoder('unknown'),
      clientOperationId: stringDecoder,
      transportClass: optionalDecoder(mutationTransportClassDecoder),
    }),
  ]);
}

const mutationKindDecoder: Decoder<MutationKind> = enumDecoder('MutationKind', [
  'commit',
  'push',
  'merge',
  'remove',
] as const);

const mutationStateDecoder: Decoder<MutationState> = enumDecoder('MutationState', [
  'claimed',
  'running',
  'succeeded',
  'failed',
] as const);

/**
 * Business Logic（为什么需要这个 decoder）:
 *   ledger intent 是 unknown 后对账唯一来源，kind 错位不得进入矩阵。
 *
 * Code Logic（这个 decoder 做什么）:
 *   tag=kind 联合解码 commit/push/merge/remove intent 字段。
 */
export const mutationIntentDecoder: Decoder<MutationIntent> = unionDecoder<MutationIntent>(
  'MutationIntent',
  [
    objectDecoder('MutationIntentCommit', {
      kind: literalDecoder('commit'),
      projectId: stringDecoder,
      worktreeId: stringDecoder,
      beforeHead: nullableDecoder(stringDecoder),
      expectedTree: stringDecoder,
    }),
    objectDecoder('MutationIntentPush', {
      kind: literalDecoder('push'),
      projectId: stringDecoder,
      worktreeId: stringDecoder,
      localRef: stringDecoder,
      remoteRef: stringDecoder,
      localHead: stringDecoder,
    }),
    objectDecoder('MutationIntentMerge', {
      kind: literalDecoder('merge'),
      projectId: stringDecoder,
      sourceWorktreeId: stringDecoder,
      sourceHead: stringDecoder,
      mainHead: stringDecoder,
    }),
    objectDecoder('MutationIntentRemove', {
      kind: literalDecoder('remove'),
      projectId: stringDecoder,
      worktreeId: stringDecoder,
      path: stringDecoder,
      branch: nullableDecoder(stringDecoder),
    }),
  ],
);

/**
 * Business Logic（为什么需要这个 decoder）:
 *   get_workbench_mutation_operation 返回 ledger 行，损坏不得驱动对账。
 *
 * Code Logic（这个 decoder 做什么）:
 *   解码 operation 字段；outcome 允许任意 JSON（unknown）。
 */
export const workbenchMutationOperationDecoder: Decoder<WorkbenchMutationOperation> = objectDecoder(
  'WorkbenchMutationOperation',
  {
    clientOperationId: stringDecoder,
    kind: mutationKindDecoder,
    payloadHash: stringDecoder,
    intent: mutationIntentDecoder,
    state: mutationStateDecoder,
    // outcome 是权威 value JSON，运行时保持 unknown（允许任意 JSON，含 null 由 nullable 处理）。
    outcome: nullableDecoder(defineDecoder<unknown>('MutationOutcomeJson', (value) => value)),
    errorMessage: nullableDecoder(stringDecoder),
    projectId: nullableDecoder(stringDecoder),
    worktreeId: nullableDecoder(stringDecoder),
    createdAt: stringDecoder,
    updatedAt: stringDecoder,
  },
);

const workbenchMergeStageDecoder: Decoder<WorkbenchMergeStage> = objectDecoder(
  'WorkbenchMergeStage',
  {
    id: stringDecoder as Decoder<WorkbenchMergeStage['id']>,
    status: stringDecoder as Decoder<WorkbenchMergeStage['status']>,
    message: stringDecoder,
  },
);

/**
 * Business Logic（为什么需要这个 decoder）:
 *   merge envelope 的 value 是阶段结果，损坏阶段 id/status 不得写入进度条。
 *
 * Code Logic（这个 decoder 做什么）:
 *   解码 ok/worktreeId/stages。
 */
export const workbenchMergeResultDecoder: Decoder<WorkbenchMergeResult> = objectDecoder(
  'WorkbenchMergeResult',
  {
    ok: booleanDecoder,
    worktreeId: stringDecoder,
    stages: arrayDecoder(workbenchMergeStageDecoder),
  },
);

/** remove 成功 value：{ok, worktreeId}。 */
export const workbenchRemoveResultDecoder: Decoder<{ ok: boolean; worktreeId: string }> =
  objectDecoder('WorkbenchRemoveResult', {
    ok: booleanDecoder,
    worktreeId: stringDecoder,
  });

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

/* ---------------------------------------------------------------------------
 * Workbench launch summary（继续工作启动摘要）
 * ------------------------------------------------------------------------- */

/**
 * Business Logic（为什么需要这个 decoder）:
 *   启动摘要 section 是 ready|error 联合；损坏 payload 不得进入 launch surface。
 *
 * Code Logic（这个 decoder 做什么）:
 *   tag=kind 联合；ready 用 itemDecoder 解 value 数组；error 仅 message。
 */
export function workbenchLaunchSectionDecoder<T>(
  name: string,
  itemDecoder: Decoder<T>,
): Decoder<WorkbenchLaunchSectionWire<T>> {
  return unionDecoder<WorkbenchLaunchSectionWire<T>>(name, [
    objectDecoder(`${name}Ready`, {
      kind: literalDecoder('ready'),
      value: arrayDecoder(itemDecoder),
    }),
    objectDecoder(`${name}Error`, {
      kind: literalDecoder('error'),
      message: stringDecoder,
    }),
  ]);
}

const workbenchLaunchProjectItemDecoder: Decoder<WorkbenchLaunchProject> = objectDecoder(
  'WorkbenchLaunchProject',
  {
    id: stringDecoder,
    name: stringDecoder,
    kind: stringDecoder,
    deviceId: stringDecoder,
    deviceName: stringDecoder,
    path: stringDecoder,
    lastOpenedAt: stringDecoder,
  },
);

const workbenchLaunchSessionItemDecoder: Decoder<WorkbenchLaunchSession> = objectDecoder(
  'WorkbenchLaunchSession',
  {
    id: stringDecoder,
    projectId: stringDecoder,
    projectName: stringDecoder,
    worktreeId: optionalDecoder(nullableDecoder(stringDecoder)),
    name: stringDecoder,
    status: stringDecoder,
    startedAt: stringDecoder,
  },
);

const workbenchLaunchTaskItemDecoder: Decoder<WorkbenchLaunchTask> = objectDecoder(
  'WorkbenchLaunchTask',
  {
    id: stringDecoder,
    projectId: stringDecoder,
    projectName: optionalDecoder(nullableDecoder(stringDecoder)),
    title: stringDecoder,
    status: stringDecoder,
    workflowState: stringDecoder,
    runState: stringDecoder,
    updatedAt: stringDecoder,
  },
);

const workbenchLaunchTransferItemDecoder: Decoder<WorkbenchLaunchTransfer> = objectDecoder(
  'WorkbenchLaunchTransfer',
  {
    id: stringDecoder,
    filename: stringDecoder,
    status: stringDecoder,
    direction: stringDecoder,
    progress: optionalDecoder(nullableDecoder(numberDecoder)),
    size: optionalDecoder(nullableDecoder(numberDecoder)),
    updatedAt: optionalDecoder(nullableDecoder(stringDecoder)),
    createdAt: optionalDecoder(nullableDecoder(stringDecoder)),
  },
);

const workbenchLaunchDeviceItemDecoder: Decoder<WorkbenchLaunchDevice> = objectDecoder(
  'WorkbenchLaunchDevice',
  {
    id: stringDecoder,
    name: stringDecoder,
    online: booleanDecoder,
    lastSeen: optionalDecoder(nullableDecoder(stringDecoder)),
    address: optionalDecoder(nullableDecoder(stringDecoder)),
  },
);

/**
 * Business Logic（为什么需要这个 decoder）:
 *   get_workbench_launch_summary 是启动页唯一权威只读源；fail-closed 防止假指标。
 *
 * Code Logic（这个 decoder 做什么）:
 *   解码五 section wire + generatedAt。
 */
export const workbenchLaunchSummaryDecoder: Decoder<WorkbenchLaunchSummaryWire> = objectDecoder(
  'WorkbenchLaunchSummary',
  {
    projects: workbenchLaunchSectionDecoder(
      'WorkbenchLaunchProjects',
      workbenchLaunchProjectItemDecoder,
    ),
    sessions: workbenchLaunchSectionDecoder(
      'WorkbenchLaunchSessions',
      workbenchLaunchSessionItemDecoder,
    ),
    tasks: workbenchLaunchSectionDecoder('WorkbenchLaunchTasks', workbenchLaunchTaskItemDecoder),
    transfers: workbenchLaunchSectionDecoder(
      'WorkbenchLaunchTransfers',
      workbenchLaunchTransferItemDecoder,
    ),
    devices: workbenchLaunchSectionDecoder(
      'WorkbenchLaunchDevices',
      workbenchLaunchDeviceItemDecoder,
    ),
    generatedAt: stringDecoder,
  },
);
