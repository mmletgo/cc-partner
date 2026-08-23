/** 同一前台恢复事件可能连发 visibilitychange/pageshow/online，最短间隔避免重复重建。 */
export const DOCUMENT_RESUME_RECONNECT_MIN_INTERVAL_MS = 1_000;

/**
 * 页面从后台恢复时是否应立刻重建长连接。
 *
 * Business Logic（为什么需要这个函数）:
 *   手机浏览器缩到后台几分钟后，fetch NDJSON 与输入 WebSocket 会变成半开连接；
 *   JS 定时器被冻结，35s watchdog 不会在后台触发。回到前台必须主动重连，
 *   不能等半开流自己报错，也不能在首屏握手完成前误杀新建连接。
 *
 * Code Logic（这个函数做什么）:
 *   hidden → false；bfcache pageshow（persisted）→ true；
 *   尚未建立会话且未见过后台 → false；距上次重连不足 minInterval → false；
 *   其余可见且（已建立会话或曾进入后台）→ true。
 */
export function shouldReconnectOnDocumentResume(input: {
  visible: boolean;
  persistedPageshow?: boolean;
  hasEstablishedSession: boolean;
  wasBackgrounded?: boolean;
  nowMs: number;
  lastReconnectAtMs: number | null;
  minIntervalMs?: number;
}): boolean {
  const minIntervalMs = input.minIntervalMs ?? DOCUMENT_RESUME_RECONNECT_MIN_INTERVAL_MS;
  if (
    input.lastReconnectAtMs != null &&
    input.nowMs - input.lastReconnectAtMs < minIntervalMs
  ) {
    return false;
  }
  if (input.persistedPageshow) return true;
  if (!input.visible) return false;
  return input.hasEstablishedSession || Boolean(input.wasBackgrounded);
}
