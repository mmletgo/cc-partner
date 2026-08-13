/**
 * Business Logic（为什么需要这个函数）:
 *   多屏健康遮罩必须基于同一个后端权威结束时间展示一致且不会为负数的休息倒计时。
 *
 * Code Logic（这个函数做什么）:
 *   用结束 Unix 秒减去当前 Unix 秒并 clamp 到 0；nowSec 可注入以便稳定测试。
 */
export function computeRestLeft(endTs: number, nowSec?: number): number {
  const now = nowSec ?? Math.floor(Date.now() / 1000);
  return Math.max(0, endTs - now);
}
