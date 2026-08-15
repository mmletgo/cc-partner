/**
 * 本地时间格式化（纯函数，不依赖 i18n 模块初始化）
 */

/**
 * formatLocalDateTimeSeconds
 *
 * Business Logic（为什么需要这个函数）:
 *   Agent 使用统计等处的原始时间是 RFC3339 UTC 字符串
 *   (如 2026-08-14T13:12:45.530901+00:00),直接展示既带时区偏移又有微秒小数,
 *   用户需要看到按本机时区换算、精确到秒的本地时间。
 *
 * Code Logic（这个函数做什么）:
 *   用 Intl.DateTimeFormat(undefined, ...) 按系统 locale 与本机时区格式化,
 *   年月日 + 时:分:秒(24 小时制,无秒以下小数,无时区偏移后缀);
 *   非法/空输入原样返回字符串,便于排查。
 */
export function formatLocalDateTimeSeconds(iso: string | null | undefined): string {
  if (!iso) return '';
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  return new Intl.DateTimeFormat(undefined, {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
    hour12: false,
  }).format(date);
}
