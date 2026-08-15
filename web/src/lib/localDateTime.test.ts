/**
 * formatLocalDateTimeSeconds 单元测试
 *
 * Business Logic（为什么需要这个测试文件）:
 *   Agent 使用统计的时间展示口径（本机时区、精确到秒、无 +00:00 偏移、无微秒小数）
 *   是用户可见约定，必须防止回归为原始 RFC3339 字符串。
 *
 * Code Logic（这个测试文件做什么）:
 *   用形态断言（不绑定具体时区/分隔符 locale）：包含年月日与 时:分:秒，
 *   不含 'T'/时区偏移 '+'/微秒小数；非法输入原样返回。
 */
import { describe, expect, it } from 'vitest';
import { formatLocalDateTimeSeconds } from './localDateTime';

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
