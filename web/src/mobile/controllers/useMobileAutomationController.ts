import { useCallback, useEffect, useId, useMemo, useRef, useState } from 'react';
import type { RefObject } from 'react';
import { useTranslation } from 'react-i18next';
import { toRuntimeLoadError } from '@/api/orchestratorRuntimeTransportError';
import { transferHttp } from '@/api/transferHttp';
import {
  createHttpOrchestratorClientRequestId,
  httpOrchestratorTransport,
} from '@/api/workbenchHttp';
import type { HttpCreateOrchestratorTaskAction } from '@/api/workbenchHttp';
import {
  canCreateOrchestratorTaskBlock,
  type OrchestratorPeerProtocolHint,
} from '@/lib/orchestratorCapabilities';
import { requestAttentionInvalidation } from '@/hooks/attentionInvalidation';
import {
  splitOrchestratorTaskViews,
  upsertOrchestratorTaskBlockCreated,
  upsertOrchestratorTaskView,
} from '@/lib/orchestratorRemote';
import type { OrchestratorRenderableTask } from '@/lib/orchestratorRemote';
import {
  groupBoardItems,
  MAX_ORCHESTRATOR_BLOCK_MEMBERS,
  MIN_ORCHESTRATOR_BLOCK_MEMBERS,
  type OrchestratorBoardGroups,
} from '@/pages/Orchestrator/orchestratorBoard';
import type { OrchestratorCreateForm } from '@/pages/Orchestrator/orchestratorViewHelpers';
import { EMPTY_ORCHESTRATOR_CREATE_FORM, emptyOrchestratorBlockMembers } from '@/pages/Orchestrator/orchestratorViewHelpers';
import type { OrchestratorCreateMode } from '@/pages/Orchestrator/views/OrchestratorBoard';
import type { OrchestratorCreateDialogKind } from '@/pages/Orchestrator/views/OrchestratorCreateDialog';
import type {
  OrchestratorAttemptPhase,
  OrchestratorEvidence,
  OrchestratorRemoteOutboxItem,
  OrchestratorRemoteOutboxStatus,
  OrchestratorRemoteRuntimeStatus,
  OrchestratorRunState,
  OrchestratorRuntimeDisplayState,
  OrchestratorTask,
  OrchestratorTaskView,
  OrchestratorWorkflowState,
  WorkbenchProject,
} from '@/lib/types';
import {
  applyMobileRuntimeSnapshotFailure,
  applyMobileRuntimeSnapshotSuccess,
  beginMobileRuntimeSnapshotLoad,
  emptyMobileRuntimeDisplayState,
  nextMobileRuntimeSnapshotRequestSeq,
  resetMobileRuntimeSnapshotStore,
  selectMobileRuntimeDisplayForProject,
  type OwnedMobileRuntimeDisplayState,
} from '../mobileRuntimeSnapshotStore';
import { useAutoDismissedStatus } from '../mobileTransientStatus';

/**
 * Business Logic（为什么需要这个类型）:
 *   详情「打开执行现场」需要把 task 绑定的 worktree/session 交给 MobileWorkbench 现有 terminal 面板。
 *
 * Code Logic（这个类型做什么）:
 *   描述一次从自动化详情跳转到终端执行现场所需的 project/worktree/session 标识。
 */
export interface MobileAutomationExecutionContext {
  projectId: string;
  worktreeId: string | null;
  sessionId: string | null;
}

/**
 * Business Logic（为什么需要这个类型）:
 *   移动端自动化面板由 MobileWorkbench 注入项目上下文与 Attention 聚焦回调。
 *
 * Code Logic（这个类型做什么）:
 *   定义面板入参：active project、执行现场打开回调，以及 task/outbox focus 与结果回报。
 */
export interface MobileAutomationPanelProps {
  project: WorkbenchProject | null;
  onOpenExecutionContext?: (context: MobileAutomationExecutionContext) => void;
  /** Attention 跳转：聚焦真实任务详情/Evidence。 */
  focusTaskId?: string | null;
  /** Attention 跳转：聚焦 failed outbox 行。 */
  focusOutboxId?: string | null;
  /** 聚焦结果回调：missing 时父级刷新 Inbox 并回到 Attention。 */
  onFocusResult?: (result: {
    status: 'found' | 'missing';
    entity: 'task' | 'outbox';
    id: string;
  }) => void;
}

/**
 * Business Logic（为什么需要这个类型）:
 *   controller 入参与面板 props 对齐，便于 Panel 直接透传。
 *
 * Code Logic（这个类型做什么）:
 *   复用 MobileAutomationPanelProps 作为 hook 参数别名。
 */
export type UseMobileAutomationControllerParams = MobileAutomationPanelProps;

export type MobileAutomationTaskGroups = OrchestratorBoardGroups;

export interface MobileAutomationCreateActionConfig {
  createAction: HttpCreateOrchestratorTaskAction;
  labelKey: MobileAutomationCreateActionLabelKey;
  statusKey: MobileAutomationCreateActionStatusKey;
}

export type MobileAutomationCreateActionLabelKey =
  | 'workbench:mobile.automationPanel.createBacklog'
  | 'workbench:mobile.automationPanel.createTodo'
  | 'workbench:mobile.automationPanel.createStart';

export type MobileAutomationCreateActionStatusKey =
  | 'workbench:mobile.automationPanel.createdBacklog'
  | 'workbench:mobile.automationPanel.createdTodo'
  | 'workbench:mobile.automationPanel.createdStart';

type MobileAutomationEvidenceKindLabelKey =
  | 'orchestrator:evidence.kind.developmentAttempt'
  | 'orchestrator:evidence.kind.verificationOutput'
  | 'orchestrator:evidence.kind.verificationReview'
  | 'orchestrator:evidence.kind.repairPrompt'
  | 'orchestrator:evidence.kind.remoteOutbox'
  | 'orchestrator:evidence.kind.delivery'
  | 'orchestrator:evidence.kind.generic';

export const MOBILE_AUTOMATION_WORKFLOW_STATES: readonly OrchestratorWorkflowState[] = [
  'backlog',
  'todo',
  'inProgress',
  'humanReview',
  'rework',
  'merging',
  'done',
  'canceled',
];

export const MOBILE_AUTOMATION_CREATE_ACTIONS: readonly MobileAutomationCreateActionConfig[] = [
  {
    createAction: 'backlog',
    labelKey: 'workbench:mobile.automationPanel.createBacklog',
    statusKey: 'workbench:mobile.automationPanel.createdBacklog',
  },
  {
    createAction: 'todo',
    labelKey: 'workbench:mobile.automationPanel.createTodo',
    statusKey: 'workbench:mobile.automationPanel.createdTodo',
  },
  {
    createAction: 'start',
    labelKey: 'workbench:mobile.automationPanel.createStart',
    statusKey: 'workbench:mobile.automationPanel.createdStart',
  },
];

export const MOBILE_AUTOMATION_WORKFLOW_LABEL_KEYS: Record<
  OrchestratorWorkflowState,
  `workbench:mobile.automationPanel.workflow.${OrchestratorWorkflowState}`
> = {
  backlog: 'workbench:mobile.automationPanel.workflow.backlog',
  todo: 'workbench:mobile.automationPanel.workflow.todo',
  inProgress: 'workbench:mobile.automationPanel.workflow.inProgress',
  humanReview: 'workbench:mobile.automationPanel.workflow.humanReview',
  rework: 'workbench:mobile.automationPanel.workflow.rework',
  merging: 'workbench:mobile.automationPanel.workflow.merging',
  done: 'workbench:mobile.automationPanel.workflow.done',
  canceled: 'workbench:mobile.automationPanel.workflow.canceled',
};

export const MOBILE_AUTOMATION_RUN_LABEL_KEYS: Record<
  OrchestratorRunState,
  `workbench:mobile.automationPanel.runState.${OrchestratorRunState}`
> = {
  idle: 'workbench:mobile.automationPanel.runState.idle',
  queued: 'workbench:mobile.automationPanel.runState.queued',
  preparing: 'workbench:mobile.automationPanel.runState.preparing',
  running: 'workbench:mobile.automationPanel.runState.running',
  verifying: 'workbench:mobile.automationPanel.runState.verifying',
  retrying: 'workbench:mobile.automationPanel.runState.retrying',
  blocked: 'workbench:mobile.automationPanel.runState.blocked',
  delivering: 'workbench:mobile.automationPanel.runState.delivering',
};

