import { useCallback, useEffect, useId, useMemo, useRef, useState } from 'react';
import type { ReactElement } from 'react';
import { useTranslation } from 'react-i18next';
import { httpOrchestratorTransport } from '@/api/workbenchHttp';
import type { HttpCreateOrchestratorTaskAction } from '@/api/workbenchHttp';
import { orchestratorEvidenceKindTone } from '@/lib/orchestrator';
import {
  splitOrchestratorTaskViews,
  upsertOrchestratorTaskView,
} from '@/lib/orchestratorRemote';
import type { OrchestratorRenderableTask } from '@/lib/orchestratorRemote';
import type {
  OrchestratorAttemptPhase,
  OrchestratorEvidence,
  OrchestratorRemoteOutboxItem,
  OrchestratorRemoteOutboxStatus,
  OrchestratorRunState,
  OrchestratorTask,
  OrchestratorTaskView,
  OrchestratorWorkflowState,
  WorkbenchProject,
} from '@/lib/types';
import styles from '../MobileWorkbench.module.css';

export interface MobileAutomationExecutionContext {
  projectId: string;
  worktreeId: string | null;
  sessionId: string | null;
}

export interface MobileAutomationPanelProps {
  project: WorkbenchProject | null;
  onOpenExecutionContext?: (context: MobileAutomationExecutionContext) => void;
}

type MobileAutomationTaskGroups = Record<
  OrchestratorWorkflowState,
  OrchestratorRenderableTask[]
>;

interface MobileAutomationCreateActionConfig {
  createAction: HttpCreateOrchestratorTaskAction;
  labelKey: MobileAutomationCreateActionLabelKey;
  statusKey: MobileAutomationCreateActionStatusKey;
}

type MobileAutomationCreateActionLabelKey =
  | 'workbench:mobile.automationPanel.createBacklog'
  | 'workbench:mobile.automationPanel.createTodo'
  | 'workbench:mobile.automationPanel.createStart';

type MobileAutomationCreateActionStatusKey =
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

const MOBILE_AUTOMATION_WORKFLOW_STATES: readonly OrchestratorWorkflowState[] = [
  'backlog',
  'todo',
  'inProgress',
  'humanReview',
  'rework',
  'merging',
  'done',
  'canceled',
];

