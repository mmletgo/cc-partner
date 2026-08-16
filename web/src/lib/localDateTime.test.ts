/**
 * 本地时间格式化单元测试
 *
 * Business Logic（为什么需要这个测试文件）:
 *   Agent 使用统计与 Token 趋势图的时间展示口径（设备时区、无 RFC3339 偏移）
 *   是用户可见约定，必须防止回归为 UTC 子串或原始 ISO。
 *
 * Code Logic（这个测试文件做什么）:
 *   formatLocalDateTimeSeconds 用形态断言；bucket 轴/tooltip 钉死
 *   Asia/Shanghai 与 America/Los_Angeles；非法输入原样返回。
 */
import { describe, expect, it } from 'vitest';
import {
  formatLocalBucketLabel,
  formatLocalBucketTooltip,
  formatLocalDateTimeSeconds,
} from './localDateTime';

describe('formatLocalDateTimeSeconds', () => {
  it('RFC3339 UTC 时间被格式化为本地时间且精确到秒', () => {
    const out = formatLocalDateTimeSeconds('2026-08-14T13:12:45.530901+00:00');
    // 日期部分（年月日顺序随系统 locale）+ 精确到秒的 时:分:秒
    expect(out).toMatch(/\d{4}/);
    expect(out).toMatch(/\d{1,2}:\d{2}:\d{2}$/);
    expect(out).not.toContain('T');
    expect(out).not.toContain('+00:00');
    expect(out).not.toContain('.530901');
  });

  it('非法或空输入原样返回（便于排查）', () => {
    expect(formatLocalDateTimeSeconds('')).toBe('');
    expect(formatLocalDateTimeSeconds(undefined)).toBe('');
    expect(formatLocalDateTimeSeconds('not-a-date')).toBe('not-a-date');
  });
});

describe('formatLocalBucketLabel', () => {
  it('hour 桶按指定时区显示 HH:mm，不切片 UTC 子串', () => {
    expect(formatLocalBucketLabel('2026-07-15T12:00:00Z', 'hour', 'Asia/Shanghai')).toBe('20:00');
    expect(formatLocalBucketLabel('2026-07-15T12:00:00Z', 'hour', 'America/Los_Angeles')).toBe(
      '05:00',
    );
  });

  it('day 桶按指定时区显示 MM-DD，跨日时落到本地日期', () => {
    expect(formatLocalBucketLabel('2026-07-15T00:00:00Z', 'day', 'Asia/Shanghai')).toBe('07-15');
    expect(formatLocalBucketLabel('2026-07-15T00:00:00Z', 'day', 'America/Los_Angeles')).toBe(
      '07-14',
    );
  });

  it('非法或空输入原样返回', () => {
    expect(formatLocalBucketLabel('', 'hour')).toBe('');
    expect(formatLocalBucketLabel(undefined, 'day')).toBe('');
    expect(formatLocalBucketLabel('not-a-date', 'hour')).toBe('not-a-date');
  });
});

describe('formatLocalBucketTooltip', () => {
  it('hour 桶 tooltip 显示本地 YYYY-MM-DD HH:mm', () => {
    expect(formatLocalBucketTooltip('2026-07-15T12:00:00Z', 'hour', 'Asia/Shanghai')).toBe(
      '2026-07-15 20:00',
    );
  });

  it('day 桶 tooltip 显示本地 YYYY-MM-DD，洛杉矶跨日到前一天', () => {
    expect(formatLocalBucketTooltip('2026-07-15T00:00:00Z', 'day', 'America/Los_Angeles')).toBe(
      '2026-07-14',
    );
  });

  it('非法或空输入原样返回', () => {
    expect(formatLocalBucketTooltip('', 'hour')).toBe('');
    expect(formatLocalBucketTooltip(undefined, 'day')).toBe('');
    expect(formatLocalBucketTooltip('not-a-date', 'hour')).toBe('not-a-date');
  });
});