export const MOBILE_AUTOMATION_ATTEMPT_PHASE_LABEL_KEYS: Record<
  OrchestratorAttemptPhase,
  `workbench:mobile.automationPanel.attemptPhase.${OrchestratorAttemptPhase}`
> = {
  preparingWorkspace: 'workbench:mobile.automationPanel.attemptPhase.preparingWorkspace',
  buildingPrompt: 'workbench:mobile.automationPanel.attemptPhase.buildingPrompt',
  launchingRunner: 'workbench:mobile.automationPanel.attemptPhase.launchingRunner',
  initializingSession: 'workbench:mobile.automationPanel.attemptPhase.initializingSession',
  streaming: 'workbench:mobile.automationPanel.attemptPhase.streaming',
  finishing: 'workbench:mobile.automationPanel.attemptPhase.finishing',
  succeeded: 'workbench:mobile.automationPanel.attemptPhase.succeeded',
  failed: 'workbench:mobile.automationPanel.attemptPhase.failed',
  timedOut: 'workbench:mobile.automationPanel.attemptPhase.timedOut',
  stalled: 'workbench:mobile.automationPanel.attemptPhase.stalled',
  canceledByReconciliation:
    'workbench:mobile.automationPanel.attemptPhase.canceledByReconciliation',
};

export const MOBILE_AUTOMATION_PENDING_STATUS_LABEL_KEYS: Record<
  OrchestratorRemoteOutboxStatus,
  `workbench:mobile.automationPanel.pendingStatus.${OrchestratorRemoteOutboxStatus}`
> = {
  pending: 'workbench:mobile.automationPanel.pendingStatus.pending',
  sending: 'workbench:mobile.automationPanel.pendingStatus.sending',
  mirrored: 'workbench:mobile.automationPanel.pendingStatus.mirrored',
  failed: 'workbench:mobile.automationPanel.pendingStatus.failed',
  discarded: 'workbench:mobile.automationPanel.pendingStatus.discarded',
};

const MOBILE_AUTOMATION_EVIDENCE_KIND_LABEL_KEYS = {
  developmentAttempt: 'orchestrator:evidence.kind.developmentAttempt',
  verificationOutput: 'orchestrator:evidence.kind.verificationOutput',
  verificationReview: 'orchestrator:evidence.kind.verificationReview',
  repairPrompt: 'orchestrator:evidence.kind.repairPrompt',
  remoteOutbox: 'orchestrator:evidence.kind.remoteOutbox',
  delivery: 'orchestrator:evidence.kind.delivery',
} as const satisfies Record<string, MobileAutomationEvidenceKindLabelKey>;

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端 Orchestrator HTTP 请求失败时需要给用户展示后端返回的可读错误，而不是只显示 unknown。
 *
 * Code Logic（这个函数做什么）:
 *   读取 Error.message；如果抛出值不是 Error，则转成字符串作为兜底展示。
 */
