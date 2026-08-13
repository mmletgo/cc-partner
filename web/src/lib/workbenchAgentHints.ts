/**
 * 工作台 Agent 等待/完成 hint 纯规则。
 *
 * Business Logic（为什么需要这个模块）:
 *   项目卡、worktree、窗口 tab 需要按 window 统计「等人」和「未看完成」，
 *   激活窗口只消完成、等待必须跟 phase 走；刷新不得让已看完成复活。
 *
 * Code Logic（这个模块做什么）:
 *   维护 per-terminal waiting/completed、acked 集合与 seen-completed 边沿；
 *   提供 apply/ack/selector 与 localStorage 序列化。
 */

import type { AgentSessionRuntimeDto } from './types/agentRuntime';

export const ACKED_COMPLETED_STORAGE_KEY = 'cp-workbench-acked-completed';
export const SEEN_COMPLETED_STORAGE_KEY = 'cp-workbench-seen-completed';
export const ACKED_COMPLETED_CAP = 500;
export const SEEN_COMPLETED_CAP = 200;
export const SEEN_COMPLETED_TTL_MS = 7 * 24 * 60 * 60 * 1000;

export type AgentHintTone = 'wait' | 'complete';

export interface AgentHintCounts {
  waitingCount: number;
  completedCount: number;
  count: number;
  tone: AgentHintTone | null;
}

export interface AgentHintTerminalRow {
  projectId: string;
  worktreeId?: string;
  lastVersion?: number;
  waitingAgentId?: string;
  completedAgentId?: string;
  completedVersion?: number;
  completedEndedAt?: string;
}

export interface SeenCompletedEdge {
  agentSessionId: string;
  terminalSessionId: string;
  projectId: string;
  worktreeId?: string;
  version: number;
  endedAt: string;
}

export interface WorkbenchAgentHintState {
  byTerminal: ReadonlyMap<string, AgentHintTerminalRow>;
  ackedCompletedIds: ReadonlySet<string>;
}

export interface ApplyAgentHintOptions {
  sessionWorktreeByTerminal?: Record<string, string | undefined>;
}

export interface PersistedHintExtras {
  ackedCompletedIds: string[];
  seenCompleted: SeenCompletedEdge[];
}

export const EMPTY_HINT_COUNTS: AgentHintCounts = {
  waitingCount: 0,
  completedCount: 0,
  count: 0,
  tone: null,
};

const EMPTY_COUNTS = EMPTY_HINT_COUNTS;

/**
 * Business Logic（为什么需要这个函数）:
 *   未握手前需要确定性空态，避免残留旧 Map。
 *
 * Code Logic（这个函数做什么）:
 *   返回空 byTerminal 与空 acked 集合。
 */
