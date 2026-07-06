/**
 * Orchestrator 页面 - 自动化任务编排入口
 *
 * Business Logic（为什么需要这个页面）:
 *   用户需要在当前 Workbench 项目下管理项目级自动化任务队列，包括本机任务、远端任务和离线远端待发送项。
 *   自动化配置统一由 Settings 自动化 tab 管理，本面板只展示任务执行与证据。
 *   当前前端只提供任务、验证证据与 blocked 控制入口，并可把任务定位回对应 Workbench 上下文。
 *
 * Code Logic（这个组件做什么）:
 *   - 按 activeProject 拉取 Orchestrator task view 列表，真实任务按状态分组，pending remote 单独展示
 *   - 按 activeProject 与 selected task 拉取 evidence
 *   - 通过独立弹窗创建任务，支持手动填写三字段或用简单 Prompt 调 AI 自动完善
 *   - 允许选中的 draft 任务切换为 queued；action 响应只替换列表，同项目任务切换后不抢回 selection
 *   - 创建成功后把真实任务插入列表并选中；pending remote 只插入待发送区并清空表单
 *   - Running 任务可触发后端验证与交付，Blocked 任务可打开 Workbench deep link、重试或终止
 *   - full-auto 交付由后端 complete 命令执行，前端只负责触发命令和展示状态/evidence
 *   - hooks 全部位于渲染分支之前，避免 early return 破坏调用顺序
 */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { FormEvent, JSX } from 'react';
import { useTranslation } from 'react-i18next';
import { createPortal } from 'react-dom';
import { useNavigate } from 'react-router-dom';
import { orchestratorApi } from '@/api/orchestrator';
import { promptOptimizerApi } from '@/api/promptOptimizer';
import { Button, Card, Input, Pill } from '@/components/primitives';
import { useWorkbenchProjects } from '@/hooks/workbenchProjectsContext';
import { CheckIcon, FolderIcon, PlayIcon, PlusIcon, StopIcon, SyncIcon, XIcon } from '@/lib/icons';
import {
  canCompleteAgentRunForProject,
  canControlBlockedTaskForProject,
  canQueueOrchestratorTaskForProject,
  ORCHESTRATOR_STATUSES,
  orchestratorAttemptLabel,
  orchestratorCreateResultMatchesProject,
  orchestratorEvidenceKindLabel,
  orchestratorEvidenceKindTone,
  orchestratorStatusTone,
  orchestratorTaskProgressMessage,
  resolveOrchestratorActionSelection,
  resolveOrchestratorTaskLoad,
} from '@/lib/orchestrator';
import {
  getOrchestratorTaskViewTaskId,
  groupOrchestratorRenderableTasks,
  isLocalOrchestratorTaskView,
  isOrchestratorTaskViewActionable,
  splitOrchestratorTaskViews,
  upsertOrchestratorTaskView,
} from '@/lib/orchestratorRemote';
import type {
  OrchestratorEvidence,
  OrchestratorRemoteOutboxStatus,
  OrchestratorTask,
  OrchestratorTaskStatus,
  OrchestratorTaskView,
} from '@/lib/types';
import { buildWorkbenchDeepLink } from '@/pages/Workbench/workbenchDeepLink';
import styles from './Orchestrator.module.css';

/**
 * Business Logic（为什么需要这个类型）:
 *   i18next v26 对动态 key 有严格类型校验，状态文案需要提前收敛为静态 key 联合。
 *
 * Code Logic（这个类型做什么）:
 *   枚举 Orchestrator 所有状态对应的完整 i18n key。
 */
type OrchestratorStatusLabelKey =
  | 'orchestrator:status.draft'
  | 'orchestrator:status.queued'
  | 'orchestrator:status.preparing'
  | 'orchestrator:status.running'
  | 'orchestrator:status.verifying'
  | 'orchestrator:status.delivering'
  | 'orchestrator:status.done'
  | 'orchestrator:status.blocked'
  | 'orchestrator:status.aborted';

/**
 * Business Logic（为什么需要这个类型）:
 *   Evidence summary 由后端保存为稳定短值，前端需要映射到本地化文案展示。
 *
 * Code Logic（这个类型做什么）:
 *   枚举已知 summary 对应的完整 i18n key，并为未知值提供兜底文案。
 */
type OrchestratorEvidenceSummaryLabelKey =
  | 'orchestrator:evidence.summary.passed'
  | 'orchestrator:evidence.summary.failed'
  | 'orchestrator:evidence.summary.blocked'
  | 'orchestrator:evidence.summary.skipped'
  | 'orchestrator:evidence.summary.running'
  | 'orchestrator:evidence.summary.generic';

/**
 * Business Logic（为什么需要这个类型）:
 *   远端任务待发送状态来自后端短值，前端必须映射成本地化文案展示。
 *
 * Code Logic（这个类型做什么）:
 *   枚举 pending remote outbox status 对应的完整 i18n key。
 */
type OrchestratorRemoteOutboxStatusLabelKey =
  | 'orchestrator:pending.status.pending'
  | 'orchestrator:pending.status.sending'
  | 'orchestrator:pending.status.mirrored'
  | 'orchestrator:pending.status.failed';