export function getErrorMessage(reason: unknown): string {
  if (reason instanceof Error && reason.message.trim()) return reason.message;
  return String(reason);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   远端离线创建落入 outbox 时仍应在手机端展示用户填写的任务标题，避免只看到设备路径。
 *
 * Code Logic（这个函数做什么）:
 *   从 outbox requestJson 中解析 title；解析失败时使用远端项目路径兜底。
 */
export function pendingRemoteTaskTitle(item: OrchestratorRemoteOutboxItem): string {
  try {
    const value = JSON.parse(item.requestJson) as { title?: unknown };
    if (typeof value.title === 'string' && value.title.trim()) return value.title.trim();
  } catch {
    // requestJson 来自本机 outbox，异常时展示路径兜底即可。
  }
  return item.remoteProjectPath;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   自动化面板需要按固定 workflow 泳道展示真实任务，即使后端返回顺序变化也保持稳定。
 *
 * Code Logic（这个函数做什么）:
 *   初始化每个 workflowState 的空数组，供后续单次遍历追加渲染任务。
 */
export function createEmptyMobileAutomationGroups(): MobileAutomationTaskGroups {
  return MOBILE_AUTOMATION_WORKFLOW_STATES.reduce<MobileAutomationTaskGroups>((groups, state) => {
    groups[state] = [];
    return groups;
  }, {} as MobileAutomationTaskGroups);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   手机端自动化列表必须是桌面 workflow board 的 compact grouped-list，块要落在 head 泳道。
 *
 * Code Logic（这个函数做什么）:
 *   委托 groupBoardItems，按 blockId 聚合并把块放到 head 泳道。
 */
export function groupMobileAutomationTasks(
  tasks: OrchestratorRenderableTask[],
): MobileAutomationTaskGroups {
  return groupBoardItems(tasks);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   任务列表刷新后应保留用户正在查看的详情，但如果任务已不在列表中则关闭详情避免展示过期数据。
 *
 * Code Logic（这个函数做什么）:
 *   local/remote 按 task.id 匹配最新 view；pendingRemote 和空选择都返回 null。
 */
export function resolveSelectedTaskViewAfterRefresh(
  current: OrchestratorTaskView | null,
  nextViews: OrchestratorTaskView[],
): OrchestratorTaskView | null {
  if (!current || current.origin === 'pendingRemote') return null;
  return nextViews.find((view) => view.origin !== 'pendingRemote' && view.task.id === current.task.id) ?? null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Runner runtime 字段可能尚未被后端关联，移动端必须显示 unknown fallback 而不是空白。
 *
 * Code Logic（这个函数做什么）:
 *   对 null、undefined 和空白字符串返回 fallback；其它字符串 trim 后展示。
 */
export function runtimeValue(value: string | null | undefined, fallback: string): string {
  const trimmed = value?.trim();
  return trimmed ? trimmed : fallback;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Evidence 时间线需要展示可读时间；无效时间不能让详情崩溃。
 *
 * Code Logic（这个函数做什么）:
 *   使用浏览器本地化时间格式化 ISO 字符串；解析失败时返回原始值。
 */
export function formatAutomationTimestamp(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString();
}

/**
 * Business Logic（为什么需要这个函数）:
 *   runtime 状态条需要把后端 remoteStatus 映射成移动端可本地化的文案 key。
 *
 * Code Logic（这个函数做什么）:
 *   按四态返回 workbench mobile automation i18n key；offline 主文案为中性“离线”。
 */
export function mobileRuntimeStatusLabelKey(
  status: OrchestratorRemoteRuntimeStatus,
):
  | 'workbench:mobile.automationPanel.runtimeStatusLive'
  | 'workbench:mobile.automationPanel.runtimeStatusOffline'
  | 'workbench:mobile.automationPanel.runtimeStatusUnsupported'
  | 'workbench:mobile.automationPanel.runtimeStatusUnavailable' {
  // display state 的 remoteStatus 已把本机 local 归一为 null，此处只处理远端四态。
  switch (status) {
    case 'live':
      return 'workbench:mobile.automationPanel.runtimeStatusLive';
    case 'offline':
      return 'workbench:mobile.automationPanel.runtimeStatusOffline';
    case 'unsupported':
      return 'workbench:mobile.automationPanel.runtimeStatusUnsupported';
    case 'unavailable':
      return 'workbench:mobile.automationPanel.runtimeStatusUnavailable';
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   缓存时间需要可读展示，帮助用户判断 offline 快照新旧。
 *
 * Code Logic（这个函数做什么）:
 *   复用 evidence 时间格式化 helper。
 */
export function formatRuntimeTimestamp(value: string): string {
  return formatAutomationTimestamp(value);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   详情面板只支持真实 local/remote 任务，pendingRemote outbox 不应触发 evidence 或 action。
 *
 * Code Logic（这个函数做什么）:
 *   从 task view union 中安全提取 task；pendingRemote 或空值返回 null。
 */
export function getTaskFromView(view: OrchestratorTaskView | null): OrchestratorTask | null {
  if (!view || view.origin === 'pendingRemote') return null;
  return view.task;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端 evidence 列表需要复用 Orchestrator namespace 文案，但组件主 namespace 是 workbench。
 *
 * Code Logic（这个函数做什么）:
 *   把后端 evidence kind 映射为带 namespace 的字面量 i18n key；未知 kind 使用 generic。
 */
export function mobileAutomationEvidenceKindLabelKey(
  kind: string,
): MobileAutomationEvidenceKindLabelKey {
  return (
    MOBILE_AUTOMATION_EVIDENCE_KIND_LABEL_KEYS[
      kind as keyof typeof MOBILE_AUTOMATION_EVIDENCE_KIND_LABEL_KEYS
    ] ?? 'orchestrator:evidence.kind.generic'
  );
}

/**
 * Business Logic（为什么需要这个类型）:
 *   面板壳层只负责 header / error / runtime strip 组合，需要窄的数据与动作包。
 *
 * Code Logic（这个类型做什么）:
 *   聚合壳层渲染所需的 id、状态、runtime 显示与刷新/创建入口回调。
 */
export interface MobileAutomationShellProps {
  titleId: string;
  runtimeTitleId: string;
  hasProject: boolean;
  loading: boolean;
  error: string | null;
  status: string | null;
  isListEmpty: boolean;
  runtimeDisplay: OrchestratorRuntimeDisplayState;
  runtimeStatusLabel: string;
  showRuntimeCachedHint: boolean;
  onRefresh: () => void;
  onOpenCreateDialog: () => void;
}

/**
 * Business Logic（为什么需要这个类型）:
 *   任务列表视图只渲染 grouped-list，不持有 transport 或业务 state。
 *
 * Code Logic（这个类型做什么）:
 *   描述可见泳道、分组任务、选中 id、unknown 文案与选中回调。
 */
export interface MobileAutomationTaskListProps {
  visibleWorkflowStates: readonly OrchestratorWorkflowState[];
  groupedTasks: MobileAutomationTaskGroups;
  selectedTaskId: string | null;
  unknownLabel: string;
  expandedBlockIds: readonly string[];
  onSelectTaskView: (view: OrchestratorTaskView) => void;
  onToggleBlock: (blockId: string) => void;
  canCreateTaskBlock: boolean;
  onOpenLaneCreate: (lane: OrchestratorWorkflowState, mode: OrchestratorCreateMode) => void;
  onOpenAppend: (blockId: string) => void;
  onReorderBlock: (blockId: string, orderedTaskIds: string[]) => void;
}

/**
 * Business Logic（为什么需要这个类型）:
 *   任务详情视图展示 goal/runtime/evidence 与打开执行现场，不含 API 调用。
 *
 * Code Logic（这个类型做什么）:
 *   描述选中任务、evidence 状态、执行现场可用性与关闭/打开回调。
 */
export interface MobileAutomationTaskDetailProps {
  selectedTask: OrchestratorTask | null;
  detailTitleId: string;
  unknownLabel: string;
  evidenceItems: OrchestratorEvidence[];
  evidenceLoading: boolean;
  evidenceError: string | null;
  canOpenExecutionContext: boolean;
  onCloseDetails: () => void;
  onOpenExecutionContext: () => void;
}

/**
 * Business Logic（为什么需要这个类型）:
 *   创建任务弹窗只负责表单与 Dialog chrome，状态与 HTTP 留在 controller。
 *
 * Code Logic（这个类型做什么）:
 *   描述 dialog open、表单字段、busy 态、create actions 与事件回调。
 */
export interface MobileAutomationCreateDialogProps {
  open: boolean;
  dialogTitleId: string;
  dialogKind: OrchestratorCreateDialogKind;
  createMode: OrchestratorCreateMode;
  preferredCreateAction: HttpCreateOrchestratorTaskAction | null;
  promptDraftRef: RefObject<HTMLTextAreaElement | null>;
  creating: boolean;
  completingPrompt: boolean;
  creatingAction: HttpCreateOrchestratorTaskAction | null;
  appending: boolean;
  promptDraft: string;
  title: string;
  goal: string;
  acceptanceCriteria: string;
  blockTitle: string;
  blockMembers: OrchestratorCreateForm[];
  canCompletePrompt: boolean;
  canSubmit: boolean;
  canCreateBlock: boolean;
  canCreateTaskBlock: boolean;
  canAppend: boolean;
  createActions: readonly MobileAutomationCreateActionConfig[];
  onClose: () => void;
  onCreateModeChange: (mode: OrchestratorCreateMode) => void;
  onPromptDraftChange: (value: string) => void;
  onTitleChange: (value: string) => void;
  onGoalChange: (value: string) => void;
  onAcceptanceCriteriaChange: (value: string) => void;
  onBlockTitleChange: (value: string) => void;
  onUpdateBlockMember: (index: number, field: keyof OrchestratorCreateForm, value: string) => void;
  onAddBlockMember: () => void;
  onRemoveBlockMember: (index: number) => void;
  onCompletePrompt: () => void;
  onCreateTask: (
    createAction: HttpCreateOrchestratorTaskAction,
    statusKey: MobileAutomationCreateActionStatusKey,
  ) => void;
  onAppendSubmit: () => void;
}

/**
 * Business Logic（为什么需要这个类型）:
 *   pending remote outbox 列表与 Retry/Discard 动作渲染需要窄 props。
 *
 * Code Logic（这个类型做什么）:
 *   描述 pending 条目、聚焦 id、busy id 与 retry/discard 回调。
 */
export interface MobileAutomationOutboxProps {
  pendingRemoteItems: OrchestratorRemoteOutboxItem[];
  focusedOutboxId: string | null;
  outboxActionId: string | null;
  onRetry: (outboxId: string) => void;
  onDiscard: (outboxId: string) => void;
}

/**
 * Business Logic（为什么需要这个类型）:
 *   Panel 组合层一次解构 shell + 四个视图 props，避免再触碰业务 state。
 *
 * Code Logic（这个类型做什么）:
 *   汇总 controller 返回的五组 props bundle。
 */
export interface MobileAutomationExperimentsProps {
  experiments: import('@/lib/types').OrchestratorExperiment[];
  onApproveRecommended: (experimentId: string, winnerTaskId: string) => void;
  onCancel: (experimentId: string) => void;
}

export interface UseMobileAutomationControllerResult {
  shell: MobileAutomationShellProps;
  taskList: MobileAutomationTaskListProps;
  taskDetail: MobileAutomationTaskDetailProps;
  createDialog: MobileAutomationCreateDialogProps;
  outbox: MobileAutomationOutboxProps;
  experiments: MobileAutomationExperimentsProps;
}

/**
 * useMobileAutomationController（移动端自动化面板控制器）
 *
 * Business Logic（为什么需要这个函数）:
 *   移动端自动化面板需要把 transport、runtime store、任务选择与创建幂等逻辑集中在一处，
 *   让 TaskList/Detail/CreateDialog/Outbox 保持纯展示，便于静态所有权合同与后续扩展。
 *
 * Code Logic（这个函数做什么）:
 *   持有全部 state/effect/handler 与 HTTP/runtime 调用；返回 shell + 四个视图的 typed props bundles。
 *   不渲染 Dialog / 任务行 map 等大块 JSX。
 */
export function useMobileAutomationController({
  project,
  onOpenExecutionContext,
  focusTaskId = null,
  focusOutboxId = null,
  onFocusResult,
}: UseMobileAutomationControllerParams): UseMobileAutomationControllerResult {
  const { t } = useTranslation(['workbench', 'orchestrator']);
  const [taskViews, setTaskViews] = useState<OrchestratorTaskView[]>([]);
  const [selectedTaskView, setSelectedTaskView] = useState<OrchestratorTaskView | null>(null);
  const [focusedOutboxId, setFocusedOutboxId] = useState<string | null>(null);
  const appliedFocusTaskIdRef = useRef<string | null>(null);
  const appliedFocusOutboxIdRef = useRef<string | null>(null);
  const [evidenceItems, setEvidenceItems] = useState<OrchestratorEvidence[]>([]);
  const [evidenceLoading, setEvidenceLoading] = useState<boolean>(false);
  const [evidenceError, setEvidenceError] = useState<string | null>(null);
  const [title, setTitle] = useState<string>('');
  const [goal, setGoal] = useState<string>('');
  const [acceptanceCriteria, setAcceptanceCriteria] = useState<string>('');
  const [createDialogOpen, setCreateDialogOpen] = useState<boolean>(false);
  const [createDialogKind, setCreateDialogKind] = useState<OrchestratorCreateDialogKind>('create');
  const [createMode, setCreateMode] = useState<OrchestratorCreateMode>('task');
  const [preferredCreateAction, setPreferredCreateAction] =
    useState<HttpCreateOrchestratorTaskAction | null>(null);
  const [blockTitle, setBlockTitle] = useState('');
  const [blockMembers, setBlockMembers] = useState<OrchestratorCreateForm[]>(
    emptyOrchestratorBlockMembers,
  );
  const [appending, setAppending] = useState(false);
  const [appendBlockId, setAppendBlockId] = useState<string | null>(null);
  const [expandedBlockIds, setExpandedBlockIds] = useState<string[]>([]);
  const [ownerPeer, setOwnerPeer] = useState<OrchestratorPeerProtocolHint | null>(null);
  const [promptDraft, setPromptDraft] = useState<string>('');
  const [loading, setLoading] = useState<boolean>(false);
  const [outboxActionId, setOutboxActionId] = useState<string | null>(null);
  const [creatingAction, setCreatingAction] =
    useState<HttpCreateOrchestratorTaskAction | null>(null);
  const [completingPrompt, setCompletingPrompt] = useState<boolean>(false);
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  useAutoDismissedStatus(status, setStatus);
  const [runtimeDisplayState, setRuntimeDisplay] = useState<OwnedMobileRuntimeDisplayState>(
    () => emptyMobileRuntimeDisplayState(false),
  );
  const [experiments, setExperiments] = useState<
    import('@/lib/types').OrchestratorExperiment[]
  >([]);
  const requestIdRef = useRef<number>(0);
  const evidenceRequestIdRef = useRef<number>(0);
  const activeProjectIdRef = useRef<string | null>(null);
  /**
   * 一次逻辑创建提交的幂等键：表单内容不变的重试复用；成功/取消/表单变更后清空。
   */
  const createClientRequestIdRef = useRef<string | null>(null);
  const createClientRequestFingerprintRef = useRef<string | null>(null);
  const promptDraftRef = useRef<HTMLTextAreaElement | null>(null);
  const titleId = useId();
  const dialogTitleId = useId();
  const detailTitleId = useId();
  const runtimeTitleId = useId();
  const hasProject = Boolean(project);
  // Render 阶段隔离：A→B 首帧 effect 尚未重置 state 时，不得展示旧项目 runtime 快照。
  const runtimeDisplay = selectMobileRuntimeDisplayForProject(
    runtimeDisplayState,
    project?.id ?? null,
  );
  const creating = creatingAction !== null;
  const trimmedPromptDraft = promptDraft.trim();
  const trimmedTitle = title.trim();
  const trimmedGoal = goal.trim();
  const trimmedAcceptanceCriteria = acceptanceCriteria.trim();
  const selectedTask = getTaskFromView(selectedTaskView);
  const unknownLabel = t('workbench:mobile.automationPanel.unknown');
  const { tasks, pendingRemoteItems } = useMemo(
    () => splitOrchestratorTaskViews(taskViews),
    [taskViews],
  );
  const groupedTasks = useMemo(() => groupMobileAutomationTasks(tasks), [tasks]);
  const visibleWorkflowStates = useMemo(() => {
    const present = new Set(
      MOBILE_AUTOMATION_WORKFLOW_STATES.filter((state) => groupedTasks[state].length > 0),
    );
    if (hasProject) {
      present.add('backlog');
      present.add('todo');
    }
    return MOBILE_AUTOMATION_WORKFLOW_STATES.filter((state) => present.has(state));
  }, [groupedTasks, hasProject]);
  const taskCount = tasks.length;
  const pendingCount = pendingRemoteItems.length;
  const isListEmpty = !loading && taskCount === 0 && pendingCount === 0;
  const canCompletePrompt = Boolean(
    hasProject &&
      trimmedPromptDraft &&
      !completingPrompt &&
      !creating &&
      !loading,
  );
  const canSubmit = Boolean(
    hasProject &&
      trimmedTitle &&
      trimmedGoal &&
      trimmedAcceptanceCriteria &&
      !creating &&
      !completingPrompt &&
      !loading,
  );
  const canCreateBlock = Boolean(
    hasProject &&
      blockTitle.trim() &&
      blockMembers.length >= MIN_ORCHESTRATOR_BLOCK_MEMBERS &&
      blockMembers.length <= MAX_ORCHESTRATOR_BLOCK_MEMBERS &&
      blockMembers.every(
        (member) => member.title.trim() && member.goal.trim() && member.acceptanceCriteria.trim(),
      ) &&
      !creating &&
      !completingPrompt &&
      !loading,
  );
  const canCreateTaskBlock = canCreateOrchestratorTaskBlock({
    projectKind: project?.kind,
    peer: ownerPeer,
  });
  const canAppend = Boolean(
    hasProject &&
      appendBlockId &&
      trimmedTitle &&
      trimmedGoal &&
      trimmedAcceptanceCriteria &&
      !appending &&
      !completingPrompt &&
      !loading,
  );
  const canOpenExecutionContext = Boolean(
    selectedTask &&
      onOpenExecutionContext &&
      (selectedTask.worktreeId || selectedTask.sessionId),
  );
  const runtimeStatusLabel = useMemo(() => {
    // local 成功：display remoteStatus 归一为 null，但 snapshot.remoteStatus 仍是 local。
    if (runtimeDisplay.snapshot?.remoteStatus === 'local') {
      return t('workbench:mobile.automationPanel.runtimeStatusLocal');
    }
    const statusValue = runtimeDisplay.remoteStatus;
    if (!statusValue) {
      // cold offline / 未知：中性未知，不声称“显示缓存”。
      return t('workbench:mobile.automationPanel.runtimeStatusUnknown');
    }
    return t(mobileRuntimeStatusLabelKey(statusValue));
  }, [runtimeDisplay.remoteStatus, runtimeDisplay.snapshot, t]);
  // 仅 warm offline（有 snapshot + cachedAt）展示缓存提示；cold offline 不声称有缓存。
  const showRuntimeCachedHint =
    runtimeDisplay.remoteStatus === 'offline' &&
    runtimeDisplay.snapshot !== null &&
    runtimeDisplay.cachedAt !== null;

  /**
   * Business Logic（为什么需要这个函数）:
   *   自动化任务列表需要手动刷新和项目切换时自动刷新，并防止旧项目响应覆盖当前项目。
   *
   * Code Logic（这个函数做什么）:
   *   按 projectId 调用 HTTP list route；用递增 request id 和 active project ref 做 stale guard。
   */
  const loadTasks = useCallback(async (projectId: string): Promise<void> => {
    const requestId = requestIdRef.current + 1;
    requestIdRef.current = requestId;
    setLoading(true);
    setError(null);

    try {
      const [nextTaskViews, nextExperiments] = await Promise.all([
        httpOrchestratorTransport.tasks.listViews(projectId),
        httpOrchestratorTransport.experiments.list(projectId).catch(() => []),
      ]);
      if (requestIdRef.current !== requestId) return;
      if (activeProjectIdRef.current !== projectId) return;
      setTaskViews(nextTaskViews);
      setExperiments(nextExperiments);
      setSelectedTaskView((current) => resolveSelectedTaskViewAfterRefresh(current, nextTaskViews));
    } catch (reason) {
      if (requestIdRef.current !== requestId) return;
      if (activeProjectIdRef.current !== projectId) return;
      setError(`${t('workbench:mobile.automationPanel.errors.list')}: ${getErrorMessage(reason)}`);
    } finally {
      if (requestIdRef.current === requestId && activeProjectIdRef.current === projectId) {
        setLoading(false);
      }
    }
  }, [t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   项目切换或手动刷新时需要拉取 remote-aware runtime snapshot，并与任务列表解耦。
   *
   * Code Logic（这个函数做什么）:
   *   通过模块 store 生成 request seq、调用 mobile HTTP route，成功/失败归并显示缓存；缓存不驱动动作。
   */
  const loadRuntimeSnapshot = useCallback(async (projectId: string): Promise<void> => {
    const requestSeq = nextMobileRuntimeSnapshotRequestSeq(projectId);
    // 同项目 refresh 可保留本项目 live 缓存作骨架；跨项目时 begin 只会读到目标 project 缓存。
    setRuntimeDisplay(beginMobileRuntimeSnapshotLoad(projectId));
    try {
      const snapshot = await httpOrchestratorTransport.getRuntimeSnapshot(projectId);
      if (activeProjectIdRef.current !== projectId) return;
      const next = applyMobileRuntimeSnapshotSuccess(projectId, requestSeq, snapshot);
      // next 已 stamp projectId；active 守卫后再 set，避免旧项目响应写入。
      if (next && activeProjectIdRef.current === projectId) setRuntimeDisplay(next);
    } catch (reason) {
      if (activeProjectIdRef.current !== projectId) return;
      // 必须保留 adapter 的 OrchestratorRuntimeTransportError.kind；
      // 用 new Error(message) 会抹掉 network，warm offline 缓存永远不可达。
      const next = applyMobileRuntimeSnapshotFailure(
        projectId,
        requestSeq,
        toRuntimeLoadError(reason),
      );
      if (next && activeProjectIdRef.current === projectId) setRuntimeDisplay(next);
    }
  }, []);

  /* eslint-disable react-hooks/set-state-in-effect -- 项目切换时必须同步自动化任务上下文 */
  useEffect(() => {
    const projectId = project?.id ?? null;
    activeProjectIdRef.current = projectId;
    requestIdRef.current += 1;
    evidenceRequestIdRef.current += 1;
    appliedFocusTaskIdRef.current = null;
    appliedFocusOutboxIdRef.current = null;
    setTaskViews([]);
    setExperiments([]);
    setSelectedTaskView(null);
    setFocusedOutboxId(null);
    setEvidenceItems([]);
    setEvidenceError(null);
    setEvidenceLoading(false);
    setError(null);
    setStatus(null);
    setLoading(false);
    setCreatingAction(null);
    setCompletingPrompt(false);
    setCreateDialogOpen(false);
    setPromptDraft('');
    setTitle('');
    setGoal('');
    setAcceptanceCriteria('');
    // 同步重置为归属新项目的空/loading 态，避免 effect 间首帧串台（render selector 是双保险）。
    setRuntimeDisplay(
      projectId
        ? emptyMobileRuntimeDisplayState(true, null, projectId)
        : emptyMobileRuntimeDisplayState(false),
    );

    if (projectId) {
      void loadTasks(projectId);
      void loadRuntimeSnapshot(projectId);
    } else {
      resetMobileRuntimeSnapshotStore();
    }
  }, [loadRuntimeSnapshot, loadTasks, project?.id]);

  /**
   * Business Logic（为什么需要这个 effect）:
   *   remote shortcut 的「添加任务块」必须看 owner 能力；本机项目与当前 host 同版本。
   *
   * Code Logic（这个 effect 做什么）:
   *   remote 时 GET /api/mobile/devices 匹配 project.deviceId；失败或非 remote 清空 peer（local 不依赖 peer）。
   */
  useEffect(() => {
    if (!project || project.kind !== 'remote') {
      setOwnerPeer(null);
      return;
    }
    let cancelled = false;
    void transferHttp
      .listDevices()
      .then((devices) => {
        if (cancelled) return;
        const owner = devices.find((device) => device.id === project.deviceId) ?? null;
        setOwnerPeer(owner);
      })
      .catch(() => {
        if (!cancelled) setOwnerPeer(null);
      });
    return () => {
      cancelled = true;
    };
  }, [project, project?.deviceId, project?.id, project?.kind]);

  /**
   * Business Logic（为什么需要这个 effect）:
   *   Attention 跳转到 automation 后需要在列表加载完成时聚焦 task 或 outbox；找不到则回报 missing。
   *
   * Code Logic（这个 effect 做什么）:
   *   在非 loading 时匹配 focusTaskId/focusOutboxId；成功选中详情或高亮 outbox，失败回调 onFocusResult(missing)。
   */
  useEffect(() => {
    if (loading) return;
    if (!project?.id) return;

    if (focusTaskId && appliedFocusTaskIdRef.current !== focusTaskId) {
      appliedFocusTaskIdRef.current = focusTaskId;
      const match = taskViews.find(
        (view) => view.origin !== 'pendingRemote' && view.task.id === focusTaskId,
      );
      if (match) {
        setSelectedTaskView(match);
        onFocusResult?.({ status: 'found', entity: 'task', id: focusTaskId });
      } else {
        setSelectedTaskView(null);
        onFocusResult?.({ status: 'missing', entity: 'task', id: focusTaskId });
      }
    }

    if (focusOutboxId && appliedFocusOutboxIdRef.current !== focusOutboxId) {
      appliedFocusOutboxIdRef.current = focusOutboxId;
      const match = pendingRemoteItems.find((item) => item.id === focusOutboxId);
      if (match) {
        setFocusedOutboxId(focusOutboxId);
        onFocusResult?.({ status: 'found', entity: 'outbox', id: focusOutboxId });
      } else {
        setFocusedOutboxId(null);
        onFocusResult?.({ status: 'missing', entity: 'outbox', id: focusOutboxId });
      }
    }
  }, [
    focusOutboxId,
    focusTaskId,
    loading,
    onFocusResult,
    pendingRemoteItems,
    project?.id,
    taskViews,
  ]);

  useEffect(() => {
    const projectId = activeProjectIdRef.current;
    if (!projectId || !selectedTask) {
      evidenceRequestIdRef.current += 1;
      setEvidenceItems([]);
      setEvidenceError(null);
      setEvidenceLoading(false);
      return;
    }

    const taskId = selectedTask.id;
    const requestId = evidenceRequestIdRef.current + 1;
    evidenceRequestIdRef.current = requestId;
    setEvidenceLoading(true);
    setEvidenceError(null);
    setEvidenceItems([]);

    httpOrchestratorTransport.tasks.listEvidence(projectId, taskId)
      .then((items) => {
        if (evidenceRequestIdRef.current !== requestId) return;
        if (activeProjectIdRef.current !== projectId) return;
        if (getTaskFromView(selectedTaskView)?.id !== taskId) return;
        setEvidenceItems(items);
      })
      .catch((reason: unknown) => {
        if (evidenceRequestIdRef.current !== requestId) return;
        if (activeProjectIdRef.current !== projectId) return;
        setEvidenceError(
          `${t('workbench:mobile.automationPanel.errors.evidence')}: ${getErrorMessage(reason)}`,
        );
      })
      .finally(() => {
        if (evidenceRequestIdRef.current === requestId && activeProjectIdRef.current === projectId) {
          setEvidenceLoading(false);
        }
      });
  }, [selectedTask, selectedTaskView, t]);
  /* eslint-enable react-hooks/set-state-in-effect */

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户点击刷新时需要重新读取当前项目的任务列表与 runtime 状态，未选项目不应发起请求。
   *
   * Code Logic（这个函数做什么）:
   *   校验当前 project id，存在时并行调用 loadTasks 与 loadRuntimeSnapshot。
   */
  const handleRefresh = useCallback((): void => {
    const projectId = activeProjectIdRef.current;
    if (!projectId) return;
    void loadTasks(projectId);
    void loadRuntimeSnapshot(projectId);
  }, [loadRuntimeSnapshot, loadTasks]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   手机 Automation 面板对 failed outbox 点 Retry 时，应在本机把条目回到 pending、刷新列表，
   *   并立即失效全局 Inbox 投影。
   *
   * Code Logic（这个函数做什么）:
   *   校验 project 与 busy 状态，调用 httpOrchestratorTransport.outbox.retry，
   *   成功后 loadTasks 并 requestAttentionInvalidation。
   */
  const handleRetryRemoteOutbox = useCallback(
    async (outboxId: string): Promise<void> => {
      const projectId = activeProjectIdRef.current;
      if (!projectId || outboxActionId) return;
      setOutboxActionId(outboxId);
      setError(null);
      try {
        await httpOrchestratorTransport.outbox.retry(projectId, outboxId);
        if (activeProjectIdRef.current !== projectId) return;
        await loadTasks(projectId);
        requestAttentionInvalidation();
      } catch (reason) {
        if (activeProjectIdRef.current !== projectId) return;
        setError(
          `${t('workbench:mobile.automationPanel.errors.retryOutbox')}: ${getErrorMessage(reason)}`,
        );
      } finally {
        if (activeProjectIdRef.current === projectId) {
          setOutboxActionId(null);
        }
      }
    },
    [loadTasks, outboxActionId, t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   手机 Automation 面板对 failed outbox 点 Discard 时，需要确认后进入 discarded 审计终态，
   *   并从 Inbox 移除对应 failed outbox 投影。
   *
   * Code Logic（这个函数做什么）:
   *   window.confirm 后调用 outbox.discard，成功后 loadTasks 并 requestAttentionInvalidation。
   */
  const handleDiscardRemoteOutbox = useCallback(
    async (outboxId: string): Promise<void> => {
      const projectId = activeProjectIdRef.current;
      if (!projectId || outboxActionId) return;
      const confirmed = window.confirm(t('workbench:mobile.automationPanel.pendingDiscardConfirm'));
      if (!confirmed) return;
      setOutboxActionId(outboxId);
      setError(null);
      try {
        await httpOrchestratorTransport.outbox.discard(projectId, outboxId);
        if (activeProjectIdRef.current !== projectId) return;
        await loadTasks(projectId);
        requestAttentionInvalidation();
      } catch (reason) {
        if (activeProjectIdRef.current !== projectId) return;
        setError(
          `${t('workbench:mobile.automationPanel.errors.discardOutbox')}: ${getErrorMessage(reason)}`,
        );
      } finally {
        if (activeProjectIdRef.current === projectId) {
          setOutboxActionId(null);
        }
      }
    },
    [loadTasks, outboxActionId, t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   手机端创建任务入口需要从列表页进入独立弹窗，避免表单常驻挤占任务队列空间。
   *
   * Code Logic（这个函数做什么）:
   *   打开创建弹窗并清理上一轮状态提示；表单草稿保留，让用户误关前可继续编辑。
   */
  const handleOpenCreateDialog = useCallback((): void => {
    if (!activeProjectIdRef.current) return;
    // 打开新弹窗视为新逻辑提交周期，清空旧幂等键。
    createClientRequestIdRef.current = null;
    createClientRequestFingerprintRef.current = null;
    setError(null);
    setStatus(null);
    setCreateDialogKind('create');
    setCreateMode('task');
    setPreferredCreateAction(null);
    setAppendBlockId(null);
    setCreateDialogOpen(true);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   移动端 Backlog/Todo 分组头 + 必须打开同一套独立 Dialog。
   *
   * Code Logic（这个函数做什么）:
   *   设置 createMode 与 preferredCreateAction 后打开弹窗。
   */
  const handleOpenLaneCreate = useCallback(
    (lane: OrchestratorWorkflowState, mode: OrchestratorCreateMode) => {
      if (!activeProjectIdRef.current || (lane !== 'backlog' && lane !== 'todo')) return;
      createClientRequestIdRef.current = null;
      createClientRequestFingerprintRef.current = null;
      setError(null);
      setStatus(null);
      setCreateDialogKind('create');
      setCreateMode(mode === 'taskBlock' && !canCreateTaskBlock ? 'task' : mode);
      setPreferredCreateAction(lane);
      setAppendBlockId(null);
      setCreateDialogOpen(true);
    },
    [canCreateTaskBlock],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   块组末尾追加只填三字段。
   *
   * Code Logic（这个函数做什么）:
   *   切到 append 弹窗并清空单任务三字段。
   */
  const handleOpenAppend = useCallback((blockId: string) => {
    if (!activeProjectIdRef.current) return;
    createClientRequestIdRef.current = null;
    createClientRequestFingerprintRef.current = null;
    setError(null);
    setStatus(null);
    setCreateDialogKind('append');
    setCreateMode('task');
    setPreferredCreateAction(null);
    setAppendBlockId(blockId);
    setTitle('');
    setGoal('');
    setAcceptanceCriteria('');
    setCreateDialogOpen(true);
  }, []);

  const handleCreateModeChange = useCallback(
    (mode: OrchestratorCreateMode) => {
      if (mode === 'taskBlock' && !canCreateTaskBlock) return;
      setCreateMode(mode);
    },
    [canCreateTaskBlock],
  );

  const handleToggleBlock = useCallback((blockId: string) => {
    setExpandedBlockIds((current) =>
      current.includes(blockId)
        ? current.filter((id) => id !== blockId)
        : [...current, blockId],
    );
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   弹窗关闭应避免打断正在创建或 AI 完善的请求，防止用户误以为操作已取消。
   *
   * Code Logic（这个函数做什么）:
   *   若没有 pending 请求则关闭 dialog 并清空逻辑提交幂等键；请求中忽略关闭动作。
   */
  const handleCloseCreateDialog = useCallback((): void => {
    if (creating || completingPrompt || appending) return;
    createClientRequestIdRef.current = null;
    createClientRequestFingerprintRef.current = null;
    setCreateDialogOpen(false);
    setAppendBlockId(null);
    setCreateDialogKind('create');
  }, [appending, creating, completingPrompt]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户常会只输入一句简单需求，手机端也应能像桌面端一样让 AI 结构化生成任务标题、目标和验收标准。
   *
   * Code Logic（这个函数做什么）:
   *   校验当前项目和 prompt 后调用 HTTP complete-prompt route；成功时把返回的三字段填入创建表单。
   */
  const handleCompletePrompt = useCallback(async (): Promise<void> => {
    const projectId = activeProjectIdRef.current;
    const workingDirectory = project?.kind === 'local' ? project.path : null;
    if (!projectId) {
      setError(t('workbench:mobile.automationPanel.noProject'));
      return;
    }
    if (!trimmedPromptDraft) {
      setError(t('workbench:mobile.automationPanel.errors.promptRequired'));
      return;
    }

    setCompletingPrompt(true);
    setError(null);
    setStatus(null);
    try {
      const completed = await httpOrchestratorTransport.tasks.completePrompt({
        projectId,
        prompt: trimmedPromptDraft,
        workingDirectory,
      });
      if (activeProjectIdRef.current !== projectId) return;
      setTitle(completed.title.trim());
      setGoal(completed.goal.trim());
      setAcceptanceCriteria(completed.acceptanceCriteria.trim());
    } catch (reason) {
      if (activeProjectIdRef.current !== projectId) return;
      setError(
        `${t('workbench:mobile.automationPanel.errors.completePrompt')}: ${getErrorMessage(reason)}`,
      );
    } finally {
      if (activeProjectIdRef.current === projectId) {
        setCompletingPrompt(false);
      }
    }
  }, [project, t, trimmedPromptDraft]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   手机端创建任务必须显式支持 Backlog、Todo 和 Start 三种业务动作，与桌面 workflow board 对齐。
   *   响应丢失后用户重试必须复用同一 clientRequestId，避免 owner 侧重复建任务。
   *
   * Code Logic（这个函数做什么）:
   *   校验项目和表单；按表单指纹在逻辑提交周期内复用 clientRequestId；调用 HTTP createView；
   *   成功后清空弹窗与幂等键，失败保留键供重试。
   */
  const handleCreateTask = useCallback(async (
    createAction: HttpCreateOrchestratorTaskAction,
    statusKey: MobileAutomationCreateActionStatusKey,
  ): Promise<void> => {
    const projectId = activeProjectIdRef.current;
    if (!projectId) {
      setError(t('workbench:mobile.automationPanel.noProject'));
      return;
    }
    if (!trimmedTitle || !trimmedGoal || !trimmedAcceptanceCriteria) {
      setError(t('workbench:mobile.automationPanel.errors.required'));
      return;
    }

    // 表单内容指纹：任一字段变化视为新逻辑提交，重新 mint 幂等键。
    const fingerprint = [
      projectId,
      trimmedTitle,
      trimmedGoal,
      trimmedAcceptanceCriteria,
      createAction,
    ].join('\u0001');
    if (
      createClientRequestIdRef.current === null
      || createClientRequestFingerprintRef.current !== fingerprint
    ) {
      createClientRequestIdRef.current = createHttpOrchestratorClientRequestId();
      createClientRequestFingerprintRef.current = fingerprint;
    }
    const clientRequestId = createClientRequestIdRef.current;

    setCreatingAction(createAction);
    setError(null);
    setStatus(null);
    try {
      const createdTaskView = await httpOrchestratorTransport.tasks.createView({
        projectId,
        title: trimmedTitle,
        goal: trimmedGoal,
        acceptanceCriteria: trimmedAcceptanceCriteria,
        priority: 0,
        createAction,
        clientRequestId,
      });
      if (activeProjectIdRef.current !== projectId) return;
      setTaskViews((current) => upsertOrchestratorTaskView(current, createdTaskView));
      if (createdTaskView.origin !== 'pendingRemote') {
        setSelectedTaskView(createdTaskView);
      }
      setTitle('');
      setGoal('');
      setAcceptanceCriteria('');
      setPromptDraft('');
      createClientRequestIdRef.current = null;
      createClientRequestFingerprintRef.current = null;
      setCreateDialogOpen(false);
      setStatus(t(statusKey));
    } catch (reason) {
      if (activeProjectIdRef.current !== projectId) return;
      setError(
        `${t('workbench:mobile.automationPanel.errors.create')}: ${getErrorMessage(reason)}`,
      );
    } finally {
      if (activeProjectIdRef.current === projectId) {
        setCreatingAction(null);
      }
    }
  }, [t, trimmedAcceptanceCriteria, trimmedGoal, trimmedTitle]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   任务详情里的“打开执行现场”应进入现有 terminal 面板，而不是创建第二套终端模型。
   *
   * Code Logic（这个函数做什么）:
   *   从当前选中任务取 project/worktree/session id，经父组件回调切换 MobileWorkbench 的 active 上下文。
   */
  const handleOpenExecutionContext = useCallback((): void => {
    if (!selectedTask || !onOpenExecutionContext) return;
    onOpenExecutionContext({
      projectId: project?.id ?? selectedTask.projectId,
      worktreeId: selectedTask.worktreeId,
      sessionId: selectedTask.sessionId,
    });
  }, [onOpenExecutionContext, project?.id, selectedTask]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   列表点击真实任务应展开详情，而不是拖拽改状态。
   *
   * Code Logic（这个函数做什么）:
   *   将选中 task view 写入 selectedTaskView state。
   */
  const handleSelectTaskView = useCallback((view: OrchestratorTaskView): void => {
    setSelectedTaskView(view);
  }, []);


  /**
   * Business Logic（为什么需要这个函数）:
   *   用户关闭详情后应回到纯列表，避免残留过期 evidence 区域。
   *
   * Code Logic（这个函数做什么）:
   *   清空 selectedTaskView。
   */
  const handleCloseDetails = useCallback((): void => {
    setSelectedTaskView(null);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   创建弹窗表单字段变更时需要同步清理成功 status，避免旧提示误导用户。
   *
   * Code Logic（这个函数做什么）:
   *   更新对应字段 state 并 setStatus(null)。
   */
  const handlePromptDraftChange = useCallback((value: string): void => {
    setPromptDraft(value);
    setStatus(null);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   标题字段编辑时清理 status，避免“创建成功”文案与草稿并存。
   *
   * Code Logic（这个函数做什么）:
   *   setTitle 并清空 status。
   */
  const handleTitleChange = useCallback((value: string): void => {
    setTitle(value);
    setStatus(null);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   目标字段编辑时清理 status。
   *
   * Code Logic（这个函数做什么）:
   *   setGoal 并清空 status。
   */
  const handleGoalChange = useCallback((value: string): void => {
    setGoal(value);
    setStatus(null);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   验收标准字段编辑时清理 status。
   *
   * Code Logic（这个函数做什么）:
   *   setAcceptanceCriteria 并清空 status。
   */
  const handleAcceptanceCriteriaChange = useCallback((value: string): void => {
    setAcceptanceCriteria(value);
    setStatus(null);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   Outbox 视图需要 fire-and-forget 包装，避免在 JSX 内直接 await。
   *
   * Code Logic（这个函数做什么）:
   *   调用 handleRetryRemoteOutbox 并忽略 Promise。
   */
  const handleRetryOutbox = useCallback((outboxId: string): void => {
    void handleRetryRemoteOutbox(outboxId);
  }, [handleRetryRemoteOutbox]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   Outbox 视图需要 fire-and-forget 包装 discard。
   *
   * Code Logic（这个函数做什么）:
   *   调用 handleDiscardRemoteOutbox 并忽略 Promise。
   */
  const handleDiscardOutbox = useCallback((outboxId: string): void => {
    void handleDiscardRemoteOutbox(outboxId);
  }, [handleDiscardRemoteOutbox]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   CreateDialog 按钮点击后应触发 AI 完善，不在视图层持有 Promise 状态。
   *
   * Code Logic（这个函数做什么）:
   *   fire-and-forget 调用 handleCompletePrompt。
   */
  const handleCompletePromptClick = useCallback((): void => {
    void handleCompletePrompt();
  }, [handleCompletePrompt]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   CreateDialog 三个创建按钮需要把 createAction/statusKey 交给 controller 执行。
   *
   * Code Logic（这个函数做什么）:
   *   fire-and-forget 调用 handleCreateTask。
   */
  /**
   * Business Logic（为什么需要这个函数）:
   *   移动端创建任务块必须走 create-block HTTP，不能拆成多次 createView。
   *
   * Code Logic（这个函数做什么）:
   *   POST createBlock，成功 upsert 成员并关闭弹窗。
   */
  const handleCreateTaskBlock = useCallback(
    async (
      createAction: HttpCreateOrchestratorTaskAction,
      statusKey: MobileAutomationCreateActionStatusKey,
    ): Promise<void> => {
      const projectId = activeProjectIdRef.current;
      if (!projectId) {
        setError(t('workbench:mobile.automationPanel.noProject'));
        return;
      }
      if (!canCreateTaskBlock) {
        setError(t('orchestrator:create.unsupportedBlocks'));
        return;
      }
      const nextTitle = blockTitle.trim();
      const members = blockMembers.map((member) => ({
        title: member.title.trim(),
        goal: member.goal.trim(),
        acceptanceCriteria: member.acceptanceCriteria.trim(),
      }));
      if (
        !nextTitle ||
        members.length < MIN_ORCHESTRATOR_BLOCK_MEMBERS ||
        members.some((member) => !member.title || !member.goal || !member.acceptanceCriteria)
      ) {
        setError(t('workbench:mobile.automationPanel.errors.blockRequired'));
        return;
      }
      const fingerprint = [projectId, nextTitle, JSON.stringify(members), createAction].join('\u0001');
      if (
        createClientRequestIdRef.current === null ||
        createClientRequestFingerprintRef.current !== fingerprint
      ) {
        createClientRequestIdRef.current = createHttpOrchestratorClientRequestId();
        createClientRequestFingerprintRef.current = fingerprint;
      }
      const clientRequestId = createClientRequestIdRef.current;
      setCreatingAction(createAction);
      setError(null);
      setStatus(null);
      try {
        const created = await httpOrchestratorTransport.tasks.createBlock({
          projectId,
          title: nextTitle,
          members,
          createAction,
          clientRequestId,
        });
        if (activeProjectIdRef.current !== projectId) return;
        setTaskViews((current) => upsertOrchestratorTaskBlockCreated(current, created));
        setBlockTitle('');
        setBlockMembers(emptyOrchestratorBlockMembers());
        setPromptDraft('');
        createClientRequestIdRef.current = null;
        createClientRequestFingerprintRef.current = null;
        setCreateDialogOpen(false);
        setStatus(t(statusKey));
      } catch (reason) {
        if (activeProjectIdRef.current !== projectId) return;
        setError(
          `${t('workbench:mobile.automationPanel.errors.createBlock')}: ${getErrorMessage(reason)}`,
        );
      } finally {
        if (activeProjectIdRef.current === projectId) {
          setCreatingAction(null);
        }
      }
    },
    [blockMembers, blockTitle, canCreateTaskBlock, t],
  );

  const handleCreateTaskClick = useCallback(
    (
      createAction: HttpCreateOrchestratorTaskAction,
      statusKey: MobileAutomationCreateActionStatusKey,
    ): void => {
      if (createMode === 'taskBlock') {
        void handleCreateTaskBlock(createAction, statusKey);
        return;
      }
      void handleCreateTask(createAction, statusKey);
    },
    [createMode, handleCreateTask, handleCreateTaskBlock],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   移动端块末尾追加必须走 append-block-member。
   *
   * Code Logic（这个函数做什么）:
   *   POST appendBlockMember 后刷新列表。
   */
  const handleAppendSubmit = useCallback(async (): Promise<void> => {
    const projectId = activeProjectIdRef.current;
    if (!projectId || !appendBlockId) {
      setError(t('workbench:mobile.automationPanel.noProject'));
      return;
    }
    if (!trimmedTitle || !trimmedGoal || !trimmedAcceptanceCriteria) {
      setError(t('workbench:mobile.automationPanel.errors.required'));
      return;
    }
    setAppending(true);
    setError(null);
    try {
      const created = await httpOrchestratorTransport.tasks.appendBlockMember({
        projectId,
        blockId: appendBlockId,
        title: trimmedTitle,
        goal: trimmedGoal,
        acceptanceCriteria: trimmedAcceptanceCriteria,
        clientRequestId: createHttpOrchestratorClientRequestId(),
      });
      if (activeProjectIdRef.current !== projectId) return;
      setTaskViews((current) => upsertOrchestratorTaskView(current, created));
      setTitle('');
      setGoal('');
      setAcceptanceCriteria('');
      setCreateDialogOpen(false);
      setAppendBlockId(null);
      setCreateDialogKind('create');
    } catch (reason) {
      if (activeProjectIdRef.current !== projectId) return;
      setError(
        `${t('workbench:mobile.automationPanel.errors.appendBlock')}: ${getErrorMessage(reason)}`,
      );
    } finally {
      if (activeProjectIdRef.current === projectId) {
        setAppending(false);
      }
    }
  }, [appendBlockId, t, trimmedAcceptanceCriteria, trimmedGoal, trimmedTitle]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   移动端 Backlog/Todo 块提供上移/下移。
   *
   * Code Logic（这个函数做什么）:
   *   POST reorderBlockMembers 后按返回任务 upsert。
   */
  const handleReorderBlock = useCallback(
    (blockId: string, orderedTaskIds: string[]): void => {
      const projectId = activeProjectIdRef.current;
      if (!projectId) return;
      void (async () => {
        try {
          const updated = await httpOrchestratorTransport.tasks.reorderBlockMembers({
            projectId,
            blockId,
            orderedTaskIds,
            clientRequestId: createHttpOrchestratorClientRequestId(),
          });
          if (activeProjectIdRef.current !== projectId) return;
          setTaskViews((current) =>
            updated.reduce((views, view) => upsertOrchestratorTaskView(views, view), current),
          );
        } catch (reason) {
          if (activeProjectIdRef.current !== projectId) return;
          setError(
            `${t('workbench:mobile.automationPanel.errors.reorderBlock')}: ${getErrorMessage(reason)}`,
          );
        }
      })();
    },
    [t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   手机端 NeedsDecision 时采用推荐 winner。
   *
   * Code Logic（这个函数做什么）:
   *   approveWinner 后刷新 experiments 与 task list。
   */
  const handleApproveExperiment = useCallback(
    (experimentId: string, winnerTaskId: string): void => {
      const projectId = activeProjectIdRef.current;
      if (!projectId) return;
      void (async () => {
        try {
          const updated = await httpOrchestratorTransport.experiments.approveWinner(
            experimentId,
            winnerTaskId,
          );
          if (activeProjectIdRef.current !== projectId) return;
          setExperiments((current) =>
            current.map((item) => (item.id === updated.id ? updated : item)),
          );
          void loadTasks(projectId);
          requestAttentionInvalidation();
        } catch (reason) {
          if (activeProjectIdRef.current !== projectId) return;
          setError(getErrorMessage(reason));
        }
      })();
    },
    [loadTasks],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   手机端取消整组实验。
   *
   * Code Logic（这个函数做什么）:
   *   cancel 后更新 experiments。
   */
  const handleCancelExperiment = useCallback(
    (experimentId: string): void => {
      const projectId = activeProjectIdRef.current;
      if (!projectId) return;
      void (async () => {
        try {
          const updated = await httpOrchestratorTransport.experiments.cancel(experimentId);
          if (activeProjectIdRef.current !== projectId) return;
          setExperiments((current) =>
            current.map((item) => (item.id === updated.id ? updated : item)),
          );
          void loadTasks(projectId);
          requestAttentionInvalidation();
        } catch (reason) {
          if (activeProjectIdRef.current !== projectId) return;
          setError(getErrorMessage(reason));
        }
      })();
    },
    [loadTasks],
  );

  return {
    shell: {
      titleId,
      runtimeTitleId,
      hasProject,
      loading,
      error,
      status,
      isListEmpty,
      runtimeDisplay,
      runtimeStatusLabel,
      showRuntimeCachedHint,
      onRefresh: handleRefresh,
      onOpenCreateDialog: handleOpenCreateDialog,
    },
    taskList: {
      visibleWorkflowStates,
      groupedTasks,
      selectedTaskId: selectedTask?.id ?? null,
      unknownLabel,
      expandedBlockIds,
      canCreateTaskBlock,
      onSelectTaskView: handleSelectTaskView,
      onToggleBlock: handleToggleBlock,
      onOpenLaneCreate: handleOpenLaneCreate,
      onOpenAppend: handleOpenAppend,
      onReorderBlock: handleReorderBlock,
    },
    taskDetail: {
      selectedTask,
      detailTitleId,
      unknownLabel,
      evidenceItems,
      evidenceLoading,
      evidenceError,
      canOpenExecutionContext,
      onCloseDetails: handleCloseDetails,
      onOpenExecutionContext: handleOpenExecutionContext,
    },
    createDialog: {
      open: createDialogOpen,
      dialogTitleId,
      dialogKind: createDialogKind,
      createMode,
      preferredCreateAction,
      promptDraftRef,
      creating,
      completingPrompt,
      creatingAction,
      appending,
      promptDraft,
      title,
      goal,
      acceptanceCriteria,
      blockTitle,
      blockMembers,
      canCompletePrompt,
      canSubmit,
      canCreateBlock,
      canCreateTaskBlock,
      canAppend,
      createActions: MOBILE_AUTOMATION_CREATE_ACTIONS,
      onClose: handleCloseCreateDialog,
      onCreateModeChange: handleCreateModeChange,
      onPromptDraftChange: handlePromptDraftChange,
      onTitleChange: handleTitleChange,
      onGoalChange: handleGoalChange,
      onAcceptanceCriteriaChange: handleAcceptanceCriteriaChange,
      onBlockTitleChange: setBlockTitle,
      onUpdateBlockMember: (index, field, value) => {
        setBlockMembers((current) =>
          current.map((member, memberIndex) =>
            memberIndex === index ? { ...member, [field]: value } : member,
          ),
        );
      },
      onAddBlockMember: () => {
        setBlockMembers((current) =>
          current.length >= MAX_ORCHESTRATOR_BLOCK_MEMBERS
            ? current
            : [...current, { ...EMPTY_ORCHESTRATOR_CREATE_FORM }],
        );
      },
      onRemoveBlockMember: (index) => {
        setBlockMembers((current) =>
          current.length <= MIN_ORCHESTRATOR_BLOCK_MEMBERS
            ? current
            : current.filter((_, memberIndex) => memberIndex !== index),
        );
      },
      onCompletePrompt: handleCompletePromptClick,
      onCreateTask: handleCreateTaskClick,
      onAppendSubmit: () => {
        void handleAppendSubmit();
      },
    },
    outbox: {
      pendingRemoteItems,
      focusedOutboxId,
      outboxActionId,
      onRetry: handleRetryOutbox,
      onDiscard: handleDiscardOutbox,
    },
    experiments: {
      experiments,
      onApproveRecommended: handleApproveExperiment,
      onCancel: handleCancelExperiment,
    },
  };
}
