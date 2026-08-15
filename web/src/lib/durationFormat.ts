/**
 * 时长拆分（纯函数，不依赖 i18n 模块初始化）
 */

/** 时长拆分结果：天/时/分/秒 各分量。 */
export interface DurationParts {
  days: number;
  hours: number;
  minutes: number;
  seconds: number;
}

/**
 * splitDurationParts
 *
 * Business Logic（为什么需要这个函数）:
 *   Agent 使用统计的时长原始值为毫秒,直接展示(如 90061000 ms)无法直观感知;
 *   需要按天/时/分/秒自动划分且精确到秒,由视图层按 locale 拼接单位文案。
 *
 * Code Logic（这个函数做什么）:
 *   非有限数或负数返回 null(交由调用方显示「未提供」);
 *   否则整除换算 d/h/m/s,秒向下取整(不四舍五入,避免 59.9s 进位到 1m)。
 */
export function splitDurationParts(ms: number | null | undefined): DurationParts | null {
  if (ms == null || !Number.isFinite(ms) || ms < 0) return null;
  const totalSeconds = Math.floor(ms / 1000);
  const days = Math.floor(totalSeconds / 86_400);
  const hours = Math.floor((totalSeconds % 86_400) / 3_600);
  const minutes = Math.floor((totalSeconds % 3_600) / 60);
  const seconds = totalSeconds % 60;
  return { days, hours, minutes, seconds };
}
