/**
 * Token 统计自定义区间：datetime-local ↔ RFC3339。
 *
 * Business Logic（为什么需要这个模块）:
 *   datetime-local 控件用本地墙钟，后端只要 RFC3339 UTC。
 *
 * Code Logic（这个模块做什么）:
 *   非法/空值归一为空串或 null；合法值走 Date + toISOString。
 */

/**
 * Business Logic（为什么需要）:
 *   把已存 RFC3339 回填到 datetime-local 控件。
 *
 * Code Logic（做什么）:
 *   非法/空 ISO → 空串；否则写成 YYYY-MM-DDTHH:mm（本地）。
 */
export function datetimeLocalFromRfc3339(iso: string | null | undefined): string {
  if (!iso) return '';
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return '';
  const pad = (value: number) => String(value).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/**
 * Business Logic（为什么需要）:
 *   用户选完本地时间后必须转成 RFC3339 才能进 summarize/list/export。
 *
 * Code Logic（做什么）:
 *   空串 → null；非法日期 → null；否则 `toISOString()`。
 */
export function rfc3339FromDatetimeLocal(value: string): string | null {
  const trimmed = value.trim();
  if (!trimmed) return null;
  const date = new Date(trimmed);
  if (Number.isNaN(date.getTime())) return null;
  return date.toISOString();
}
