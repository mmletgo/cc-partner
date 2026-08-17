/**
 * Orchestrator 面板 controller
 *
 * Business Logic（为什么需要这个 controller）:
 *   Orchestrator 面板需要在当前 Workbench 项目下管理 task view 列表、详情/Evidence、创建弹窗、
 *   pending remote outbox 与 Attention deep link；这些状态机与 API 调用必须从 JSX 视图中剥离，
 *   以便 Board/Drawer/Outbox/Create 只做渲染。
 *
 * Code Logic（这个 controller 做什么）:
 *   - 持有全部 useState/useEffect/useCallback/useMemo（task list、selection、busy flags、form、evidence）
 *   - 调用 orchestratorApi / promptOptimizerApi / requestAttentionInvalidation
 *   - 解析 Attention focusTaskId/focusOutboxId deep link
 *   - 导出视图所需的派生数据与 handler；不渲染 Drawer/Dialog/board 泳道 JSX
 */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { DragEvent, FormEvent, RefObject } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';
import { orchestratorApi } from '@/api/orchestrator';
import { promptOptimizerApi } from '@/api/promptOptimizer';
import { buildExperimentCandidates, useAgentAdapterCatalog } from './useAgentAdapterCatalog';
import { requestAttentionInvalidation } from '@/hooks/attentionInvalidation';
import { useOrchestratorRuntimeSnapshot } from '@/hooks/useOrchestratorRuntimeSnapshot';
import { useWorkbenchProjects } from '@/hooks/workbenchProjectsContext';
import {
  canCancelOrchestratorTaskForProject,
  canCompleteAgentRunForProject,
  canControlBlockedTaskForProject,
  canDeliverReviewedTaskForProject,
  canRequestReworkForProject,
  canStartOrchestratorTaskForProject,
  orchestratorCreateResultMatchesProject,
  orchestratorTaskProgressMessage,
  resolveOrchestratorActionSelection,
  resolveOrchestratorTaskLoad,
} from '@/lib/orchestrator';
import {
  getOrchestratorTaskViewTaskId,
  type OrchestratorRenderableTask,
  isLocalOrchestratorTaskView,
  isOrchestratorTaskViewActionable,
  splitOrchestratorTaskViews,
  upsertOrchestratorTaskView,
} from '@/lib/orchestratorRemote';
import type {
  OrchestratorEvidence,
  OrchestratorExperiment,
  OrchestratorTask,
  OrchestratorTaskView,
  OrchestratorWorkflowState,
  WorkbenchProject,
  WorkflowDiagnostic,
  WorkflowDocumentLoadState,
  WorkflowDocumentStatus,
  OrchestratorAgentAdapterCatalogItem,
} from '@/lib/types';
import type { OrchestratorDetailTab } from '../views/OrchestratorTaskDrawer';
import type { OrchestratorBoardGroups } from '../orchestratorBoard';
import {
  canMoveRenderableTaskToWorkflowState,
  groupRenderableTasksByWorkflowState,
  ORCHESTRATOR_BOARD_LANES,
} from '../orchestratorBoard';
import { resolveOrchestratorFocusTarget } from '../orchestratorFocus';
import type { OrchestratorCreateAction, OrchestratorCreateForm } from '../orchestratorViewHelpers';
import {
  buildWorkbenchTaskUrl,
  displayOrchestratorErrorMessage,
  EMPTY_ORCHESTRATOR_CREATE_FORM,
  evidenceItemsByKind,
  latestEvidenceByKind,
} from '../orchestratorViewHelpers';
import { buildWorkbenchDeepLink } from '@/pages/Workbench/workbenchDeepLink';

/**
 * Business Logic（为什么需要这个类型）:
 *   Orchestrator task view 列表必须严格归属于当前 Workbench 项目，项目切换时不能短暂展示旧项目任务或 pending outbox。
 *
 * Code Logic（这个类型做什么）:
 *   把一次 task view 请求的 projectId、view 结果和错误一起保存，渲染阶段只使用 projectId 匹配当前项目的结果。
 */
interface OrchestratorTaskListResult {
  projectId: string;
  views: OrchestratorTaskView[];
  error: string | null;
}

/**
 * Business Logic（为什么需要这个类型）:
 *   Evidence 必须绑定 selected task 和 active project，项目或任务切换时不能显示旧请求结果。
 *
 * Code Logic（这个类型做什么）:
 *   保存一次 evidence 请求的 projectId、taskId、结果和错误，渲染阶段只使用完全匹配的结果。
 */
interface OrchestratorEvidenceResult {
  projectId: string;
  taskId: string;
  items: OrchestratorEvidence[];
  error: string | null;
}

const EMPTY_ORCHESTRATOR_EVIDENCE_ITEMS: OrchestratorEvidence[] = [];

/**
 * Business Logic（为什么需要这个类型）:
 *   创建/入队这类用户动作的错误只应该在触发动作的项目下展示，项目切换后不能残留旧错误。
 *
 * Code Logic（这个类型做什么）:
 *   保存动作发生时的 projectId 和错误文案，渲染阶段按当前 activeProjectId 过滤。
 */
interface OrchestratorActionError {
  projectId: string | null;
  message: string;
}

/**
 * Business Logic（为什么需要这个类型）:
 *   自动化看板既要保留独立页面壳，也要嵌入 Workbench 中作为项目 workspace view 使用。
 *
 * Code Logic（这个类型做什么）:
 *   embedded 控制是否隐藏页面级标题栏；onOpenWorkbench 允许嵌入方接管 blocked 任务的现场跳转；
 *   focus* / onFocus* 承接 Attention deep link。
 */
export interface OrchestratorPanelProps {
  embedded?: boolean;
  onOpenWorkbench?: (url: string) => void;
  /** Attention deep link：加载完成后打开任务详情/Evidence。 */
  focusTaskId?: string | null;
  /** Attention deep link：加载完成后聚焦 failed outbox 行。 */
  focusOutboxId?: string | null;
  /** 焦点目标已成功应用后的回调（供 Workbench 清 staged 标记）。 */
  onFocusTargetResolved?: (result: { kind: 'task' | 'outbox'; id: string }) => void;
  /**
   * 焦点目标不存在/已解决时的类型化回调。
   * Workbench 协调器应 refresh Attention 并回退 `/attention`，不得打开空白详情或终端。
   */
  onFocusTargetNotFound?: (result: { kind: 'task' | 'outbox'; id: string }) => void;
}

/**
 * Business Logic（为什么需要这个类型）:
 *   组合层与各 view 需要一份稳定、窄接口的派生状态与 handler，避免各自复制逻辑。
 *
 * Code Logic（这个类型做什么）:
 *   汇总 controller 对外暴露的全部 state、派生字段与事件处理函数。
 */
