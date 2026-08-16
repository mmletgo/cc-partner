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

/** 趋势桶粒度（与 Token 统计 hour|day 对齐）。 */
export type LocalTimeBucket = 'hour' | 'day';

/**
 * Business Logic（为什么需要）:
 *   轴标签与 tooltip 共用同一套墙钟零件，避免 hour/day 各写一遍 Intl。
 *
 * Code Logic（做什么）:
 *   解析 RFC3339；非法返回原串；合法则按 IANA 时区取出年/月/日/时/分。
 */
function localWallParts(
  iso: string | null | undefined,
  timeZone?: string,
): { year: string; month: string; day: string; hour: string; minute: string } | string {
  if (!iso) return '';
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  const parts = new Intl.DateTimeFormat('en-US', {
    timeZone,
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    hourCycle: 'h23',
  }).formatToParts(date);
  const get = (type: Intl.DateTimeFormatPartTypes) =>
    parts.find((part) => part.type === type)?.value ?? '';
  return {
    year: get('year'),
    month: get('month'),
    day: get('day'),
    hour: get('hour'),
    minute: get('minute'),
  };
}

/**
 * formatLocalBucketLabel
 *
 * Business Logic（为什么需要这个函数）:
 *   Token 统计趋势图横轴需要短标签；旧实现切片 UTC ISO 子串，东八区用户
 *   会把 20:00 的用量看成 12:00。
 *
 * Code Logic（这个函数做什么）:
 *   解析 RFC3339，按可选 IANA 时区（缺省=设备时区）格式化为 hour→HH:mm、
 *   day→MM-DD；非法/空输入与 formatLocalDateTimeSeconds 同口径。
 */
export function formatLocalBucketLabel(
  iso: string | null | undefined,
  bucket: LocalTimeBucket,
  timeZone?: string,
): string {
  const parts = localWallParts(iso, timeZone);
  if (typeof parts === 'string') return parts;
  return bucket === 'hour' ? `${parts.hour}:${parts.minute}` : `${parts.month}-${parts.day}`;
}

/**
 * formatLocalBucketTooltip
 *
 * Business Logic（为什么需要这个函数）:
 *   趋势图 tooltip 原先直接展示 RFC3339 UTC，需要换成设备时区的可读时间。
 *
 * Code Logic（这个函数做什么）:
 *   hour → YYYY-MM-DD HH:mm；day → YYYY-MM-DD；时区语义同 formatLocalBucketLabel。
 */
export function formatLocalBucketTooltip(
  iso: string | null | undefined,
  bucket: LocalTimeBucket,
  timeZone?: string,
): string {
  const parts = localWallParts(iso, timeZone);
  if (typeof parts === 'string') return parts;
  const date = `${parts.year}-${parts.month}-${parts.day}`;
  return bucket === 'hour' ? `${date} ${parts.hour}:${parts.minute}` : date;
}
