/**
 * 用户级镜像 Pull/Push 纯展示与门闩 helper。
 *
 * Business Logic（为什么需要这个模块）:
 *   对话框必须按 Agent 展示写/替换/删除/停用计数与凭据披露，
 *   且 apply 只能在 preview + 破坏性确认之后；换设备必须作废旧 plan。
 *
 * Code Logic（这个模块做什么）:
 *   汇总 plan 计数、构造 preview 请求、计算 canApply/reconcile/tone；无 React、无 API。
 */

import {
  USER_MIRROR_PREVIEW_REQUIRED,
  USER_MIRROR_STALE,
  type PreviewUserMirrorRequest,
  type UserMirrorAgentPlanDto,
  type UserMirrorDirection,
  type UserMirrorItemState,
  type UserMirrorPlanDto,
  type UserMirrorResultDto,
} from '@/lib/types/userMirror';
import type { AgentTarget } from '@/lib/types/agentHub';

/** 单个 Agent 在预览中的变更计数。 */
export interface UserMirrorAgentSummary {
  target: AgentTarget;
  writes: number;
  upserts: number;
  deletes: number;
  disables: number;
  credentialBearing: boolean;
}

/**
 * Business Logic: 用户按 Agent 看到将写入、新增/替换、删除、停用的数量，而不是笼统「同步」。
 * Code Logic: writes=指令文件；upserts=portable 新增/替换；deletes=portable+MCP；disables=Plugin。
 */
export function summarizeAgentPlan(agent: UserMirrorAgentPlanDto): UserMirrorAgentSummary {
  return {
    target: agent.target,
    writes: agent.instructionWrites.length,
    upserts: agent.portableUpserts.length,
    deletes: agent.portableDeletes.length + agent.mcpDeletes.length,
    disables: agent.pluginDisables.length,
    credentialBearing:
      agent.portableUpserts.some((change) => change.credentialBearing) ||
      agent.portableDeletes.some((change) => change.credentialBearing) ||
      agent.pluginDisables.some((change) => change.credentialBearing) ||
      agent.mcpDeletes.some((change) => change.credentialBearing),
  };
}

/**
 * Business Logic: 预览区按 Agent 分组列出计数。
 * Code Logic: plan 为空则空数组。
 */
export function summarizePlanAgents(plan: UserMirrorPlanDto | null): UserMirrorAgentSummary[] {
  if (!plan) return [];
  return plan.agents.map(summarizeAgentPlan);
}

/**
 * Business Logic: apply 必须同时具备绑定 plan、破坏性确认、非忙碌、非 stale。
 * Code Logic: `Boolean(plan) && confirmed && !busy && !stale`。
 */
export function canApplyUserMirror(input: {
  plan: UserMirrorPlanDto | null;
  confirmed: boolean;
  busy: boolean;
  stale: boolean;
}): boolean {
  return Boolean(input.plan) && input.confirmed && !input.busy && !input.stale;
}

/**
 * Business Logic: partial / unknown 不得标全成功，必须提供核对。
 * Code Logic: result.partial 或任一 Agent failed/outcomeUnknown。
 */
export function needsUserMirrorReconcile(result: UserMirrorResultDto | null): boolean {
  if (!result) return false;
  if (result.partial) return true;
  return result.agents.some((agent) => agent.state === 'outcomeUnknown' || agent.state === 'failed');
}

/**
 * Business Logic: 结果行 tone 不得把 unknown 压成成功。
 * Code Logic: succeeded=success；skipped=neutral；unknown=warn；failed=danger。
 */
export function userMirrorItemStateTone(
  state: UserMirrorItemState,
): 'success' | 'warn' | 'danger' | 'neutral' {
  switch (state) {
    case 'succeeded':
      return 'success';
    case 'skipped':
      return 'neutral';
    case 'outcomeUnknown':
      return 'warn';
    case 'failed':
      return 'danger';
  }
}

/**
 * Business Logic: Pull 只带源设备；Push 只带对端列表；无条目/mode/冲突策略。
 * Code Logic: 缺设备返回 null；Pull 带空 peerDeviceIds 以满足类型。
 */
export function buildPreviewRequest(
  direction: UserMirrorDirection,
  sourceDeviceId: string,
  selectedPeerIds: readonly string[],
): PreviewUserMirrorRequest | null {
  if (direction === 'pull') {
    if (!sourceDeviceId) return null;
    return {
      direction: 'pull',
      sourceDeviceId,
      peerDeviceIds: [],
    };
  }
  if (selectedPeerIds.length === 0) return null;
  return {
    direction: 'push',
    peerDeviceIds: [...selectedPeerIds],
  };
}

/**
 * Business Logic: TTL 到期的 plan 不得再 apply。
 * Code Logic: 解析 expiresAt；无效时间戳视为未过期（交给后端 STALE）。
 */
export function isUserMirrorPlanExpired(
  plan: UserMirrorPlanDto | null,
  nowMs: number = Date.now(),
): boolean {
  if (!plan) return false;
  const expires = Date.parse(plan.expiresAt);
  return Number.isFinite(expires) && expires <= nowMs;
}

/**
 * Business Logic: 稳定 code 优先展示，便于对照协议表。
 * Code Logic: Error.code 前缀；对象 code/error/message 回退。
 */
export function formatUserMirrorError(reason: unknown): string {
  if (!reason) return 'unknown_error';
  if (reason instanceof Error) {
    const code = (reason as { code?: unknown }).code;
    if (typeof code === 'string' && code.length > 0) {
      return `${code}: ${reason.message}`;
    }
    return reason.message || 'unknown_error';
  }
  if (typeof reason === 'object') {
    const obj = reason as { code?: unknown; error?: unknown; message?: unknown };
    if (typeof obj.code === 'string') {
      const msg =
        typeof obj.error === 'string'
          ? obj.error
          : typeof obj.message === 'string'
            ? obj.message
            : '';
      return msg ? `${obj.code}: ${msg}` : obj.code;
    }
    if (typeof obj.error === 'string') return obj.error;
    if (typeof obj.message === 'string') return obj.message;
  }
  return String(reason);
}

/**
 * Business Logic: 源/目标漂移必须清 plan 并禁止 apply。
 * Code Logic: 识别 USER_MIRROR_STALE（Error.code 或对象 code）。
 */
export function isUserMirrorStaleError(reason: unknown): boolean {
  if (!reason || typeof reason !== 'object') return false;
  const code = (reason as { code?: unknown }).code;
  return code === USER_MIRROR_STALE;
}

export { USER_MIRROR_PREVIEW_REQUIRED, USER_MIRROR_STALE };
