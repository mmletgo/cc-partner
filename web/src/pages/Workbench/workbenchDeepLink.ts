/**
 * Workbench deep link 参数。
 *
 * Business Logic（为什么需要这个类型）:
 *   Orchestrator 任务、Attention Inbox 与 WORKFLOW 向导都需要用统一结构定位
 *   项目现场、自动化控制台或文件工作区路径，不能各端拼装不同 query 规则。
 *
 * Code Logic（字段说明）:
 *   projectId/worktreeId/sessionId 定位执行现场；view=automation 打开自动化控制台；
 *   view=files + path 打开文件工作区相对路径；taskId/outboxId 聚焦详情/outbox。
 *   可选字段省略时按 null 处理以兼容旧调用方。
 */
export interface WorkbenchDeepLink {
  projectId: string | null;
  worktreeId: string | null;
  sessionId: string | null;
  view?: 'automation' | 'files' | null;
  taskId?: string | null;
  outboxId?: string | null;
  path?: string | null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   files deep link 与 openFileByPath 只能打开 worktree 内相对路径，必须拒绝绝对路径与目录穿越。
 *
 * Code Logic（这个函数做什么）:
 *   trim 后拒绝空串、绝对路径、Windows 盘符、.. 段与 NUL。
 */
export function isSafeWorkbenchRelativePath(path: string): boolean {
  const trimmed = path.trim();
  if (!trimmed) return false;
  if (trimmed.includes('\0')) return false;
  if (trimmed.startsWith('/') || trimmed.startsWith('\\')) return false;
  if (/^[A-Za-z]:[\\/]/.test(trimmed)) return false;
  const segments = trimmed.split(/[/\\]+/);
  if (segments.some((segment) => segment === '..')) return false;
  return true;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Orchestrator、Attention 与 Workbench 内部导航需要同一套 query 规则定位项目现场，
 *   尤其远端 project/worktree/session id 必须保留 `remote:<device>:...` 前缀。
 *
 * Code Logic（这个函数做什么）:
 *   接收可选 deep link 字段，使用 URLSearchParams 统一编码非空值并返回 `/workbench` URL。
 *   path 仅在通过相对路径安全校验后写入。
 */
export function buildWorkbenchDeepLink(target: WorkbenchDeepLink): string {
  const params = new URLSearchParams();
  if (target.projectId?.trim()) params.set('projectId', target.projectId.trim());
  if (target.worktreeId?.trim()) params.set('worktreeId', target.worktreeId.trim());
  if (target.sessionId?.trim()) params.set('sessionId', target.sessionId.trim());
  if (target.view === 'automation' || target.view === 'files') {
    params.set('view', target.view);
  }
  if (target.taskId?.trim()) params.set('taskId', target.taskId.trim());
  if (target.outboxId?.trim()) params.set('outboxId', target.outboxId.trim());
  const path = target.path?.trim() ?? '';
  if (path && isSafeWorkbenchRelativePath(path)) {
    params.set('path', path);
  }
  const query = params.toString();
  return query ? `/workbench?${query}` : '/workbench';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用户从 Orchestrator/Attention 打开 Workbench 时，应自动定位到关联项目、
 *   可选 worktree/session、automation 视图或 files 相对路径。
 *
 * Code Logic（这个函数做什么）:
 *   解析 location.search；缺失、空字符串或纯空白值统一返回 null；
 *   仅识别 view=automation|files；path 未通过安全校验时视为 null。
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
  const view = rawView === 'automation' || rawView === 'files' ? rawView : null;
  const taskId = params.get('taskId')?.trim() || null;
  const outboxId = params.get('outboxId')?.trim() || null;
  const rawPath = params.get('path')?.trim() || null;
  const path = rawPath && isSafeWorkbenchRelativePath(rawPath) ? rawPath : null;

  return { projectId, worktreeId, sessionId, view, taskId, outboxId, path };
}
