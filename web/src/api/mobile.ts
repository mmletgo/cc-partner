/**
 * 移动端访问入口 API。
 *
 * Business Logic（为什么需要这个模块）:
 *   桌面端需要向用户展示手机浏览器可访问的局域网 `/mobile` 地址。
 *
 * Code Logic（这个模块做什么）:
 *   封装 `/api/mobile/access-info` 同源 HTTP 调用，返回统一的 MobileAccessInfo DTO。
 */

import type { MobileAccessInfo } from '@/lib/types';
import { getJson } from './workbenchHttp';

/**
 * Business Logic（为什么需要这个函数）:
 *   设置页或移动端入口组件需要获取当前设备名、HTTP 端口和可访问 URL 列表。
 *
 * Code Logic（这个函数做什么）:
 *   GET `/api/mobile/access-info`，非 2xx 时沿用 HTTP helper 的 error/message 解析并抛出 Error。
 */
export function getMobileAccessInfo(): Promise<MobileAccessInfo> {
  return getJson<MobileAccessInfo>('/api/mobile/access-info');
}
