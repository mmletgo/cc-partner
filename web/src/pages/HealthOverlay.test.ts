import { describe, it, expect } from 'vitest';

import { computeRestLeft } from './HealthOverlay';

describe('computeRestLeft', () => {
  it('返回未来结束时间戳的剩余秒数', () => {
    expect(computeRestLeft(1000, 900)).toBe(100);
  });

  it('恰好在结束时间戳时返回 0', () => {
    expect(computeRestLeft(1000, 1000)).toBe(0);
  });

  it('结束时间戳已过时 clamp 到 0（多屏时钟漂移下不会显示负数）', () => {
    expect(computeRestLeft(1000, 2000)).toBe(0);
  });
});