/**
 * Business Logic（为什么需要这个类型）:
 *   创建任务表单需要同时管理标题、目标和验收标准，集中成对象便于清空和提交校验。
 *
 * Code Logic（这个类型做什么）:
 *   定义页面本地表单状态，字段与 createTask 请求文本字段一一对应。
 */
interface OrchestratorCreateForm {
  title: string;
  goal: string;
  acceptanceCriteria: string;
}

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
 *   embedded 控制是否隐藏页面级标题栏；onOpenWorkbench 允许嵌入方接管 blocked 任务的现场跳转。
 */
interface OrchestratorPanelProps {
  embedded?: boolean;
  onOpenWorkbench?: (url: string) => void;
}

interface OrchestratorDialogPortalProps {
  children: JSX.Element;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   Workbench 嵌入自动化面板时，创建任务弹窗必须脱离滚动容器和终端全屏层，避免 PC 端弹窗被裁剪或遮挡。
 *
 * Code Logic（这个组件做什么）:
 *   在浏览器环境中把弹窗内容 portal 到 document.body；非浏览器渲染时返回 null，避免访问不存在的 document。
 */
function OrchestratorDialogPortal(
  props: OrchestratorDialogPortalProps,
): ReturnType<typeof createPortal> | null {
  if (typeof document === 'undefined') return null;
  return createPortal(props.children, document.body);
}

const EMPTY_FORM: OrchestratorCreateForm = {
  title: '',
  goal: '',
  acceptanceCriteria: '',
};

const STATUS_LABEL_KEYS: Record<OrchestratorTaskStatus, OrchestratorStatusLabelKey> = {
  draft: 'orchestrator:status.draft',
  queued: 'orchestrator:status.queued',
  preparing: 'orchestrator:status.preparing',
  running: 'orchestrator:status.running',
  verifying: 'orchestrator:status.verifying',
  delivering: 'orchestrator:status.delivering',
  done: 'orchestrator:status.done',
  blocked: 'orchestrator:status.blocked',
  aborted: 'orchestrator:status.aborted',
};

const EVIDENCE_SUMMARY_LABEL_KEYS: Record<string, OrchestratorEvidenceSummaryLabelKey> = {
  passed: 'orchestrator:evidence.summary.passed',
  failed: 'orchestrator:evidence.summary.failed',
  blocked: 'orchestrator:evidence.summary.blocked',
  skipped: 'orchestrator:evidence.summary.skipped',
  running: 'orchestrator:evidence.summary.running',
};

const PENDING_REMOTE_STATUS_LABEL_KEYS: Record<
  OrchestratorRemoteOutboxStatus,
  OrchestratorRemoteOutboxStatusLabelKey
> = {
  pending: 'orchestrator:pending.status.pending',
  sending: 'orchestrator:pending.status.sending',
  mirrored: 'orchestrator:pending.status.mirrored',
  failed: 'orchestrator:pending.status.failed',
};

/**
 * Business Logic（为什么需要这个函数）:
 *   API 调用失败时页面需要优先显示后端返回的可读错误，并在缺少 message 时回退到本地化通用提示。
 *
 * Code Logic（这个函数做什么）:
 *   从 unknown 错误中提取非空字符串；如果无法提取，返回调用方传入的 i18n fallback。
 */
function displayOrchestratorErrorMessage(error: unknown, fallback: string): string {
  const message =
    error instanceof Error ? error.message : typeof error === 'string' ? error : '';
  return message.trim() || fallback;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   任务详情需要展示创建/更新时间，让用户判断队列信息是否新鲜。
 *
 * Code Logic（这个函数做什么）:
 *   将 ISO 时间字符串转换为浏览器本地短日期时间；解析失败时保留原始字符串。
 */
function formatTaskTimestamp(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString([], {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  });
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Evidence 卡需要用一致颜色表达验证结果，帮助用户快速区分通过、失败和跳过。
 *
 * Code Logic（这个函数做什么）:
 *   将 summary 短值映射到 Pill 支持的 tone；未知值按 neutral 展示。
 */
function evidenceSummaryTone(summary: string): 'neutral' | 'success' | 'warn' | 'danger' | 'accent' {
  switch (summary) {
    case 'passed':
      return 'success';
    case 'failed':
      return 'danger';
    case 'running':
      return 'accent';
    case 'blocked':
    case 'skipped':
      return 'warn';
    default:
      return 'neutral';
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   远端待发送任务需要用一致颜色表达待发送、发送中、已镜像和失败状态。
 *
 * Code Logic（这个函数做什么）:
 *   将 outbox status 映射到 Pill 支持的 tone；失败为 danger，发送中为 accent，其余为 neutral/success。
 */
function pendingRemoteStatusTone(
  status: OrchestratorRemoteOutboxStatus,
): 'neutral' | 'success' | 'accent' | 'danger' {
  switch (status) {
    case 'failed':
      return 'danger';
    case 'sending':
      return 'accent';
    case 'mirrored':
      return 'success';
    case 'pending':
      return 'neutral';
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Evidence summary 不能直接把后端短值当用户文案展示，需要走 i18n。
 *
 * Code Logic（这个函数做什么）:
 *   返回已知 summary 的 i18n key；未知值返回 generic 兜底 key。
 */
function evidenceSummaryLabelKey(summary: string): OrchestratorEvidenceSummaryLabelKey {
  return EVIDENCE_SUMMARY_LABEL_KEYS[summary] ?? 'orchestrator:evidence.summary.generic';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   任务详情需要从完整 evidence 列表中提取最新 verifier 结论和修复指令，避免用户在长列表里手动寻找。
 *
 * Code Logic（这个函数做什么）:
 *   从尾到头查找指定 kind 的第一条 evidence；未找到时返回 null，不改变原数组顺序。
 */
function latestEvidenceByKind(
  items: OrchestratorEvidence[],
  kind: string,
): OrchestratorEvidence | null {
  for (let index = items.length - 1; index >= 0; index -= 1) {
    if (items[index].kind === kind) return items[index];
  }
  return null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   详情页需要展示历史开发轮次，后端当前通过 developmentAttempt evidence 暴露可审计记录。
 *
 * Code Logic（这个函数做什么）:
 *   过滤指定 kind 的 evidence 并保持后端返回顺序，供 UI 列表稳定渲染。
 */
function evidenceItemsByKind(
  items: OrchestratorEvidence[],
  kind: string,
): OrchestratorEvidence[] {
  return items.filter((item) => item.kind === kind);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Blocked 任务的修复入口应尽量回到任务关联的 Workbench 项目、worktree 和终端窗口。
 *
 * Code Logic（这个函数做什么）:
 *   根据任务中存在的 id 构造 `/workbench` deep link；缺少任务或缺少某个 id 时省略对应 query 参数。
 */
function buildWorkbenchTaskUrl(task: OrchestratorTask | null): string {
  return buildWorkbenchDeepLink({
    projectId: task?.projectId ?? null,
    worktreeId: task?.worktreeId ?? null,
    sessionId: task?.sessionId ?? null,
  });
}

/**
 * Orchestrator 可嵌入面板组件
 *
 * Business Logic（为什么需要这个函数）:
 *   Workbench 需要把自动化看板作为终端、文件预览同级的工作区视图，同时保留页面壳复用能力。
 *
 * Code Logic（这个函数做什么）:
 *   维持 activeProject、task view 列表与 evidence stale guard；embedded=true 时省略页面级 header。
 */
export function OrchestratorPanel(props: OrchestratorPanelProps): JSX.Element {
  const { embedded = false, onOpenWorkbench } = props;
  const { t } = useTranslation(['orchestrator', 'nav', 'common']);
  const navigate = useNavigate();
  const { activeProject, projectsLoading } = useWorkbenchProjects();
  const [taskListResult, setTaskListResult] = useState<OrchestratorTaskListResult | null>(null);
  const [selectedTaskId, setSelectedTaskId] = useState<string | null>(null);
  const [form, setForm] = useState<OrchestratorCreateForm>(EMPTY_FORM);
  const [creating, setCreating] = useState(false);
  const [queueingTaskId, setQueueingTaskId] = useState<string | null>(null);
  const [completingTaskId, setCompletingTaskId] = useState<string | null>(null);
  const [retryingTaskId, setRetryingTaskId] = useState<string | null>(null);
  const [abortingTaskId, setAbortingTaskId] = useState<string | null>(null);
  const [evidenceResult, setEvidenceResult] = useState<OrchestratorEvidenceResult | null>(null);
  const [actionError, setActionError] = useState<OrchestratorActionError | null>(null);
  const [createDialogOpen, setCreateDialogOpen] = useState<boolean>(false);
  const [completionPrompt, setCompletionPrompt] = useState('');
  const [completingPrompt, setCompletingPrompt] = useState(false);
  const completionPromptRef = useRef<HTMLTextAreaElement | null>(null);
  const activeProjectId = activeProject?.id ?? null;
  const activeProjectIdRef = useRef<string | null>(activeProjectId);
  const taskLoadDecision = useMemo(
    () => resolveOrchestratorTaskLoad(projectsLoading, activeProjectId),
    [activeProjectId, projectsLoading],
  );

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

  const groups = useMemo(() => groupOrchestratorRenderableTasks(tasks), [tasks]);
  const selectedRenderableTask = useMemo(() => {
    return tasks.find((item) => item.task.id === selectedTaskId) ?? tasks[0] ?? null;
  }, [selectedTaskId, tasks]);
  const selectedTaskView = selectedRenderableTask?.view ?? null;
  const selectedTask = selectedRenderableTask?.task ?? null;
  const selectedTaskCanQueue = canQueueOrchestratorTaskForProject(selectedTask, activeProjectId);
  const selectedTaskCanComplete =
    isLocalOrchestratorTaskView(selectedTaskView) &&
    canCompleteAgentRunForProject(selectedTask, activeProjectId);
  const selectedTaskCanControlBlocked = canControlBlockedTaskForProject(
    selectedTask,
    activeProjectId,
  );
  const selectedTaskCanOpenWorkbench = Boolean(
    selectedTask?.projectId && (selectedTask.worktreeId || selectedTask.sessionId),
  );
  const selectedTaskProgressMessage = orchestratorTaskProgressMessage(selectedTaskView, t);
  const selectedTaskTerminalLabel = selectedTask?.sessionId ?? selectedTask?.worktreeId ?? null;
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
    !creating;
  const canCompletePrompt =
    Boolean(activeProjectId) && completionPrompt.trim().length > 0 && !completingPrompt;

  const createPromptWorkingDirectory = useMemo(() => {
    if (!activeProject || activeProject.kind !== 'local') return null;
    return activeProject.path;
  }, [activeProject]);

  const handleOpenCreateDialog = useCallback(() => {
    setActionError(null);
    setCreateDialogOpen(true);
  }, []);

  const handleCloseCreateDialog = useCallback(() => {
    if (creating || completingPrompt) return;
    setCreateDialogOpen(false);
  }, [completingPrompt, creating]);

  useEffect(() => {
    if (!createDialogOpen) return undefined;
    const focusTimer = window.setTimeout(() => completionPromptRef.current?.focus(), 0);
    return () => window.clearTimeout(focusTimer);
  }, [createDialogOpen]);

  useEffect(() => {
    if (!createDialogOpen) return undefined;
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') handleCloseCreateDialog();
    };
    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [createDialogOpen, handleCloseCreateDialog]);

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
          return nextSplit.tasks[0]?.task.id ?? null;
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

  const updateFormField = useCallback(
    (field: keyof OrchestratorCreateForm, value: string) => {
      setForm((current) => ({ ...current, [field]: value }));
    },
    [],
  );

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

  const handleCreateTask = useCallback(
    async (event: FormEvent<HTMLFormElement>) => {
      event.preventDefault();
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
      };
      if (!payload.title || !payload.goal || !payload.acceptanceCriteria) {
        setActionError({ projectId, message: t('orchestrator:errors.required') });
        return;
      }
      setCreating(true);
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
        const createdTaskId = getOrchestratorTaskViewTaskId(created);
        if (createdTaskId) setSelectedTaskId(createdTaskId);
        setForm(EMPTY_FORM);
        setCompletionPrompt('');
        setCreateDialogOpen(false);
      } catch (err) {
        setActionError({
          projectId,
          message: displayOrchestratorErrorMessage(err, t('orchestrator:errors.create')),
        });
      } finally {
        setCreating(false);
      }
    },
    [activeProject, activeProjectId, form, t],
  );

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

  const handleQueueSelectedTask = useCallback(async () => {
    if (
      !selectedTask ||
      !isOrchestratorTaskViewActionable(selectedTaskView) ||
      !canQueueOrchestratorTaskForProject(selectedTask, activeProjectIdRef.current)
    ) {
      return;
    }
    const taskId = selectedTask.id;
    const projectId = selectedTask.projectId;
    setQueueingTaskId(taskId);
    setActionError(null);
    try {
      const queued = await orchestratorApi.queueTaskView(projectId, taskId);
      const queuedProjectId = queued.origin === 'pendingRemote' ? null : queued.task.projectId;
      if (
        !orchestratorCreateResultMatchesProject(activeProjectIdRef.current, projectId) ||
        queuedProjectId !== projectId
      ) {
        return;
      }
      replaceTaskViewInCurrentProject(projectId, queued);
    } catch (err) {
      if (orchestratorCreateResultMatchesProject(activeProjectIdRef.current, projectId)) {
        setActionError({
          projectId,
          message: displayOrchestratorErrorMessage(err, t('orchestrator:errors.queue')),
        });
      }
    } finally {
      setQueueingTaskId((current) => (current === taskId ? null : current));
    }
  }, [replaceTaskViewInCurrentProject, selectedTask, selectedTaskView, t]);

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
  }, [completingTaskId, replaceTaskViewInCurrentProject, selectedTask, selectedTaskView, t]);

  const handleOpenWorkbench = useCallback(() => {
    const url = buildWorkbenchTaskUrl(selectedTask);
    if (onOpenWorkbench) {
      onOpenWorkbench(url);
      return;
    }
    navigate(url);
  }, [navigate, onOpenWorkbench, selectedTask]);

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
  }, [replaceTaskViewInCurrentProject, retryingTaskId, selectedTask, selectedTaskView, t]);

  const handleAbortTask = useCallback(async () => {
    if (
      !selectedTask ||
      !isOrchestratorTaskViewActionable(selectedTaskView) ||
      !canControlBlockedTaskForProject(selectedTask, activeProjectIdRef.current) ||
      abortingTaskId === selectedTask.id
    ) {
      return;
    }
    const taskId = selectedTask.id;
    const projectId = selectedTask.projectId;
    setAbortingTaskId(taskId);
    setActionError(null);
    try {
      const updated = await orchestratorApi.abortTaskView(projectId, taskId);
      const updatedProjectId = updated.origin === 'pendingRemote' ? null : updated.task.projectId;
      if (
        !orchestratorCreateResultMatchesProject(activeProjectIdRef.current, projectId) ||
        updatedProjectId !== projectId
      ) {
        return;
      }
      replaceTaskViewInCurrentProject(projectId, updated);
    } catch (err) {
      if (orchestratorCreateResultMatchesProject(activeProjectIdRef.current, projectId)) {
        setActionError({
          projectId,
          message: displayOrchestratorErrorMessage(err, t('orchestrator:errors.abort')),
        });
      }
    } finally {
      setAbortingTaskId((current) => (current === taskId ? null : current));
    }
  }, [abortingTaskId, replaceTaskViewInCurrentProject, selectedTask, selectedTaskView, t]);

  return (
    <div className={embedded ? styles.embedded : styles.page}>
      {!embedded ? (
        <header className={styles.header}>
          <div className={styles.headerText}>
            <span className={styles.eyebrow}>{t('nav:orchestrator')}</span>
            <h1 className={styles.title}>{t('orchestrator:title')}</h1>
            <p className={styles.subtitle}>{t('orchestrator:subtitle')}</p>
          </div>
          <div className={styles.projectStatus}>
            <Pill tone={activeProject ? 'success' : 'warn'} dot>
              {activeProject ? activeProject.name : t('orchestrator:noProject')}
            </Pill>
          </div>
        </header>
      ) : null}

      {error ? (
        <div className={styles.error} role="alert">
          {error}
        </div>
      ) : null}

      <div className={styles.grid}>
        <Card variant="outlined" padding="md" className={styles.queue}>
          <Card.Header className={styles.cardHeader}>
            <div>
              <h2 className={styles.sectionTitle}>{t('orchestrator:queue.title')}</h2>
              <p className={styles.sectionLead}>{t('orchestrator:queue.subtitle')}</p>
            </div>
            <div className={styles.queueActions}>
              <Pill tone="neutral">{tasks.length + pendingRemoteItems.length}</Pill>
              <Button
                variant="primary"
                size="sm"
                icon={<PlusIcon />}
                disabled={!activeProjectId}
                onClick={handleOpenCreateDialog}
              >
                {t('orchestrator:create.open')}
              </Button>
            </div>
          </Card.Header>
          <Card.Body className={styles.queueBody}>
            {loading ? <p className={styles.muted}>{t('common:loading')}</p> : null}
            {!loading && tasks.length === 0 && pendingRemoteItems.length === 0 ? (
              <div className={styles.empty}>
                <h3 className={styles.emptyTitle}>{t('orchestrator:emptyTitle')}</h3>
                <p className={styles.emptyBody}>{t('orchestrator:emptyBody')}</p>
              </div>
            ) : null}
            {!loading && tasks.length > 0
              ? ORCHESTRATOR_STATUSES.map((status) => (
                  <section className={styles.group} key={status}>
                    <div className={styles.groupHeader}>
                      <span>{t(STATUS_LABEL_KEYS[status])}</span>
                      <Pill tone={orchestratorStatusTone(status)}>{groups[status].length}</Pill>
                    </div>
                    <div className={styles.taskList}>
                      {groups[status].map((item) => {
                        const { task } = item;
                        const active = selectedTask?.id === task.id;
                        return (
                          <button
                            className={`${styles.task} ${active ? styles.taskActive : ''}`}
                            type="button"
                            aria-pressed={active}
                            aria-label={t('orchestrator:queue.taskAria', { title: task.title })}
                            key={task.id}
                            onClick={() => setSelectedTaskId(task.id)}
                          >
                            <span className={styles.taskTitle}>{task.title}</span>
                            <span className={styles.taskMeta}>
                              {t('orchestrator:queue.priority', { priority: task.priority })}
                              {' · '}
                              {item.origin === 'remote'
                                ? t('orchestrator:queue.remoteTask', {
                                    deviceName:
                                      item.deviceName ?? t('orchestrator:queue.unknownDevice'),
                                  })
                                : t('orchestrator:queue.localTask')}
                            </span>
                          </button>
                        );
                      })}
                    </div>
                  </section>
                ))
              : null}
            {!loading && pendingRemoteItems.length > 0 ? (
              <section className={styles.group}>
                <div className={styles.groupHeader}>
                  <span>{t('orchestrator:pending.title')}</span>
                  <Pill tone="warn">{pendingRemoteItems.length}</Pill>
                </div>
                <div className={styles.taskList}>
                  {pendingRemoteItems.map((item) => (
                    <div className={styles.pendingTask} key={item.id}>
                      <div className={styles.pendingTaskHeader}>
                        <span className={styles.taskTitle}>{item.deviceName}</span>
                        <Pill tone={pendingRemoteStatusTone(item.status)}>
                          {t(PENDING_REMOTE_STATUS_LABEL_KEYS[item.status])}
                        </Pill>
                      </div>
                      <span className={styles.taskMeta}>
                        {t('orchestrator:pending.remoteProjectPath', {
                          path: item.remoteProjectPath,
                        })}
                      </span>
                      {item.lastError ? (
                        <p className={styles.pendingError}>
                          {t('orchestrator:pending.lastError', { error: item.lastError })}
                        </p>
                      ) : null}
                    </div>
                  ))}
                </div>
              </section>
            ) : null}
          </Card.Body>
        </Card>

        <div className={styles.detail}>
          <Card variant="outlined" padding="md">
            <Card.Header className={styles.cardHeader}>
              <div>
                <h2 className={styles.sectionTitle}>{t('orchestrator:detail.title')}</h2>
                <p className={styles.sectionLead}>{t('orchestrator:detail.subtitle')}</p>
              </div>
              <div className={styles.detailActions}>
                {selectedTaskCanQueue ? (
                  <Button
                    variant="primary"
                    size="sm"
                    icon={<PlayIcon />}
                    loading={queueingTaskId === selectedTask?.id}
                    onClick={handleQueueSelectedTask}
                  >
                    {t('orchestrator:detail.queue')}
                  </Button>
                ) : null}
                {selectedTaskCanComplete ? (
                  <Button
                    variant="primary"
                    size="sm"
                    icon={<CheckIcon />}
                    loading={completingTaskId === selectedTask?.id}
                    onClick={handleCompleteAgentRun}
                  >
                    {t('orchestrator:detail.completeAgentRun')}
                  </Button>
                ) : null}
                {selectedRenderableTask ? (
                  <Pill
                    tone={selectedRenderableTask.origin === 'remote' ? 'accent' : 'neutral'}
                    dot
                  >
                    {selectedRenderableTask.origin === 'remote'
                      ? t('orchestrator:detail.remoteTask', {
                          deviceName:
                            selectedRenderableTask.deviceName ??
                            t('orchestrator:queue.unknownDevice'),
                        })
                      : t('orchestrator:detail.localTask')}
                  </Pill>
                ) : null}
                {selectedTask ? (
                  <Pill tone={orchestratorStatusTone(selectedTask.status)} dot>
                    {t(STATUS_LABEL_KEYS[selectedTask.status])}
                  </Pill>
                ) : null}
              </div>
            </Card.Header>
            <Card.Body className={styles.detailBody}>
              {selectedTask ? (
                <>
                  <div className={styles.detailTitleRow}>
                    <h3 className={styles.detailTitle}>{selectedTask.title}</h3>
                  </div>
                  {selectedTaskProgressMessage ? (
                    <p className={styles.progressMessage}>{selectedTaskProgressMessage}</p>
                  ) : null}
                  <div className={styles.detailBlock}>
                    <span className={styles.label}>{t('orchestrator:detail.goal')}</span>
                    <p className={styles.detailText}>{selectedTask.goal}</p>
                  </div>
                  <div className={styles.detailBlock}>
                    <span className={styles.label}>
                      {t('orchestrator:detail.acceptanceCriteria')}
                    </span>
                    <p className={styles.detailText}>{selectedTask.acceptanceCriteria}</p>
                  </div>
                  <dl className={styles.metaGrid}>
                    <div>
                      <dt>{t('orchestrator:detail.branch')}</dt>
                      <dd>{selectedTask.branchName ?? t('orchestrator:detail.unassigned')}</dd>
                    </div>
                    <div>
                      <dt>{t('orchestrator:detail.attempt')}</dt>
                      <dd>{orchestratorAttemptLabel(selectedTask, t)}</dd>
                    </div>
                    <div>
                      <dt>{t('orchestrator:detail.activeSession')}</dt>
                      <dd>
                        {selectedTaskTerminalLabel && selectedTaskCanOpenWorkbench ? (
                          <button
                            type="button"
                            className={styles.inlineLinkButton}
                            onClick={handleOpenWorkbench}
                          >
                            {selectedTaskTerminalLabel}
                          </button>
                        ) : (
                          t('orchestrator:detail.unassigned')
                        )}
                      </dd>
                    </div>
                    {selectedRenderableTask?.origin === 'remote' ? (
                      <div>
                        <dt>{t('orchestrator:detail.executionDevice')}</dt>
                        <dd>
                          {selectedRenderableTask.deviceName ??
                            t('orchestrator:queue.unknownDevice')}
                        </dd>
                      </div>
                    ) : null}
                    <div>
                      <dt>{t('orchestrator:detail.createdAt')}</dt>
                      <dd>{formatTaskTimestamp(selectedTask.createdAt)}</dd>
                    </div>
                    <div>
                      <dt>{t('orchestrator:detail.updatedAt')}</dt>
                      <dd>{formatTaskTimestamp(selectedTask.updatedAt)}</dd>
                    </div>
                  </dl>
                  {selectedTask.status === 'blocked' ? (
                    <div className={styles.blockedReason}>
                      <span className={styles.label}>{t('orchestrator:detail.blockedReason')}</span>
                      <p>{selectedTask.blockedReason ?? t('orchestrator:detail.noBlockedReason')}</p>
                    </div>
                  ) : null}
                  {latestVerifierEvidence ? (
                    <div className={styles.detailEvidenceSummary}>
                      <div className={styles.detailEvidenceHeader}>
                        <span className={styles.label}>
                          {t('orchestrator:detail.latestVerifierResult')}
                        </span>
                        <Pill tone={evidenceSummaryTone(latestVerifierEvidence.summary)}>
                          {t(evidenceSummaryLabelKey(latestVerifierEvidence.summary))}
                        </Pill>
                      </div>
                      <pre className={styles.detailEvidenceContent}>
                        {latestVerifierEvidence.content}
                      </pre>
                    </div>
                  ) : null}
                  {latestRepairPromptEvidence ? (
                    <div className={styles.detailEvidenceSummary}>
                      <div className={styles.detailEvidenceHeader}>
                        <span className={styles.label}>
                          {t('orchestrator:detail.latestRepairPrompt')}
                        </span>
                        <Pill tone={orchestratorEvidenceKindTone(latestRepairPromptEvidence.kind)}>
                          {orchestratorEvidenceKindLabel(latestRepairPromptEvidence.kind, t)}
                        </Pill>
                      </div>
                      <pre className={styles.detailEvidenceContent}>
                        {latestRepairPromptEvidence.content}
                      </pre>
                    </div>
                  ) : null}
                  {developmentAttemptEvidenceItems.length > 0 ? (
                    <div className={styles.attemptHistory}>
                      <span className={styles.label}>
                        {t('orchestrator:detail.priorAttempts')}
                      </span>
                      <ul className={styles.attemptHistoryList}>
                        {developmentAttemptEvidenceItems.map((item) => (
                          <li className={styles.attemptHistoryItem} key={item.id}>
                            <span>{item.title}</span>
                            <Pill tone={evidenceSummaryTone(item.summary)}>
                              {t(evidenceSummaryLabelKey(item.summary))}
                            </Pill>
                          </li>
                        ))}
                      </ul>
                    </div>
                  ) : null}
                  {selectedTaskCanControlBlocked ? (
                    <div className={styles.blockedControls}>
                      <Button
                        variant="secondary"
                        size="sm"
                        icon={<FolderIcon />}
                        onClick={handleOpenWorkbench}
                      >
                        {t('orchestrator:detail.openWorkbench')}
                      </Button>
                      <Button
                        variant="primary"
                        size="sm"
                        icon={<SyncIcon />}
                        loading={retryingTaskId === selectedTask.id}
                        onClick={handleRetryTask}
                      >
                        {t('orchestrator:detail.retry')}
                      </Button>
                      <Button
                        variant="danger"
                        size="sm"
                        icon={<StopIcon />}
                        loading={abortingTaskId === selectedTask.id}
                        onClick={handleAbortTask}
                      >
                        {t('orchestrator:detail.abort')}
                      </Button>
                    </div>
                  ) : null}
                </>
              ) : (
                <div className={styles.empty}>
                  <h3 className={styles.emptyTitle}>{t('orchestrator:emptyTitle')}</h3>
                  <p className={styles.emptyBody}>{t('orchestrator:emptyBody')}</p>
                </div>
              )}
            </Card.Body>
          </Card>

        </div>

        <div className={styles.rightStack}>
          <Card variant="outlined" padding="md" className={styles.evidence}>
            <Card.Header className={styles.cardHeader}>
              <div>
                <h2 className={styles.sectionTitle}>{t('orchestrator:evidence.title')}</h2>
                <p className={styles.sectionLead}>{t('orchestrator:evidence.subtitle')}</p>
              </div>
              {selectedTask ? <Pill tone="neutral">{evidenceItems.length}</Pill> : null}
            </Card.Header>
            <Card.Body className={styles.evidenceBody}>
              {!selectedTask ? (
                <div className={styles.empty}>
                  <h3 className={styles.emptyTitle}>
                    {t('orchestrator:evidence.placeholderLabel')}
                  </h3>
                  <p className={styles.emptyBody}>{t('orchestrator:evidence.placeholderBody')}</p>
                </div>
              ) : null}
              {selectedTask && evidenceLoading ? (
                <p className={styles.muted}>{t('orchestrator:evidence.loading')}</p>
              ) : null}
              {selectedTask && !evidenceLoading && evidenceError ? (
                <div className={styles.errorBox} role="alert">
                  {evidenceError}
                </div>
              ) : null}
              {selectedTask && !evidenceLoading && !evidenceError && evidenceItems.length === 0 ? (
                <div className={styles.empty}>
                  <h3 className={styles.emptyTitle}>{t('orchestrator:evidence.emptyTitle')}</h3>
                  <p className={styles.emptyBody}>{t('orchestrator:evidence.emptyBody')}</p>
                </div>
              ) : null}
              {selectedTask && !evidenceLoading && !evidenceError && evidenceItems.length > 0 ? (
                <ul className={styles.evidenceList}>
                  {evidenceItems.map((item) => (
                    <li className={styles.evidenceItem} key={item.id}>
                      <div className={styles.evidenceItemHeader}>
                        <div>
                          <h3 className={styles.evidenceTitle}>{item.title}</h3>
                          <p className={styles.evidenceMeta}>
                            {formatTaskTimestamp(item.createdAt)}
                          </p>
                        </div>
                        <div className={styles.evidencePills}>
                          <Pill tone={orchestratorEvidenceKindTone(item.kind)}>
                            {orchestratorEvidenceKindLabel(item.kind, t)}
                          </Pill>
                          <Pill tone={evidenceSummaryTone(item.summary)}>
                            {t(evidenceSummaryLabelKey(item.summary))}
                          </Pill>
                        </div>
                      </div>
                      <pre className={styles.evidenceContent}>{item.content}</pre>
                    </li>
                  ))}
                </ul>
              ) : null}
            </Card.Body>
          </Card>
        </div>
      </div>
      {createDialogOpen ? (
        <OrchestratorDialogPortal>
          <div
            className={styles.createDialogOverlay}
            onMouseDown={(event) => {
              if (event.target === event.currentTarget) handleCloseCreateDialog();
            }}
          >
            <Card
              variant="elevated"
              padding="md"
              className={styles.createDialog}
              role="dialog"
              aria-modal="true"
              aria-labelledby="orchestrator-create-dialog-title"
            >
              <Card.Header className={styles.dialogHeader}>
                <div>
                  <h2 id="orchestrator-create-dialog-title" className={styles.sectionTitle}>
                    {t('orchestrator:create.title')}
                  </h2>
                  <p className={styles.sectionLead}>{t('orchestrator:create.subtitle')}</p>
                </div>
                <Button
                  variant="icon"
                  aria-label={t('orchestrator:create.close')}
                  icon={<XIcon />}
                  disabled={creating || completingPrompt}
                  onClick={handleCloseCreateDialog}
                />
              </Card.Header>
              <Card.Body className={styles.dialogBody}>
                <div className={styles.aiAssistBlock}>
                  <label className={styles.field}>
                    <span>{t('orchestrator:create.quickPrompt')}</span>
                    <textarea
                      ref={completionPromptRef}
                      className={styles.textarea}
                      value={completionPrompt}
                      onChange={(event) => setCompletionPrompt(event.target.value)}
                      placeholder={t('orchestrator:create.quickPromptPlaceholder')}
                      aria-label={t('orchestrator:create.quickPrompt')}
                      rows={4}
                      disabled={completingPrompt || creating}
                    />
                  </label>
                  <div className={styles.aiAssistActions}>
                    <Button
                      variant="secondary"
                      size="sm"
                      icon={<SyncIcon />}
                      loading={completingPrompt}
                      disabled={!canCompletePrompt || creating}
                      onClick={handleCompleteTaskPrompt}
                    >
                      {t('orchestrator:create.completeWithAi')}
                    </Button>
                  </div>
                </div>
                <form className={styles.form} onSubmit={handleCreateTask}>
                  <label className={styles.field}>
                    <span>{t('orchestrator:create.taskTitle')}</span>
                    <Input
                      value={form.title}
                      onChange={(event) => updateFormField('title', event.target.value)}
                      placeholder={t('orchestrator:create.taskTitlePlaceholder')}
                      aria-label={t('orchestrator:create.taskTitle')}
                      disabled={creating}
                    />
                  </label>
                  <label className={styles.field}>
                    <span>{t('orchestrator:create.goal')}</span>
                    <textarea
                      className={styles.textarea}
                      value={form.goal}
                      onChange={(event) => updateFormField('goal', event.target.value)}
                      placeholder={t('orchestrator:create.goalPlaceholder')}
                      aria-label={t('orchestrator:create.goal')}
                      rows={5}
                      disabled={creating}
                    />
                  </label>
                  <label className={styles.field}>
                    <span>{t('orchestrator:create.acceptanceCriteria')}</span>
                    <textarea
                      className={styles.textarea}
                      value={form.acceptanceCriteria}
                      onChange={(event) =>
                        updateFormField('acceptanceCriteria', event.target.value)
                      }
                      placeholder={t('orchestrator:create.acceptanceCriteriaPlaceholder')}
                      aria-label={t('orchestrator:create.acceptanceCriteria')}
                      rows={5}
                      disabled={creating}
                    />
                  </label>
                  <div className={styles.dialogActions}>
                    <Button
                      variant="ghost"
                      size="md"
                      disabled={creating || completingPrompt}
                      onClick={handleCloseCreateDialog}
                    >
                      {t('common:action.cancel')}
                    </Button>
                    <Button
                      variant="primary"
                      size="md"
                      type="submit"
                      icon={<PlusIcon />}
                      loading={creating}
                      disabled={!canCreate || completingPrompt}
                    >
                      {t('orchestrator:create.submit')}
                    </Button>
                  </div>
                </form>
              </Card.Body>
            </Card>
          </div>
        </OrchestratorDialogPortal>
      ) : null}
    </div>
  );
}

/**
 * Orchestrator 页面组件
 *
 * Business Logic（为什么需要这个函数）:
 *   旧页面入口仍可作为独立渲染边界保留，便于内部复用和未来路由调整。
 *
 * Code Logic（这个函数做什么）:
 *   渲染非嵌入模式 OrchestratorPanel，保留完整页面级 header shell。
 */
export function Orchestrator(): JSX.Element {
  return <OrchestratorPanel />;
}
