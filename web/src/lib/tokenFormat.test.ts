/**
 * formatTokenCount 单元测试
 *
 * Business Logic（为什么需要这个测试文件）:
 *   Agent 使用统计的 token 展示口径（>5,000 → k，>=1,000,000 → M，3 位小数）
 *   是用户可见的格式约定，必须防止回归。
 *
 * Code Logic（这个测试文件做什么）:
 *   直接断言 formatTokenCount 的边界值输出（原样 / k / M / null）。
 */
import { describe, expect, it } from 'vitest';
import { formatTokenCount, formatTokenRate } from './tokenFormat';

describe('formatTokenCount', () => {
  it('5000 及以下直接显示整数', () => {
    expect(formatTokenCount(0)).toBe('0');
    expect(formatTokenCount(4999)).toBe('4999');
    expect(formatTokenCount(5000)).toBe('5000');
  });

  it('超过 5000 以 k 为单位并保留 3 位小数', () => {
    expect(formatTokenCount(5001)).toBe('5.001k');
    expect(formatTokenCount(12345)).toBe('12.345k');
    expect(formatTokenCount(999_999)).toBe('999.999k');
  });

  it('达到 1,000,000 以 M 为单位并保留 3 位小数', () => {
    expect(formatTokenCount(1_000_000)).toBe('1.000M');
    expect(formatTokenCount(1_234_567)).toBe('1.235M');
    expect(formatTokenCount(52_000_000)).toBe('52.000M');
  });

  it('null / 非有限数 / 负数返回 null（由调用方显示「未提供」）', () => {
    expect(formatTokenCount(null)).toBeNull();
    expect(formatTokenCount(undefined)).toBeNull();
    expect(formatTokenCount(Number.NaN)).toBeNull();
    expect(formatTokenCount(-1)).toBeNull();
  });
});

describe('formatTokenRate', () => {
  it('< 10 tok/s 保留 2 位小数', () => {
    expect(formatTokenRate(0)).toBe('0.00 tok/s');
    expect(formatTokenRate(0.05)).toBe('0.05 tok/s');
    expect(formatTokenRate(4.32)).toBe('4.32 tok/s');
    expect(formatTokenRate(9.999)).toBe('10.00 tok/s');
  });

  it('10 ≤ tps < 1000 保留 1 位小数', () => {
    expect(formatTokenRate(10)).toBe('10.0 tok/s');
    expect(formatTokenRate(123.4)).toBe('123.4 tok/s');
    expect(formatTokenRate(999.9)).toBe('999.9 tok/s');
  });

  it('>= 1000 tok/s 以 k 缩写（2 位小数）', () => {
    expect(formatTokenRate(1_000)).toBe('1.00k tok/s');
    expect(formatTokenRate(12_345)).toBe('12.35k tok/s');
  });

  it('>= 1,000,000 tok/s 以 M 缩写（2 位小数）', () => {
    expect(formatTokenRate(1_000_000)).toBe('1.00M tok/s');
    expect(formatTokenRate(2_500_000)).toBe('2.50M tok/s');
  });

  it('null / undefined / NaN / 负数返回 null', () => {
    expect(formatTokenRate(null)).toBeNull();
    expect(formatTokenRate(undefined)).toBeNull();
    expect(formatTokenRate(Number.NaN)).toBeNull();
    expect(formatTokenRate(-0.5)).toBeNull();
  });
});
