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
  OrchestratorReviewDiff,
  OrchestratorReviewDiffLoadState,
  OrchestratorTask,
  OrchestratorTaskView,
  OrchestratorWorkflowState,
  WorkbenchProject,
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
  reviewDiffState: OrchestratorReviewDiffLoadState;
  reviewDiff: OrchestratorReviewDiff | null;
  reviewDiffError: string | null;
  selectedReviewFilePath: string | null;
  setSelectedReviewFilePath: (path: string | null) => void;
  handleRetryReviewDiff: () => void;
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
  const [reviewDiffState, setReviewDiffState] =
    useState<OrchestratorReviewDiffLoadState>('idle');
  const [reviewDiff, setReviewDiff] = useState<OrchestratorReviewDiff | null>(null);
  const [reviewDiffError, setReviewDiffError] = useState<string | null>(null);
  const [selectedReviewFilePath, setSelectedReviewFilePath] = useState<string | null>(null);
  const reviewRequestSeqRef = useRef(0);
  const [reworkDialogOpen, setReworkDialogOpen] = useState(false);
  const [reworkError, setReworkError] = useState<string | null>(null);
  const activeProjectId = activeProject?.id ?? null;
  const activeProjectIdRef = useRef<string | null>(activeProjectId);
  const taskLoadDecision = useMemo(
    () => resolveOrchestratorTaskLoad(projectsLoading, activeProjectId),
    [activeProjectId, projectsLoading],
  );
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
  const selectedTaskCanDeliver =
    selectedTaskShowDeliver &&
    (reviewDiffState === 'unsupported' ||
      (reviewDiffState === 'ready' && Boolean(reviewDiff?.reviewDigest)));
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
    if (creatingAction || completingPrompt) return;
    setCreateDialogOpen(false);
  }, [completingPrompt, creatingAction]);

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
   * Business Logic（为什么需要这个 effect）:
   *   Human Review / Rework 任务的 Changes 与 Deliver 依赖有界 review diff；
   *   project/task/attempt 变化必须 abort 旧请求，旧响应不得回填。
   *
   * Code Logic（这个 effect 做什么）:
   *   非复核态清空 review state；复核态递增 requestSeq 拉 getReviewDiff；
   *   仅当 projectId/taskId/attempt/requestSeq 仍匹配时写入 ready/error/unsupported。
   */
  useEffect(() => {
    if (taskLoadDecision.kind !== 'load' || !selectedTask) {
      setReviewDiffState('idle');
      setReviewDiff(null);
      setReviewDiffError(null);
      setSelectedReviewFilePath(null);
      return undefined;
    }
    if (selectedTask.projectId !== taskLoadDecision.projectId) {
      return undefined;
    }
    const needsReview =
      selectedTask.workflowState === 'humanReview' || selectedTask.workflowState === 'rework';
    if (!needsReview) {
      setReviewDiffState('idle');
      setReviewDiff(null);
      setReviewDiffError(null);
      setSelectedReviewFilePath(null);
      return undefined;
    }

    const projectId = taskLoadDecision.projectId;
    const taskId = selectedTask.id;
    const nextSeq = reviewRequestSeqRef.current + 1;
    reviewRequestSeqRef.current = nextSeq;
        setReviewDiffState('loading');
    setReviewDiff(null);
    setReviewDiffError(null);
    setSelectedReviewFilePath(null);

    let cancelled = false;
    void orchestratorApi
      .getReviewDiff(projectId, taskId)
      .then((diff) => {
        if (
          cancelled ||
          activeProjectIdRef.current !== projectId ||
          reviewRequestSeqRef.current !== nextSeq
        ) {
          return;
        }
        setReviewDiff(diff);
        setReviewDiffState('ready');
        setReviewDiffError(null);
        setSelectedReviewFilePath(diff.files[0]?.path ?? null);
      })
      .catch((err: unknown) => {
        if (
          cancelled ||
          activeProjectIdRef.current !== projectId ||
          reviewRequestSeqRef.current !== nextSeq
        ) {
          return;
        }
        const message = displayOrchestratorErrorMessage(err, t('orchestrator:errors.reviewDiff'));
        // 不记录 full patch；仅展示 message/capability 诊断。
        if (
          message.includes('orchestrator.review-diff.v1') ||
          message.includes('不支持能力')
        ) {
          setReviewDiffState('unsupported');
          setReviewDiff(null);
          setReviewDiffError(null);
          return;
        }
        setReviewDiffState('error');
        setReviewDiff(null);
        setReviewDiffError(message);
      });

    return () => {
      cancelled = true;
    };
  }, [selectedTask, taskLoadDecision, t]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   用户可在 Changes tab 手动重试加载 review diff。
   *
   * Code Logic（这个函数做什么）:
   *   递增 requestSeq 并重拉 getReviewDiff，仅接受当前 key 响应。
   */
  const handleRetryReviewDiff = useCallback(() => {
    if (!selectedTask || taskLoadDecision.kind !== 'load') return;
    if (selectedTask.projectId !== taskLoadDecision.projectId) return;
    const projectId = taskLoadDecision.projectId;
    const taskId = selectedTask.id;
    const nextSeq = reviewRequestSeqRef.current + 1;
    reviewRequestSeqRef.current = nextSeq;
        setReviewDiffState('loading');
    setReviewDiff(null);
    setReviewDiffError(null);
    void orchestratorApi
      .getReviewDiff(projectId, taskId)
      .then((diff) => {
        if (
          activeProjectIdRef.current !== projectId ||
          reviewRequestSeqRef.current !== nextSeq
        ) {
          return;
        }
        setReviewDiff(diff);
        setReviewDiffState('ready');
        setReviewDiffError(null);
        setSelectedReviewFilePath(diff.files[0]?.path ?? null);
      })
      .catch((err: unknown) => {
        if (
          activeProjectIdRef.current !== projectId ||
          reviewRequestSeqRef.current !== nextSeq
        ) {
          return;
        }
        const message = displayOrchestratorErrorMessage(err, t('orchestrator:errors.reviewDiff'));
        if (
          message.includes('orchestrator.review-diff.v1') ||
          message.includes('不支持能力')
        ) {
          setReviewDiffState('unsupported');
          setReviewDiff(null);
          setReviewDiffError(null);
          return;
        }
        setReviewDiffState('error');
        setReviewDiff(null);
        setReviewDiffError(message);
      });
  }, [selectedTask, t, taskLoadDecision]);

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
   *   用户审阅通过后可交付任务，进入 merging/done 链路。
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
    // diff-capable: require ready digest; unsupported keeps legacy deliver without digest.
    if (reviewDiffState !== 'unsupported') {
      if (reviewDiffState !== 'ready' || !reviewDiff?.reviewDigest) {
        return;
      }
    }
    const taskId = selectedTask.id;
    const projectId = selectedTask.projectId;
    const expectedDigest =
      reviewDiffState === 'ready' ? reviewDiff?.reviewDigest ?? null : null;
    setDeliveringTaskId(taskId);
    setActionError(null);
    try {
      const updated = await orchestratorApi.deliverReviewedTaskView(
        projectId,
        taskId,
        expectedDigest,
      );

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
        if (message.includes('review_diff_changed')) {
          setReviewDiffState('idle');
          setReviewDiff(null);
          setReviewDiffError(t('orchestrator:review.diffChangedConflict'));
          setSelectedReviewFilePath(null);
          setActionError({
            projectId,
            message: t('orchestrator:review.diffChangedConflict'),
          });
          // force re-review: bump seq so effect/retry can reload
          const nextSeq = reviewRequestSeqRef.current + 1;
          reviewRequestSeqRef.current = nextSeq;
                    setReviewDiffState('loading');
          void orchestratorApi
            .getReviewDiff(projectId, taskId)
            .then((diff) => {
              if (
                activeProjectIdRef.current !== projectId ||
                reviewRequestSeqRef.current !== nextSeq
              ) {
                return;
              }
              setReviewDiff(diff);
              setReviewDiffState('ready');
              setReviewDiffError(null);
              setSelectedReviewFilePath(diff.files[0]?.path ?? null);
            })
            .catch((reloadErr: unknown) => {
              if (
                activeProjectIdRef.current !== projectId ||
                reviewRequestSeqRef.current !== nextSeq
              ) {
                return;
              }
              setReviewDiffState('error');
              setReviewDiff(null);
              setReviewDiffError(
                displayOrchestratorErrorMessage(
                  reloadErr,
                  t('orchestrator:errors.reviewDiff'),
                ),
              );
            });
        } else {
          setActionError({ projectId, message });
        }
      }
    } finally {
      setDeliveringTaskId((current) => (current === taskId ? null : current));
    }
    }, [
    deliveringTaskId,
    refreshRuntimeSnapshot,
    replaceTaskViewInCurrentProject,
    reviewDiff,
    reviewDiffState,
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
      const nextViews = await orchestratorApi.listTaskViews(projectId);
      if (activeProjectIdRef.current !== projectId) return;
      const nextSplit = splitOrchestratorTaskViews(nextViews);
      setTaskListResult({ projectId, views: nextViews, error: null });
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
    reviewDiffState,
    reviewDiff,
    reviewDiffError,
    selectedReviewFilePath,
    setSelectedReviewFilePath,
    handleRetryReviewDiff,
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
  };
}
