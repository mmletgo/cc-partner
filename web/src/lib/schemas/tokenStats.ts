/**
 * Token 统计页 runtime schema + pure helpers。
 *
 * Business Logic（为什么需要这个模块）:
 *   IPC / 移动端 HTTP 边界都可能损坏；写入页面状态前必须 fail-closed，
 *   拒绝把 unknown 当 0、把未知 enum 当默认。所有派生指标（cacheHitRate /
 *   realConsumed / totalCostByCurrency）由后端 SQL 提供，前端 helper 只
 *   做纯前端二次加工（同源 defensively，方便脱机 mock 与单测）。
 *
 * Code Logic（这个模块做什么）:
 *   严格 decoder：window/outcome/coverage/bucket 枚举、非负计数、可空 token、
 *   currencyAmount 与 group/trend/page 结构；同时导出 cache hit / real
 *   consumed / primary currency picker / trend bucketize 等 pure helpers。
 */

import type {
  AgentLedgerFilters,
  AgentLedgerGroupRow,
  AgentLedgerOutcome,
  AgentLedgerSessionEntry,
  AgentLedgerSessionPage,
  AgentLedgerSummary,
  AgentLedgerTrendPoint,
  CurrencyAmount,
  ExportFormat,
  TokenStatsBucket,
  TokenStatsWindow,
  UsageCoverage,
} from '../types/tokenStats';
import {
  arrayDecoder,
  defineDecoder,
  enumDecoder,
  nullableDecoder,
  numberDecoder,
  objectDecoder,
  optionalDecoder,
  stringDecoder,
  type Decoder,
} from '../runtimeSchema';
import { ContractDecodeError } from '../runtimeSchema';

/* ----------------------- 公共原语 ----------------------- */

/**
 * Business Logic（为什么需要这个 decoder）:
 *   计数（sessions/outcome/durationMs 等）不得为负，否则 KPI 卡误导。
 *
 * Code Logic（这个 decoder 做什么）:
 *   numberDecoder + 整数 + 非负校验。
 */
export const nonNegativeIntDecoder: Decoder<number> = defineDecoder(
  'NonNegativeInt',
  (value, path = '$') => {
    const n = numberDecoder.decode(value, path);
    if (!Number.isFinite(n) || n < 0 || !Number.isInteger(n)) {
      throw new ContractDecodeError('NonNegativeInt', path, 'primitive');
    }
    return n;
  },
);

/* ----------------------- 枚举 decoder ----------------------- */

export const tokenStatsWindowDecoder: Decoder<TokenStatsWindow> = enumDecoder(
  'TokenStatsWindow',
  ['24h', '7d', '30d'] as const,
);

export const tokenStatsBucketDecoder: Decoder<TokenStatsBucket> = enumDecoder(
  'TokenStatsBucket',
  ['hour', 'day'] as const,
);

export const tokenStatsOutcomeDecoder: Decoder<AgentLedgerOutcome> = enumDecoder(
  'AgentLedgerOutcome',
  ['completed', 'failed', 'cancelled', 'disconnected'] as const,
);

export const usageCoverageDecoder: Decoder<UsageCoverage> = enumDecoder(
  'UsageCoverage',
  ['complete', 'partial', 'unavailable'] as const,
);

export const exportFormatDecoder: Decoder<ExportFormat> = enumDecoder('ExportFormat', [
  'csv',
  'json',
] as const);

/* ----------------------- 复合 decoder ----------------------- */

/**
 * 单货币成本桶；minorUnits 非负。
 *
 * Business Logic（为什么需要）:
 *   costByCurrency / totalCostByCurrency / group / trend 共用；缺失或负值拒收。
 *
 * Code Logic（做什么）:
 *   objectDecoder({ currency, minorUnits })，每字段独立 strict。
 */
export const currencyAmountDecoder: Decoder<CurrencyAmount> = objectDecoder('CurrencyAmount', {
  currency: stringDecoder,
  minorUnits: nonNegativeIntDecoder,
});