export interface UseOrchestratorControllerResult {
  embedded: boolean;
  activeProject: WorkbenchProject | null | undefined;
  activeProjectId: string | null;
  loading: boolean;
  error: string | null;
  tasks: OrchestratorRenderableTask[];
  pendingRemoteItems: ReturnType<typeof splitOrchestratorTaskViews>['pendingRemoteItems'];
  groups: OrchestratorBoardGroups;
  selectedTaskId: string | null;
  setSelectedTaskId: (taskId: string | null) => void;
  focusedOutboxId: string | null;
  selectedRenderableTask: OrchestratorRenderableTask | null;
  selectedTask: OrchestratorTask | null;
  selectedTaskCanStart: boolean;
  selectedTaskCanComplete: boolean;
  selectedTaskCanRequestRework: boolean;
  selectedTaskShowDeliver: boolean;
  selectedTaskCanDeliver: boolean;
  selectedTaskCanCancel: boolean;
  selectedTaskCanControlBlocked: boolean;
  selectedTaskCanOpenWorkbench: boolean;
  selectedTaskProgressMessage: string | null;
  selectedTaskTerminalLabel: string | null;
  creatingAction: OrchestratorCreateAction | null;
  startingTaskId: string | null;
  completingTaskId: string | null;
  reworkingTaskId: string | null;
  deliveringTaskId: string | null;
  retryingTaskId: string | null;
  cancelingTaskId: string | null;
  refreshingProjectId: string | null;
  movingTaskId: string | null;
  outboxActionId: string | null;
  form: OrchestratorCreateForm;
  createDialogOpen: boolean;
  completionPrompt: string;
  setCompletionPrompt: (value: string) => void;
  completingPrompt: boolean;
  completionPromptRef: RefObject<HTMLTextAreaElement | null>;
  canCreate: boolean;
  canCompletePrompt: boolean;
  evidenceItems: OrchestratorEvidence[];
  evidenceLoading: boolean;
  evidenceError: string | null;
  latestVerifierEvidence: OrchestratorEvidence | null;
  latestRepairPromptEvidence: OrchestratorEvidence | null;
  developmentAttemptEvidenceItems: OrchestratorEvidence[];
  runtimeSnapshot: ReturnType<typeof useOrchestratorRuntimeSnapshot>['snapshot'];
  runtimeRemoteStatus: ReturnType<typeof useOrchestratorRuntimeSnapshot>['remoteStatus'];
  runtimeCachedAt: ReturnType<typeof useOrchestratorRuntimeSnapshot>['cachedAt'];
  runtimeSnapshotLoading: boolean;
  runtimeSnapshotErrorMessage: string | null;
  showRuntimeSnapshotContent: boolean;
  handleOpenCreateDialog: () => void;
  handleCloseCreateDialog: () => void;
  handleCloseTaskDrawer: () => void;
  handleRefreshRuntimeSnapshot: () => void;
  handleOpenAutomationSettings: () => void;
  handleRetryRemoteOutbox: (outboxId: string) => Promise<void>;
  handleDiscardRemoteOutbox: (outboxId: string) => Promise<void>;
  updateFormField: (field: keyof OrchestratorCreateForm, value: string) => void;
  handleCompleteTaskPrompt: () => Promise<void>;
  handleCreateFormSubmit: (event: FormEvent<HTMLFormElement>) => void;
  handleCreateTaskAction: (createAction: OrchestratorCreateAction) => Promise<void>;
  handleTaskDragStart: (event: DragEvent<HTMLButtonElement>, item: OrchestratorRenderableTask) => void;
  handleTaskDragEnd: () => void;
  handleLaneDragOver: (event: DragEvent<HTMLElement>, targetState: OrchestratorWorkflowState) => void;
  handleLaneDrop: (event: DragEvent<HTMLElement>, targetState: OrchestratorWorkflowState) => Promise<void>;
  handleStartSelectedTask: () => Promise<void>;
  handleCompleteAgentRun: () => Promise<void>;
  detailTab: OrchestratorDetailTab;
  setDetailTab: (tab: OrchestratorDetailTab) => void;
  reworkDialogOpen: boolean;
  reworkError: string | null;
  handleOpenReworkDialog: () => void;
  handleCloseReworkDialog: () => void;
  handleSubmitRework: (reason: string) => Promise<void>;
  handleDeliverReviewedTask: () => Promise<void>;
  handleOpenWorkbench: () => void;
  handleRetryTask: () => Promise<void>;
  handleCancelTask: () => Promise<void>;
  handleRefreshProject: () => Promise<void>;
  workflowWizardOpen: boolean;
  workflowLoadState: WorkflowDocumentLoadState;
  workflowDocumentStatus: WorkflowDocumentStatus | null;
  workflowDraft: string;
  workflowExpectedHash: string;
  workflowDiagnostics: WorkflowDiagnostic[];
  workflowPreview: string | null;
  workflowLoadError: string | null;
  workflowSaveError: string | null;
  workflowConflict: boolean;
  workflowBusy: boolean;
  workflowFocusedDiagnosticLine: number | null;
  workflowDraftTextareaRef: RefObject<HTMLTextAreaElement | null>;
  handleOpenWorkflowWizard: () => void;
  handleCloseWorkflowWizard: () => void;
  handleWorkflowDraftChange: (value: string) => void;
  handleCreateWorkflowFromTemplate: () => void;
  handleValidateWorkflowDocument: () => Promise<void>;
  handleSaveWorkflowDocument: () => Promise<void>;
  handleReloadWorkflowDocument: () => Promise<void>;
  handleOpenWorkflowFile: () => void;
  handleFocusWorkflowDiagnostic: (diagnostic: WorkflowDiagnostic) => void;
  experiments: OrchestratorExperiment[];
  experimentsLoading: boolean;
  creatingExperiment: boolean;
  experimentActionId: string | null;
  handleCreateExperiment: () => Promise<void>;
  handleApproveExperimentWinner: (experimentId: string, winnerTaskId: string) => Promise<void>;
  handleCancelExperiment: (experimentId: string) => Promise<void>;
  agentAdapters: OrchestratorAgentAdapterCatalogItem[];
  canCreateExperiment: boolean;
}

/**
 * Business Logic（为什么需要这个 hook）:
 *   OrchestratorPanel 组合层与各视图需要共享同一套任务状态机，不能把 API/stale guard 散落在 JSX 中。
 *
 * Code Logic（这个 hook 做什么）:
 *   接受与 OrchestratorPanel 相同的 props，返回全部状态、派生 props 与 handler；不含 modal/board JSX。
 */
