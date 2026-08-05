/**
 * 用户级指令 V2 pure presentation helpers。
 *
 * Business Logic（为什么需要这个模块）:
 *   页面必须从合法 DTO 映射到一个主状态与一个能力说明，不能重现 legacy 多 pill 矛盾组合。
 *
 * Code Logic（这个模块做什么）:
 *   集中计算摘要、主状态、有效来源和待处理数量；不含 React、API 或 i18n 实例。
 */

import type {
  UserInstructionSourceDto,
  UserInstructionTargetDto,
  UserInstructionWorkspaceDto,
} from '@/lib/types/agentHub';

export interface UserInstructionSummaryPresentation {
  key:
    | 'empty'
    | 'readyToReview'
    | 'configuredHealthy'
    | 'configuredActionRequired'
    | 'configuredBlocked';
  managedCount: number;
  actionCount: number;
}

export interface UserInstructionTargetPresentation {
  stateKey:
    | 'unmanaged'
    | 'external'
    | 'fallback'
    | 'fallbackDisabled'
    | 'paused'
    | 'pending'
    | 'inSync'
    | 'drift'
    | 'detached'
    | 'conflict'
    | 'collision'
    | 'activationRequired'
    | 'failed'
    | 'blocked';
  tone: 'success' | 'neutral' | 'warn' | 'danger';
  activeSource: UserInstructionSourceDto | null;
  shadowedSources: UserInstructionSourceDto[];
  capabilityKey: 'automatic' | 'scanOnly' | 'scanBlocked';
}

/**
 * Business Logic（为什么需要）:
 *   用户未选择的 target 是中性 unmanaged，不计入“待处理”。
 *
 * Code Logic（做什么）:
 *   只统计 managedActive 且非 inSync 的 target；managedPaused/unmanaged 均忽略。
 */
export function getUserInstructionSummaryPresentation(
  workspace: UserInstructionWorkspaceDto,
): UserInstructionSummaryPresentation {
  const managed = workspace.targets.filter((target) => target.managementMode !== 'unmanaged');
  const actionCount = workspace.targets.filter(
    (target) =>
      target.managementMode === 'managedActive' && target.projection.state !== 'inSync',
  ).length;
  if (!workspace.canonical && workspace.setupState === 'unconfigured') {
    return { key: 'empty', managedCount: 0, actionCount: 0 };
  }
  if (managed.length === 0) {
    return { key: 'readyToReview', managedCount: 0, actionCount: 0 };
  }
  if (workspace.healthState === 'blocked') {
    return { key: 'configuredBlocked', managedCount: managed.length, actionCount };
  }
  if (actionCount > 0 || workspace.healthState === 'actionRequired') {
    return { key: 'configuredActionRequired', managedCount: managed.length, actionCount };
  }
  return { key: 'configuredHealthy', managedCount: managed.length, actionCount: 0 };
}

/**
 * Business Logic（为什么需要）:
 *   Codex override/OpenCode fallback 等优先级必须以 adapter 的 active 标记为准。
 *
 * Code Logic（做什么）:
 *   优先 effectiveSourceId，再取 active=true；无法证明时返回 null，不猜测。
 */
export function getEffectiveUserInstructionSource(
  target: UserInstructionTargetDto,
): UserInstructionSourceDto | null {
  if (target.effectiveSourceId) {
    return target.sources.find((source) => source.sourceId === target.effectiveSourceId) ?? null;
  }
  return target.sources.find((source) => source.active) ?? null;
}

/**
 * Business Logic（为什么需要）:
 *   每个 target card 只能有一个人类可理解主状态和一个能力说明。
 *
 * Code Logic（做什么）:
 *   projection 异常优先；其后按 management mode 与 active source role 归一化。
 */
export function getUserInstructionTargetPresentation(
  target: UserInstructionTargetDto,
): UserInstructionTargetPresentation {
  const activeSource = getEffectiveUserInstructionSource(target);
  const shadowedSources = target.sources.filter((source) => source.role === 'shadowed');
  const capabilityKey =
    target.capability.scan === 'blocked'
      ? 'scanBlocked'
      : target.capability.write === 'supported'
        ? 'automatic'
        : 'scanOnly';

  if (target.managementMode === 'unmanaged') {
    if (
      target.target === 'opencode' &&
      !activeSource &&
      target.capability.reasonCode?.includes('FALLBACK_DISABLED')
    ) {
      return {
        stateKey: 'fallbackDisabled',
        tone: 'neutral',
        activeSource,
        shadowedSources,
        capabilityKey,
      };
    }
    return {
      stateKey: activeSource?.role === 'fallback' ? 'fallback' : activeSource ? 'external' : 'unmanaged',
      tone: 'neutral',
      activeSource,
      shadowedSources,
      capabilityKey,
    };
  }
  if (target.managementMode === 'managedPaused') {
    return { stateKey: 'paused', tone: 'neutral', activeSource, shadowedSources, capabilityKey };
  }
  const state = target.projection.state;
  if (state === 'inSync') {
    return { stateKey: 'inSync', tone: 'success', activeSource, shadowedSources, capabilityKey };
  }
  if (state === 'pending') {
    return { stateKey: 'pending', tone: 'neutral', activeSource, shadowedSources, capabilityKey };
  }
  if (state === 'drift') {
    return { stateKey: 'drift', tone: 'warn', activeSource, shadowedSources, capabilityKey };
  }
  if (state === 'detached') {
    return { stateKey: 'detached', tone: 'warn', activeSource, shadowedSources, capabilityKey };
  }
  if (state === 'conflict') {
    return { stateKey: 'conflict', tone: 'danger', activeSource, shadowedSources, capabilityKey };
  }
  if (state === 'collision') {
    return { stateKey: 'collision', tone: 'warn', activeSource, shadowedSources, capabilityKey };
  }
  if (state === 'activationRequired') {
    return {
      stateKey: 'activationRequired',
      tone: 'warn',
      activeSource,
      shadowedSources,
      capabilityKey,
    };
  }
  if (state === 'failed') {
    return { stateKey: 'failed', tone: 'danger', activeSource, shadowedSources, capabilityKey };
  }
  return { stateKey: 'blocked', tone: 'warn', activeSource, shadowedSources, capabilityKey };
}
