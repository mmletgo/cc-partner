/**
 * Workbench deep link 参数。
 *
 * Business Logic（为什么需要这个类型）:
 *   Orchestrator 任务可能绑定项目、worktree 和终端窗口，Workbench 入口需要用统一结构表达这些可选目标。
 *
 * Code Logic（字段说明）:
 *   每个字段都是 query string 中对应 id 的归一化结果；缺失或空字符串统一为 null。
 */
export interface WorkbenchDeepLink {
  projectId: string | null;
  worktreeId: string | null;
  sessionId: string | null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Orchestrator、Workbench 内部导航和后续移动端入口都需要用同一套 query 规则定位项目现场，
 *   尤其远端项目/worktree/session id 必须保留 `remote:<device>:...` 前缀。
 *
 * Code Logic（这个函数做什么）:
 *   接收可选 projectId/worktreeId/sessionId，使用 URLSearchParams 统一编码非空值并返回 `/workbench` URL。
 */
export function buildWorkbenchDeepLink(target: WorkbenchDeepLink): string {
  const params = new URLSearchParams();
  if (target.projectId?.trim()) params.set('projectId', target.projectId.trim());
  if (target.worktreeId?.trim()) params.set('worktreeId', target.worktreeId.trim());
  if (target.sessionId?.trim()) params.set('sessionId', target.sessionId.trim());
  const query = params.toString();
  return query ? `/workbench?${query}` : '/workbench';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用户从 Orchestrator 打开 Workbench 时，应自动定位到任务关联的项目、worktree 和终端窗口。
 *
 * Code Logic（这个函数做什么）:
 *   解析 location.search 中的 projectId/worktreeId/sessionId；缺失、空字符串或纯空白值统一返回 null。
 */
export function parseWorkbenchDeepLink(search: string): WorkbenchDeepLink {
  const params = new URLSearchParams(search);
  const projectId = params.get('projectId')?.trim() || null;
  const worktreeId = params.get('worktreeId')?.trim() || null;
  const sessionId = params.get('sessionId')?.trim() || null;

  return { projectId, worktreeId, sessionId };
}
