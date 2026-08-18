import type { WorkbenchDependencyStatus } from './types';

export type WorkbenchDependencyTone = 'success' | 'warning' | 'danger' | 'neutral';

/**
 * Business Logic（为什么需要这个函数）:
 *   Workbench 和 Settings 都需要用一致的视觉语义展示 tmux 依赖状态。
 *
 * Code Logic（这个函数做什么）:
 *   将后端状态映射为 UI tone，供 Pill/Card 样式复用。
 */
export function dependencyStatusTone(status: WorkbenchDependencyStatus): WorkbenchDependencyTone {
  if (status.status === 'ready') return 'success';
  if (status.status === 'missing' || status.status === 'unsupported') return 'warning';
  if (status.status === 'failed') return 'danger';
  return 'neutral';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   缺少 tmux 时用户应能主动安装，但安装中或不可安装平台不应重复触发安装。
 *
 * Code Logic（这个函数做什么）:
 *   根据状态、installable 和 available 判断是否展示可点击安装动作。
 */
export function canInstallWorkbenchDependency(status: WorkbenchDependencyStatus): boolean {
  return !status.available && status.installable && status.status === 'missing';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用户完成手动安装、安装失败或安装命令结束后，需要能重新检测 tmux。
 *
 * Code Logic（这个函数做什么）:
 *   排除 checking/installing 两个进行中状态，其余状态允许 recheck。
 */
export function canRecheckWorkbenchDependency(status: WorkbenchDependencyStatus): boolean {
  return status.status !== 'checking' && status.status !== 'installing';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   终端工作台只应在 tmux 真正缺失或安装失败时占位；就绪成功页属于 Settings。
 *   对端后端版本缺自动安装能力时，不能把「无法探测」误报成「需要安装 tmux」挡住终端。
 *
 * Code Logic（这个函数做什么）:
 *   ready / checking / unsupported 返回 false；missing / installing / 失败 / 待重检返回 true。
 */
export function shouldShowWorkbenchDependencyNotice(
  status: WorkbenchDependencyStatus['status'],
): boolean {
  return status !== 'ready' && status !== 'checking' && status !== 'unsupported';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   安装确认框需要展示即将执行的命令，带空格的参数必须可读且不误导用户。
 *
 * Code Logic（这个函数做什么）:
 *   将 argv 格式化为 shell-like 预览；包含空白的参数使用双引号包裹。
 */
/**
 * Business Logic（为什么需要这个函数）:
 *   缺 capability 时卡片必须显示 unsupported，不能把错误当成安装成功。
 *
 * Code Logic（这个函数做什么）:
 *   含 capability_unsupported 的错误 → unsupported DTO；其它 → failed。
 */
export function dependencyStatusFromError(error: unknown): WorkbenchDependencyStatus {
  const message = error instanceof Error ? error.message : String(error);
  const unsupported = message.includes('capability_unsupported');
  return {
    status: unsupported ? 'unsupported' : 'failed',
    available: false,
    version: null,
    backend: '',
    path: null,
    installable: false,
    installCommandPreview: [],
    error: message,
    output: [],
    statusChangedAt: '',
  };
}

export function formatInstallCommandPreview(command: string[]): string {
  return command
    .map((part) => (/\s/.test(part) ? `"${part.replaceAll('"', '\\"')}"` : part))
    .join(' ');
}
