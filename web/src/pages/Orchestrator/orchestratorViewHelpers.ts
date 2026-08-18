/**
 * Orchestrator 视图纯 helper
 *
 * Business Logic（为什么需要这个模块）:
 *   Board/Drawer/Outbox/Create 视图只负责渲染，不能依赖 API 模块；状态文案 key、时间格式与 tone
 *   映射等纯逻辑需要从控制器与巨型页面中抽离，供多个视图安全复用。
 *
 * Code Logic（这个模块做什么）:
 *   导出 i18n label key 映射、创建动作配置、时间/runtime 格式化、Pill tone 与 evidence 查找纯函数；
 *   不 import `@/api/*`、transport 或 React 组件。
 */
import { buildWorkbenchDeepLink } from '@/pages/Workbench/workbenchDeepLink';
import type {
  OrchestratorAttemptPhase,
  OrchestratorEvidence,
  OrchestratorRemoteOutboxStatus,
  OrchestratorRunState,
  OrchestratorTask,
  OrchestratorTaskStatus,
  OrchestratorWorkflowState,
} from '@/lib/types';

/**
 * Business Logic（为什么需要这个类型）:
 *   创建任务按钮的动作值必须是固定三态，视图与控制器都依赖该联合类型但视图不得 import API。
 *
 * Code Logic（这个类型做什么）:
 *   与 `@/api/orchestrator` 的 OrchestratorCreateAction 字面量保持一致：backlog | todo | start。
 */
export type OrchestratorCreateAction = 'backlog' | 'todo' | 'start';

/**
 * Business Logic（为什么需要这个类型）:
 *   i18next v26 对动态 key 有严格类型校验，状态文案需要提前收敛为静态 key 联合。
 *
 * Code Logic（这个类型做什么）:
 *   枚举 Orchestrator 所有状态对应的完整 i18n key。
 */
export type OrchestratorStatusLabelKey =
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
export type OrchestratorEvidenceSummaryLabelKey =
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
export type OrchestratorRemoteOutboxStatusLabelKey =
  | 'orchestrator:pending.status.pending'
  | 'orchestrator:pending.status.sending'
  | 'orchestrator:pending.status.mirrored'
  | 'orchestrator:pending.status.failed'
  | 'orchestrator:pending.status.discarded';

/**
 * Business Logic（为什么需要这个类型）:
 *   看板泳道与详情页需要 workflow state 的静态 i18n key。
 *
 * Code Logic（这个类型做什么）:
 *   枚举全部 workflow 泳道对应的完整 i18n key。
 */
export type OrchestratorWorkflowStateLabelKey =
  | 'orchestrator:workflow.backlog'
  | 'orchestrator:workflow.todo'
  | 'orchestrator:workflow.inProgress'
  | 'orchestrator:workflow.humanReview'
  | 'orchestrator:workflow.rework'
  | 'orchestrator:workflow.merging'
  | 'orchestrator:workflow.done'
  | 'orchestrator:workflow.canceled';

/**
 * Business Logic（为什么需要这个类型）:
 *   任务卡片与 runtime 摘要需要 runState 的静态 i18n key。
 *
 * Code Logic（这个类型做什么）:
 *   枚举全部 run state 对应的完整 i18n key。
 */
export type OrchestratorRunStateLabelKey =
  | 'orchestrator:run.idle'
  | 'orchestrator:run.queued'
  | 'orchestrator:run.preparing'
  | 'orchestrator:run.running'
  | 'orchestrator:run.verifying'
  | 'orchestrator:run.retrying'
  | 'orchestrator:run.blocked'
  | 'orchestrator:run.delivering';

/**
 * Business Logic（为什么需要这个类型）:
 *   attempt phase 文案必须走 i18n，不能直接展示后端短值。
 *
 * Code Logic（这个类型做什么）:
 *   枚举全部 attempt phase 对应的完整 i18n key。
 */
