/**
 * splitDurationParts 单元测试
 *
 * Business Logic（为什么需要这个测试文件）:
 *   Agent 使用统计的时长展示口径（毫秒 → 天/时/分/秒，秒向下取整）
 *   是用户可见约定，必须防止回归。
 *
 * Code Logic（这个测试文件做什么）:
 *   直接断言拆分边界（跨天、全零、毫秒截断、非法输入 null）。
 */
import { describe, expect, it } from 'vitest';
import { splitDurationParts } from './durationFormat';

describe('splitDurationParts', () => {
  it('毫秒被换算为天/时/分/秒分量', () => {
    // 1天 1小时 1分 1秒 = 90061000 ms
    expect(splitDurationParts(90_061_000)).toEqual({
      days: 1,
      hours: 1,
      minutes: 1,
      seconds: 1,
    });
  });

  it('秒向下取整，不四舍五入进位', () => {
    expect(splitDurationParts(59_999)).toEqual({
      days: 0,
      hours: 0,
      minutes: 0,
      seconds: 59,
    });
  });

  it('零与非法输入：零返回全零分量，非法返回 null', () => {
    expect(splitDurationParts(0)).toEqual({ days: 0, hours: 0, minutes: 0, seconds: 0 });
    expect(splitDurationParts(null)).toBeNull();
    expect(splitDurationParts(Number.NaN)).toBeNull();
    expect(splitDurationParts(-1)).toBeNull();
  });
});
