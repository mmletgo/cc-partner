/**
 * 充电剩余时间格式化纯函数。
 *
 * Business Logic（为什么需要这个模块）:
 *   footer title、卫星瘦 footer、设置页大号剩余必须共用同一套分钟/小时文案。
 *
 * Code Logic（这个模块做什么）:
 *   毫秒向下取整为分钟；>=60 分拆小时。
 */

import { BATTERY_MS_PER_MINUTE } from './types/battery';

/**
 * Business Logic（为什么需要这个函数）:
 *   用户看到的是工作分钟，不是毫秒。
 *
 * Code Logic（这个函数做什么）:
 *   floor(ms / 60000)，负数钳 0。
 */
export function remainingMinutesFromMs(ms: number): number {
  if (!Number.isFinite(ms) || ms <= 0) return 0;
  return Math.floor(ms / BATTERY_MS_PER_MINUTE);
}

/**
 * Business Logic（为什么需要这个函数）:
 *   title / 卫星剩余 / 设置大号数字需要可读时长。
 *
 * Code Logic（这个函数做什么）:
 *   0 → time.zero；<60 → time.minutes；否则 hoursMinutes。
 */
export function formatBatteryTime(ms: number, t: unknown): string {
  const minutes = remainingMinutesFromMs(ms);
  const translate = t as (key: string, options?: Record<string, unknown>) => string;
  if (minutes <= 0) return translate('time.zero');
  if (minutes < 60) return translate('time.minutes', { count: minutes });
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  return translate('time.hoursMinutes', { hours, minutes: rest });
}