export type OrchestratorAttemptPhaseLabelKey =
  | 'orchestrator:attempt.preparingWorkspace'
  | 'orchestrator:attempt.buildingPrompt'
  | 'orchestrator:attempt.launchingRunner'
  | 'orchestrator:attempt.initializingSession'
  | 'orchestrator:attempt.streaming'
  | 'orchestrator:attempt.finishing'
  | 'orchestrator:attempt.succeeded'
  | 'orchestrator:attempt.failed'
  | 'orchestrator:attempt.timedOut'
  | 'orchestrator:attempt.stalled'
  | 'orchestrator:attempt.canceledByReconciliation';

/**
 * Business Logic（为什么需要这个类型）:
 *   创建弹窗三个提交按钮需要静态 label key，避免动态拼接破坏 i18n 类型。
 *
 * Code Logic（这个类型做什么）:
 *   枚举 backlog/todo/start 三个创建动作的 i18n key。
 */
export type OrchestratorCreateActionLabelKey =
  | 'orchestrator:create.createBacklog'
  | 'orchestrator:create.createTodo'
  | 'orchestrator:create.createStart';

/**
 * Business Logic（为什么需要这个类型）:
 *   创建动作按钮配置需要把 createAction、文案 key 与视觉 variant 绑在一起。
 *
 * Code Logic（这个类型做什么）:
 *   描述单个创建提交按钮的动作值、i18n key 与 Button variant。
 */
export interface OrchestratorCreateActionConfig {
  createAction: OrchestratorCreateAction;
  labelKey: OrchestratorCreateActionLabelKey;
  variant: 'primary' | 'secondary';
}

/**
 * Business Logic（为什么需要这个常量）:
 *   桌面创建弹窗必须固定提供“创建到 Backlog / Todo / 创建并启动”三个显式动作按钮。
 *
 * Code Logic（这个常量做什么）:
 *   按 backlog → todo → start 顺序导出按钮配置，供 CreateDialog 原样映射渲染。
 */
export const ORCHESTRATOR_CREATE_ACTIONS: readonly OrchestratorCreateActionConfig[] = [
  {
    createAction: 'backlog',
    labelKey: 'orchestrator:create.createBacklog',
    variant: 'secondary',
  },
  {
    createAction: 'todo',
    labelKey: 'orchestrator:create.createTodo',
    variant: 'secondary',
  },
  {
    createAction: 'start',
    labelKey: 'orchestrator:create.createStart',
    variant: 'primary',
  },
];

/**
 * Business Logic（为什么需要这个类型）:
 *   创建任务表单需要同时管理标题、目标和验收标准，集中成对象便于清空和提交校验。
 *
 * Code Logic（这个类型做什么）:
 *   定义页面本地表单状态，字段与 createTask 请求文本字段一一对应。
 */
export interface OrchestratorCreateForm {
  title: string;
  goal: string;
  acceptanceCriteria: string;
}

/**
 * Business Logic（为什么需要这个常量）:
 *   创建成功或关闭后需要把表单重置为空白初始值。
 *
 * Code Logic（这个常量做什么）:
 *   提供 title/goal/acceptanceCriteria 全空字符串的表单初始状态。
 */
export const EMPTY_ORCHESTRATOR_CREATE_FORM: OrchestratorCreateForm = {
  title: '',
  goal: '',
  acceptanceCriteria: '',
};

/**
 * Business Logic（为什么需要这个常量）:
 *   任务块创建至少 2 个成员，打开弹窗时就要给出可填空表。
 *
 * Code Logic（这个常量做什么）:
 *   返回两份空三字段表单的浅拷贝。
 */
export function emptyOrchestratorBlockMembers(): OrchestratorCreateForm[] {
  return [
    { ...EMPTY_ORCHESTRATOR_CREATE_FORM },
    { ...EMPTY_ORCHESTRATOR_CREATE_FORM },
  ];
}

/**
 * Business Logic（为什么需要这个映射）:
 *   legacy status 必须映射到静态 i18n key 才能通过 i18next 类型检查。
 *
 * Code Logic（这个映射做什么）:
 *   OrchestratorTaskStatus → 完整 status.* i18n key。
 */
