/**
 * 工作台窗口标题同步。
 *
 * Business Logic（为什么需要这个模块）:
 *   多屏时 Dock / 任务栏需要看出每扇窗打开的是哪个项目。
 *
 * Code Logic（这个模块做什么）:
 *   按项目名拼 `{name} — cc-partner`；无项目回落默认标题；失败静默。
 */

export const DEFAULT_WORKBENCH_WINDOW_TITLE = 'cc-partner';

/**
 * Business Logic（为什么需要这个函数）:
 *   标题只应含项目名，不得夹路径或 token。
 *
 * Code Logic（这个函数做什么）:
 *   trim 后空则默认标题。
 */
export function formatWorkbenchWindowTitle(projectName: string | null | undefined): string {
  const name = projectName?.trim() ?? '';
  return name ? `${name} — ${DEFAULT_WORKBENCH_WINDOW_TITLE}` : DEFAULT_WORKBENCH_WINDOW_TITLE;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   本窗切项目后 OS 标题必须跟上；浏览器调试环境没有 setTitle。
 *
 * Code Logic（这个函数做什么）:
 *   调用 `setTitle`，失败吞掉。
 */
export async function syncWorkbenchWindowTitle(
  setTitle: (title: string) => Promise<unknown>,
  projectName: string | null | undefined,
): Promise<void> {
  try {
    await setTitle(formatWorkbenchWindowTitle(projectName));
  } catch {
    // 无 Tauri / 权限不足时保持系统默认标题。
  }
}
