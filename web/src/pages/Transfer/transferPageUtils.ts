/**
 * Transfer 页面纯工具（从 Transfer.tsx 拆出以压 soft line budget）。
 */

import { ContractDecodeError } from '@/lib/runtimeSchema';

interface TauriInternalsWindow extends Window {
  __TAURI_INTERNALS__?: {
    transformCallback?: unknown;
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   普通浏览器调试环境没有 Tauri event internals，页面不得注册不可用的桌面事件。
 *
 * Code Logic（这个函数做什么）:
 *   检测 window.__TAURI_INTERNALS__.transformCallback 是否为函数。
 */
export function canListenToTauriEvents(): boolean {
  const internals = (window as TauriInternalsWindow).__TAURI_INTERNALS__;
  return typeof internals?.transformCallback === 'function';
}

/**
 * Business Logic（为什么需要这个函数）:
 *   解码失败只允许日志暴露 contract/path，禁止打印 payload。
 *
 * Code Logic（这个函数做什么）:
 *   ContractDecodeError 输出 contract + path；其它错误仅输出安全 message。
 */
export function warnTransferEventDecodeFailure(eventName: string, reason: unknown): void {
  if (reason instanceof ContractDecodeError) {
    console.warn(
      `[transfer] skip ${eventName}: contract=${reason.contract} path=${reason.path}`,
    );
    return;
  }
  const message = reason instanceof Error ? reason.message : String(reason);
  console.warn(`[transfer] skip ${eventName}: ${message}`);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   UI 只能展示 basename，不得暴露完整绝对路径。
 *
 * Code Logic（这个函数做什么）:
 *   按最后一次 / 或 \\ 切分路径，得到文件名；路径本身不做改写。
 */
export function basenameFromPath(path: string): string {
  const normalized = path.replace(/\\/g, '/');
  const parts = normalized.split('/');
  const last = parts[parts.length - 1] ?? '';
  return last.length > 0 ? last : path;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   错误对象可能是 Error 或未知 reject，需要稳定可读文案。
 *
 * Code Logic（这个函数做什么）:
 *   Error 取 message，其余 String()。
 */
export function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