/**
 * Filter 形态（每字段 optional+nullable，允许缺失与 null 共存）。
 *
 * Business Logic（为什么需要）:
 *   页面 controller 可能以 partial filter 调用，前端类型与 wire 请求 shape 必须兼容。
 *
 * Code Logic（做什么）:
 *   标量 optionalDecoder(nullableDecoder(...))，数组 optionalDecoder(nullableDecoder(arrayDecoder))。
 */
export const agentLedgerFiltersDecoder: Decoder<AgentLedgerFilters> = objectDecoder(
  'AgentLedgerFilters',
  {
    window: optionalDecoder(nullableDecoder(tokenStatsWindowDecoder)),
    projectId: optionalDecoder(nullableDecoder(stringDecoder)),
    providerIds: optionalDecoder(nullableDecoder(arrayDecoder(stringDecoder))),
    modelIds: optionalDecoder(nullableDecoder(arrayDecoder(stringDecoder))),
    projectIds: optionalDecoder(nullableDecoder(arrayDecoder(stringDecoder))),
    worktreeId: optionalDecoder(nullableDecoder(stringDecoder)),
    outcome: optionalDecoder(nullableDecoder(tokenStatsOutcomeDecoder)),
    startedAfter: optionalDecoder(nullableDecoder(stringDecoder)),
    startedBefore: optionalDecoder(nullableDecoder(stringDecoder)),
    bucket: optionalDecoder(nullableDecoder(tokenStatsBucketDecoder)),
  },
);

/**
 * 单聚合分组行（byModel/byProvider/byProject 共 decoder）。
 *
 * Business Logic（为什么需要）:
 *   Group 表逐行展示；未知额外字段（兼容性前向版本）由 objectDecoder 自然忽略。
 *
 * Code Logic（做什么）:
 *   必填非负计数 + nullable token + costByCurrency 数组。
 */
export const agentLedgerGroupRowDecoder: Decoder<AgentLedgerGroupRow> = objectDecoder(
  'AgentLedgerGroupRow',
  {
    key: stringDecoder,
    label: optionalDecoder(nullableDecoder(stringDecoder)),
    sessions: nonNegativeIntDecoder,
    completed: nonNegativeIntDecoder,
    failed: nonNegativeIntDecoder,
    cancelled: nonNegativeIntDecoder,
    disconnected: nonNegativeIntDecoder,
    inputTokens: nullableDecoder(nonNegativeIntDecoder),
    outputTokens: nullableDecoder(nonNegativeIntDecoder),
    cacheReadTokens: nullableDecoder(nonNegativeIntDecoder),
    cacheWriteTokens: nullableDecoder(nonNegativeIntDecoder),
    costByCurrency: arrayDecoder(currencyAmountDecoder),
  },
);

/**
 * 趋势桶内聚合点。
 *
 * Business Logic（为什么需要）:
 *   Trend 图按 bucketStart 直接画坐标。
 *
 * Code Logic（做什么）:
 *   必填 bucketStart（RFC3339 UTC，page 层做格式校验）+ nullable token + cost 数组。
 */
export const agentLedgerTrendPointDecoder: Decoder<AgentLedgerTrendPoint> = objectDecoder(
  'AgentLedgerTrendPoint',
  {
    bucketStart: stringDecoder,
    inputTokens: nullableDecoder(nonNegativeIntDecoder),
    outputTokens: nullableDecoder(nonNegativeIntDecoder),
    cacheReadTokens: nullableDecoder(nonNegativeIntDecoder),
    cacheWriteTokens: nullableDecoder(nonNegativeIntDecoder),
    costByCurrency: arrayDecoder(currencyAmountDecoder),
  },
);

/**
 * KPI 聚合主对象（同时含派生指标与 trend / group 列表）。
 *
 * Business Logic（为什么需要）:
 *   summary 是 controller 单一真源；要求全字段存在（costByCurrency /
 *   totalCostByCurrency / byModel/.../trend 缺一不可）。
 *
 * Code Logic（做什么）:
 *   objectDecoder；trend 与 group 数组均允许空（空窗零 token）。
 */
