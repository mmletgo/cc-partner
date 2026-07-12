/**
 * Workbench deep link 参数。
 *
 * Business Logic（为什么需要这个类型）:
 *   Orchestrator 任务、Attention Inbox 与后续移动端入口都需要用统一结构定位
 *   项目现场或自动化控制台内的任务/outbox，不能各端拼装不同 query 规则。
 *
 * Code Logic（字段说明）:
 *   projectId/worktreeId/sessionId 定位执行现场；view=automation 打开自动化控制台；
 *   taskId/outboxId 在数据加载后聚焦详情/outbox，不直接打开终端。
 *   automation 相关字段可选，省略时按 null 处理以兼容旧调用方。
 */
export interface WorkbenchDeepLink {
  projectId: string | null;
  worktreeId: string | null;
  sessionId: string | null;
  view?: 'automation' | null;
  taskId?: string | null;
  outboxId?: string | null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Orchestrator、Attention 与 Workbench 内部导航需要同一套 query 规则定位项目现场，
 *   尤其远端 project/worktree/session id 必须保留 `remote:<device>:...` 前缀。
 *
 * Code Logic（这个函数做什么）:
 *   接收可选 deep link 字段，使用 URLSearchParams 统一编码非空值并返回 `/workbench` URL。
 */
export function buildWorkbenchDeepLink(target: WorkbenchDeepLink): string {
  const params = new URLSearchParams();
  if (target.projectId?.trim()) params.set('projectId', target.projectId.trim());
  if (target.worktreeId?.trim()) params.set('worktreeId', target.worktreeId.trim());
  if (target.sessionId?.trim()) params.set('sessionId', target.sessionId.trim());
  if (target.view === 'automation') params.set('view', 'automation');
  if (target.taskId?.trim()) params.set('taskId', target.taskId.trim());
  if (target.outboxId?.trim()) params.set('outboxId', target.outboxId.trim());
  const query = params.toString();
  return query ? `/workbench?${query}` : '/workbench';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用户从 Orchestrator/Attention 打开 Workbench 时，应自动定位到关联项目、
 *   可选 worktree/session，或分阶段打开 automation 视图并聚焦 task/outbox。
 *
 * Code Logic（这个函数做什么）:
 *   解析 location.search；缺失、空字符串或纯空白值统一返回 null；
 *   仅识别 view=automation，其他 view 值视为 null。
 */
export function parseWorkbenchDeepLink(search: string): WorkbenchDeepLink {
  const normalized =
    search === '' || search.startsWith('?')
      ? search
      : search.startsWith('/workbench')
        ? search.includes('?')
          ? search.slice(search.indexOf('?'))
          : ''
        : `?${search}`;
  const params = new URLSearchParams(normalized);
  const projectId = params.get('projectId')?.trim() || null;
  const worktreeId = params.get('worktreeId')?.trim() || null;
  const sessionId = params.get('sessionId')?.trim() || null;
  const rawView = params.get('view')?.trim() || null;
  const view = rawView === 'automation' ? 'automation' : null;
  const taskId = params.get('taskId')?.trim() || null;
  const outboxId = params.get('outboxId')?.trim() || null;

  return { projectId, worktreeId, sessionId, view, taskId, outboxId };
}
