/**
 * token 数格式化（纯函数，不依赖 i18n）
 */

/**
 * formatTokenCount
 *
 * Business Logic（为什么需要这个函数）:
 *   Agent 使用统计的 token 数动辄几十万,直接展示原始数字可读性差;
 *   超过 5,000 需要以 k(千)为单位、达到 1,000,000 以 M(百万)为单位展示,
 *   均保留 3 位小数,让用户快速感知量级。
 *
 * Code Logic（这个函数做什么）:
 *   非有限数或负数返回 null(交由调用方显示「未提供」);
 *   >= 1,000,000 → `(v/1e6).toFixed(3) + 'M'`;
 *   > 5,000 → `(v/1e3).toFixed(3) + 'k'`;
 *   其余直接返回整数字符串。
 */
export function formatTokenCount(value: number | null | undefined): string | null {
  if (value == null || !Number.isFinite(value) || value < 0) return null;
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(3)}M`;
  if (value > 5_000) return `${(value / 1_000).toFixed(3)}k`;
  return String(value);
}

/**
 * formatTokenRate
 *
 * Business Logic（为什么需要这个函数）:
 *   工作台右侧「当前会话」卡需要把终态平均速率（output_tokens / durationMs * 1000）
 *   格式化为「12.3 tok/s」一类的紧凑可读字符串；速率的量级通常远小于累计 tokens，
 *   但仍需按 k/M 收敛到合理位数，避免 0.000123 tok/s 这种噪声读数。
 *
 * Code Logic（这个函数做什么）:
 *   - 非有限数 /  < 0 → null（交由调用方显示「未提供」）；
 *   - 单位为 `tok/s`（与 ccstatusline 一致）；
 *   - < 10：保留 2 位小数（`4.32 tok/s`）；
 *   - < 1,000：保留 1 位小数（`123.4 tok/s`）；
 *   - >= 1,000：以 k 缩写保留 2 位小数（`1.23k tok/s`）；
 *   - >= 1,000,000：以 M 缩写保留 2 位小数（`1.23M tok/s`）。
 */
/**
 * formatContextTokens
 *
 * Business Logic（为什么需要这个函数）:
 *   状态卡「上下文用量 / 上下文长度」必须对齐 ccstatusline-zh：
 *   占用通常在 k 量级，不得把累计计费百万 token 格式化成 M 误导用户。
 *   仅当 k 值会进位成 1000k 时才升到 1.0M。
 *
 * Code Logic（这个函数做什么）:
 *   对照 ccstatusline `formatTokens(count, decimals)`：
 *   - 非有限 / 负数 → null；
 *   - >= 1_000_000 - 500/10^decimals → 1 位小数 M；
 *   - >= 1000 → decimals 位小数 k；
 *   - 其余整数。
 */
export function formatContextTokens(
  value: number | null | undefined,
  decimals = 1,
): string | null {
  if (value == null || !Number.isFinite(value) || value < 0) return null;
  const promoteAt = 1_000_000 - 500 / 10 ** decimals;
  if (value >= promoteAt) return `${(value / 1_000_000).toFixed(1)}M`;
  if (value >= 1000) return `${(value / 1000).toFixed(decimals)}k`;
  return String(Math.round(value));
}

export function formatTokenRate(value: number | null | undefined): string | null {
  if (value == null || !Number.isFinite(value) || value < 0) return null;
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(2)}M tok/s`;
  if (value >= 1_000) return `${(value / 1_000).toFixed(2)}k tok/s`;
  if (value >= 10) return `${value.toFixed(1)} tok/s`;
  return `${value.toFixed(2)} tok/s`;
}

/**
 * formatFirstTokenLatency
 *
 * Business Logic（为什么需要这个函数）:
 *   状态卡「首 token 平均」要把「发出指令 → 首条回复」均值显示成可读时长。
 *
 * Code Logic（这个函数做什么）:
 *   - 非有限 / ≤0 → null；
 *   - < 1s → 整数 ms；
 *   - < 10s → 2 位小数 s；
 *   - 其余 1 位小数 s。
 */
export function formatFirstTokenLatency(ms: number | null | undefined): string | null {
  if (ms == null || !Number.isFinite(ms) || ms <= 0) return null;
  if (ms < 1000) return `${Math.round(ms)} ms`;
  if (ms < 10_000) return `${(ms / 1000).toFixed(2)} s`;
  return `${(ms / 1000).toFixed(1)} s`;
}

/**
 * formatCacheHitRate
 *
 * Business Logic（为什么需要这个函数）:
 *   状态卡需要把 0–1 命中率显示成一位小数百分比。
 *
 * Code Logic（这个函数做什么）:
 *   非有限 / 越界 → null；否则 `87.0%`。
 */
export function formatCacheHitRate(rate: number | null | undefined): string | null {
  if (rate == null || !Number.isFinite(rate) || rate < 0 || rate > 1) return null;
  return `${(rate * 100).toFixed(1)}%`;
}