export function emptyAgentHintState(): WorkbenchAgentHintState {
  return {
    byTerminal: new Map(),
    ackedCompletedIds: new Set(),
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   点上的颜色必须等待优先，数字是等待窗口 + 未看完成窗口。
 *
 * Code Logic（这个函数做什么）:
 *   waiting>0 → wait；否则 completed>0 → complete；否则 null。
 */
/**
 * Business Logic（为什么需要这个函数）:
 *   无障碍文案必须按等待/完成分段，0 段省略。
 *
 * Code Logic（这个函数做什么）:
 *   返回 i18n key 后缀：waiting / completed / both / null。
 */
export function hintAriaKind(
  counts: AgentHintCounts,
): 'waiting' | 'completed' | 'both' | null {
  if (counts.waitingCount > 0 && counts.completedCount > 0) return 'both';
  if (counts.waitingCount > 0) return 'waiting';
  if (counts.completedCount > 0) return 'completed';
  return null;
}

export function hintCountsFrom(waitingCount: number, completedCount: number): AgentHintCounts {
  const waiting = Math.max(0, waitingCount);
  const completed = Math.max(0, completedCount);
  const count = waiting + completed;
  const tone: AgentHintTone | null = waiting > 0 ? 'wait' : completed > 0 ? 'complete' : null;
  return { waitingCount: waiting, completedCount: completed, count, tone };
}

function resolveWorktreeId(
  dto: Pick<AgentSessionRuntimeDto, 'worktreeId' | 'terminalSessionId'>,
  options?: ApplyAgentHintOptions,
): string | undefined {
  if (dto.worktreeId != null && dto.worktreeId !== '') return dto.worktreeId;
  const fallback = options?.sessionWorktreeByTerminal?.[dto.terminalSessionId];
  return fallback && fallback !== '' ? fallback : undefined;
}

function cloneRow(row: AgentHintTerminalRow | undefined, projectId: string): AgentHintTerminalRow {
  return row ? { ...row, projectId } : { projectId };
}

function pruneEmptyRow(row: AgentHintTerminalRow): AgentHintTerminalRow | undefined {
  if (row.waitingAgentId || row.completedAgentId) return row;
  return undefined;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   snapshot/live 入站必须按 phase 更新该 window 的等待/完成，且不得覆盖已 ack 的完成。
 *
 * Code Logic（这个函数做什么）:
 *   needsInput 写 waiting；completed 且未 ack 写 completed；其它非终态清 waiting。
 */
export function applyAgentHintSession(
  state: WorkbenchAgentHintState,
  dto: AgentSessionRuntimeDto,
  options?: ApplyAgentHintOptions,
): WorkbenchAgentHintState {
  const terminalId = dto.terminalSessionId;
  if (!terminalId) return state;

  const nextByTerminal = new Map(state.byTerminal);
  const worktreeId = resolveWorktreeId(dto, options);
  const existing = nextByTerminal.get(terminalId);
  if (existing?.lastVersion !== undefined && dto.version < existing.lastVersion) {
    return state;
  }
  const current = cloneRow(existing, dto.projectId);
  if (worktreeId) current.worktreeId = worktreeId;
  current.lastVersion = dto.version;

  if (dto.phase === 'needsInput') {
    current.waitingAgentId = dto.id;
    current.completedAgentId = undefined;
    current.completedVersion = undefined;
    current.completedEndedAt = undefined;
  } else if (dto.phase === 'completed') {
    current.waitingAgentId = undefined;
    if (!state.ackedCompletedIds.has(dto.id)) {
      current.completedAgentId = dto.id;
      current.completedVersion = dto.version;
      current.completedEndedAt = dto.endedAt ?? dto.lastActivityAt;
    }
  } else if (
    dto.phase === 'working' ||
    dto.phase === 'idle' ||
    dto.phase === 'launching'
  ) {
    current.waitingAgentId = undefined;
  } else if (dto.phase === 'failed' || dto.phase === 'disconnected') {
    current.waitingAgentId = undefined;
  }

  const pruned = pruneEmptyRow(current);
  if (pruned) nextByTerminal.set(terminalId, pruned);
  else nextByTerminal.delete(terminalId);

  return {
    ...state,
    byTerminal: nextByTerminal,
  };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用户激活某个窗口后，该窗口未看完成应消失，等待不得被清掉。
 *
 * Code Logic（这个函数做什么）:
 *   只移除该 terminal 的 completedAgentId，并把 id 写入 acked FIFO。
 */
export function ackCompletedForTerminal(
  state: WorkbenchAgentHintState,
  terminalSessionId: string,
): WorkbenchAgentHintState {
  const row = state.byTerminal.get(terminalSessionId);
  if (!row?.completedAgentId) return state;

  const nextAcked = appendAcked(state.ackedCompletedIds, row.completedAgentId);
  const nextByTerminal = new Map(state.byTerminal);
  const nextRow = {
    ...row,
    completedAgentId: undefined,
    completedVersion: undefined,
    completedEndedAt: undefined,
  };
  const pruned = pruneEmptyRow(nextRow);
  if (pruned) nextByTerminal.set(terminalSessionId, pruned);
  else nextByTerminal.delete(terminalSessionId);

  return {
    byTerminal: nextByTerminal,
    ackedCompletedIds: nextAcked,
  };
}

function appendAcked(existing: ReadonlySet<string>, id: string): Set<string> {
  const list = [...existing].filter((item) => item !== id);
  list.push(id);
  return new Set(list.slice(-ACKED_COMPLETED_CAP));
}

function rowCounts(row: AgentHintTerminalRow | undefined): AgentHintCounts {
  if (!row) return EMPTY_COUNTS;
  const waiting = row.waitingAgentId ? 1 : 0;
  const completed = !row.waitingAgentId && row.completedAgentId ? 1 : 0;
  return hintCountsFrom(waiting, completed);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   窗口 tab 只需该 terminal 的数字与颜色。
 *
 * Code Logic（这个函数做什么）:
 *   查 byTerminal 一行并转 counts。
 */
export function hintsForTerminal(
  state: WorkbenchAgentHintState,
  terminalSessionId: string,
): AgentHintCounts {
  return rowCounts(state.byTerminal.get(terminalSessionId));
}

function aggregateRows(rows: Iterable<AgentHintTerminalRow>): AgentHintCounts {
  let waiting = 0;
  let completed = 0;
  for (const row of rows) {
    if (row.waitingAgentId) waiting += 1;
    else if (row.completedAgentId) completed += 1;
  }
  return hintCountsFrom(waiting, completed);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   项目卡右上角数字是该项目所有 window 的等待 + 未看完成。
 *
 * Code Logic（这个函数做什么）:
 *   过滤 projectId 后聚合。
 */
export function hintsForProject(state: WorkbenchAgentHintState, projectId: string): AgentHintCounts {
  return aggregateRows(
    [...state.byTerminal.values()].filter((row) => row.projectId === projectId),
  );
}

/**
 * Business Logic（为什么需要这个函数）:
 *   worktree chip 只统计绑到该 worktree 的 window。
 *
 * Code Logic（这个函数做什么）:
 *   过滤 projectId + worktreeId 后聚合。
 */
export function hintsForWorktree(
  state: WorkbenchAgentHintState,
  projectId: string,
  worktreeId: string,
): AgentHintCounts {
  return aggregateRows(
    [...state.byTerminal.values()].filter(
      (row) => row.projectId === projectId && row.worktreeId === worktreeId,
    ),
  );
}

function isFiniteTimestamp(raw: string): number | null {
  const ts = Date.parse(raw);
  return Number.isFinite(ts) ? ts : null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   ack 集合刷新后仍须挡住已看完成。
 *
 * Code Logic（这个函数做什么）:
 *   解析 JSON 字符串数组，FIFO 截到 cap。
 */
export function serializeAckedCompleted(ids: readonly string[]): string {
  return JSON.stringify(ids.slice(-ACKED_COMPLETED_CAP));
}

/**
 * Business Logic（为什么需要这个函数）:
 *   默认 snapshot 不含 completed，刷新后要靠本地边沿恢复未看完成。
 *
 * Code Logic（这个函数做什么）:
 *   序列化 seen-completed 边沿。
 */
export function serializeSeenCompleted(edges: readonly SeenCompletedEdge[]): string {
  return JSON.stringify(edges);
}

function parseStringArray(raw: string | undefined): string[] {
  if (!raw) return [];
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter((item): item is string => typeof item === 'string' && item !== '');
  } catch {
    return [];
  }
}

function parseSeenEdges(raw: string | undefined, now = Date.now()): SeenCompletedEdge[] {
  if (!raw) return [];
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return [];
  }
  if (!Array.isArray(parsed)) return [];
  const edges: SeenCompletedEdge[] = [];
  for (const item of parsed) {
    if (!item || typeof item !== 'object') continue;
    const rec = item as Record<string, unknown>;
    if (
      typeof rec.agentSessionId !== 'string' ||
      typeof rec.terminalSessionId !== 'string' ||
      typeof rec.projectId !== 'string' ||
      typeof rec.version !== 'number' ||
      typeof rec.endedAt !== 'string'
    ) {
      continue;
    }
    const endedAtTs = isFiniteTimestamp(rec.endedAt);
    if (endedAtTs === null || now - endedAtTs > SEEN_COMPLETED_TTL_MS) continue;
    const edge: SeenCompletedEdge = {
      agentSessionId: rec.agentSessionId,
      terminalSessionId: rec.terminalSessionId,
      projectId: rec.projectId,
      version: rec.version,
      endedAt: rec.endedAt,
    };
    if (typeof rec.worktreeId === 'string' && rec.worktreeId !== '') {
      edge.worktreeId = rec.worktreeId;
    }
    edges.push(edge);
  }
  return edges.slice(0, SEEN_COMPLETED_CAP);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   hook 启动时从 localStorage 恢复 ack 与未看完成边沿。
 *
 * Code Logic（这个函数做什么）:
 *   读两个 key，ack FIFO cap，seen 丢过期后 cap。
 */
export function loadPersistedHintExtras(
  storage: Record<string, string | undefined>,
  now = Date.now(),
): PersistedHintExtras {
  const acked = parseStringArray(storage[ACKED_COMPLETED_STORAGE_KEY]).slice(-ACKED_COMPLETED_CAP);
  const seenCompleted = parseSeenEdges(storage[SEEN_COMPLETED_STORAGE_KEY], now);
  return { ackedCompletedIds: acked, seenCompleted };
}

/**
 * Business Logic（为什么需要这个函数）:
 *   刷新后 snapshot 只有 active；未看 completed 必须从 persist 边沿灌回。
 *
 * Code Logic（这个函数做什么）:
 *   把未过期、未 ack 的 seen-completed 当成 completed session apply。
 */
/**
 * Business Logic（为什么需要这个函数）:
 *   Gap/握手后 snapshot 只有 active；waiting 必须以 snapshot 为准，未看完成不能被冲掉。
 *
 * Code Logic（这个函数做什么）:
 *   清掉全部 waiting，保留 completed，再 apply snapshot sessions。
 */
export function replaceActiveWaitingFromSnapshot(
  state: WorkbenchAgentHintState,
  sessions: readonly AgentSessionRuntimeDto[],
  options?: ApplyAgentHintOptions,
): WorkbenchAgentHintState {
  const nextByTerminal = new Map<string, AgentHintTerminalRow>();
  for (const [terminalId, row] of state.byTerminal) {
    if (!row.completedAgentId) continue;
    nextByTerminal.set(terminalId, { ...row, waitingAgentId: undefined });
  }
  let next: WorkbenchAgentHintState = {
    ...state,
    byTerminal: nextByTerminal,
  };
  for (const dto of sessions) {
    next = applyAgentHintSession(next, dto, options);
  }
  return next;
}

export function restoreSeenCompleted(
  state: WorkbenchAgentHintState,
  edges: readonly SeenCompletedEdge[],
  now = Date.now(),
): WorkbenchAgentHintState {
  let next = state;
  for (const edge of edges) {
    const endedAtTs = isFiniteTimestamp(edge.endedAt);
    if (endedAtTs === null || now - endedAtTs > SEEN_COMPLETED_TTL_MS) continue;
    if (next.ackedCompletedIds.has(edge.agentSessionId)) continue;
    next = applyAgentHintSession(next, {
      id: edge.agentSessionId,
      projectId: edge.projectId,
      worktreeId: edge.worktreeId ?? null,
      terminalSessionId: edge.terminalSessionId,
      orchestratorTaskId: null,
      orchestratorAttempt: null,
      providerId: 'genericTerminal',
      phase: 'completed',
      version: edge.version,
      startedAt: edge.endedAt,
      lastActivityAt: edge.endedAt,
      endedAt: edge.endedAt,
      outcomeCode: null,
      resumedFromAgentSessionId: null,
      isActive: false,
    });
  }
  return next;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   persist 层需要从当前 state 抽出可序列化的 completed 边沿。
 *
 * Code Logic（这个函数做什么）:
 *   遍历有 completedAgentId 的 terminal，缺 endedAt 时用 epoch 0 以外的 lastActivity 占位由调用方补。
 */
export function collectSeenCompleted(
  state: WorkbenchAgentHintState,
  endedAtByAgentId: ReadonlyMap<string, string> = new Map(),
): SeenCompletedEdge[] {
  const edges: SeenCompletedEdge[] = [];
  for (const [terminalSessionId, row] of state.byTerminal) {
    if (!row.completedAgentId) continue;
    const endedAt = row.completedEndedAt ?? endedAtByAgentId.get(row.completedAgentId);
    if (!endedAt) continue;
    const edge: SeenCompletedEdge = {
      agentSessionId: row.completedAgentId,
      terminalSessionId,
      projectId: row.projectId,
      version: row.completedVersion ?? 0,
      endedAt,
    };
    if (row.worktreeId) edge.worktreeId = row.worktreeId;
    edges.push(edge);
  }
  return edges.slice(0, SEEN_COMPLETED_CAP);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用 persist extras 重建空 state 的 acked 集合。
 *
 * Code Logic（这个函数做什么）:
 *   空 byTerminal + acked Set。
 */
export function stateWithAcked(ackedCompletedIds: readonly string[]): WorkbenchAgentHintState {
  return {
    byTerminal: new Map(),
    ackedCompletedIds: new Set(ackedCompletedIds.slice(-ACKED_COMPLETED_CAP)),
  };
}
