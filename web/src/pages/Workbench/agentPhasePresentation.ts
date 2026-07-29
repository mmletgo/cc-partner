/**
 * Agent phase → UI presentation（纯 helper）。
 *
 * Business Logic（为什么需要这个模块）:
 *   Desktop terminal tab/status 需要低噪音展示 phase 与 provider 短标签，
 *   文案/tone 不得散落在多个叶子组件。
 *
 * Code Logic（这个模块做什么）:
 *   phase→Pill tone；phase→i18n key；providerId→短标签；可访问 aria 组合。
 */

import type { AgentPhase, AgentSessionProjection } from '@/lib/types/agentRuntime';

/** Pill tone 子集（与 primitives Pill 对齐）。 */
export type AgentPhasePillTone = 'neutral' | 'success' | 'warn' | 'danger' | 'accent';

/**
 * Business Logic（为什么需要这个函数）:
 *   needsInput/failed 必须醒目；working/idle 保持静默无动画。
 *
 * Code Logic（这个函数做什么）:
 *   phase 映射 tone；未知回落 neutral。
 */
export function agentPhaseTone(phase: AgentPhase): AgentPhasePillTone {
  switch (phase) {
    case 'launching':
      return 'accent';
    case 'working':
      return 'success';
    case 'needsInput':
      return 'warn';
    case 'idle':
      return 'neutral';
    case 'completed':
      return 'success';
    case 'failed':
      return 'danger';
    case 'disconnected':
      return 'danger';
    default:
      return 'neutral';
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   i18n key 必须字面量稳定，供 t() 与测试断言。
 *
 * Code Logic（这个函数做什么）:
 *   返回 workbench:agentPhase.* 后缀。
 */
export function agentPhaseI18nKey(
  phase: AgentPhase,
):
  | 'agentPhase.launching'
  | 'agentPhase.working'
  | 'agentPhase.needsInput'
  | 'agentPhase.idle'
  | 'agentPhase.completed'
  | 'agentPhase.failed'
  | 'agentPhase.disconnected' {
  switch (phase) {
    case 'launching':
      return 'agentPhase.launching';
    case 'working':
      return 'agentPhase.working';
    case 'needsInput':
      return 'agentPhase.needsInput';
    case 'idle':
      return 'agentPhase.idle';
    case 'completed':
      return 'agentPhase.completed';
    case 'failed':
      return 'agentPhase.failed';
    case 'disconnected':
      return 'agentPhase.disconnected';
    default:
      return 'agentPhase.idle';
  }
}

/**
 * Business Logic（为什么需要这个函数）:
 *   tab 上只显示短 provider 标签；未知 id 原样安全展示。
 *
 * Code Logic（这个函数做什么）:
 *   已知 provider 映射短名；否则截断过长 id。
 */
export function agentProviderShortLabel(providerId: string): string {
  if (providerId === 'claudeCodeVisible' || providerId === 'claudeCode') {
    return 'Claude';
  }
  if (providerId === 'codexVisible' || providerId === 'codex') {
    return 'Codex';
  }
  if (providerId === 'genericTerminal') {
    return 'Generic';
  }
  if (providerId === 'openCodeVisible' || providerId === 'opencode') {
    return 'OpenCode';
  }
  if (providerId.length > 16) {
    return `${providerId.slice(0, 14)}…`;
  }
  return providerId;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   可访问标签需包含 phase 文案，点击只聚焦 terminal。
 *
 * Code Logic（这个函数做什么）:
 *   拼接 provider 短标签 + 已翻译 phase 文本。
 */
export function agentStatusAriaLabel(
  agent: AgentSessionProjection,
  phaseLabel: string,
): string {
  return `${agentProviderShortLabel(agent.providerId)} · ${phaseLabel}`;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   cached/offline/unsupported 需显式后缀，禁止伪装 live。
 *
 * Code Logic（这个函数做什么）:
 *   返回 freshness i18n 后缀或 null（live）。
 */
export function agentFreshnessI18nKey(
  freshness: AgentSessionProjection['freshness'],
): 'agentFreshness.cached' | 'agentFreshness.offline' | 'agentFreshness.unsupported' | null {
  if (freshness === 'live') return null;
  if (freshness === 'cached') return 'agentFreshness.cached';
  if (freshness === 'offline') return 'agentFreshness.offline';
  return 'agentFreshness.unsupported';
}