export function useOrchestratorController(
  props: OrchestratorPanelProps,
): UseOrchestratorControllerResult {
  const {
    embedded = false,
    onOpenWorkbench,
    focusTaskId = null,
    focusOutboxId = null,
    onFocusTargetResolved,
    onFocusTargetNotFound,
  } = props;
  const { t } = useTranslation(['orchestrator', 'nav', 'common']);
  const navigate = useNavigate();
  const { activeProject, projectsLoading } = useWorkbenchProjects();
  const [taskListResult, setTaskListResult] = useState<OrchestratorTaskListResult | null>(null);
  const [selectedTaskId, setSelectedTaskId] = useState<string | null>(null);
  const [focusedOutboxId, setFocusedOutboxId] = useState<string | null>(null);
  const focusHandledRef = useRef<string | null>(null);
  const [form, setForm] = useState<OrchestratorCreateForm>(EMPTY_ORCHESTRATOR_CREATE_FORM);
  const [creatingAction, setCreatingAction] = useState<OrchestratorCreateAction | null>(null);
  const [startingTaskId, setStartingTaskId] = useState<string | null>(null);
  const [completingTaskId, setCompletingTaskId] = useState<string | null>(null);
  const [reworkingTaskId, setReworkingTaskId] = useState<string | null>(null);
  const [deliveringTaskId, setDeliveringTaskId] = useState<string | null>(null);
  const [retryingTaskId, setRetryingTaskId] = useState<string | null>(null);
  const [cancelingTaskId, setCancelingTaskId] = useState<string | null>(null);
  const [refreshingProjectId, setRefreshingProjectId] = useState<string | null>(null);
  const [movingTaskId, setMovingTaskId] = useState<string | null>(null);
  const [draggedTaskId, setDraggedTaskId] = useState<string | null>(null);
  const [evidenceResult, setEvidenceResult] = useState<OrchestratorEvidenceResult | null>(null);
  const [actionError, setActionError] = useState<OrchestratorActionError | null>(null);
  const [outboxActionId, setOutboxActionId] = useState<string | null>(null);
  const [createDialogOpen, setCreateDialogOpen] = useState<boolean>(false);
  const [completionPrompt, setCompletionPrompt] = useState('');
  const [completingPrompt, setCompletingPrompt] = useState(false);
  const completionPromptRef = useRef<HTMLTextAreaElement | null>(null);
  const [detailTab, setDetailTab] = useState<OrchestratorDetailTab>('summary');
  const [reworkDialogOpen, setReworkDialogOpen] = useState(false);
  const [reworkError, setReworkError] = useState<string | null>(null);
  const [workflowWizardOpen, setWorkflowWizardOpen] = useState(false);
  const [workflowLoadState, setWorkflowLoadState] = useState<WorkflowDocumentLoadState>('idle');
  const [workflowDocumentStatus, setWorkflowDocumentStatus] =
    useState<WorkflowDocumentStatus | null>(null);
  const [workflowDraft, setWorkflowDraft] = useState('');
  const [workflowExpectedHash, setWorkflowExpectedHash] = useState('');
  const [workflowDiagnostics, setWorkflowDiagnostics] = useState<WorkflowDiagnostic[]>([]);
  const [workflowPreview, setWorkflowPreview] = useState<string | null>(null);
  const [workflowLoadError, setWorkflowLoadError] = useState<string | null>(null);
  const [workflowSaveError, setWorkflowSaveError] = useState<string | null>(null);
  const [workflowConflict, setWorkflowConflict] = useState(false);
  const [workflowBusy, setWorkflowBusy] = useState(false);
  const [workflowFocusedDiagnosticLine, setWorkflowFocusedDiagnosticLine] = useState<number | null>(
    null,
  );
  const workflowDraftTextareaRef = useRef<HTMLTextAreaElement | null>(null);
  const workflowRequestSeqRef = useRef(0);
  const [experiments, setExperiments] = useState<OrchestratorExperiment[]>([]);
  const [experimentsLoading, setExperimentsLoading] = useState(false);
  const [boundExperimentsKey, setBoundExperimentsKey] = useState<string | null | undefined>(
    undefined,
  );
  const [creatingExperiment, setCreatingExperiment] = useState(false);
  const [experimentActionId, setExperimentActionId] = useState<string | null>(null);
  const agentAdapters = useAgentAdapterCatalog();
  const canCreateExperiment = useMemo(
    () => buildExperimentCandidates(agentAdapters).length >= 2,
    [agentAdapters],
  );
  const activeProjectId = activeProject?.id ?? null;
  const activeProjectIdRef = useRef<string | null>(activeProjectId);
  const taskLoadDecision = useMemo(
    () => resolveOrchestratorTaskLoad(projectsLoading, activeProjectId),
    [activeProjectId, projectsLoading],
  );
  // project 键变化时在 render 中清空实验组，避免 setState-in-effect 同步清空
  const experimentsProjectKey =
    taskLoadDecision.kind === 'load' ? taskLoadDecision.projectId : null;
  if (boundExperimentsKey !== experimentsProjectKey) {
    setBoundExperimentsKey(experimentsProjectKey);
    setExperiments([]);
    setExperimentsLoading(experimentsProjectKey !== null);
  }
  const runtimeSnapshotEnabled = taskLoadDecision.kind === 'load';
  const runtimeSnapshotProjectId = runtimeSnapshotEnabled ? taskLoadDecision.projectId : null;
  const {
    snapshot: runtimeSnapshot,
    remoteStatus: runtimeRemoteStatus,
    cachedAt: runtimeCachedAt,
    loading: runtimeSnapshotLoading,
    error: runtimeSnapshotError,
    refresh: refreshRuntimeSnapshot,
  } = useOrchestratorRuntimeSnapshot({
    projectId: runtimeSnapshotProjectId,
    enabled: runtimeSnapshotEnabled,
  });

  useEffect(() => {
    activeProjectIdRef.current = activeProjectId;
  }, [activeProjectId]);

  const activeTaskListResult =
    taskLoadDecision.kind === 'load' && taskListResult?.projectId === taskLoadDecision.projectId
      ? taskListResult
      : null;
  const taskViews = useMemo(() => activeTaskListResult?.views ?? [], [activeTaskListResult]);
  const taskViewSplit = useMemo(() => splitOrchestratorTaskViews(taskViews), [taskViews]);
  const tasks = taskViewSplit.tasks;
  const pendingRemoteItems = taskViewSplit.pendingRemoteItems;
  const loading =
    taskLoadDecision.kind === 'waiting' ||
    (taskLoadDecision.kind === 'load' && !activeTaskListResult);
  const taskLoadError = activeTaskListResult?.error ?? null;
  const visibleActionError =
    actionError?.projectId === activeProjectId ? actionError.message : null;
  const error = visibleActionError ?? taskLoadError;

  /**
   * Business Logic（为什么需要这个 effect）:
   *   Attention deep link 必须在 task/outbox 列表加载后聚焦详情或失败项；
   *   目标已解决时要类型化回报协调器，不能渲染空白详情或打开终端。
   *
   * Code Logic（这个 effect 做什么）:
   *   用 resolveOrchestratorFocusTarget 判定；found 时选中任务/高亮 outbox；
   *   not_found 时调用 onFocusTargetNotFound 一次（按 focus key 去重）。
   */
  useEffect(() => {
    const focusKey = `${focusTaskId ?? ''}|${focusOutboxId ?? ''}|${activeProjectId ?? ''}`;
    const result = resolveOrchestratorFocusTarget({
      loading,
      focusTaskId,
      focusOutboxId,
      taskIds: tasks.map((item) => item.task.id),
      outboxIds: pendingRemoteItems.map((item) => item.id),
    });

    if (result.status === 'none' || result.status === 'pending') {
      if (result.status === 'none') {
        focusHandledRef.current = null;
      }
      return;
    }

    if (focusHandledRef.current === focusKey) return;
    focusHandledRef.current = focusKey;

    if (result.status === 'found') {
      queueMicrotask(() => {
        if (result.kind === 'task') {
          setSelectedTaskId(result.id);
          setFocusedOutboxId(null);
        } else {
          setFocusedOutboxId(result.id);
        }
        onFocusTargetResolved?.(result);
      });
      return;
    }

    queueMicrotask(() => {
      onFocusTargetNotFound?.(result);
    });
  }, [
    activeProjectId,
    focusOutboxId,
    focusTaskId,
    loading,
    onFocusTargetNotFound,
    onFocusTargetResolved,
    pendingRemoteItems,
    tasks,
  ]);

  const groups = useMemo(() => groupRenderableTasksByWorkflowState(tasks), [tasks]);
  const selectedRenderableTask = useMemo(() => {
    return tasks.find((item) => item.task.id === selectedTaskId) ?? null;
  }, [selectedTaskId, tasks]);
  const selectedTaskView = selectedRenderableTask?.view ?? null;
  const selectedTask = selectedRenderableTask?.task ?? null;
  const selectedTaskCanStart = canStartOrchestratorTaskForProject(selectedTask, activeProjectId);
  const selectedTaskCanComplete =
    isLocalOrchestratorTaskView(selectedTaskView) &&
    canCompleteAgentRunForProject(selectedTask, activeProjectId);
  const selectedTaskCanRequestRework = canRequestReworkForProject(selectedTask, activeProjectId);
  const selectedTaskShowDeliver = canDeliverReviewedTaskForProject(selectedTask, activeProjectId);
  const selectedTaskCanDeliver = selectedTaskShowDeliver;
  const selectedTaskCanCancel = canCancelOrchestratorTaskForProject(selectedTask, activeProjectId);
  const selectedTaskCanControlBlocked = canControlBlockedTaskForProject(
    selectedTask,
    activeProjectId,
  );
  const selectedTaskCanOpenWorkbench = Boolean(
    selectedTask?.projectId && (selectedTask.worktreeId || selectedTask.sessionId),
  );
  const selectedTaskProgressMessage = orchestratorTaskProgressMessage(selectedTaskView, t);
  const selectedTaskTerminalLabel = selectedTask?.sessionId ?? selectedTask?.worktreeId ?? null;
  const runtimeSnapshotErrorMessage = runtimeSnapshotError
    ? displayOrchestratorErrorMessage(runtimeSnapshotError, t('orchestrator:errors.snapshot'))
    : null;
  const showRuntimeSnapshotContent = Boolean(runtimeSnapshot);
  const activeEvidenceResult =
    selectedTask &&
    taskLoadDecision.kind === 'load' &&
    evidenceResult?.projectId === taskLoadDecision.projectId &&
    evidenceResult.taskId === selectedTask.id
      ? evidenceResult
      : null;
  const evidenceItems = activeEvidenceResult?.items ?? EMPTY_ORCHESTRATOR_EVIDENCE_ITEMS;
  const latestVerifierEvidence = useMemo(
    () => latestEvidenceByKind(evidenceItems, 'verificationReview'),
    [evidenceItems],
  );
  const latestRepairPromptEvidence = useMemo(
    () => latestEvidenceByKind(evidenceItems, 'repairPrompt'),
    [evidenceItems],
  );
  const developmentAttemptEvidenceItems = useMemo(
    () => evidenceItemsByKind(evidenceItems, 'developmentAttempt'),
    [evidenceItems],
  );
  const evidenceLoading =
    Boolean(selectedTask) &&
    taskLoadDecision.kind === 'load' &&
    selectedTask?.projectId === taskLoadDecision.projectId &&
    !activeEvidenceResult;
  const evidenceError = activeEvidenceResult?.error ?? null;

  const canCreate =
    Boolean(activeProjectId) &&
    form.title.trim().length > 0 &&
    form.goal.trim().length > 0 &&
    form.acceptanceCriteria.trim().length > 0 &&
    !creatingAction;
  const canCompletePrompt =
    Boolean(activeProjectId) && completionPrompt.trim().length > 0 && !completingPrompt;

  const createPromptWorkingDirectory = useMemo(() => {
    if (!activeProject || activeProject.kind !== 'local') return null;
    return activeProject.path;
  }, [activeProject]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户从队列打开创建弹窗前需要清掉旧动作错误。
   *
   * Code Logic（这个函数做什么）:
   *   清空 actionError 并打开 createDialogOpen。
   */
  const handleOpenCreateDialog = useCallback(() => {
    setActionError(null);
    setCreateDialogOpen(true);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   创建/AI 完善进行中时禁止关闭弹窗，避免中断进行中的提交。
   *
   * Code Logic（这个函数做什么）:
   *   creatingAction 或 completingPrompt 为真时 early-return；否则关闭 createDialogOpen。
   */
  const handleCloseCreateDialog = useCallback(() => {
    if (creatingAction || completingPrompt || creatingExperiment) return;
    setCreateDialogOpen(false);
  }, [completingPrompt, creatingAction, creatingExperiment]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   关闭任务详情抽屉后应回到纯看板视图。
   *
   * Code Logic（这个函数做什么）:
   *   将 selectedTaskId 置为 null。
   */
  const handleCloseTaskDrawer = useCallback(() => {
    setSelectedTaskId(null);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户需要手动刷新 runtime snapshot 状态条。
   *
   * Code Logic（这个函数做什么）:
   *   有 active project 时调用 refreshRuntimeSnapshot。
   */
  const handleRefreshRuntimeSnapshot = useCallback(() => {
    if (!activeProjectId) return;
    void refreshRuntimeSnapshot();
  }, [activeProjectId, refreshRuntimeSnapshot]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   failed outbox 的 Retry/Discard 成功后需要刷新当前项目的 task-view 列表，
   *   让 pending 区反映 pending/discarded 变化，并立即失效全局 Inbox 投影。
   *
   * Code Logic（这个函数做什么）:
   *   用 active projectId 调 listTaskViews，并用 activeProjectIdRef 做 stale guard；
   *   列表刷新成功后调用 requestAttentionInvalidation。
   */
  const reloadTaskViewsForActiveProject = useCallback(async (): Promise<void> => {
    const projectId = activeProjectIdRef.current;
    if (!projectId) return;
    try {
      const nextViews = await orchestratorApi.listTaskViews(projectId);
      if (activeProjectIdRef.current !== projectId) return;
      setTaskListResult({ projectId, views: nextViews, error: null });
      setSelectedTaskId((current) => {
        const nextSplit = splitOrchestratorTaskViews(nextViews);
        if (current && nextSplit.tasks.some((item) => item.task.id === current)) return current;
        return null;
      });
      requestAttentionInvalidation();
    } catch (err) {
      if (activeProjectIdRef.current !== projectId) return;
      setActionError({
        projectId,
        message: displayOrchestratorErrorMessage(err, t('orchestrator:errors.load')),
      });
    }
  }, [t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户在原 Automation UI 对 failed outbox 点 Retry 时，应保留 payload 回到 pending。
   *
   * Code Logic（这个函数做什么）:
   *   校验 active project 与 busy 状态，调用 orchestratorApi.retryRemoteOutbox，成功后 reload 列表。
   */
  const handleRetryRemoteOutbox = useCallback(
    async (outboxId: string): Promise<void> => {
      const projectId = activeProjectIdRef.current;
      if (!projectId || outboxActionId) return;
      setOutboxActionId(outboxId);
      setActionError(null);
      try {
        await orchestratorApi.retryRemoteOutbox(projectId, outboxId);
        if (activeProjectIdRef.current !== projectId) return;
        await reloadTaskViewsForActiveProject();
      } catch (err) {
        if (activeProjectIdRef.current !== projectId) return;
        setActionError({
          projectId,
          message: displayOrchestratorErrorMessage(err, t('orchestrator:errors.retryOutbox')),
        });
      } finally {
        if (activeProjectIdRef.current === projectId) {
          setOutboxActionId(null);
        }
      }
    },
    [outboxActionId, reloadTaskViewsForActiveProject, t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户在原 Automation UI 对 failed outbox 点 Discard 时，需要确认后再进入 discarded 审计终态。
   *
   * Code Logic（这个函数做什么）:
   *   window.confirm 后调用 discardRemoteOutbox，成功后 reload 列表。
   */
  const handleDiscardRemoteOutbox = useCallback(
    async (outboxId: string): Promise<void> => {
      const projectId = activeProjectIdRef.current;
      if (!projectId || outboxActionId) return;
      const confirmed = window.confirm(t('orchestrator:pending.discardConfirm'));
      if (!confirmed) return;
      setOutboxActionId(outboxId);
      setActionError(null);
      try {
        await orchestratorApi.discardRemoteOutbox(projectId, outboxId);
        if (activeProjectIdRef.current !== projectId) return;
        await reloadTaskViewsForActiveProject();
      } catch (err) {
        if (activeProjectIdRef.current !== projectId) return;
        setActionError({
          projectId,
          message: displayOrchestratorErrorMessage(err, t('orchestrator:errors.discardOutbox')),
        });
      } finally {
        if (activeProjectIdRef.current === projectId) {
          setOutboxActionId(null);
        }
      }
    },
    [outboxActionId, reloadTaskViewsForActiveProject, t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   状态条入口应跳到 Settings 自动化 tab，而不是在 Orchestrator 内编辑配置。
   *
   * Code Logic（这个函数做什么）:
   *   navigate 到 `/settings?tab=automation`。
   */
  const handleOpenAutomationSettings = useCallback(() => {
    navigate('/settings?tab=automation');
  }, [navigate]);
  useEffect(() => {
    if (taskLoadDecision.kind !== 'load') return undefined;

    let cancelled = false;
    const projectId = taskLoadDecision.projectId;
    void orchestratorApi
      .listTaskViews(projectId)
      .then((nextViews) => {
        if (cancelled || activeProjectIdRef.current !== projectId) return;
        const nextSplit = splitOrchestratorTaskViews(nextViews);
        setTaskListResult({ projectId, views: nextViews, error: null });
        setSelectedTaskId((current) => {
          if (current && nextSplit.tasks.some((item) => item.task.id === current)) return current;
          return null;
        });
      })
      .catch((err: unknown) => {
        if (cancelled || activeProjectIdRef.current !== projectId) return;
        setTaskListResult((current) => ({
          projectId,
          views: current?.projectId === projectId ? current.views : [],
          error: displayOrchestratorErrorMessage(err, t('orchestrator:errors.load')),
        }));
      });
    return () => {
      cancelled = true;
    };
  }, [taskLoadDecision, t]);

  /**
   * Business Logic（为什么需要这个 effect）:
   *   实验组列表必须与当前项目同步，支持 NeedsDecision 决策入口。
   *
   * Code Logic（这个 effect 做什么）:
   *   project 加载后 listExperiments；清空已在 render 中按 key 完成；stale guard 丢弃过期响应。
   */
  useEffect(() => {
    if (taskLoadDecision.kind !== 'load') {
      return undefined;
    }
    let cancelled = false;
    const projectId = taskLoadDecision.projectId;
    void orchestratorApi
      .listExperiments(projectId)
      .then((items) => {
        if (cancelled || activeProjectIdRef.current !== projectId) return;
        setExperiments(items);
        setExperimentsLoading(false);
      })
      .catch((err: unknown) => {
        if (cancelled || activeProjectIdRef.current !== projectId) return;
        setExperiments([]);
        setExperimentsLoading(false);
        setActionError({
          projectId,
          message: displayOrchestratorErrorMessage(err, t('orchestrator:errors.experimentsLoad')),
        });
      });
    return () => {
      cancelled = true;
    };
  }, [taskLoadDecision, t]);

  useEffect(() => {
    if (taskLoadDecision.kind !== 'load' || !selectedTask) return undefined;
    if (selectedTask.projectId !== taskLoadDecision.projectId) return undefined;

    let cancelled = false;
    const projectId = taskLoadDecision.projectId;
    const taskId = selectedTask.id;
    void orchestratorApi
      .listEvidence(projectId, taskId)
      .then((items) => {
        if (cancelled || activeProjectIdRef.current !== projectId) return;
        setEvidenceResult({ projectId, taskId, items, error: null });
      })
      .catch((err: unknown) => {
        if (cancelled || activeProjectIdRef.current !== projectId) return;
        setEvidenceResult({
          projectId,
          taskId,
          items: [],
          error: displayOrchestratorErrorMessage(err, t('orchestrator:errors.evidence')),
        });
      });
    return () => {
      cancelled = true;
    };
  }, [selectedTask, taskLoadDecision, t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   创建表单三个字段需要独立更新且保留其它字段。
   *
   * Code Logic（这个函数做什么）:
   *   按 field key 更新 form 中对应字符串值。
   */
  const updateFormField = useCallback((field: keyof OrchestratorCreateForm, value: string) => {
    setForm((current) => ({ ...current, [field]: value }));
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户可用简单 Prompt 让 AI 填充 title/goal/acceptanceCriteria，而不直接创建任务。
   *
   * Code Logic（这个函数做什么）:
   *   调用 promptOptimizerApi.completeOrchestratorTaskPrompt，成功后写回 form；含 project stale guard。
   */
  const handleCompleteTaskPrompt = useCallback(async () => {
    if (!activeProject) {
      setActionError({ projectId: activeProjectId, message: t('orchestrator:errors.noProject') });
      return;
    }
    const prompt = completionPrompt.trim();
    if (!prompt) {
      setActionError({ projectId: activeProject.id, message: t('orchestrator:errors.promptRequired') });
      return;
    }
    const projectId = activeProject.id;
    setCompletingPrompt(true);
    setActionError(null);
    try {
      const completed = await promptOptimizerApi.completeOrchestratorTaskPrompt(prompt, {
        workingDirectory: createPromptWorkingDirectory,
      });
      if (activeProjectIdRef.current !== projectId) return;
      setForm({
        title: completed.title.trim(),
        goal: completed.goal.trim(),
        acceptanceCriteria: completed.acceptanceCriteria.trim(),
      });
    } catch (err) {
      if (activeProjectIdRef.current === projectId) {
        setActionError({
          projectId,
          message: displayOrchestratorErrorMessage(err, t('orchestrator:errors.completePrompt')),
        });
      }
    } finally {
      setCompletingPrompt(false);
    }
  }, [activeProject, activeProjectId, completionPrompt, createPromptWorkingDirectory, t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   创建提交按钮必须显式传入 createAction，不能隐式默认 backlog。
   *
   * Code Logic（这个函数做什么）:
   *   校验表单后调用 orchestratorApi.createTaskView(payload 含 createAction)，成功 upsert 列表并关闭弹窗。
   */
  const submitCreateTask = useCallback(
    async (createAction: OrchestratorCreateAction) => {
      if (!activeProject) {
        setActionError({ projectId: activeProjectId, message: t('orchestrator:errors.noProject') });
        return;
      }
      const projectId = activeProject.id;
      const payload = {
        projectId,
        title: form.title.trim(),
        goal: form.goal.trim(),
        acceptanceCriteria: form.acceptanceCriteria.trim(),
        createAction,
      };
      if (!payload.title || !payload.goal || !payload.acceptanceCriteria) {
        setActionError({ projectId, message: t('orchestrator:errors.required') });
        return;
      }
      setCreatingAction(createAction);
      setActionError(null);
      try {
        const created = await orchestratorApi.createTaskView(payload);
        if (!orchestratorCreateResultMatchesProject(activeProjectIdRef.current, projectId)) {
          return;
        }
        setTaskListResult((current) => {
          const currentViews = current?.projectId === projectId ? current.views : [];
          return {
            projectId,
            views: upsertOrchestratorTaskView(currentViews, created),
            error: null,
          };
        });
        setForm(EMPTY_ORCHESTRATOR_CREATE_FORM);
        setCompletionPrompt('');
        setCreateDialogOpen(false);
        void refreshRuntimeSnapshot();
      } catch (err) {
        setActionError({
          projectId,
          message: displayOrchestratorErrorMessage(err, t('orchestrator:errors.create')),
        });
      } finally {
        setCreatingAction(null);
      }
    },
    [activeProject, activeProjectId, form, refreshRuntimeSnapshot, t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   表单 submit 只用于阻止默认浏览器提交，真正创建由三个显式按钮触发。
   *
   * Code Logic（这个函数做什么）:
   *   event.preventDefault()，不发起创建。
   */
  const handleCreateFormSubmit = useCallback((event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   三个创建按钮需要把各自的 createAction 传给提交逻辑。
   *
   * Code Logic（这个函数做什么）:
   *   转发 createAction 到 submitCreateTask。
   */
  const handleCreateTaskAction = useCallback(
    async (createAction: OrchestratorCreateAction) => {
      await submitCreateTask(createAction);
    },
    [submitCreateTask],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户可从同一创建表单字段发起 2-candidate 比较实验（默认两条策略）。
   *
   * Code Logic（这个函数做什么）:
   *   用 title/goal/acceptance 调用 createExperiment（2 candidates, maxParallel=1）。
   */
  const handleCreateExperiment = useCallback(async () => {
    if (!activeProject) {
      setActionError({ projectId: activeProjectId, message: t('orchestrator:errors.noProject') });
      return;
    }
    const projectId = activeProject.id;
    const title = form.title.trim();
    const goal = form.goal.trim();
    const acceptance = form.acceptanceCriteria.trim();
    if (!title || !goal || !acceptance) {
      setActionError({ projectId, message: t('orchestrator:errors.required') });
      return;
    }
    const candidates = buildExperimentCandidates(agentAdapters);
    if (candidates.length < 2) {
      setActionError({ projectId, message: t('orchestrator:errors.experimentsCreate') });
      return;
    }
    setCreatingExperiment(true);
    setActionError(null);
    try {
      const created = await orchestratorApi.createExperiment({
        clientRequestId: crypto.randomUUID(),
        projectId,
        title,
        goal,
        acceptance,
        maxParallel: 1,
        candidates,
      });
      if (activeProjectIdRef.current !== projectId) return;
      setExperiments((current) => {
        const without = current.filter((item) => item.id !== created.id);
        return [created, ...without];
      });
      setForm(EMPTY_ORCHESTRATOR_CREATE_FORM);
      setCompletionPrompt('');
      setCreateDialogOpen(false);
      void refreshRuntimeSnapshot();
      requestAttentionInvalidation();
      // 实验创建会插入 candidate tasks，刷新 task board
      void reloadTaskViewsForActiveProject();
    } catch (err) {
      if (activeProjectIdRef.current === projectId) {
        setActionError({
          projectId,
          message: displayOrchestratorErrorMessage(err, t('orchestrator:errors.experimentsCreate')),
        });
      }
    } finally {
      if (activeProjectIdRef.current === projectId) {
        setCreatingExperiment(false);
      }
    }
  }, [activeProject, activeProjectId, agentAdapters, form.acceptanceCriteria, form.goal, form.title, refreshRuntimeSnapshot, reloadTaskViewsForActiveProject, t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   NeedsDecision / WinnerReady 时用户可采用推荐 winner。
   *
   * Code Logic（这个函数做什么）:
   *   approveExperimentWinner 后更新 experiments 列表并 invalidation Attention。
   */
  const handleApproveExperimentWinner = useCallback(
    async (experimentId: string, winnerTaskId: string) => {
      const projectId = activeProjectIdRef.current;
      if (!projectId || experimentActionId) return;
      setExperimentActionId(experimentId);
      setActionError(null);
      try {
        const updated = await orchestratorApi.approveExperimentWinner(
          experimentId,
          winnerTaskId,
          t('orchestrator:experiments.approveReason'),
        );
        if (activeProjectIdRef.current !== projectId) return;
        setExperiments((current) =>
          current.map((item) => (item.id === updated.id ? updated : item)),
        );
        requestAttentionInvalidation();
        void reloadTaskViewsForActiveProject();
        void refreshRuntimeSnapshot();
      } catch (err) {
        if (activeProjectIdRef.current === projectId) {
          setActionError({
            projectId,
            message: displayOrchestratorErrorMessage(
              err,
              t('orchestrator:errors.experimentsApprove'),
            ),
          });
        }
      } finally {
        if (activeProjectIdRef.current === projectId) {
          setExperimentActionId(null);
        }
      }
    },
    [experimentActionId, refreshRuntimeSnapshot, reloadTaskViewsForActiveProject, t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户可取消整组实验。
   *
   * Code Logic（这个函数做什么）:
   *   cancelExperiment 后更新列表。
   */
  const handleCancelExperiment = useCallback(
    async (experimentId: string) => {
      const projectId = activeProjectIdRef.current;
      if (!projectId || experimentActionId) return;
      setExperimentActionId(experimentId);
      setActionError(null);
      try {
        const updated = await orchestratorApi.cancelExperiment(experimentId);
        if (activeProjectIdRef.current !== projectId) return;
        setExperiments((current) =>
          current.map((item) => (item.id === updated.id ? updated : item)),
        );
        requestAttentionInvalidation();
        void reloadTaskViewsForActiveProject();
      } catch (err) {
        if (activeProjectIdRef.current === projectId) {
          setActionError({
            projectId,
            message: displayOrchestratorErrorMessage(
              err,
              t('orchestrator:errors.experimentsCancel'),
            ),
          });
        }
      } finally {
        if (activeProjectIdRef.current === projectId) {
          setExperimentActionId(null);
        }
      }
    },
    [experimentActionId, reloadTaskViewsForActiveProject, t],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   各 task action 成功后需要把返回的 task view 写回当前项目列表，并尽量保持 selection。
   *
   * Code Logic（这个函数做什么）:
   *   projectId 匹配时 upsert 视图；若有 taskId 则经 resolveOrchestratorActionSelection 更新 selection。
   */
  const replaceTaskViewInCurrentProject = useCallback(
    (projectId: string, nextView: OrchestratorTaskView) => {
      setTaskListResult((current) => {
        if (!current || current.projectId !== projectId) return current;
        return {
          ...current,
          views: upsertOrchestratorTaskView(current.views, nextView),
          error: null,
        };
      });
      const nextTaskId = getOrchestratorTaskViewTaskId(nextView);
      if (nextTaskId) {
        setSelectedTaskId((current) => resolveOrchestratorActionSelection(current, nextTaskId));
      }
    },
    [],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   拖拽 drop 时需要按 taskId 找回可渲染任务，以校验是否允许移动。
   *
   * Code Logic（这个函数做什么）:
   *   在当前 tasks 中查找 task.id 匹配项，找不到返回 null。
   */
  const getRenderableTaskById = useCallback(
    (taskId: string | null): OrchestratorRenderableTask | null => {
      if (!taskId) return null;
      return tasks.find((item) => item.task.id === taskId) ?? null;
    },
    [tasks],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   只有本机非活跃且可移至相邻泳道的任务才允许开始拖拽。
   *
   * Code Logic（这个函数做什么）:
   *   校验 canMove；通过则 setData 与 setDraggedTaskId，否则 preventDefault。
   */
  const handleTaskDragStart = useCallback(
    (event: DragEvent<HTMLButtonElement>, item: OrchestratorRenderableTask) => {
      const canMoveToAdjacentLane = ORCHESTRATOR_BOARD_LANES.some((lane) =>
        canMoveRenderableTaskToWorkflowState(item, lane),
      );
      if (!canMoveToAdjacentLane || movingTaskId === item.task.id) {
        event.preventDefault();
        return;
      }
      event.dataTransfer.effectAllowed = 'move';
      event.dataTransfer.setData('text/plain', item.task.id);
      setDraggedTaskId(item.task.id);
    },
    [movingTaskId],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   拖拽结束后需要清掉 dragged 高亮态。
   *
   * Code Logic（这个函数做什么）:
   *   setDraggedTaskId(null)。
   */
  const handleTaskDragEnd = useCallback(() => {
    setDraggedTaskId(null);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   泳道 dragOver 只在合法相邻移动时允许 drop。
   *
   * Code Logic（这个函数做什么）:
   *   校验 canMove 后 preventDefault 并设置 dropEffect。
   */
  const handleLaneDragOver = useCallback(
    (event: DragEvent<HTMLElement>, targetState: OrchestratorWorkflowState) => {
      if (movingTaskId) return;
      const draggedItem = getRenderableTaskById(draggedTaskId);
      if (!canMoveRenderableTaskToWorkflowState(draggedItem, targetState)) return;
      event.preventDefault();
      event.dataTransfer.dropEffect = 'move';
    },
    [draggedTaskId, getRenderableTaskById, movingTaskId],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   合法 drop 需要把本机任务移动到相邻 workflow 泳道。
   *
   * Code Logic（这个函数做什么）:
   *   调用 orchestratorApi.moveTaskWorkflowState，成功后 replace 列表并刷新 runtime snapshot。
   */
  const handleLaneDrop = useCallback(
    async (event: DragEvent<HTMLElement>, targetState: OrchestratorWorkflowState) => {
      const droppedTaskId = event.dataTransfer.getData('text/plain') || draggedTaskId;
      const droppedItem = getRenderableTaskById(droppedTaskId);
      if (!droppedItem || movingTaskId || !canMoveRenderableTaskToWorkflowState(droppedItem, targetState)) {
        return;
      }
      event.preventDefault();
      const taskId = droppedItem.task.id;
      const projectId = droppedItem.task.projectId;
      setMovingTaskId(taskId);
      setActionError(null);
      try {
        const moved = await orchestratorApi.moveTaskWorkflowState(projectId, taskId, targetState);
        const movedProjectId = moved.origin === 'pendingRemote' ? null : moved.task.projectId;
        if (activeProjectIdRef.current !== projectId || movedProjectId !== projectId) {
          return;
        }
        replaceTaskViewInCurrentProject(projectId, moved);
        void refreshRuntimeSnapshot();
      } catch (err) {
        if (activeProjectIdRef.current === projectId) {
          setActionError({
            projectId,
            message: displayOrchestratorErrorMessage(err, t('orchestrator:errors.move')),
          });
        }
      } finally {
        setMovingTaskId((current) => (current === taskId ? null : current));
        setDraggedTaskId((current) => (current === taskId ? null : current));
      }
    },
    [
      draggedTaskId,
      getRenderableTaskById,
      movingTaskId,
      refreshRuntimeSnapshot,
      replaceTaskViewInCurrentProject,
      t,
    ],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   详情抽屉对可启动任务提供 Start 入口。
   *
   * Code Logic（这个函数做什么）:
   *   校验 canStart 后调用 orchestratorApi.startTaskView，成功 replace 并刷新 snapshot。
   */
  const handleStartSelectedTask = useCallback(async () => {
    if (
      !selectedTask ||
      !isOrchestratorTaskViewActionable(selectedTaskView) ||
      !canStartOrchestratorTaskForProject(selectedTask, activeProjectIdRef.current)
    ) {
      return;
    }
    const taskId = selectedTask.id;
    const projectId = selectedTask.projectId;
    setStartingTaskId(taskId);
    setActionError(null);
    try {
      const started = await orchestratorApi.startTaskView(projectId, taskId);
      const startedProjectId = started.origin === 'pendingRemote' ? null : started.task.projectId;
      if (
        !orchestratorCreateResultMatchesProject(activeProjectIdRef.current, projectId) ||
        startedProjectId !== projectId
      ) {
        return;
      }
      replaceTaskViewInCurrentProject(projectId, started);
      void refreshRuntimeSnapshot();
    } catch (err) {
      if (orchestratorCreateResultMatchesProject(activeProjectIdRef.current, projectId)) {
        setActionError({
          projectId,
          message: displayOrchestratorErrorMessage(err, t('orchestrator:errors.start')),
        });
      }
    } finally {
      setStartingTaskId((current) => (current === taskId ? null : current));
    }
  }, [refreshRuntimeSnapshot, replaceTaskViewInCurrentProject, selectedTask, selectedTaskView, t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   本机 running 任务可触发 complete agent run，进入验证/交付链路。
   *
   * Code Logic（这个函数做什么）:
   *   仅 local 可完成任务调用 completeAgentRun，成功后 replace 并重新拉 evidence。
   */
  const handleCompleteAgentRun = useCallback(async () => {
    if (
      !selectedTask ||
      !isLocalOrchestratorTaskView(selectedTaskView) ||
      !canCompleteAgentRunForProject(selectedTask, activeProjectIdRef.current) ||
      completingTaskId === selectedTask.id
    ) {
      return;
    }
    const taskId = selectedTask.id;
    const projectId = selectedTask.projectId;
    setCompletingTaskId(taskId);
    setActionError(null);
    try {
      const updated = await orchestratorApi.completeAgentRun(taskId);
      if (
        !orchestratorCreateResultMatchesProject(activeProjectIdRef.current, projectId) ||
        updated.projectId !== projectId
      ) {
        return;
      }
      replaceTaskViewInCurrentProject(projectId, { origin: 'local', task: updated });
      void refreshRuntimeSnapshot();
      try {
        const items = await orchestratorApi.listEvidence(projectId, taskId);
        if (activeProjectIdRef.current === projectId) {
          setEvidenceResult({ projectId, taskId, items, error: null });
        }
      } catch (err) {
        if (activeProjectIdRef.current === projectId) {
          setEvidenceResult({
            projectId,
            taskId,
            items: [],
            error: displayOrchestratorErrorMessage(err, t('orchestrator:errors.evidence')),
          });
        }
      }
    } catch (err) {
      if (orchestratorCreateResultMatchesProject(activeProjectIdRef.current, projectId)) {
        setActionError({
          projectId,
          message: displayOrchestratorErrorMessage(err, t('orchestrator:errors.complete')),
        });
      }
    } finally {
      setCompletingTaskId((current) => (current === taskId ? null : current));
    }
  }, [
    completingTaskId,
    refreshRuntimeSnapshot,
    replaceTaskViewInCurrentProject,
    selectedTask,
    selectedTaskView,
    t,
  ]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   打开返工 Dialog，用共享 Dialog 替代 window.prompt。
   *
   * Code Logic（这个函数做什么）:
   *   校验可 rework 后 setReworkDialogOpen(true) 并清空 reworkError。
   */
  const handleOpenReworkDialog = useCallback(() => {
    if (
      !selectedTask ||
      !isOrchestratorTaskViewActionable(selectedTaskView) ||
      !canRequestReworkForProject(selectedTask, activeProjectIdRef.current) ||
      reworkingTaskId === selectedTask.id
    ) {
      return;
    }
    setReworkError(null);
    setReworkDialogOpen(true);
  }, [reworkingTaskId, selectedTask, selectedTaskView]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户取消返工 Dialog 时不应留下错误态。
   *
   * Code Logic（这个函数做什么）:
   *   busy 时忽略；否则关闭 dialog 并清空 reworkError。
   */
  const handleCloseReworkDialog = useCallback(() => {
    if (reworkingTaskId) return;
    setReworkDialogOpen(false);
    setReworkError(null);
  }, [reworkingTaskId]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   human review 任务可请求 rework，并把原因带回 runner；失败时保留 Dialog 与意见。
   *
   * Code Logic（这个函数做什么）:
   *   调用 requestReworkTaskView(reason)；成功关闭 dialog 并刷新 evidence；失败写 reworkError。
   */
  const handleSubmitRework = useCallback(
    async (reason: string) => {
      if (
        !selectedTask ||
        !isOrchestratorTaskViewActionable(selectedTaskView) ||
        !canRequestReworkForProject(selectedTask, activeProjectIdRef.current) ||
        reworkingTaskId === selectedTask.id
      ) {
        return;
      }
      const trimmed = reason.trim();
      if (trimmed.length < 1 || trimmed.length > 2000) {
        setReworkError(t('orchestrator:detail.reworkReasonRequired'));
        return;
      }
      const taskId = selectedTask.id;
      const projectId = selectedTask.projectId;
      setReworkingTaskId(taskId);
      setReworkError(null);
      setActionError(null);
      try {
        const updated = await orchestratorApi.requestReworkTaskView(projectId, taskId, trimmed);
        const updatedProjectId = updated.origin === 'pendingRemote' ? null : updated.task.projectId;
        if (
          !orchestratorCreateResultMatchesProject(activeProjectIdRef.current, projectId) ||
          updatedProjectId !== projectId
        ) {
          return;
        }
        replaceTaskViewInCurrentProject(projectId, updated);
        void refreshRuntimeSnapshot();
        requestAttentionInvalidation();
        setReworkDialogOpen(false);
        const items = await orchestratorApi.listEvidence(projectId, taskId);
        if (activeProjectIdRef.current === projectId) {
          setEvidenceResult({ projectId, taskId, items, error: null });
        }
      } catch (err) {
        if (orchestratorCreateResultMatchesProject(activeProjectIdRef.current, projectId)) {
          setReworkError(
            displayOrchestratorErrorMessage(err, t('orchestrator:errors.requestRework')),
          );
        }
      } finally {
        setReworkingTaskId((current) => (current === taskId ? null : current));
      }
    },
    [
      refreshRuntimeSnapshot,
      replaceTaskViewInCurrentProject,
      reworkingTaskId,
      selectedTask,
      selectedTaskView,
      t,
    ],
  );

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户确认后可交付任务，进入 merging/done 链路。A0 后无 digest 门禁。
   *
   * Code Logic（这个函数做什么）:
   *   调用 deliverReviewedTaskView，成功后 invalidation Attention 并刷新 evidence。
   */
  const handleDeliverReviewedTask = useCallback(async () => {
    if (
      !selectedTask ||
      !isOrchestratorTaskViewActionable(selectedTaskView) ||
      !canDeliverReviewedTaskForProject(selectedTask, activeProjectIdRef.current) ||
      deliveringTaskId === selectedTask.id
    ) {
      return;
    }
    const taskId = selectedTask.id;
    const projectId = selectedTask.projectId;
    setDeliveringTaskId(taskId);
    setActionError(null);
    try {
      const updated = await orchestratorApi.deliverReviewedTaskView(projectId, taskId);

      const updatedProjectId = updated.origin === 'pendingRemote' ? null : updated.task.projectId;
      if (
        !orchestratorCreateResultMatchesProject(activeProjectIdRef.current, projectId) ||
        updatedProjectId !== projectId
      ) {
        return;
      }
      replaceTaskViewInCurrentProject(projectId, updated);
      void refreshRuntimeSnapshot();
      requestAttentionInvalidation();
      const items = await orchestratorApi.listEvidence(projectId, taskId);
      if (activeProjectIdRef.current === projectId) {
        setEvidenceResult({ projectId, taskId, items, error: null });
      }
    } catch (err) {
      if (orchestratorCreateResultMatchesProject(activeProjectIdRef.current, projectId)) {
        const message = displayOrchestratorErrorMessage(err, t('orchestrator:errors.deliver'));
        setActionError({ projectId, message });
      }
    } finally {
      setDeliveringTaskId((current) => (current === taskId ? null : current));
    }
  }, [
    deliveringTaskId,
    refreshRuntimeSnapshot,
    replaceTaskViewInCurrentProject,
    selectedTask,
    selectedTaskView,
    t,
  ]);


  const handleOpenWorkbench = useCallback(() => {
    const url = buildWorkbenchTaskUrl(selectedTask);
    if (onOpenWorkbench) {
      onOpenWorkbench(url);
      return;
    }
    navigate(url);
  }, [navigate, onOpenWorkbench, selectedTask]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   Blocked 任务可在原 Automation UI 中重试。
   *
   * Code Logic（这个函数做什么）:
   *   调用 retryTaskView，成功后 invalidation Attention。
   */
  const handleRetryTask = useCallback(async () => {
    if (
      !selectedTask ||
      !isOrchestratorTaskViewActionable(selectedTaskView) ||
      !canControlBlockedTaskForProject(selectedTask, activeProjectIdRef.current) ||
      retryingTaskId === selectedTask.id
    ) {
      return;
    }
    const taskId = selectedTask.id;
    const projectId = selectedTask.projectId;
    setRetryingTaskId(taskId);
    setActionError(null);
    try {
      const updated = await orchestratorApi.retryTaskView(projectId, taskId);
      const updatedProjectId = updated.origin === 'pendingRemote' ? null : updated.task.projectId;
      if (
        !orchestratorCreateResultMatchesProject(activeProjectIdRef.current, projectId) ||
        updatedProjectId !== projectId
      ) {
        return;
      }
      replaceTaskViewInCurrentProject(projectId, updated);
      void refreshRuntimeSnapshot();
      requestAttentionInvalidation();
    } catch (err) {
      if (orchestratorCreateResultMatchesProject(activeProjectIdRef.current, projectId)) {
        setActionError({
          projectId,
          message: displayOrchestratorErrorMessage(err, t('orchestrator:errors.retry')),
        });
      }
    } finally {
      setRetryingTaskId((current) => (current === taskId ? null : current));
    }
  }, [
    refreshRuntimeSnapshot,
    replaceTaskViewInCurrentProject,
    retryingTaskId,
    selectedTask,
    selectedTaskView,
    t,
  ]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户可显式取消仍可取消的任务。
   *
   * Code Logic（这个函数做什么）:
   *   调用 cancelTaskView，成功后 replace 列表并刷新 snapshot。
   */
  const handleCancelTask = useCallback(async () => {
    if (
      !selectedTask ||
      !isOrchestratorTaskViewActionable(selectedTaskView) ||
      !canCancelOrchestratorTaskForProject(selectedTask, activeProjectIdRef.current) ||
      cancelingTaskId === selectedTask.id
    ) {
      return;
    }
    const taskId = selectedTask.id;
    const projectId = selectedTask.projectId;
    setCancelingTaskId(taskId);
    setActionError(null);
    try {
      const updated = await orchestratorApi.cancelTaskView(projectId, taskId);
      const updatedProjectId = updated.origin === 'pendingRemote' ? null : updated.task.projectId;
      if (
        !orchestratorCreateResultMatchesProject(activeProjectIdRef.current, projectId) ||
        updatedProjectId !== projectId
      ) {
        return;
      }
      replaceTaskViewInCurrentProject(projectId, updated);
      void refreshRuntimeSnapshot();
    } catch (err) {
      if (orchestratorCreateResultMatchesProject(activeProjectIdRef.current, projectId)) {
        setActionError({
          projectId,
          message: displayOrchestratorErrorMessage(err, t('orchestrator:errors.cancel')),
        });
      }
    } finally {
      setCancelingTaskId((current) => (current === taskId ? null : current));
    }
  }, [
    cancelingTaskId,
    refreshRuntimeSnapshot,
    replaceTaskViewInCurrentProject,
    selectedTask,
    selectedTaskView,
    t,
  ]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   队列刷新按钮需要触发项目级 refresh 并重拉 task views。
   *
   * Code Logic（这个函数做什么）:
   *   调用 refreshProject + listTaskViews，成功后 invalidation Attention 并刷新 snapshot。
   */
  const handleRefreshProject = useCallback(async () => {
    if (!activeProjectId || refreshingProjectId === activeProjectId) return;
    const projectId = activeProjectId;
    setRefreshingProjectId(projectId);
    setActionError(null);
    try {
      await orchestratorApi.refreshProject(projectId);
      const [nextViews, nextExperiments] = await Promise.all([
        orchestratorApi.listTaskViews(projectId),
        orchestratorApi.listExperiments(projectId),
      ]);
      if (activeProjectIdRef.current !== projectId) return;
      const nextSplit = splitOrchestratorTaskViews(nextViews);
      setTaskListResult({ projectId, views: nextViews, error: null });
      setExperiments(nextExperiments);
      setSelectedTaskId((current) => {
        if (current && nextSplit.tasks.some((item) => item.task.id === current)) return current;
        return null;
      });
      void refreshRuntimeSnapshot();
      requestAttentionInvalidation();
    } catch (err) {
      if (activeProjectIdRef.current === projectId) {
        setActionError({
          projectId,
          message: displayOrchestratorErrorMessage(err, t('orchestrator:errors.refresh')),
        });
      }
    } finally {
      setRefreshingProjectId((current) => (current === projectId ? null : current));
    }
  }, [activeProjectId, refreshRuntimeSnapshot, refreshingProjectId, t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   向导打开后必须拉取权威 WORKFLOW 文档状态，并初始化草稿/hash/诊断。
   *
   * Code Logic（这个函数做什么）:
   *   递增 requestSeq 调用 getWorkflowDocument；仅接受当前 project 的最新响应。
   */
  const loadWorkflowDocument = useCallback(async () => {
    if (!activeProjectId) return;
    const projectId = activeProjectId;
    const requestSeq = workflowRequestSeqRef.current + 1;
    workflowRequestSeqRef.current = requestSeq;
    setWorkflowLoadState('loading');
    setWorkflowLoadError(null);
    setWorkflowSaveError(null);
    setWorkflowConflict(false);
    try {
      const document = await orchestratorApi.getWorkflowDocument(projectId);
      if (activeProjectIdRef.current !== projectId || workflowRequestSeqRef.current !== requestSeq) {
        return;
      }
      setWorkflowDocumentStatus(document.status);
      setWorkflowDraft(document.content ?? document.preview ?? '');
      setWorkflowExpectedHash(document.contentHash ?? '');
      setWorkflowDiagnostics(document.diagnostics);
      setWorkflowPreview(document.preview ?? null);
      setWorkflowFocusedDiagnosticLine(document.diagnostics[0]?.line ?? null);
      setWorkflowLoadState('ready');
    } catch (err) {
      if (activeProjectIdRef.current !== projectId || workflowRequestSeqRef.current !== requestSeq) {
        return;
      }
      setWorkflowLoadState('error');
      setWorkflowLoadError(
        displayOrchestratorErrorMessage(err, t('orchestrator:errors.workflowDocument')),
      );
    }
  }, [activeProjectId, t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户从 runtime 状态条进入 WORKFLOW 向导时需要打开弹窗并检测文档。
   *
   * Code Logic（这个函数做什么）:
   *   打开 wizard 并触发 loadWorkflowDocument。
   */
  const handleOpenWorkflowWizard = useCallback(() => {
    if (!activeProjectId) return;
    setWorkflowWizardOpen(true);
    void loadWorkflowDocument();
  }, [activeProjectId, loadWorkflowDocument]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   busy 时不得关闭向导以免丢失 in-flight 保存上下文。
   *
   * Code Logic（这个函数做什么）:
   *   workflowBusy 时 early-return；否则关闭弹窗。
   */
  const handleCloseWorkflowWizard = useCallback(() => {
    if (workflowBusy) return;
    setWorkflowWizardOpen(false);
  }, [workflowBusy]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户编辑草稿时需同步 controller draft 并清除旧冲突标记外的保存错误。
   *
   * Code Logic（这个函数做什么）:
   *   写入 workflowDraft；不改 expectedHash，以保留 N3 CAS 基线。
   */
  const handleWorkflowDraftChange = useCallback((value: string) => {
    setWorkflowDraft(value);
    setWorkflowSaveError(null);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   missing 状态需要用内置默认模板填充草稿供用户创建。
   *
   * Code Logic（这个函数做什么）:
   *   优先使用 preview 模板正文写入 draft；expectedHash 保持空串以表达“创建缺失文件”。
   */
  const handleCreateWorkflowFromTemplate = useCallback(() => {
    if (workflowPreview) {
      setWorkflowDraft(workflowPreview);
    }
    setWorkflowExpectedHash('');
    setWorkflowConflict(false);
    setWorkflowSaveError(null);
    setWorkflowDiagnostics([]);
  }, [workflowPreview]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   保存前必须调用后端权威 validator，并展示 diagnostics/preview。
   *
   * Code Logic（这个函数做什么）:
   *   validateWorkflowDocument；成功/失败都更新 diagnostics 与 status，不改磁盘。
   */
  const handleValidateWorkflowDocument = useCallback(async () => {
    if (!activeProjectId || workflowBusy) return;
    const projectId = activeProjectId;
    setWorkflowBusy(true);
    setWorkflowSaveError(null);
    try {
      const document = await orchestratorApi.validateWorkflowDocument(projectId, workflowDraft);
      if (activeProjectIdRef.current !== projectId) return;
      setWorkflowDocumentStatus(document.status);
      setWorkflowDiagnostics(document.diagnostics);
      setWorkflowPreview(document.preview ?? null);
      setWorkflowFocusedDiagnosticLine(document.diagnostics[0]?.line ?? null);
    } catch (err) {
      if (activeProjectIdRef.current === projectId) {
        setWorkflowSaveError(
          displayOrchestratorErrorMessage(err, t('orchestrator:errors.workflowValidate')),
        );
      }
    } finally {
      if (activeProjectIdRef.current === projectId) {
        setWorkflowBusy(false);
      }
    }
  }, [activeProjectId, t, workflowBusy, workflowDraft]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   向导 CAS 保存 WORKFLOW.md；冲突时必须保留草稿并提供重新加载。
   *
   * Code Logic（这个函数做什么）:
   *   saveWorkflowDocument(expectedHash, draft)；workflow_document_changed 置 conflict 且不覆盖 draft；
   *   成功后刷新 snapshot，不自动 dispatch。
   */
  const handleSaveWorkflowDocument = useCallback(async () => {
    if (!activeProjectId || workflowBusy) return;
    const projectId = activeProjectId;
    const draftSnapshot = workflowDraft;
    const expectedHash = workflowExpectedHash;
    setWorkflowBusy(true);
    setWorkflowSaveError(null);
    setWorkflowConflict(false);
    try {
      const document = await orchestratorApi.saveWorkflowDocument(
        projectId,
        expectedHash,
        draftSnapshot,
      );
      if (activeProjectIdRef.current !== projectId) return;
      setWorkflowDocumentStatus(document.status);
      setWorkflowDraft(document.content ?? draftSnapshot);
      setWorkflowExpectedHash(document.contentHash ?? '');
      setWorkflowDiagnostics(document.diagnostics);
      setWorkflowPreview(document.preview ?? null);
      setWorkflowConflict(false);
      void refreshRuntimeSnapshot();
    } catch (err) {
      if (activeProjectIdRef.current !== projectId) return;
      const message = displayOrchestratorErrorMessage(
        err,
        t('orchestrator:errors.workflowSave'),
      );
      if (message.includes('workflow_document_changed')) {
        setWorkflowConflict(true);
        setWorkflowSaveError(message);
        // 故意保留 draftSnapshot，不回写磁盘内容。
        setWorkflowDraft(draftSnapshot);
      } else {
        setWorkflowSaveError(message);
      }
    } finally {
      if (activeProjectIdRef.current === projectId) {
        setWorkflowBusy(false);
      }
    }
  }, [
    activeProjectId,
    refreshRuntimeSnapshot,
    t,
    workflowBusy,
    workflowDraft,
    workflowExpectedHash,
  ]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   冲突后用户可选择重新加载磁盘内容，但必须主动触发，避免静默覆盖草稿。
   *
   * Code Logic（这个函数做什么）:
   *   调用 loadWorkflowDocument 重新 hydrate。
   */
  const handleReloadWorkflowDocument = useCallback(async () => {
    await loadWorkflowDocument();
  }, [loadWorkflowDocument]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   已有 WORKFLOW 文件应通过 typed deep link 打开文件工作区，而不是在向导内重写正文。
   *
   * Code Logic（这个函数做什么）:
   *   构造 view=files&path=WORKFLOW.md deep link，优先交给 onOpenWorkbench。
   */
  const handleOpenWorkflowFile = useCallback(() => {
    if (!activeProjectId) return;
    const url = buildWorkbenchDeepLink({
      projectId: activeProjectId,
      worktreeId: null,
      sessionId: null,
      view: 'files',
      path: 'WORKFLOW.md',
    });
    if (onOpenWorkbench) {
      onOpenWorkbench(url);
      return;
    }
    navigate(url);
  }, [activeProjectId, navigate, onOpenWorkbench]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   invalid 诊断点击后应聚焦到对应行，方便用户修正 YAML。
   *
   * Code Logic（这个函数做什么）:
   *   记录 focused line，并 best-effort 聚焦 textarea。
   */
  const handleFocusWorkflowDiagnostic = useCallback((diagnostic: WorkflowDiagnostic) => {
    setWorkflowFocusedDiagnosticLine(diagnostic.line);
    const el = workflowDraftTextareaRef.current;
    if (el) {
      el.focus();
    }
  }, []);

  return {
    embedded,
    activeProject,
    activeProjectId,
    loading,
    error,
    tasks,
    pendingRemoteItems,
    groups,
    selectedTaskId,
    setSelectedTaskId,
    focusedOutboxId,
    selectedRenderableTask,
    selectedTask,
    selectedTaskCanStart,
    selectedTaskCanComplete,
    selectedTaskCanRequestRework,
    selectedTaskShowDeliver,
    selectedTaskCanDeliver,
    selectedTaskCanCancel,
    selectedTaskCanControlBlocked,
    selectedTaskCanOpenWorkbench,
    selectedTaskProgressMessage,
    selectedTaskTerminalLabel,
    creatingAction,
    startingTaskId,
    completingTaskId,
    reworkingTaskId,
    deliveringTaskId,
    retryingTaskId,
    cancelingTaskId,
    refreshingProjectId,
    movingTaskId,
    outboxActionId,
    form,
    createDialogOpen,
    completionPrompt,
    setCompletionPrompt,
    completingPrompt,
    completionPromptRef,
    canCreate,
    canCompletePrompt,
    evidenceItems,
    evidenceLoading,
    evidenceError,
    latestVerifierEvidence,
    latestRepairPromptEvidence,
    developmentAttemptEvidenceItems,
    runtimeSnapshot,
    runtimeRemoteStatus,
    runtimeCachedAt,
    runtimeSnapshotLoading,
    runtimeSnapshotErrorMessage,
    showRuntimeSnapshotContent,
    handleOpenCreateDialog,
    handleCloseCreateDialog,
    handleCloseTaskDrawer,
    handleRefreshRuntimeSnapshot,
    handleOpenAutomationSettings,
    handleRetryRemoteOutbox,
    handleDiscardRemoteOutbox,
    updateFormField,
    handleCompleteTaskPrompt,
    handleCreateFormSubmit,
    handleCreateTaskAction,
    handleTaskDragStart,
    handleTaskDragEnd,
    handleLaneDragOver,
    handleLaneDrop,
    detailTab,
    setDetailTab,
    reworkDialogOpen,
    reworkError,
    handleOpenReworkDialog,
    handleCloseReworkDialog,
    handleSubmitRework,
    handleStartSelectedTask,
    handleCompleteAgentRun,
    handleDeliverReviewedTask,
    handleOpenWorkbench,
    handleRetryTask,
    handleCancelTask,
    handleRefreshProject,
    workflowWizardOpen,
    workflowLoadState,
    workflowDocumentStatus,
    workflowDraft,
    workflowExpectedHash,
    workflowDiagnostics,
    workflowPreview,
    workflowLoadError,
    workflowSaveError,
    workflowConflict,
    workflowBusy,
    workflowFocusedDiagnosticLine,
    workflowDraftTextareaRef,
    handleOpenWorkflowWizard,
    handleCloseWorkflowWizard,
    handleWorkflowDraftChange,
    handleCreateWorkflowFromTemplate,
    handleValidateWorkflowDocument,
    handleSaveWorkflowDocument,
    handleReloadWorkflowDocument,
    handleOpenWorkflowFile,
    handleFocusWorkflowDiagnostic,
    experiments,
    experimentsLoading,
    creatingExperiment,
    experimentActionId,
    handleCreateExperiment,
    handleApproveExperimentWinner,
    handleCancelExperiment,
    agentAdapters,
    canCreateExperiment,
  };
}