export const agentLedgerSummaryDecoder: Decoder<AgentLedgerSummary> = objectDecoder(
  'AgentLedgerSummary',
  {
    window: tokenStatsWindowDecoder,
    projectId: optionalDecoder(nullableDecoder(stringDecoder)),
    sessions: nonNegativeIntDecoder,
    completed: nonNegativeIntDecoder,
    failed: nonNegativeIntDecoder,
    cancelled: nonNegativeIntDecoder,
    disconnected: nonNegativeIntDecoder,
    durationMs: nonNegativeIntDecoder,
    inputTokens: nullableDecoder(nonNegativeIntDecoder),
    outputTokens: nullableDecoder(nonNegativeIntDecoder),
    cacheReadTokens: nullableDecoder(nonNegativeIntDecoder),
    cacheWriteTokens: nullableDecoder(nonNegativeIntDecoder),
    realConsumedTokens: nullableDecoder(nonNegativeIntDecoder),
    cacheHitRate: nullableDecoder(numberDecoder),
    requestsCount: nonNegativeIntDecoder,
    costByCurrency: arrayDecoder(currencyAmountDecoder),
    totalCostByCurrency: arrayDecoder(currencyAmountDecoder),
    usageCoverage: usageCoverageDecoder,
    byModel: arrayDecoder(agentLedgerGroupRowDecoder),
    byProvider: arrayDecoder(agentLedgerGroupRowDecoder),
    byProject: arrayDecoder(agentLedgerGroupRowDecoder),
    trend: arrayDecoder(agentLedgerTrendPointDecoder),
    bucket: tokenStatsBucketDecoder,
  },
);

/**
 * 单条 ledger session（仅 metadata）。
 *
 * Business Logic（为什么需要）:
 *   明细表与 session 详情卡消费；必须不含 transcript/cwd/nativeSessionId。
 *
 * Code Logic（做什么）:
 *   cost 拆 costMinorUnits/costCurrency；token 全部 nullable。
 */
export const agentLedgerSessionEntryDecoder: Decoder<AgentLedgerSessionEntry> = objectDecoder(
  'AgentLedgerSessionEntry',
  {
    id: stringDecoder,
    agentSessionId: stringDecoder,
    projectId: stringDecoder,
    worktreeId: nullableDecoder(stringDecoder),
    providerId: stringDecoder,
    modelId: nullableDecoder(stringDecoder),
    startedAt: stringDecoder,
    endedAt: stringDecoder,
    durationMs: nonNegativeIntDecoder,
    outcome: tokenStatsOutcomeDecoder,
    inputTokens: nullableDecoder(nonNegativeIntDecoder),
    outputTokens: nullableDecoder(nonNegativeIntDecoder),
    cacheReadTokens: nullableDecoder(nonNegativeIntDecoder),
    cacheWriteTokens: nullableDecoder(nonNegativeIntDecoder),
    costMinorUnits: nullableDecoder(nonNegativeIntDecoder),
    costCurrency: nullableDecoder(stringDecoder),
    terminalTitle: nullableDecoder(stringDecoder),
  },
);

/** 分页结果。 */
export const agentLedgerSessionPageDecoder: Decoder<AgentLedgerSessionPage> = objectDecoder(
  'AgentLedgerSessionPage',
  {
    items: arrayDecoder(agentLedgerSessionEntryDecoder),
    nextCursor: nullableDecoder(stringDecoder),
  },
);

/** `clear_agent_ledger` 返回删除行数（>=0）。 */
export const clearTokenStatsResultDecoder: Decoder<number> = nonNegativeIntDecoder;

/* ----------------------- pure helpers ----------------------- */

/**
 * 计算 cache 命中率。
 *
 * Business Logic（为什么需要）:
 *   KPI 卡直接展示命中率；分母为 0 或任一字段缺失都必须显式 null，
 *   UI 才能显示「未提供」而不是 0%。
 *
 * Code Logic（做什么）:
 *   任一 null 或 (cacheRead+input)===0 → null；
 *   否则 cacheRead / (cacheRead + input)。
 *
 * @param cacheRead cache_read tokens（可为 null）
 * @param input input tokens（可为 null）
 */
