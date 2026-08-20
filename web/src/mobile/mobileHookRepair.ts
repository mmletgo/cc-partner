/**
 * 移动端 Git 钩子失败修复的纯展示/状态类型。
 *
 * Business Logic（为什么需要这个模块）:
 *   终端 FAB 与 Git 面板都要展示 failedHook 的「让 AI 修复 / 重试 / 忽略」，文案与桌面 Git 历史对齐。
 *
 * Code Logic（这个模块做什么）:
 *   导出 hookRepair 状态形状与钩子输出拼接，不含网络副作用。
 */

import type { WorkbenchHookFailure } from '@/lib/types';

export type MobileHookRepair = {
  kind: 'commit' | 'push';
  hookFailure: WorkbenchHookFailure;
  clientOperationId: string;
  terminalSessionId?: string;
};

/**
 * Business Logic（为什么需要这个函数）:
 *   钩子 stdout/stderr 可能只有一端有内容，UI 需要稳定拼接后给用户展开查看。
 *
 * Code Logic（这个函数做什么）:
 *   两端都有则换行拼接；只保留非空端；都空返回空串，由 UI 填 i18n 占位。
 */
export function formatMobileHookRepairOutput(hookFailure: {
  stdout: string;
  stderr: string;
}): string {
  const stdout = hookFailure.stdout.trim();
  const stderr = hookFailure.stderr.trim();
  if (stdout && stderr) return `${stdout}\n${stderr}`;
  return stdout || stderr;
}
