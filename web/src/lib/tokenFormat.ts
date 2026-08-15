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