export function computeCacheHitRate(
  cacheRead: number | null,
  input: number | null,
): number | null {
  if (cacheRead == null || input == null) return null;
  if (!Number.isFinite(cacheRead) || !Number.isFinite(input)) return null;
  const denom = cacheRead + input;
  if (denom === 0) return null;
  return cacheRead / denom;
}

/**
 * 计算真实消耗 token（input + cacheWrite + output）。
 *
 * Business Logic（为什么需要）:
 *   realConsumed 是 Anthropic 计费的真实视角；旧 ledger 行可能三项全 null，
 *   必须显式 null 透传。
 *
 * Code Logic（做什么）:
 *   三项全 null → null；否则 (i ?? 0) + (cw ?? 0) + (o ?? 0)。
 *
 * @param input input tokens
 * @param cacheWrite cache_creation tokens
 * @param output output tokens
 */
export function computeRealConsumed(
  input: number | null | undefined,
  cacheWrite: number | null | undefined,
  output: number | null | undefined,
): number | null {
  const i = input ?? null;
  const cw = cacheWrite ?? null;
  const o = output ?? null;
  if (i == null && cw == null && o == null) return null;
  return (i ?? 0) + (cw ?? 0) + (o ?? 0);
}

/**
 * 主货币 picker：把 currency list 切成 primary + others。
 *
 * Business Logic（为什么需要）:
 *   KPI total cost 大数展示统一用主货币；多币种场景用 chips 兜底而不折算。
 *
 * Code Logic（做什么）:
 *   1) 从 rows 中筛出常用货币集合里的项（USD/EUR/CNY/JPY/GBP）；
 *      若非空，取其中 minorUnits 最大作为 primary；others = 余下按 minorUnits desc。
 *   2) 否则 primary = 全部 rows 中 minorUnits 最大；others = 余下按 minorUnits desc。
 *   3) 空数组 → primary = null，others = []。
 *
 * @param rows 货币桶（可能空）
 * @param systemLocale 当前语言（影响主货币偏好；只用作辅助排序 hint）
 */
export function pickPrimaryCurrencyCost(
  rows: CurrencyAmount[],
  systemLocale: string,
): { primary: CurrencyAmount | null; others: CurrencyAmount[] } {
  if (rows.length === 0) return { primary: null, others: [] };

  // systemLocale 仅用作 hint；偏好顺序按 zh → CNY 在前，其它 → USD 在前。
  const priority = systemLocale.toLowerCase().startsWith('zh')
    ? ['CNY', 'USD', 'EUR', 'JPY', 'GBP']
    : ['USD', 'EUR', 'CNY', 'JPY', 'GBP'];

  const commonRows = priority
    .map((cur) => rows.find((r) => r.currency === cur))
    .filter((r): r is CurrencyAmount => Boolean(r));

  const orderedAll = [...rows].sort((a, b) => b.minorUnits - a.minorUnits);

  if (commonRows.length > 0) {
    const primary = commonRows.sort((a, b) => b.minorUnits - a.minorUnits)[0]!;
    const others = orderedAll.filter((r) => r !== primary);
    return { primary, others };
  }

  return { primary: orderedAll[0] ?? null, others: orderedAll.slice(1) };
}

/**
 * 按 ISO 字符串对趋势点排序 + 去重（保留首次出现）。
 *
 * Business Logic（为什么需要）:
 *   Trend 图必须按时间升序渲染；后端聚合已排序但 mock/前端缓存可能乱序。
 *
 * Code Logic（做什么）:
 *   1) 按 bucketStart 字典序升序（RFC3339 UTC 字典序 = 时序）；
 *   2) 同 bucketStart 保留首次出现；
 *   3) 复制后排序，原数组不变（pure）。
 *
 * @param points 原始 trend points
 */
export function bucketizeTrendByISO(points: AgentLedgerTrendPoint[]): AgentLedgerTrendPoint[] {
  const seen = new Set<string>();
  const uniq: AgentLedgerTrendPoint[] = [];
  for (const p of points) {
    if (seen.has(p.bucketStart)) continue;
    seen.add(p.bucketStart);
    uniq.push(p);
  }
  return [...uniq].sort((a, b) => (a.bucketStart < b.bucketStart ? -1 : a.bucketStart > b.bucketStart ? 1 : 0));
}
