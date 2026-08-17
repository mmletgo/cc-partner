/**
 * Token 统计页前端 DTO（对齐 Rust `summarize_agent_ledger` /
 * `list_agent_ledger` / `export_token_stats` wire 形态）。
 *
 * Business Logic（为什么需要这个模块）:
 *   桌面端 Token 统计页与后端 token-stats Rust 命令共享一份契约；前端
 *   必须按 camelCase 显式建模，避免把 unknown 当 0、把 enum 串当枚举。
 *
 * Code Logic（这个模块做什么）:
 *   定义 window/outcome/coverage/bucket 枚举、filter、summary、group row、
 *   trend point、session entry/page 与 export payload；纯类型，不带 runtime 值。
 */

export type AgentLedgerOutcome = 'completed' | 'failed' | 'cancelled' | 'disconnected';

/** wire token 时间窗（与 SummarizeAgentLedgerReq.window 对齐，缺省 7d）。 */
export type TokenStatsWindow = '24h' | '7d' | '30d';

/** 趋势桶粒度（与 SummarizeAgentLedgerReq.bucket 对齐）。 */
export type TokenStatsBucket = 'hour' | 'day';

/** 后端根据 ledger 行元数据返回的 coverage 等级。 */
export type UsageCoverage = 'complete' | 'partial' | 'unavailable';

/**
 * 单货币成本桶（不分摊到主货币）。
 *
 * Business Logic（为什么需要）:
 *   不同货币分别展示避免汇率浮动与混合金额；与后端 costByCurrency / totalCostByCurrency 对齐。
 *
 * Code Logic（做什么）:
 *   currency ISO + minorUnits 非负整数。
 */
export interface CurrencyAmount {
  currency: string;
  minorUnits: number;
}

/**
 * Token 统计页筛选条件。
 *
 * Business Logic（为什么需要）:
 *   SummarizeAgentLedgerReq 与 ListAgentLedgerReq 共享大部分字段；
 *   前端需要一份统一 filter 形态供页面 controller 与 API helper 共用。
 *
 * Code Logic（字段说明）:
 *   window/bucket 只 optional（缺省后端推导）；其它标量既可缺省也可 null；
 *   providerIds/modelIds/projectIds 既可 null 也可缺失；
 *   outcome 已移除（Token 统计页 UI 不再按终态筛选）。
 */
export interface AgentLedgerFilters {
  /** 时间窗（缺省由后端推导为 7d；null = 自定义区间，不再套预设窗）。 */
  window?: TokenStatsWindow | null;
  /** 单项目筛选（与 projectIds 互斥；多值优先）。null/缺省 = 全项目。 */
  projectId?: string | null;
  /** 多 provider；list + summary 全量 IN。 */
  providerIds?: string[] | null;
  modelIds?: string[] | null;
  /** 多项目（与 projectId 互斥；多值优先）。 */
  projectIds?: string[] | null;
  worktreeId?: string | null;
  /** RFC3339 半开区间左端。 */
  startedAfter?: string | null;
  startedBefore?: string | null;
  /** 趋势桶；缺省由 window 推导（24h→hour，其它→day）。 */
  bucket?: TokenStatsBucket | null;
}

/**
 * 单条 ledger 历史（仅 metadata，不带 transcript/cwd）。
 *
 * Business Logic（为什么需要）:
 *   Session 明细表逐行展示；与 Rust ListAgentLedgerResp.items 对齐。
 *
 * Code Logic（字段说明）:
 *   cost 拆为 costMinorUnits/costCurrency（不折算）；
 *   输入/输出/cache 各自 nullable（旧行可能缺字段）；
 *   projectName 来自后端 LEFT JOIN workbench_proprojects，缺失回 null → UI 回退到 projectId。
 */
export interface AgentLedgerSessionEntry {
  id: string;
  agentSessionId: string;
  projectId: string;
  worktreeId: string | null;
  providerId: string;
  modelId: string | null;
  startedAt: string;
  endedAt: string;
  durationMs: number;
  inputTokens: number | null;
  outputTokens: number | null;
  cacheReadTokens: number | null;
  cacheWriteTokens: number | null;
  costMinorUnits: number | null;
  costCurrency: string | null;
  terminalTitle: string | null;
  projectName: string | null;
}

/** session list 翻页结果（hasMore 由 nextCursor 是否存在推导）。 */
export interface AgentLedgerSessionPage {
  items: AgentLedgerSessionEntry[];
  nextCursor: string | null;
}

/**
 * 单聚合分组的行（byModel/byProvider/byProject 共形）。
 *
 * Business Logic（为什么需要）:
 *   KPI 卡与 group table 展示同一数据形态，减少 controller 端条件分支。
 *
 * Code Logic（字段说明）:
 *   sessions 与四种 outcome 计数；token 字段 nullable（旧 ledger 行可能无）；
 *   costByCurrency 按货币桶（不折算）。
 */
export interface AgentLedgerGroupRow {
  /** modelId / providerId / projectId；缺失后端给 "(unknown)"。 */
  key: string;
  label?: string | null;
  sessions: number;
  completed: number;
  failed: number;
  cancelled: number;
  disconnected: number;
  inputTokens: number | null;
  outputTokens: number | null;
  cacheReadTokens: number | null;
  cacheWriteTokens: number | null;
  costByCurrency: CurrencyAmount[];
}

/**
 * 时间桶内的聚合点（trend 序列）。
 *
 * Business Logic（为什么需要）:
 *   Trend 图表直接消费 bucketStart + token + cost；不折算。
 *
 * Code Logic（字段说明）:
 *   bucketStart = RFC3339 UTC；token 字段 nullable。
 */
export interface AgentLedgerTrendPoint {
  bucketStart: string;
  inputTokens: number | null;
  outputTokens: number | null;
  cacheReadTokens: number | null;
  cacheWriteTokens: number | null;
  costByCurrency: CurrencyAmount[];
}

/**
 * Token 统计页 summary（与 Rust AgentLedgerSummary 对齐）。
 *
 * Business Logic（为什么需要）:
 *   KPI/Trend/Group 表共一份 summary；派生指标（cacheHitRate、realConsumed、
 *   totalCostByCurrency、byModel/byProvider/byProject/trend）由后端 SQL 聚合，
 *   前端不二次计算。
 *
 * Code Logic（字段说明）:
 *   window 必填；projectId 可选；sessions/四种 outcome/durationMs/requestsCount 必填非负；
 *   token 字段 nullable；bucket 必填；trend 始终返回（空数组）。
 */
export interface AgentLedgerSummary {
  window: TokenStatsWindow;
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
  cacheWriteTokens: number | null;
  /** input + cacheWrite + output；旧 ledger 行可能全 null → null。 */
  realConsumedTokens: number | null;
  /** cacheRead / (cacheRead + input)，分母 0 → null。 */
  cacheHitRate: number | null;
  /** 与 sessions 同义（保留请求级别名）。 */
  requestsCount: number;
  costByCurrency: CurrencyAmount[];
  totalCostByCurrency: CurrencyAmount[];
  usageCoverage: UsageCoverage;
  byModel: AgentLedgerGroupRow[];
  byProvider: AgentLedgerGroupRow[];
  byProject: AgentLedgerGroupRow[];
  trend: AgentLedgerTrendPoint[];
  bucket: TokenStatsBucket;
}

/** Export command 接受的格式。 */
export type ExportFormat = 'csv' | 'json';