const MOBILE_AUTOMATION_CREATE_ACTIONS: readonly MobileAutomationCreateActionConfig[] = [
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

const MOBILE_AUTOMATION_WORKFLOW_LABEL_KEYS: Record<
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

const MOBILE_AUTOMATION_RUN_LABEL_KEYS: Record<
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

const MOBILE_AUTOMATION_ATTEMPT_PHASE_LABEL_KEYS: Record<
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

const MOBILE_AUTOMATION_PENDING_STATUS_LABEL_KEYS: Record<
  OrchestratorRemoteOutboxStatus,
  `workbench:mobile.automationPanel.pendingStatus.${OrchestratorRemoteOutboxStatus}`
> = {
  pending: 'workbench:mobile.automationPanel.pendingStatus.pending',
  sending: 'workbench:mobile.automationPanel.pendingStatus.sending',
  mirrored: 'workbench:mobile.automationPanel.pendingStatus.mirrored',
  failed: 'workbench:mobile.automationPanel.pendingStatus.failed',
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
function getErrorMessage(reason: unknown): string {
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
function pendingRemoteTaskTitle(item: OrchestratorRemoteOutboxItem): string {
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
function createEmptyMobileAutomationGroups(): MobileAutomationTaskGroups {
  return MOBILE_AUTOMATION_WORKFLOW_STATES.reduce<MobileAutomationTaskGroups>((groups, state) => {
    groups[state] = [];
    return groups;
  }, {} as MobileAutomationTaskGroups);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   手机端自动化列表必须是桌面 workflow board 的 compact grouped-list，而不是 legacy status 平铺。
 *
 * Code Logic（这个函数做什么）:
 *   按 task.task.workflowState 把 local/remote 真实任务加入对应分组；pendingRemote 不会传入本函数。
 */
function groupMobileAutomationTasks(
  tasks: OrchestratorRenderableTask[],
): MobileAutomationTaskGroups {
  const groupedTasks = createEmptyMobileAutomationGroups();
  for (const task of tasks) {
    groupedTasks[task.task.workflowState].push(task);
  }
  return groupedTasks;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   任务列表刷新后应保留用户正在查看的详情，但如果任务已不在列表中则关闭详情避免展示过期数据。
 *
 * Code Logic（这个函数做什么）:
 *   local/remote 按 task.id 匹配最新 view；pendingRemote 和空选择都返回 null。
 */
function resolveSelectedTaskViewAfterRefresh(
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
function runtimeValue(value: string | null | undefined, fallback: string): string {
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
function formatAutomationTimestamp(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString();
}

/**
 * Business Logic（为什么需要这个函数）:
 *   详情面板只支持真实 local/remote 任务，pendingRemote outbox 不应触发 evidence 或 action。
 *
 * Code Logic（这个函数做什么）:
 *   从 task view union 中安全提取 task；pendingRemote 或空值返回 null。
 */
function getTaskFromView(view: OrchestratorTaskView | null): OrchestratorTask | null {
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
function mobileAutomationEvidenceKindLabelKey(
  kind: string,
): MobileAutomationEvidenceKindLabelKey {
  return (
    MOBILE_AUTOMATION_EVIDENCE_KIND_LABEL_KEYS[
      kind as keyof typeof MOBILE_AUTOMATION_EVIDENCE_KIND_LABEL_KEYS
    ] ?? 'orchestrator:evidence.kind.generic'
  );
}

/**
 * MobileAutomationPanel（移动端项目级自动化面板）
 *
 * Business Logic（为什么需要这个组件）:
 *   手机端 Workbench 需要在本机或远端项目中查看项目级 Orchestrator 任务、创建任务，并进入任务执行现场。
 *
 * Code Logic（这个组件做什么）:
 *   使用 task-view HTTP route 读取 local/remote/pendingRemote tagged union；真实任务按 workflowState 分组，
 *   pendingRemote 单独展示；点击真实任务后读取 evidence 并渲染详情，创建弹窗提供 Backlog/Todo/Start 三种动作。
 */
export function MobileAutomationPanel({
  project,
  onOpenExecutionContext,
}: MobileAutomationPanelProps): ReactElement {
  const { t } = useTranslation(['workbench', 'orchestrator']);
  const [taskViews, setTaskViews] = useState<OrchestratorTaskView[]>([]);
  const [selectedTaskView, setSelectedTaskView] = useState<OrchestratorTaskView | null>(null);
  const [evidenceItems, setEvidenceItems] = useState<OrchestratorEvidence[]>([]);
  const [evidenceLoading, setEvidenceLoading] = useState<boolean>(false);
  const [evidenceError, setEvidenceError] = useState<string | null>(null);
  const [title, setTitle] = useState<string>('');
  const [goal, setGoal] = useState<string>('');
  const [acceptanceCriteria, setAcceptanceCriteria] = useState<string>('');
  const [createDialogOpen, setCreateDialogOpen] = useState<boolean>(false);
  const [promptDraft, setPromptDraft] = useState<string>('');
  const [loading, setLoading] = useState<boolean>(false);
  const [creatingAction, setCreatingAction] =
    useState<HttpCreateOrchestratorTaskAction | null>(null);
  const [completingPrompt, setCompletingPrompt] = useState<boolean>(false);
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);
  const requestIdRef = useRef<number>(0);
  const evidenceRequestIdRef = useRef<number>(0);
  const activeProjectIdRef = useRef<string | null>(null);
  const promptDraftRef = useRef<HTMLTextAreaElement | null>(null);
  const titleId = useId();
  const dialogTitleId = useId();
  const detailTitleId = useId();
  const hasProject = Boolean(project);
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
  const visibleWorkflowStates = useMemo(
    () => MOBILE_AUTOMATION_WORKFLOW_STATES.filter((state) => groupedTasks[state].length > 0),
    [groupedTasks],
  );
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
  const canOpenExecutionContext = Boolean(
    selectedTask &&
      onOpenExecutionContext &&
      (selectedTask.worktreeId || selectedTask.sessionId),
  );

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
      const nextTaskViews = await httpOrchestratorTransport.tasks.listViews(projectId);
      if (requestIdRef.current !== requestId) return;
      if (activeProjectIdRef.current !== projectId) return;
      setTaskViews(nextTaskViews);
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

  /* eslint-disable react-hooks/set-state-in-effect -- 项目切换时必须同步自动化任务上下文 */
  useEffect(() => {
    const projectId = project?.id ?? null;
    activeProjectIdRef.current = projectId;
    requestIdRef.current += 1;
    evidenceRequestIdRef.current += 1;
    setTaskViews([]);
    setSelectedTaskView(null);
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

    if (projectId) {
      void loadTasks(projectId);
    }
  }, [loadTasks, project?.id]);

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
   *   用户点击刷新时需要重新读取当前项目的任务列表，未选项目不应发起请求。
   *
   * Code Logic（这个函数做什么）:
   *   校验当前 project id，存在时调用 loadTasks；错误由 loadTasks 写入面板状态。
   */
  const handleRefresh = useCallback((): void => {
    const projectId = activeProjectIdRef.current;
    if (!projectId) return;
    void loadTasks(projectId);
  }, [loadTasks]);

  /**
   * Business Logic（为什么需要这个函数）:
   *   手机端创建任务入口需要从列表页进入独立弹窗，避免表单常驻挤占任务队列空间。
   *
   * Code Logic（这个函数做什么）:
   *   打开创建弹窗并清理上一轮状态提示；表单草稿保留，让用户误关前可继续编辑。
   */
  const handleOpenCreateDialog = useCallback((): void => {
    if (!activeProjectIdRef.current) return;
    setError(null);
    setStatus(null);
    setCreateDialogOpen(true);
  }, []);

  /**
   * Business Logic（为什么需要这个函数）:
   *   弹窗关闭应避免打断正在创建或 AI 完善的请求，防止用户误以为操作已取消。
   *
   * Code Logic（这个函数做什么）:
   *   若没有 pending 请求则关闭 dialog；请求中忽略关闭动作。
   */
  const handleCloseCreateDialog = useCallback((): void => {
    if (creating || completingPrompt) return;
    setCreateDialogOpen(false);
  }, [creating, completingPrompt]);

  useEffect(() => {
    if (!createDialogOpen) return undefined;
    const focusTimer = window.setTimeout(() => {
      promptDraftRef.current?.focus();
    }, 0);
    return () => {
      window.clearTimeout(focusTimer);
    };
  }, [createDialogOpen]);

  useEffect(() => {
    if (!createDialogOpen) return undefined;
    const handleKeyDown = (event: KeyboardEvent): void => {
      if (event.key === 'Escape') {
        handleCloseCreateDialog();
      }
    };
    window.addEventListener('keydown', handleKeyDown);
    return () => {
      window.removeEventListener('keydown', handleKeyDown);
    };
  }, [createDialogOpen, handleCloseCreateDialog]);

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
   *
   * Code Logic（这个函数做什么）:
   *   校验项目和表单后调用 HTTP createView，并显式传 createAction；成功后合并 task view 并清空弹窗。
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

  return (
    <section className={styles.panel} aria-labelledby={titleId}>
      <div className={styles.panelHeaderRow}>
        <div className={styles.panelHeader}>
          <p className={styles.panelKicker}>{t('workbench:mobile.kicker')}</p>
          <h1 id={titleId}>{t('workbench:mobile.automationPanel.title')}</h1>
        </div>
        <div className={styles.panelHeaderActions}>
          <button
            type="button"
            className={styles.secondaryButton}
            disabled={!hasProject || loading}
            onClick={handleRefresh}
          >
            {t('workbench:refresh')}
          </button>
          <button
            type="button"
            className={styles.mobileTerminalPrimaryButton}
            disabled={!hasProject || loading}
            onClick={handleOpenCreateDialog}
          >
            {t('workbench:mobile.automationPanel.createOpen')}
          </button>
        </div>
      </div>

      {!project ? (
        <p className={styles.panelState}>{t('workbench:mobile.automationPanel.noProject')}</p>
      ) : null}
      {error ? (
        <p className={styles.panelError}>
          <span>{t('workbench:mobile.projectPanel.error')}</span>
          <span>{error}</span>
        </p>
      ) : null}
      {status ? <p className={styles.panelState}>{status}</p> : null}

      {hasProject ? (
        <>
          {loading ? <p className={styles.panelState}>{t('workbench:loading')}</p> : null}
          {isListEmpty ? (
            <p className={styles.panelState}>{t('workbench:mobile.automationPanel.empty')}</p>
          ) : null}

          <div
            className={styles.mobileList}
            aria-label={t('workbench:mobile.automationPanel.listAriaLabel')}
          >
            {visibleWorkflowStates.map((workflowState) => (
              <section className={styles.mobileAutomationGroup} key={workflowState}>
                <div className={styles.mobileAutomationGroupHeader}>
                  <span>{t(MOBILE_AUTOMATION_WORKFLOW_LABEL_KEYS[workflowState])}</span>
                  <span className={styles.mobileBadge}>
                    {t('workbench:mobile.automationPanel.taskCount', {
                      count: groupedTasks[workflowState].length,
                    })}
                  </span>
                </div>
                <div className={styles.mobileList}>
                  {groupedTasks[workflowState].map((task) => {
                    const view = task.view;
                    const taskDto = task.task;
                    const selected = selectedTask?.id === taskDto.id;
                    const originLabel =
                      task.origin === 'remote'
                        ? t('workbench:mobile.automationPanel.origin.remote', {
                            deviceName:
                              task.deviceName ??
                              t('workbench:mobile.automationPanel.origin.unknownDevice'),
                          })
                        : t('workbench:mobile.automationPanel.origin.local');
                    return (
                      <button
                        type="button"
                        key={taskDto.id}
                        className={`${styles.mobileListItem} ${
                          selected ? styles.mobileListItemActive : ''
                        }`}
                        aria-pressed={selected}
                        onClick={() => {
                          setSelectedTaskView(view);
                        }}
                      >
                        <div className={styles.mobileListTitleRow}>
                          <strong className={styles.mobileListTitle}>{taskDto.title}</strong>
                          <span className={`${styles.mobileBadge} ${styles.mobileBadgeAccent}`}>
                            {originLabel}
                          </span>
                        </div>
                        <div className={styles.automationTaskBody}>
                          <p>{taskDto.goal}</p>
                          <p>
                            {t('workbench:mobile.automationPanel.runtimeMessage', {
                              value: runtimeValue(taskDto.lastRuntimeMessage, unknownLabel),
                            })}
                          </p>
                          <p>
                            {t('workbench:mobile.automationPanel.runtimeRefs', {
                              claudeSessionId: runtimeValue(taskDto.claudeSessionId, unknownLabel),
                              transcriptPath: runtimeValue(taskDto.transcriptPath, unknownLabel),
                            })}
                          </p>
                        </div>
                        <div className={styles.mobileBadgeRow}>
                          <span className={styles.mobileBadge}>
                            {t(MOBILE_AUTOMATION_WORKFLOW_LABEL_KEYS[taskDto.workflowState])}
                          </span>
                          <span className={styles.mobileBadge}>
                            {t(MOBILE_AUTOMATION_RUN_LABEL_KEYS[taskDto.runState])}
                          </span>
                          <span className={styles.mobileBadge}>
                            {taskDto.attemptPhase
                              ? t(MOBILE_AUTOMATION_ATTEMPT_PHASE_LABEL_KEYS[taskDto.attemptPhase])
                              : unknownLabel}
                          </span>
                          <span className={styles.mobileListMeta}>
                            {t('workbench:mobile.automationPanel.attempt', {
                              attempt: taskDto.attempt,
                            })}
                          </span>
                        </div>
                      </button>
                    );
                  })}
                </div>
              </section>
            ))}

            {pendingRemoteItems.length > 0 ? (
              <section className={styles.mobileAutomationGroup}>
                <div className={styles.mobileAutomationGroupHeader}>
                  <span>{t('workbench:mobile.automationPanel.pendingTitle')}</span>
                  <span className={styles.mobileBadge}>
                    {t('workbench:mobile.automationPanel.taskCount', {
                      count: pendingRemoteItems.length,
                    })}
                  </span>
                </div>
                <div className={styles.mobileList}>
                  {pendingRemoteItems.map((item) => (
                    <article key={item.id} className={styles.mobileListItem}>
                      <div className={styles.mobileListTitleRow}>
                        <strong className={styles.mobileListTitle}>
                          {pendingRemoteTaskTitle(item)}
                        </strong>
                        <span className={`${styles.mobileBadge} ${styles.mobileBadgeAccent}`}>
                          {t(MOBILE_AUTOMATION_PENDING_STATUS_LABEL_KEYS[item.status])}
                        </span>
                      </div>
                      <div className={styles.automationTaskBody}>
                        <p>
                          {t('workbench:mobile.automationPanel.pendingDevice', {
                            deviceName: item.deviceName,
                          })}
                        </p>
                        <p>
                          {item.lastError
                            ? t('workbench:mobile.automationPanel.pendingError', {
                                error: item.lastError,
                              })
                            : t('workbench:mobile.automationPanel.remoteProjectPath', {
                                path: item.remoteProjectPath,
                              })}
                        </p>
                      </div>
                      <div className={styles.mobileBadgeRow}>
                        <span className={styles.mobileBadge}>
                          {t('workbench:mobile.automationPanel.origin.pending')}
                        </span>
                      </div>
                    </article>
                  ))}
                </div>
              </section>
            ) : null}
          </div>

          {selectedTask ? (
            <aside className={styles.mobileAutomationDetail} aria-labelledby={detailTitleId}>
              <div className={styles.mobileListTitleRow}>
                <div className={styles.panelHeader}>
                  <p className={styles.panelKicker}>
                    {t('workbench:mobile.automationPanel.detailKicker')}
                  </p>
                  <h2 id={detailTitleId}>{selectedTask.title}</h2>
                </div>
                <button
                  type="button"
                  className={styles.secondaryButton}
                  onClick={() => {
                    setSelectedTaskView(null);
                  }}
                >
                  {t('workbench:mobile.automationPanel.closeDetails')}
                </button>
              </div>

              <div className={styles.mobileAutomationDetailBlock}>
                <span>{t('workbench:mobile.automationPanel.fields.goal')}</span>
                <p>{selectedTask.goal}</p>
              </div>
              <div className={styles.mobileAutomationDetailBlock}>
                <span>{t('workbench:mobile.automationPanel.fields.acceptanceCriteria')}</span>
                <p>{selectedTask.acceptanceCriteria}</p>
              </div>

              <dl className={styles.mobileAutomationDetailGrid}>
                <div>
                  <dt>{t('workbench:mobile.automationPanel.workflowState')}</dt>
                  <dd>{t(MOBILE_AUTOMATION_WORKFLOW_LABEL_KEYS[selectedTask.workflowState])}</dd>
                </div>
                <div>
                  <dt>{t('workbench:mobile.automationPanel.runStateLabel')}</dt>
                  <dd>{t(MOBILE_AUTOMATION_RUN_LABEL_KEYS[selectedTask.runState])}</dd>
                </div>
                <div>
                  <dt>{t('workbench:mobile.automationPanel.attemptPhaseLabel')}</dt>
                  <dd>
                    {selectedTask.attemptPhase
                      ? t(MOBILE_AUTOMATION_ATTEMPT_PHASE_LABEL_KEYS[selectedTask.attemptPhase])
                      : unknownLabel}
                  </dd>
                </div>
                <div>
                  <dt>{t('workbench:mobile.automationPanel.runtimeMessageLabel')}</dt>
                  <dd>{runtimeValue(selectedTask.lastRuntimeMessage, unknownLabel)}</dd>
                </div>
                <div>
                  <dt>{t('workbench:mobile.automationPanel.claudeSession')}</dt>
                  <dd>{runtimeValue(selectedTask.claudeSessionId, unknownLabel)}</dd>
                </div>
                <div>
                  <dt>{t('workbench:mobile.automationPanel.transcript')}</dt>
                  <dd>{runtimeValue(selectedTask.transcriptPath, unknownLabel)}</dd>
                </div>
              </dl>

              <div className={styles.mobileAutomationDetailBlock}>
                <span>{t('workbench:mobile.automationPanel.blockedReason')}</span>
                <p>{runtimeValue(selectedTask.blockedReason, unknownLabel)}</p>
              </div>

              <div className={styles.mobileBadgeRow}>
                <button
                  type="button"
                  className={styles.mobileTerminalPrimaryButton}
                  disabled={!canOpenExecutionContext}
                  onClick={handleOpenExecutionContext}
                >
                  {canOpenExecutionContext
                    ? t('workbench:mobile.automationPanel.openExecutionContext')
                    : t('workbench:mobile.automationPanel.executionContextUnavailable')}
                </button>
              </div>

              <section className={styles.mobileAutomationDetailBlock}>
                <div className={styles.mobileListTitleRow}>
                  <span>{t('workbench:mobile.automationPanel.evidenceTitle')}</span>
                  <span className={styles.mobileBadge}>
                    {t('workbench:mobile.automationPanel.evidenceTimeline', {
                      count: evidenceItems.length,
                    })}
                  </span>
                </div>
                {evidenceLoading ? (
                  <p>{t('workbench:mobile.automationPanel.evidenceLoading')}</p>
                ) : null}
                {evidenceError ? <p>{evidenceError}</p> : null}
                {!evidenceLoading && !evidenceError && evidenceItems.length === 0 ? (
                  <p>{t('workbench:mobile.automationPanel.evidenceEmpty')}</p>
                ) : null}
                {evidenceItems.length > 0 ? (
                  <ul className={styles.mobileAutomationEvidenceList}>
                    {evidenceItems.map((item) => (
                      <li className={styles.mobileAutomationEvidenceItem} key={item.id}>
                        <div className={styles.mobileListTitleRow}>
                          <strong className={styles.mobileListTitle}>{item.title}</strong>
                          <span className={styles.mobileBadge}>
                            {t(mobileAutomationEvidenceKindLabelKey(item.kind))}
                          </span>
                        </div>
                        <div className={styles.mobileBadgeRow}>
                          <span className={styles.mobileBadge}>
                            {formatAutomationTimestamp(item.createdAt)}
                          </span>
                          <span
                            className={`${styles.mobileBadge} ${
                              orchestratorEvidenceKindTone(item.kind) === 'success'
                                ? styles.mobileBadgeAccent
                                : ''
                            }`}
                          >
                            {item.summary || unknownLabel}
                          </span>
                        </div>
                        <pre>{item.content}</pre>
                      </li>
                    ))}
                  </ul>
                ) : null}
              </section>
            </aside>
          ) : null}

          {createDialogOpen ? (
            <div
              className={styles.mobileDialogOverlay}
              onMouseDown={(event) => {
                if (event.target === event.currentTarget) {
                  handleCloseCreateDialog();
                }
              }}
            >
              <div
                className={styles.mobileDialog}
                role="dialog"
                aria-modal="true"
                aria-labelledby={dialogTitleId}
              >
                <div className={styles.mobileDialogHeader}>
                  <div className={styles.panelHeader}>
                    <p className={styles.panelKicker}>{t('workbench:mobile.kicker')}</p>
                    <h2 id={dialogTitleId}>{t('workbench:mobile.automationPanel.createOpen')}</h2>
                  </div>
                  <button
                    type="button"
                    className={styles.secondaryButton}
                    disabled={creating || completingPrompt}
                    onClick={handleCloseCreateDialog}
                  >
                    {t('workbench:mobile.automationPanel.closeCreate')}
                  </button>
                </div>

                <div className={styles.mobileDialogBody}>
                  <div className={styles.mobileDialogAssist}>
                    <label className={styles.mobileField}>
                      <span>{t('workbench:mobile.automationPanel.shortPrompt')}</span>
                      <textarea
                        ref={promptDraftRef}
                        className={styles.mobileTextarea}
                        value={promptDraft}
                        disabled={creating || completingPrompt}
                        placeholder={t('workbench:mobile.automationPanel.shortPromptPlaceholder')}
                        onChange={(event) => {
                          setPromptDraft(event.target.value);
                          setStatus(null);
                        }}
                      />
                    </label>
                    <button
                      type="button"
                      className={styles.mobileTerminalPrimaryButton}
                      disabled={!canCompletePrompt}
                      onClick={() => {
                        void handleCompletePrompt();
                      }}
                    >
                      {completingPrompt
                        ? t('workbench:mobile.automationPanel.completingPrompt')
                        : t('workbench:mobile.automationPanel.completeWithAi')}
                    </button>
                  </div>

                  <form
                    className={styles.mobileFormInline}
                    onSubmit={(event) => {
                      event.preventDefault();
                    }}
                  >
                    <label className={styles.mobileField}>
                      <span>{t('workbench:mobile.automationPanel.fields.title')}</span>
                      <input
                        className={styles.mobileInput}
                        value={title}
                        disabled={creating || completingPrompt}
                        placeholder={t('workbench:mobile.automationPanel.placeholders.title')}
                        onChange={(event) => {
                          setTitle(event.target.value);
                          setStatus(null);
                        }}
                      />
                    </label>

                    <label className={styles.mobileField}>
                      <span>{t('workbench:mobile.automationPanel.fields.goal')}</span>
                      <textarea
                        className={styles.mobileTextarea}
                        value={goal}
                        disabled={creating || completingPrompt}
                        placeholder={t('workbench:mobile.automationPanel.placeholders.goal')}
                        onChange={(event) => {
                          setGoal(event.target.value);
                          setStatus(null);
                        }}
                      />
                    </label>

                    <label className={styles.mobileField}>
                      <span>{t('workbench:mobile.automationPanel.fields.acceptanceCriteria')}</span>
                      <textarea
                        className={styles.mobileTextarea}
                        value={acceptanceCriteria}
                        disabled={creating || completingPrompt}
                        placeholder={t(
                          'workbench:mobile.automationPanel.placeholders.acceptanceCriteria',
                        )}
                        onChange={(event) => {
                          setAcceptanceCriteria(event.target.value);
                          setStatus(null);
                        }}
                      />
                    </label>

                    <div className={styles.mobileAutomationCreateActions}>
                      {MOBILE_AUTOMATION_CREATE_ACTIONS.map((action) => (
                        <button
                          key={action.createAction}
                          type="button"
                          className={
                            action.createAction === 'start'
                              ? styles.mobileTerminalPrimaryButton
                              : styles.secondaryButton
                          }
                          disabled={!canSubmit}
                          onClick={() => {
                            void handleCreateTask(action.createAction, action.statusKey);
                          }}
                        >
                          {creatingAction === action.createAction
                            ? t('workbench:mobile.automationPanel.creating')
                            : t(action.labelKey)}
                        </button>
                      ))}
                    </div>
                  </form>
                </div>
              </div>
            </div>
          ) : null}
        </>
      ) : null}
    </section>
  );
}
