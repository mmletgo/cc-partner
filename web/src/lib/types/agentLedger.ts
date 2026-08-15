/**
 * Agent Metadata Ledger 前端类型（与 Rust DTO camelCase 对齐）。
 *
 * Business Logic（为什么需要这个模块）:
 *   本机 drawer 与 Fleet Agent activity 需要 metadata-only 历史与时间窗聚合；
 *   不得包含 prompt/response/transcript/path。
 *
 * Code Logic（这个模块做什么）:
 *   定义 entry/page/summary/window/coverage 与查询参数。
 */

/** 终态 outcome。 */
export type AgentLedgerOutcome = 'completed' | 'failed' | 'cancelled' | 'disconnected';

/** 时间窗 wire token。 */
export type LedgerWindow = '24h' | '7d' | '30d';

/** usage 覆盖度。 */
export type LedgerUsageCoverage = 'complete' | 'partial' | 'unavailable';

/** Fleet 上 agent activity 获取状态。 */
export type FleetAgentActivityStatus = 'live' | 'unsupported' | 'unavailable';

/**
 * 按货币分组的 cost 桶（不折算）。
 */
export interface CurrencyAmount {
  currency: string;
  minorUnits: number;
}

/**
 * 单条 metadata-only 历史（本机明细）。
 */
export interface AgentLedgerEntry {
  id: string;
  agentSessionId: string;
  projectId: string;
  worktreeId: string | null;
  providerId: string;
  modelId: string | null;
  startedAt: string;
  endedAt: string;
  durationMs: number;
  outcome: AgentLedgerOutcome;
  inputTokens: number | null;
  outputTokens: number | null;
  cacheReadTokens: number | null;
  cacheWriteTokens: number | null;
  costMinorUnits: number | null;
  costCurrency: string | null;
  createdAt: string;
  updatedAt: string;
}

/**
 * 分页结果（hasMore 由 nextCursor 是否存在推导）。
 */
export interface AgentLedgerPage {
  items: AgentLedgerEntry[];
  nextCursor: string | null;
}

/**
 * 时间窗聚合摘要（本机 / Fleet / P2P）。
 */
export interface AgentLedgerSummary {
  window: LedgerWindow;
  projectId?: string | null;
  sessions: number;
  completed: number;
  failed: number;
  cancelled: number;
  disconnected: number;
  durationMs: number;
  inputTokens: number | null;
  outputTokens: number | null;
  cacheReadTokens: number | null;
  costByCurrency: CurrencyAmount[];
  usageCoverage: LedgerUsageCoverage;
}

/**
 * 本机列表查询参数。
 */
export interface AgentLedgerListParams {
  projectId?: string | null;
  providerId?: string | null;
  outcome?: AgentLedgerOutcome | null;
  endedAfter?: string | null;
  endedBefore?: string | null;
  cursor?: string | null;
  limit?: number | null;
}

/**
 * 本机 summary 查询参数。
 */
export interface AgentLedgerSummarizeParams {
  window: LedgerWindow;
  projectId?: string | null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   UI 对 null token 必须显示「未提供」，不能显示 0。
 *
 * Code Logic（这个函数做什么）:
 *   有限数字 → 字符串；否则 null。
 */
export function formatOptionalTokenCount(value: number | null | undefined): string | null {
  if (value == null || !Number.isFinite(value)) return null;
  return String(value);
}
