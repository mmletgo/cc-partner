/**
 * Agent hint 展示文案 helper。
 *
 * Business Logic（为什么需要这个模块）:
 *   rail / worktree / window 三点必须共用同一套 key 选择，避免各写一套分支。
 *
 * Code Logic（这个模块做什么）:
 *   按 waiting/completed 返回 i18n 后缀与插值。
 */

import type { AgentHintCounts } from '@/lib/workbenchAgentHints';
import { hintAriaKind } from '@/lib/workbenchAgentHints';

export type AgentHintAriaKey =
  | 'agentHints.dotAriaWaiting'
  | 'agentHints.dotAriaCompleted'
  | 'agentHints.dotAriaBoth';

export interface AgentHintAriaSpec {
  key: AgentHintAriaKey;
  values: { count?: number; waiting?: number; completed?: number };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   叶子组件用自己的 t() 翻译，避免 helper 绑死 TFunction 命名空间。
 *
 * Code Logic（这个函数做什么）:
 *   0 返回 null；否则返回 key + 插值。
 */
export function agentHintAriaSpec(counts: AgentHintCounts): AgentHintAriaSpec | null {
  const kind = hintAriaKind(counts);
  if (!kind) return null;
  if (kind === 'both') {
    return {
      key: 'agentHints.dotAriaBoth',
      values: { waiting: counts.waitingCount, completed: counts.completedCount },
    };
  }
  if (kind === 'waiting') {
    return { key: 'agentHints.dotAriaWaiting', values: { count: counts.waitingCount } };
  }
  return { key: 'agentHints.dotAriaCompleted', values: { count: counts.completedCount } };
}
