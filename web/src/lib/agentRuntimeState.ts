/**
 * Agent Runtime 纯前端 reducer。
 *
 * Business Logic（为什么需要这个模块）:
 *   Desktop/Mobile 必须用同一套 immutable 规则合并 snapshot 与 live 事件，
 *   防止旧 version 覆盖新状态、owner 切换串写，且不读 Orchestrator 旧 Claude 字段。
 *
 * Code Logic（这个模块做什么）:
 *   提供 empty state、DTO→projection 映射、snapshot 全量替换、event 条件合并、
 *   terminal 最新 agent selector。
 */

import type {
  AgentFreshness,
  AgentRuntimeEvent,
  AgentRuntimeSnapshot,
  AgentRuntimeState,
  AgentSessionProjection,
  AgentSessionRuntimeDto,
} from './types/agentRuntime';

/**
 * Business Logic（为什么需要这个函数）:
 *   未握手前与 owner 切换后需要确定性空态，避免残留旧 Map。
 *
 * Code Logic（这个函数做什么）:
 *   返回 owner=null、asOfSequence=0、空 Map 的新对象。
 */
export function emptyAgentRuntimeState(): AgentRuntimeState {
  return {
    ownerInstanceId: null,
    asOfSequence: 0,
    byAgentId: new Map(),
    latestAgentIdByTerminal: new Map(),
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   A1 DTO 含 orchestratorTaskId 等边界字段；UI projection 用 taskId + freshness。
 *
 * Code Logic（这个函数做什么）:
 *   映射必填字段；worktree/task 在 null/undefined 时省略；默认 freshness=live。
 *
 * @param dto A1 session DTO
 * @param freshness 投影新鲜度（snapshot live / 缓存回退等）
 */
export function toAgentSessionProjection(
  dto: AgentSessionRuntimeDto,
  freshness: AgentFreshness = 'live',
): AgentSessionProjection {
  const projection: AgentSessionProjection = {
    id: dto.id,
    projectId: dto.projectId,
    terminalSessionId: dto.terminalSessionId,
    providerId: dto.providerId,
    phase: dto.phase,
    version: dto.version,
    lastActivityAt: dto.lastActivityAt,
    freshness,
    isActive: dto.isActive,
    startedAt: dto.startedAt,
  };
  if (dto.worktreeId != null && dto.worktreeId !== '') {
    projection.worktreeId = dto.worktreeId;
  }
  if (dto.orchestratorTaskId != null && dto.orchestratorTaskId !== '') {
    projection.taskId = dto.orchestratorTaskId;
  }
  if (dto.outcomeCode !== undefined) {
    projection.outcomeCode = dto.outcomeCode;
  }
  if (dto.endedAt !== undefined) {
    projection.endedAt = dto.endedAt;
  }
  if (dto.usage != null) {
    projection.usage = dto.usage;
  }
  return projection;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   从全量 Map 重建 terminal → 当前/最新 agent 索引；version 只在同一个 agent 内单调，
 *   不能拿旧 Agent 的高 version 压住新 Agent 的 version=1。
 *
 * Code Logic（这个函数做什么）:
 *   遍历 byAgentId：active 优先；active 相同时取 lastActivityAt 更新者；再以 id 稳定打破平局。
 */
function rebuildLatestByTerminal(
  byAgentId: ReadonlyMap<string, AgentSessionProjection>,
): Map<string, string> {
  const latest = new Map<string, string>();
  for (const agent of byAgentId.values()) {
    const terminalId = agent.terminalSessionId;
    const prevId = latest.get(terminalId);
    const previous = prevId ? byAgentId.get(prevId) : undefined;
    const agentActivityAt = Date.parse(agent.lastActivityAt);
    const previousActivityAt = previous ? Date.parse(previous.lastActivityAt) : Number.NaN;
    const agentActivityOrder = Number.isFinite(agentActivityAt)
      ? agentActivityAt
      : Number.NEGATIVE_INFINITY;
    const previousActivityOrder = Number.isFinite(previousActivityAt)
      ? previousActivityAt
      : Number.NEGATIVE_INFINITY;
    if (
      !previous ||
      (agent.isActive && !previous.isActive) ||
      (agent.isActive === previous.isActive && agentActivityOrder > previousActivityOrder) ||
      (agent.isActive === previous.isActive &&
        agentActivityOrder === previousActivityOrder &&
        agent.id > previous.id)
    ) {
      latest.set(terminalId, agent.id);
    }
  }
  return latest;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   Gap/handshake 成功后应用 owner baseline，必须整表替换而非 merge 旧 owner。
 *
 * Code Logic（这个函数做什么）:
 *   用 snapshot.sessions 映射 projection；设置 owner/asOfSequence；重建 terminal 索引。
 *   若传入 freshness 则统一标记（cached/offline 回退）。
 *
 * @param snapshot A1 snapshot
 * @param freshness 可选统一新鲜度，默认 live
 */
export function applyAgentRuntimeSnapshot(
  snapshot: AgentRuntimeSnapshot,
  freshness: AgentFreshness = 'live',
): AgentRuntimeState {
  const byAgentId = new Map<string, AgentSessionProjection>();
  for (const session of snapshot.sessions) {
    byAgentId.set(session.id, toAgentSessionProjection(session, freshness));
  }
  return {
    ownerInstanceId: snapshot.ownerInstanceId,
    asOfSequence: snapshot.asOfSequence,
    byAgentId,
    latestAgentIdByTerminal: rebuildLatestByTerminal(byAgentId),
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   判断入站 event 的 agent 是否可覆盖当前行（同 id 时 version 必须严格更大；
 *   owner 不一致时由调用方先处理；本函数只比 version）。
 *
 * Code Logic（这个函数做什么）:
 *   无现有 → true；incoming.version > existing.version → true；否则 false。
 */
export function shouldAcceptAgentVersion(
  existing: AgentSessionProjection | undefined,
  incomingVersion: number,
): boolean {
  if (!existing) return true;
  return incomingVersion > existing.version;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   live usage 刷新不得抬 CAS version，否则会打乱 OSC expectedVersion；
 *   同 version 时仍要接受更新的 extractedAt / tokens。
 *
 * Code Logic（这个函数做什么）:
 *   version 更大 → true；更小 → false；相等则 incoming usage 更新才 true。
 */
export function shouldAcceptAgentRuntimeUpdate(
  existing: AgentSessionProjection | undefined,
  incoming: AgentSessionRuntimeDto,
): boolean {
  if (!existing) return true;
  if (incoming.version > existing.version) return true;
  if (incoming.version < existing.version) return false;
  return isNewerLiveUsage(incoming.usage, existing.usage);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   同 version 的 usage 事件只在抽取时间更新或 tokens 变化时覆盖，避免回退。
 *
 * Code Logic（这个函数做什么）:
 *   无 incoming → false；无 existing → true；extractedAt 可解析时比时间，否则比 fingerprint。
 */
function isNewerLiveUsage(
  incoming: AgentSessionRuntimeDto['usage'],
  existing: AgentSessionProjection['usage'],
): boolean {
  if (incoming == null) return false;
  if (existing == null) return true;
  const incomingAt = Date.parse(incoming.extractedAt);
  const existingAt = Date.parse(existing.extractedAt);
  if (Number.isFinite(incomingAt) && Number.isFinite(existingAt) && incomingAt !== existingAt) {
    return incomingAt > existingAt;
  }
  return liveUsageFingerprint(incoming) !== liveUsageFingerprint(existing);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   extractedAt 相同时仍要用 tokens/model 判断是否值得替换。
 *
 * Code Logic（这个函数做什么）:
 *   拼稳定字符串指纹。
 */
function liveUsageFingerprint(
  usage: NonNullable<AgentSessionRuntimeDto['usage']>,
): string {
  return [
    usage.modelId ?? '',
    usage.inputTokens ?? '',
    usage.outputTokens ?? '',
    usage.cacheReadTokens ?? '',
    usage.cacheWriteTokens ?? '',
  ].join('|');
}

/**
 * Business Logic（为什么需要这个函数）:
 *   live/buffer 事件需幂等合并；旧 version 不得覆盖新状态；可推进 asOfSequence。
 *
 * Code Logic（这个函数做什么）:
 *   - ownerInstanceId 若 event 携带且与 state 不同且 state 已有 owner：拒绝（返回原 state）
 *   - 同 agent id：version 更大，或同 version 且 live usage 更新才替换
 *   - 不可变 Map 复制后写回
 *   - sequence 大于 asOfSequence 时更新 asOfSequence
 *   - freshness 默认 live，可由参数覆盖
 *
 * @param state 当前聚合状态
 * @param event 增量事件
 * @param freshness 可选新鲜度
 */
export function applyAgentRuntimeEvent(
  state: AgentRuntimeState,
  event: AgentRuntimeEvent,
  freshness: AgentFreshness = 'live',
): AgentRuntimeState {
  const dto = event.agentSession;
  if (
    event.ownerInstanceId &&
    state.ownerInstanceId &&
    event.ownerInstanceId !== state.ownerInstanceId
  ) {
    return state;
  }

  const existing = state.byAgentId.get(dto.id);
  if (!shouldAcceptAgentRuntimeUpdate(existing, dto)) {
    return state;
  }

  const nextByAgentId = new Map(state.byAgentId);
  nextByAgentId.set(dto.id, toAgentSessionProjection(dto, freshness));

  let asOfSequence = state.asOfSequence;
  if (typeof event.sequence === 'number' && event.sequence > asOfSequence) {
    asOfSequence = event.sequence;
  }

  return {
    ownerInstanceId: event.ownerInstanceId ?? state.ownerInstanceId,
    asOfSequence,
    byAgentId: nextByAgentId,
    latestAgentIdByTerminal: rebuildLatestByTerminal(nextByAgentId),
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   terminal tab 只展示绑定该 terminal 的最新 Agent 投影。
 *
 * Code Logic（这个函数做什么）:
 *   查 latestAgentIdByTerminal 再取 byAgentId；缺失返回 null。
 *
 * @param state 聚合状态
 * @param terminalSessionId terminal session id
 */
export function latestAgentForTerminal(
  state: AgentRuntimeState,
  terminalSessionId: string,
): AgentSessionProjection | null {
  const agentId = state.latestAgentIdByTerminal.get(terminalSessionId);
  if (!agentId) return null;
  return state.byAgentId.get(agentId) ?? null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   组件/selector 按 agent id 取单条投影。
 *
 * Code Logic（这个函数做什么）:
 *   Map.get 或 null。
 */
export function agentById(
  state: AgentRuntimeState,
  agentId: string,
): AgentSessionProjection | null {
  return state.byAgentId.get(agentId) ?? null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   offline/cached 回退时需要把全部 projection 标记为同一 freshness，不改 version。
 *
 * Code Logic（这个函数做什么）:
 *   复制 Map 并更新每条 freshness；索引不变。
 */
export function markAgentRuntimeFreshness(
  state: AgentRuntimeState,
  freshness: AgentFreshness,
): AgentRuntimeState {
  const byAgentId = new Map<string, AgentSessionProjection>();
  for (const [id, agent] of state.byAgentId) {
    byAgentId.set(id, { ...agent, freshness });
  }
  return {
    ...state,
    byAgentId,
  };
}