export const STATUS_LABEL_KEYS: Record<OrchestratorTaskStatus, OrchestratorStatusLabelKey> = {
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

/**
 * Business Logic（为什么需要这个映射）:
 *   Evidence summary 短值需要映射到本地化文案。
 *
 * Code Logic（这个映射做什么）:
 *   已知 summary → evidence.summary.* i18n key；未知值由 evidenceSummaryLabelKey 兜底。
 */
export const EVIDENCE_SUMMARY_LABEL_KEYS: Record<string, OrchestratorEvidenceSummaryLabelKey> = {
  passed: 'orchestrator:evidence.summary.passed',
  failed: 'orchestrator:evidence.summary.failed',
  blocked: 'orchestrator:evidence.summary.blocked',
  skipped: 'orchestrator:evidence.summary.skipped',
  running: 'orchestrator:evidence.summary.running',
};

/**
 * Business Logic（为什么需要这个映射）:
 *   pending remote outbox 状态 pill 必须走 i18n。
 *
 * Code Logic（这个映射做什么）:
 *   OrchestratorRemoteOutboxStatus → pending.status.* i18n key。
 */
export const PENDING_REMOTE_STATUS_LABEL_KEYS: Record<
  OrchestratorRemoteOutboxStatus,
  OrchestratorRemoteOutboxStatusLabelKey
> = {
  pending: 'orchestrator:pending.status.pending',
  sending: 'orchestrator:pending.status.sending',
  mirrored: 'orchestrator:pending.status.mirrored',
  failed: 'orchestrator:pending.status.failed',
  discarded: 'orchestrator:pending.status.discarded',
};

/**
 * Business Logic（为什么需要这个映射）:
 *   看板泳道标题与详情字段需要 workflow state 文案。
 *
 * Code Logic（这个映射做什么）:
 *   OrchestratorWorkflowState → workflow.* i18n key。
 */
export const WORKFLOW_STATE_LABEL_KEYS: Record<
  OrchestratorWorkflowState,
  OrchestratorWorkflowStateLabelKey
> = {
  backlog: 'orchestrator:workflow.backlog',
  todo: 'orchestrator:workflow.todo',
  inProgress: 'orchestrator:workflow.inProgress',
  humanReview: 'orchestrator:workflow.humanReview',
  rework: 'orchestrator:workflow.rework',
  merging: 'orchestrator:workflow.merging',
  done: 'orchestrator:workflow.done',
  canceled: 'orchestrator:workflow.canceled',
};

/**
 * Business Logic（为什么需要这个映射）:
 *   任务卡片与 runtime 摘要需要 run state 文案。
 *
 * Code Logic（这个映射做什么）:
 *   OrchestratorRunState → run.* i18n key。
 */
export const RUN_STATE_LABEL_KEYS: Record<OrchestratorRunState, OrchestratorRunStateLabelKey> = {
  idle: 'orchestrator:run.idle',
  queued: 'orchestrator:run.queued',
  preparing: 'orchestrator:run.preparing',
  running: 'orchestrator:run.running',
  verifying: 'orchestrator:run.verifying',
  retrying: 'orchestrator:run.retrying',
  blocked: 'orchestrator:run.blocked',
  delivering: 'orchestrator:run.delivering',
};

/**
 * Business Logic（为什么需要这个映射）:
 *   attempt phase 文案必须走 i18n，不能直接展示后端短值。
 *
 * Code Logic（这个映射做什么）:
 *   OrchestratorAttemptPhase → attempt.* i18n key。
 */
export const ATTEMPT_PHASE_LABEL_KEYS: Record<
  OrchestratorAttemptPhase,
  OrchestratorAttemptPhaseLabelKey
> = {
  preparingWorkspace: 'orchestrator:attempt.preparingWorkspace',
  buildingPrompt: 'orchestrator:attempt.buildingPrompt',
  launchingRunner: 'orchestrator:attempt.launchingRunner',
  initializingSession: 'orchestrator:attempt.initializingSession',
  streaming: 'orchestrator:attempt.streaming',
  finishing: 'orchestrator:attempt.finishing',
  succeeded: 'orchestrator:attempt.succeeded',
  failed: 'orchestrator:attempt.failed',
  timedOut: 'orchestrator:attempt.timedOut',
  stalled: 'orchestrator:attempt.stalled',
  canceledByReconciliation: 'orchestrator:attempt.canceledByReconciliation',
};

/**
 * Business Logic（为什么需要这个函数）:
 *   API 调用失败时页面需要优先显示后端返回的可读错误，并在缺少 message 时回退到本地化通用提示。
 *
 * Code Logic（这个函数做什么）:
 *   从 unknown 错误中提取非空字符串；如果无法提取，返回调用方传入的 i18n fallback。
 */
export function displayOrchestratorErrorMessage(error: unknown, fallback: string): string {
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
export function formatTaskTimestamp(value: string): string {
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
 *   任务运行时字段经常为空，详情页需要用本地化兜底值展示缺失状态，而不是露出空白。
 *
 * Code Logic（这个函数做什么）:
 *   value 为空时返回 fallback；否则复用 formatTaskTimestamp 转换为浏览器本地短日期时间。
 */
export function formatOptionalTaskTimestamp(value: string | null, fallback: string): string {
  if (!value) return fallback;
  return formatTaskTimestamp(value);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Runner provider、Claude session 和 transcript 等字段可能未绑定，详情页需要统一的缺省显示。
 *
 * Code Logic（这个函数做什么）:
 *   去除字符串首尾空白；非空返回原值，空值或空白字符串返回调用方传入的 fallback。
 */
export function taskRuntimeValue(value: string | null | undefined, fallback: string): string {
  const normalized = value?.trim();
  return normalized || fallback;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Evidence 卡需要用一致颜色表达验证结果，帮助用户快速区分通过、失败和跳过。
 *
 * Code Logic（这个函数做什么）:
 *   将 summary 短值映射到 Pill 支持的 tone；未知值按 neutral 展示。
 */
export function evidenceSummaryTone(
  summary: string,
): 'neutral' | 'success' | 'warn' | 'danger' | 'accent' {
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
export function pendingRemoteStatusTone(
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
    case 'discarded':
      return 'neutral';
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   任务卡片需要区分 runner 空闲、运行、阻塞和交付，帮助用户快速判断任务是否可拖拽。
 *
 * Code Logic（这个函数做什么）:
 *   将 runState 映射到 Pill 支持的 tone；运行中为 accent，阻塞为 danger，交付为 success。
 */
export function runStateTone(
  state: OrchestratorRunState,
): 'neutral' | 'success' | 'accent' | 'danger' {
  switch (state) {
    case 'blocked':
      return 'danger';
    case 'delivering':
      return 'success';
    case 'queued':
    case 'preparing':
    case 'running':
    case 'verifying':
    case 'retrying':
      return 'accent';
    case 'idle':
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
export function evidenceSummaryLabelKey(summary: string): OrchestratorEvidenceSummaryLabelKey {
  return EVIDENCE_SUMMARY_LABEL_KEYS[summary] ?? 'orchestrator:evidence.summary.generic';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   任务详情需要从完整 evidence 列表中提取最新 verifier 结论和修复指令，避免用户在长列表里手动寻找。
 *
 * Code Logic（这个函数做什么）:
 *   从尾到头查找指定 kind 的第一条 evidence；未找到时返回 null，不改变原数组顺序。
 */
export function latestEvidenceByKind(
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
export function evidenceItemsByKind(
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
export function buildWorkbenchTaskUrl(task: OrchestratorTask | null): string {
  return buildWorkbenchDeepLink({
    projectId: task?.projectId ?? null,
    worktreeId: task?.worktreeId ?? null,
    sessionId: task?.sessionId ?? null,
  });
}
